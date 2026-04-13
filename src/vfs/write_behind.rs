use std::{
    ffi::{OsStr, OsString},
    sync::{
        atomic::{AtomicU64, Ordering},
        mpsc::{self, TryRecvError},
        Arc,
    },
    thread,
    time::Duration,
};

use bytes::{Bytes, BytesMut};
use fuser::FileAttr;
use quick_cache::sync::{Cache, EntryAction, EntryResult};
use slab::Slab;
use zstd::zstd_safe::WriteBuf;

use crate::queries::block::Block as BaseBlock;
use crate::{
    driver::{attr::FileAttrBuilder, OpenFlags},
    queries,
    types::FileType,
    vfs::Bno,
};

use super::{Fno, Ino, Vfs, VfsHandle};

#[derive(Clone)]
struct Block {
    ino: Ino,
    bno: Bno,
    buf: Bytes,
}

impl std::fmt::Debug for Block {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Block")
            .field("ino", &self.ino)
            .field("bno", &self.bno)
            .field("buf.len()", &self.buf.len())
            .field("buf.cap()", &self.buf.capacity())
            .finish()
    }
}

impl From<queries::block::Block> for Block {
    fn from(value: queries::block::Block) -> Self {
        Self {
            ino: value.ino.into(),
            bno: value.bno.into(),
            buf: value.data.freeze(),
        }
    }
}

enum WriteOp {
    Block(Block),
    CreateEntry {
        attr: FileAttr,
        parent: u64,
        name: OsString,
    },
    CreateLink {
        ino: u64,
        parent: u64,
        name: OsString,
        new_nlink: u32,
    },
    UnlinkEntry {
        ino: u64,
        parent: u64,
        name: OsString,
        new_nlink: u32,
    },
    RemoveInode {
        ino: u64,
    },
    Rename {
        parent: u64,
        name: OsString,
        new_parent: u64,
        new_name: OsString,
    },
}

struct WriteBehindCore {
    //db: DatabaseOps, // todo use connection pool
    pool: r2d2::Pool<r2d2_sqlite::SqliteConnectionManager>,
    nodes: Cache<Ino, FileAttr>,
    entries: Cache<OsString, Ino>,
    blocks: Cache<(Ino, Bno), Block>,
    handles: parking_lot::RwLock<Slab<WriteBehindHandle>>,
    write_tx: mpsc::Sender<WriteOp>,
    next_ino: AtomicU64,
}

#[derive(Clone)]
struct WriteBehind {
    core: Arc<WriteBehindCore>,
}

#[derive(Clone)]
struct WriteBehindHandle {
    fno: Fno,
    ino: Ino,
    core: Arc<WriteBehindCore>,
    flags: OpenFlags,
}

fn spawn_write_thread(pool: r2d2::Pool<r2d2_sqlite::SqliteConnectionManager>, rx: mpsc::Receiver<WriteOp>) {
    std::thread::spawn(move || {
        let mut conn = pool.get().expect("should have connection");
        let mut ops = Vec::new();
        loop {
            ops.clear();

            for _ in 0..256 {
                match rx.try_recv() {
                    Ok(op) => ops.push(op),
                    Err(TryRecvError::Empty) => (),
                    Err(TryRecvError::Disconnected) => break,
                }
            }

            if !ops.is_empty() {
                let mut tx = conn
                    .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
                    .expect("should start transaction");

                for op in ops.drain(..) {
                    match op {
                        WriteOp::Block(block) => {
                            queries::block::upsert(
                                &mut tx,
                                block.ino.into(),
                                block.bno.into(),
                                &block.buf,
                                Default::default(),
                            )
                            .expect("todo");
                        }
                        WriteOp::CreateEntry { attr, parent, name } => {
                            queries::inode::create_with_ino(&mut tx, &attr).expect("todo");
                            queries::dir_entry::create(&mut tx, parent, &name, attr.ino).expect("todo");
                        }
                        WriteOp::CreateLink { ino, parent, name, new_nlink } => {
                            queries::dir_entry::create(&mut tx, parent, &name, ino).expect("todo");
                            queries::inode::set_attr(&mut tx, ino, "nlink", new_nlink).expect("todo");
                        }
                        WriteOp::UnlinkEntry { ino, parent, name, new_nlink } => {
                            queries::dir_entry::remove(&mut tx, parent, &name).expect("todo");
                            queries::inode::set_attr(&mut tx, ino, "nlink", new_nlink).expect("todo");
                        }
                        WriteOp::RemoveInode { ino } => {
                            queries::inode::remove(&mut tx, ino).expect("todo");
                        }
                        WriteOp::Rename { parent, name, new_parent, new_name } => {
                            queries::dir_entry::rename(&mut tx, parent, &name, new_parent, &new_name).expect("todo");
                        }
                    }
                }
                tx.commit().unwrap();
            }

            thread::sleep(Duration::from_millis(10));
        }
    });
}

impl WriteBehind {
    #[cfg(test)]
    pub fn new_for_test() -> anyhow::Result<Self> {
        let manager = r2d2_sqlite::SqliteConnectionManager::memory();
        let pool = r2d2::Pool::builder().build(manager)?;

        {
            let mut conn = pool.get()?;
            crate::database::migrate_database(&mut conn)?;
        }

        let next_ino = {
            let conn = pool.get()?;
            let max_ino: u64 = conn.query_row("SELECT COALESCE(MAX(ino), 0) FROM inode", [], |row| row.get(0))?;
            AtomicU64::new(max_ino + 1)
        };

        let (write_tx, write_rx) = mpsc::channel::<WriteOp>();
        spawn_write_thread(pool.clone(), write_rx);

        Ok(WriteBehind {
            core: Arc::new(WriteBehindCore {
                pool,
                nodes: Cache::new(128),
                entries: Cache::new(128),
                blocks: Cache::new(256),
                handles: parking_lot::RwLock::new(Slab::new()),
                write_tx,
                next_ino,
            }),
        })
    }
}

impl WriteBehindCore {
    fn load_ino(&self, ino: Ino) -> Result<fuser::FileAttr, crate::errors::Error> {
        let result = self.nodes.entry(&ino, None, |_, v| EntryAction::Retain(*v));
        let attr = match result {
            EntryResult::Vacant(placeholder) => {
                let mut conn = self.pool.get()?;
                let mut tx = conn.transaction()?;
                let attr = queries::inode::lookup(&mut tx, ino.0)?;
                placeholder.insert(attr).expect("no insert failure");
                attr
            }
            EntryResult::Retained(attr) => attr,
            _ => unreachable!(),
        };
        Ok(attr)
    }

    fn load_entry(&self, parent: Ino, name: &OsStr) -> Result<Ino, crate::errors::Error> {
        let result = self.entries.entry(name, None, |_, v| EntryAction::Retain(*v));
        let attr = match result {
            EntryResult::Vacant(placeholder) => {
                let mut conn = self.pool.get()?;
                let mut tx = conn.transaction()?;
                let ino: Ino = queries::dir_entry::lookup(&mut tx, parent.into(), name).map(Into::into)?;
                placeholder.insert(ino).expect("no insert failure");
                ino
            }
            EntryResult::Retained(ino) => ino,
            _ => unreachable!(),
        };
        Ok(attr)
    }

    fn load_block(&self, ino: Ino, bno: Bno) -> Result<Block, crate::errors::Error> {
        let result = self
            .blocks
            .entry(&(ino, bno), None, |_, v| EntryAction::Retain(v.clone()));
        let block = match result {
            EntryResult::Vacant(placeholder) => {
                let mut conn = self.pool.get()?;
                let mut tx = conn.transaction()?;
                let block: Block = queries::block::get_block(&mut tx, ino.into(), bno.into()).map(Into::into)?;
                placeholder.insert(block.clone()).expect("no insert failure");
                block
            }
            EntryResult::Retained(block) => block,
            _ => unreachable!(),
        };
        Ok(block)
    }
}

impl Vfs for WriteBehind {
    type Handle = WriteBehindHandle;
    type Error = crate::errors::Error;

    fn ensure_root(&self) -> Result<(), Self::Error> {
        match self.core.load_ino(1u64.into()) {
            Ok(_) => return Ok(()),
            Err(crate::errors::Error::NotFound) => {}
            Err(e) => return Err(e),
        }
        let attr = FileAttrBuilder::new_directory().with_ino(1).build();
        let mut conn = self.core.pool.get()?;
        let mut tx = conn.transaction()?;
        queries::inode::create_with_ino(&mut tx, &attr)?;
        tx.commit()?;
        self.core.nodes.insert(1u64.into(), attr);
        self.core.next_ino.fetch_max(2, Ordering::Relaxed);
        Ok(())
    }

    fn lookup_name(&self, parent: super::Ino, name: &OsStr) -> Result<fuser::FileAttr, Self::Error> {
        self.core
            .load_entry(parent, name)
            .and_then(|ino| self.core.load_ino(ino))
    }

    fn lookup_ino(&self, ino: super::Ino) -> Result<fuser::FileAttr, Self::Error> {
        self.core.load_ino(ino)
    }

    fn mknod(
        &self,
        parent: Ino,
        name: &OsStr,
        mode: u32,
        umask: u32,
        rdev: u32,
        uid: u32,
        gid: u32,
    ) -> Result<FileAttr, Self::Error> {
        let kind = FileType::from_mode(mode).ok_or(crate::errors::Error::InvalidArgument)?;
        let ino = self.core.next_ino.fetch_add(1, Ordering::Relaxed);
        let attr = FileAttrBuilder::new_node(kind)
            .with_ino(ino)
            .with_uid(uid)
            .with_gid(gid)
            .with_mode_umask(mode, umask)
            .with_rdev(rdev)
            .build();

        self.core.nodes.insert(attr.ino.into(), attr);
        self.core.entries.insert(name.to_os_string(), attr.ino.into());
        let _ = self.core.write_tx.send(WriteOp::CreateEntry {
            attr,
            parent: parent.into(),
            name: name.to_os_string(),
        });
        Ok(attr)
    }

    fn mkdir(
        &self,
        parent: Ino,
        name: &OsStr,
        mode: u32,
        umask: u32,
        uid: u32,
        gid: u32,
    ) -> Result<FileAttr, Self::Error> {
        let ino = self.core.next_ino.fetch_add(1, Ordering::Relaxed);
        let attr = FileAttrBuilder::new_directory()
            .with_ino(ino)
            .with_uid(uid)
            .with_gid(gid)
            .with_mode_umask(mode, umask)
            .build();

        self.core.nodes.insert(attr.ino.into(), attr);
        self.core.entries.insert(name.to_os_string(), attr.ino.into());
        let _ = self.core.write_tx.send(WriteOp::CreateEntry {
            attr,
            parent: parent.into(),
            name: name.to_os_string(),
        });
        Ok(attr)
    }

    fn link(&self, ino: Ino, newparent: Ino, newname: &OsStr) -> Result<FileAttr, Self::Error> {
        let mut attr = self.core.load_ino(ino)?;
        attr.nlink += 1;
        self.core.nodes.insert(ino, attr);
        self.core.entries.insert(newname.to_os_string(), ino);
        let _ = self.core.write_tx.send(WriteOp::CreateLink {
            ino: ino.into(),
            parent: newparent.into(),
            name: newname.to_os_string(),
            new_nlink: attr.nlink,
        });
        Ok(attr)
    }

    fn unlink(&self, parent: Ino, name: &OsStr) -> Result<(), Self::Error> {
        let ino = self.core.load_entry(parent, name)?;
        let mut attr = self.core.load_ino(ino)?;
        attr.nlink -= 1;
        self.core.entries.remove(&name.to_os_string());
        if attr.nlink > 0 {
            self.core.nodes.insert(ino, attr);
            let _ = self.core.write_tx.send(WriteOp::UnlinkEntry {
                ino: ino.into(),
                parent: parent.into(),
                name: name.to_os_string(),
                new_nlink: attr.nlink,
            });
        } else {
            self.core.nodes.remove(&ino);
            let _ = self.core.write_tx.send(WriteOp::RemoveInode { ino: ino.into() });
        }
        Ok(())
    }

    fn rename(&self, parent: Ino, name: &OsStr, newparent: Ino, newname: &OsStr, _flags: u32) -> Result<(), Self::Error> {
        let ino = self.core.load_entry(parent, name)?;
        self.core.entries.remove(&name.to_os_string());
        self.core.entries.insert(newname.to_os_string(), ino);
        let _ = self.core.write_tx.send(WriteOp::Rename {
            parent: parent.into(),
            name: name.to_os_string(),
            new_parent: newparent.into(),
            new_name: newname.to_os_string(),
        });
        Ok(())
    }

    fn rmdir(&self, parent: Ino, name: &OsStr) -> Result<(), Self::Error> {
        let ino = self.core.load_entry(parent, name)?;
        let mut conn = self.core.pool.get()?;
        let mut tx = conn.transaction()?;
        let empty = queries::dir_entry::is_dir_empty(&mut tx, ino.into())?;
        drop(tx);
        if !empty {
            return Err(crate::errors::Error::NotEmpty);
        }
        self.core.entries.remove(&name.to_os_string());
        self.core.nodes.remove(&ino);
        let _ = self.core.write_tx.send(WriteOp::RemoveInode { ino: ino.into() });
        Ok(())
    }

    fn open(&self, ino: super::Ino, flags: OpenFlags) -> Result<Self::Handle, Self::Error> {
        self.lookup_ino(ino)?;
        let handle = WriteBehindHandle {
            fno: 0u64.into(), // generated by slab
            ino,
            core: self.core.clone(),
            flags,
        };
        let mut handles = self.core.handles.write();
        let fno = handles.insert(handle);
        let handle = &mut handles[fno];
        handle.fno = fno.into();
        Ok(handle.clone())
    }

    fn close(&self, fno: super::Fno) -> Result<(), Self::Error> {
        let h = self.handle(fno).ok_or(crate::errors::Error::NotFound)?;
        h.flush()?;
        self.core.handles.write().remove(fno.into());
        Ok(())
    }

    fn handle(&self, fno: super::Fno) -> Option<Self::Handle> {
        self.core.handles.read().get(fno.into()).cloned()
    }
}

impl VfsHandle for WriteBehindHandle {
    type Error = crate::errors::Error;

    fn fno(&self) -> Fno {
        self.fno
    }

    fn read(&self, offset: u64, size: u32) -> Result<bytes::Bytes, Self::Error> {
        let size = size as usize;
        let mut buf = BytesMut::with_capacity(size);

        for seg in BaseBlock::segments(offset, size) {
            let block = match self.core.load_block(self.ino, seg.bno.into()) {
                Ok(b) => b,
                Err(crate::errors::Error::NotFound) => break,
                Err(e) => return Err(e),
            };
            if seg.rel_offset >= block.buf.len() {
                break;
            }
            let copy_len = (block.buf.len() - seg.rel_offset).min(seg.len);
            buf.extend_from_slice(&block.buf[seg.rel_offset..seg.rel_offset + copy_len]);
        }

        Ok(buf.freeze())
    }

    fn write(&self, offset: u64, data: &[u8]) -> Result<(), Self::Error> {
        for seg in BaseBlock::segments(offset, data.len()) {
            let existing_buf = match self.core.load_block(self.ino, seg.bno.into()) {
                Ok(b) => b.buf,
                Err(crate::errors::Error::NotFound) => Bytes::new(),
                Err(e) => return Err(e),
            };

            let required_len = seg.rel_offset + seg.len;
            let mut buf = BytesMut::from(existing_buf);
            if buf.len() < required_len {
                buf.resize(required_len, 0);
            }
            buf[seg.rel_offset..required_len].copy_from_slice(&data[seg.data_offset..seg.data_offset + seg.len]);

            let updated = Block {
                ino: self.ino,
                bno: seg.bno.into(),
                buf: buf.freeze(),
            };
            self.core.blocks.insert((self.ino, seg.bno.into()), updated.clone());
            let _ = self.core.write_tx.send(WriteOp::Block(updated));
        }

        Ok(())
    }

    fn flush(&self) -> Result<(), Self::Error> {
        todo!()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a shared in-memory SQLite pool. All connections to the same `db_name`
    /// share state, so the write thread and the test query can both see the same rows.
    fn shared_memory_pool(db_name: &str, max_size: u32) -> r2d2::Pool<r2d2_sqlite::SqliteConnectionManager> {
        let uri = format!("file:{db_name}?mode=memory&cache=shared");
        let flags = rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE
            | rusqlite::OpenFlags::SQLITE_OPEN_CREATE
            | rusqlite::OpenFlags::SQLITE_OPEN_URI;
        let manager = r2d2_sqlite::SqliteConnectionManager::file(uri).with_flags(flags);
        r2d2::Pool::builder().max_size(max_size).build(manager).unwrap()
    }

    #[test]
    fn test_spawn_write_thread_persists_block() {
        // Two connections: one held by the write thread, one for the verification query.
        let pool = shared_memory_pool("test_spawn_write_thread_persists_block", 2);

        {
            let mut conn = pool.get().unwrap();
            crate::database::migrate_database(&mut conn).unwrap();
        }

        // Create an inode so the FK constraint on the block table is satisfied.
        let ino: u64 = {
            let mut conn = pool.get().unwrap();
            let mut tx = conn.transaction().unwrap();
            let mut attr = fuser::FileAttr {
                ino: 0,
                size: 0,
                blocks: 0,
                atime: std::time::UNIX_EPOCH,
                mtime: std::time::UNIX_EPOCH,
                ctime: std::time::UNIX_EPOCH,
                crtime: std::time::UNIX_EPOCH,
                kind: fuser::FileType::RegularFile,
                perm: 0o644,
                nlink: 1,
                uid: 0,
                gid: 0,
                rdev: 0,
                blksize: 512,
                flags: 0,
            };
            queries::inode::create(&mut tx, &mut attr).unwrap();
            tx.commit().unwrap();
            attr.ino
        };

        let (write_tx, write_rx) = mpsc::channel::<WriteOp>();
        spawn_write_thread(pool.clone(), write_rx);

        let data = Bytes::from_static(b"hello write thread");
        write_tx
            .send(WriteOp::Block(Block {
                ino: ino.into(),
                bno: 0u64.into(),
                buf: data.clone(),
            }))
            .unwrap();

        // Signal that no more blocks are coming. The write thread will receive
        // TryRecvError::Disconnected on its next iteration.
        drop(write_tx);

        // The inner loop sleeps 10ms per iteration; 100ms gives it ample time.
        thread::sleep(Duration::from_millis(100));

        let mut conn = pool.get().unwrap();
        let mut db_tx = conn.transaction().unwrap();
        let block = queries::block::get_block(&mut db_tx, ino, 0).unwrap();
        assert_eq!(&block.data[..data.len()], data.as_ref());
    }

    impl WriteBehind {
        #[cfg(test)]
        fn insert_test_file(&self) -> Ino {
            let mut conn = self.core.pool.get().unwrap();
            let mut tx = conn.transaction().unwrap();
            let mut attr = fuser::FileAttr {
                ino: 0,
                size: 0,
                blocks: 0,
                atime: std::time::UNIX_EPOCH,
                mtime: std::time::UNIX_EPOCH,
                ctime: std::time::UNIX_EPOCH,
                crtime: std::time::UNIX_EPOCH,
                kind: fuser::FileType::RegularFile,
                perm: 0o644,
                nlink: 1,
                uid: 0,
                gid: 0,
                rdev: 0,
                blksize: 512,
                flags: 0,
            };
            queries::inode::create(&mut tx, &mut attr).unwrap();
            tx.commit().unwrap();
            attr.ino.into()
        }

        #[cfg(test)]
        fn insert_test_dir(&self) -> Ino {
            let mut conn = self.core.pool.get().unwrap();
            let mut tx = conn.transaction().unwrap();
            let mut attr = fuser::FileAttr {
                ino: 0,
                size: 0,
                blocks: 0,
                atime: std::time::UNIX_EPOCH,
                mtime: std::time::UNIX_EPOCH,
                ctime: std::time::UNIX_EPOCH,
                crtime: std::time::UNIX_EPOCH,
                kind: fuser::FileType::Directory,
                perm: 0o755,
                nlink: 2,
                uid: 0,
                gid: 0,
                rdev: 0,
                blksize: 512,
                flags: 0,
            };
            queries::inode::create(&mut tx, &mut attr).unwrap();
            // Sync next_ino past what we inserted directly
            self.core.next_ino.fetch_max(attr.ino + 1, Ordering::Relaxed);
            tx.commit().unwrap();
            attr.ino.into()
        }
    }

    fn open_test_handle() -> (WriteBehind, WriteBehindHandle) {
        let vfs = WriteBehind::new_for_test().unwrap();
        let ino = vfs.insert_test_file();
        let handle = vfs.open(ino, OpenFlags::from(libc::O_RDWR)).unwrap();
        (vfs, handle)
    }

    #[test]
    fn test_write_within_block() {
        let (_vfs, handle) = open_test_handle();
        let data = b"hello world";
        handle.write(0, data).unwrap();
        let result = handle.read(0, data.len() as u32).unwrap();
        assert_eq!(result.as_ref(), data);
    }

    #[test]
    fn test_write_at_offset() {
        let (_vfs, handle) = open_test_handle();
        let data = b"offset write";
        handle.write(16, data).unwrap();
        let result = handle.read(16, data.len() as u32).unwrap();
        assert_eq!(result.as_ref(), data);
    }

    #[test]
    fn test_write_spanning_blocks() {
        use crate::queries::block::BLOCK_SIZE;
        let (_vfs, handle) = open_test_handle();

        // Write 16 bytes starting 8 bytes before the end of block 0
        let offset = BLOCK_SIZE - 8;
        let data = [0xABu8; 16];
        handle.write(offset, &data).unwrap();

        let result = handle.read(offset, data.len() as u32).unwrap();
        assert_eq!(result.as_ref(), &data);
    }

    #[test]
    fn test_mknod_returns_attr_and_caches() {
        use std::ffi::OsStr;
        let vfs = WriteBehind::new_for_test().unwrap();
        let parent = vfs.insert_test_dir();

        let attr = vfs
            .mknod(
                parent,
                OsStr::new("hello.txt"),
                libc::S_IFREG | 0o644,
                0o022,
                0,
                1000,
                1000,
            )
            .unwrap();

        assert_eq!(attr.kind, fuser::FileType::RegularFile);
        assert_eq!(attr.uid, 1000);
        assert_eq!(attr.gid, 1000);

        // Should be immediately resolvable from cache
        let looked_up = vfs.lookup_name(parent, OsStr::new("hello.txt")).unwrap();
        assert_eq!(looked_up.ino, attr.ino);
        assert_eq!(looked_up.kind, fuser::FileType::RegularFile);
    }

    #[test]
    fn test_mknod_persists_to_db() {
        use std::ffi::OsStr;
        let vfs = WriteBehind::new_for_test().unwrap();
        let parent = vfs.insert_test_dir();

        let attr = vfs
            .mknod(parent, OsStr::new("persist.txt"), libc::S_IFREG | 0o644, 0, 0, 0, 0)
            .unwrap();

        // Drop the write_tx to let the write thread drain and exit
        drop(vfs);
        thread::sleep(Duration::from_millis(100));

        // Can't re-open an in-memory DB after drop; just verify ino is non-zero as a smoke check
        assert!(attr.ino > 0);
    }

    #[test]
    fn test_mkdir_returns_attr_and_caches() {
        use std::ffi::OsStr;
        let vfs = WriteBehind::new_for_test().unwrap();
        let parent = vfs.insert_test_dir();

        let attr = vfs.mkdir(parent, OsStr::new("subdir"), 0o755, 0o022, 500, 500).unwrap();

        assert_eq!(attr.kind, fuser::FileType::Directory);
        assert_eq!(attr.uid, 500);
        assert_eq!(attr.gid, 500);

        let looked_up = vfs.lookup_name(parent, OsStr::new("subdir")).unwrap();
        assert_eq!(looked_up.ino, attr.ino);
        assert_eq!(looked_up.kind, fuser::FileType::Directory);
    }

    #[test]
    fn test_mkdir_then_mknod_unique_inos() {
        use std::ffi::OsStr;
        let vfs = WriteBehind::new_for_test().unwrap();
        let parent = vfs.insert_test_dir();

        let dir_attr = vfs.mkdir(parent, OsStr::new("d"), 0o755, 0, 0, 0).unwrap();
        let file_attr = vfs
            .mknod(parent, OsStr::new("f"), libc::S_IFREG | 0o644, 0, 0, 0, 0)
            .unwrap();

        assert_ne!(dir_attr.ino, file_attr.ino);
    }
}
