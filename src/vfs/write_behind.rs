use std::{
    collections::{BTreeMap, HashMap},
    ffi::{OsStr, OsString},
    sync::{mpsc, Arc},
};

use bytes::{Bytes, BytesMut};
use fuser::FileAttr;
use parking_lot::Mutex;
use quick_cache::sync::{Cache, EntryAction, EntryResult};
use slab::Slab;
use zstd::zstd_safe::WriteBuf;

use crate::queries::block::Block as BaseBlock;
use crate::{database::DatabaseOps, driver::OpenFlags, queries, vfs::Bno};

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

struct WriteBehindCore {
    //db: DatabaseOps, // todo use connection pool
    pool: r2d2::Pool<r2d2_sqlite::SqliteConnectionManager>,
    nodes: Cache<Ino, FileAttr>,
    entries: Cache<OsString, Ino>,
    blocks: Cache<(Ino, Bno), Block>,
    handles: parking_lot::RwLock<Slab<WriteBehindHandle>>,
    write_tx: mpsc::Sender<Block>,
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

fn spawn_write_thread(pool: r2d2::Pool<r2d2_sqlite::SqliteConnectionManager>, rx: mpsc::Receiver<Block>) {
    std::thread::spawn(move || loop {
        todo!()
    });
}

fn flush_batch(pool: &r2d2::Pool<r2d2_sqlite::SqliteConnectionManager>, batch: &[Block]) {
    let mut conn = match pool.get() {
        Ok(c) => c,
        Err(e) => {
            log::error!("write-behind: failed to get db connection: {e}");
            return;
        }
    };
    let mut tx = match conn.transaction() {
        Ok(t) => t,
        Err(e) => {
            log::error!("write-behind: failed to start transaction: {e}");
            return;
        }
    };
    for block in batch {
        if let Err(e) = queries::block::upsert(
            &mut tx,
            block.ino.into(),
            block.bno.into(),
            &block.buf,
            Default::default(),
        ) {
            log::error!(
                "write-behind: upsert failed for ino={} bno={}: {e}",
                block.ino.0,
                block.bno.0
            );
        }
    }
    if let Err(e) = tx.commit() {
        log::error!("write-behind: commit failed: {e}");
    }
}

impl WriteBehind {
    #[cfg(test)]
    pub fn new_for_test() -> anyhow::Result<Self> {
        let manager = r2d2_sqlite::SqliteConnectionManager::memory();
        let pool = r2d2::Pool::builder().max_size(1).build(manager)?;

        {
            let mut conn = pool.get()?;
            crate::database::migrate_database(&mut conn)?;
        }

        let (write_tx, write_rx) = mpsc::channel::<Block>();
        spawn_write_thread(pool.clone(), write_rx);

        Ok(WriteBehind {
            core: Arc::new(WriteBehindCore {
                pool,
                nodes: Cache::new(128),
                entries: Cache::new(128),
                blocks: Cache::new(256),
                handles: parking_lot::RwLock::new(Slab::new()),
                write_tx,
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
        todo!()
    }

    fn lookup_name(&self, parent: super::Ino, name: &OsStr) -> Result<fuser::FileAttr, Self::Error> {
        self.core
            .load_entry(parent, name)
            .and_then(|ino| self.core.load_ino(ino))
    }

    fn lookup_ino(&self, ino: super::Ino) -> Result<fuser::FileAttr, Self::Error> {
        self.core.load_ino(ino)
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
            // RMW: load existing block from cache/DB, or start with empty bytes
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
            let _ = self.core.write_tx.send(updated);
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
}
