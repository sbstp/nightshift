#![allow(clippy::too_many_arguments)]

use std::{ffi::OsStr, sync::Arc};

use bytes::{BufMut, Bytes};
use fuser::FileAttr;
use parking_lot::Mutex;
use slab::Slab;

use crate::{
    buffer::FixedBuffer,
    database::DatabaseOps,
    driver::{attr::FileAttrBuilder, FileHandle, OpenFlags},
    errors::{Error, Result},
    offsets::Absolute,
    queries::{self, block::Block, block::Compression, dir_entry::ListDirEntry},
    time::TimeSpec,
    types::FileType,
};

use super::{Fno, Ino, Vfs, VfsHandle};

struct SyncVfsInner {
    db: DatabaseOps,
    compression: Compression,
    handles: Slab<FileHandle>,
}

#[derive(Clone)]
pub struct SyncVfs(Arc<Mutex<SyncVfsInner>>);

#[derive(Clone)]
pub struct SyncVfsHandle {
    fno: Fno,
    inner: Arc<Mutex<SyncVfsInner>>,
}

impl SyncVfs {
    pub fn new(db: DatabaseOps, compression: Compression) -> Self {
        Self(Arc::new(Mutex::new(SyncVfsInner {
            db,
            compression,
            handles: Slab::new(),
        })))
    }
}

impl Vfs for SyncVfs {
    type Handle = SyncVfsHandle;
    type Error = Error;

    fn ensure_root(&self) -> Result<()> {
        self.0
            .lock()
            .db
            .with_write_tx(|tx| match queries::inode::lookup(tx, 1) {
                Err(Error::NotFound) => {
                    log::debug!("ino=1 requested, but does not exist yet, will create.");
                    let mut attr = FileAttrBuilder::new_directory().build();
                    queries::inode::create(tx, &mut attr)?;
                    Ok(())
                }
                Err(e) => Err(e),
                Ok(_) => Ok(()),
            })
    }

    fn lookup_name(&self, parent: Ino, name: &OsStr) -> Result<FileAttr> {
        self.0.lock().db.with_read_tx(|tx| {
            let ino = queries::dir_entry::lookup(tx, parent.into(), name)?;
            queries::inode::lookup(tx, ino)
        })
    }

    fn lookup_ino(&self, ino: Ino) -> Result<FileAttr> {
        self.0
            .lock()
            .db
            .with_read_tx(|tx| queries::inode::lookup(tx, ino.into()))
    }

    fn setattr(
        &self,
        ino: Ino,
        mode: Option<u32>,
        uid: Option<u32>,
        gid: Option<u32>,
        size: Option<u64>,
        atime: Option<TimeSpec>,
        mtime: Option<TimeSpec>,
        ctime: Option<TimeSpec>,
        _fh: Option<u64>,
        crtime: Option<TimeSpec>,
        _chgtime: Option<TimeSpec>,
        _bkuptime: Option<TimeSpec>,
        flags: Option<u32>,
    ) -> Result<FileAttr> {
        let ino: u64 = ino.into();
        let mut guard = self.0.lock();
        let compression = guard.compression;
        guard.db.with_write_tx(|tx| {
            if let Some(mode) = mode {
                queries::inode::set_attr(tx, ino, "perm", mode)?;
            }
            if let Some(uid) = uid {
                queries::inode::set_attr(tx, ino, "uid", uid)?;
            }
            if let Some(gid) = gid {
                queries::inode::set_attr(tx, ino, "gid", gid)?;
            }
            if let Some(size) = size.map(Absolute::from) {
                let bno = Block::offset_to_bno(size);
                queries::block::remove_blocks_from(tx, ino, bno + 1)?;
                match queries::block::get_block(tx, ino, bno) {
                    Ok(mut block) => {
                        block.truncate(size);
                        queries::block::update(tx, &block, compression)?;
                    }
                    Err(Error::NotFound) => {}
                    Err(e) => return Err(e),
                }
                queries::inode::set_attr(tx, ino, "size", u64::from(size))?;
            }
            if let Some(atime) = atime {
                queries::inode::set_attr(tx, ino, "atime_secs", atime.secs)?;
                queries::inode::set_attr(tx, ino, "atime_nanos", atime.nanos)?;
            }
            if let Some(mtime) = mtime {
                queries::inode::set_attr(tx, ino, "mtime_secs", mtime.secs)?;
                queries::inode::set_attr(tx, ino, "mtime_nanos", mtime.nanos)?;
            }
            if let Some(ctime) = ctime {
                queries::inode::set_attr(tx, ino, "ctime_secs", ctime.secs)?;
                queries::inode::set_attr(tx, ino, "ctime_nanos", ctime.nanos)?;
            }
            if let Some(crtime) = crtime {
                queries::inode::set_attr(tx, ino, "crtime_secs", crtime.secs)?;
                queries::inode::set_attr(tx, ino, "crtime_nanos", crtime.nanos)?;
            }
            if let Some(flags) = flags {
                queries::inode::set_attr(tx, ino, "flags", flags)?;
            }
            queries::inode::lookup(tx, ino)
        })
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
    ) -> Result<FileAttr> {
        let kind = FileType::from_mode(mode).ok_or(Error::InvalidArgument)?;
        let mut attr = FileAttrBuilder::new_node(kind)
            .with_uid(uid)
            .with_gid(gid)
            .with_mode_umask(mode, umask)
            .with_rdev(rdev)
            .build();
        self.0.lock().db.with_write_tx(|tx| {
            queries::inode::create(tx, &mut attr)?;
            queries::dir_entry::create(tx, parent.into(), name, attr.ino)?;
            Ok(attr)
        })
    }

    fn mkdir(&self, parent: Ino, name: &OsStr, mode: u32, umask: u32, uid: u32, gid: u32) -> Result<FileAttr> {
        let mut attr = FileAttrBuilder::new_directory()
            .with_mode_umask(mode, umask)
            .with_uid(uid)
            .with_gid(gid)
            .build();
        self.0.lock().db.with_write_tx(|tx| {
            queries::inode::create(tx, &mut attr)?;
            queries::dir_entry::create(tx, parent.into(), name, attr.ino)?;
            Ok(attr)
        })
    }

    fn link(&self, ino: Ino, newparent: Ino, newname: &OsStr) -> Result<FileAttr> {
        let ino: u64 = ino.into();
        self.0.lock().db.with_write_tx(|tx| {
            let mut attr = queries::inode::lookup(tx, ino)?;
            attr.nlink += 1;
            queries::dir_entry::create(tx, newparent.into(), newname, ino)?;
            queries::inode::set_attr(tx, ino, "nlink", attr.nlink)?;
            Ok(attr)
        })
    }

    fn unlink(&self, parent: Ino, name: &OsStr) -> Result<()> {
        self.0.lock().db.with_write_tx(|tx| {
            let ino = queries::dir_entry::lookup(tx, parent.into(), name)?;
            let mut attr = queries::inode::lookup(tx, ino)?;
            attr.nlink -= 1;
            if attr.nlink > 0 {
                queries::inode::set_attr(tx, ino, "nlink", attr.nlink)?;
                queries::dir_entry::remove(tx, parent.into(), name)?;
            } else {
                // If nlink == 0, the inode removal will remove the dir_entry through CASCADE.
                // The blocks will also be removed through CASCADE.
                queries::inode::remove(tx, ino)?;
            }
            Ok(())
        })
    }

    fn rmdir(&self, parent: Ino, name: &OsStr) -> Result<()> {
        self.0.lock().db.with_write_tx(|tx| {
            let ino = queries::dir_entry::lookup(tx, parent.into(), name)?;
            let empty = queries::dir_entry::is_dir_empty(tx, ino)?;
            if !empty {
                return Err(Error::NotEmpty);
            }
            queries::inode::remove(tx, ino)?; // CASCADE will remove dir_entry
            Ok(())
        })
    }

    fn readdir(&self, ino: Ino, offset: i64, f: &mut dyn FnMut(ListDirEntry) -> bool) -> Result<()> {
        self.0.lock().db.with_read_tx(|tx| {
            queries::dir_entry::list_dir(tx, ino.into(), offset, f)?;
            Ok(())
        })
    }

    fn open(&self, ino: Ino, flags: OpenFlags) -> Result<SyncVfsHandle> {
        let mut guard = self.0.lock();
        let inner = &mut *guard;
        let compression = inner.compression;
        let attr = inner.db.with_read_tx(|tx| queries::inode::lookup(tx, ino.into()))?;
        let fh = inner
            .handles
            .insert(FileHandle::new(ino.into(), attr.size, flags, compression));
        Ok(SyncVfsHandle {
            fno: Fno::from(fh),
            inner: Arc::clone(&self.0),
        })
    }

    fn close(&self, fno: Fno) -> Result<()> {
        let mut guard = self.0.lock();
        let inner = &mut *guard;
        let fh: usize = fno.into();
        let mut handle = inner.handles.try_remove(fh).ok_or(Error::NotFound)?;
        inner.db.with_write_tx(|tx| handle.flush(tx))
    }

    fn handle(&self, fno: Fno) -> Option<SyncVfsHandle> {
        let fh: usize = fno.into();
        let guard = self.0.lock();
        if guard.handles.contains(fh) {
            Some(SyncVfsHandle {
                fno,
                inner: Arc::clone(&self.0),
            })
        } else {
            None
        }
    }

    fn rename(&self, parent: Ino, name: &OsStr, newparent: Ino, newname: &OsStr, _flags: u32) -> Result<()> {
        self.0
            .lock()
            .db
            .with_write_tx(|tx| queries::dir_entry::rename(tx, parent.into(), name, newparent.into(), newname))
    }
}

impl VfsHandle for SyncVfsHandle {
    type Error = Error;

    fn fno(&self) -> Fno {
        self.fno
    }

    fn read(&self, offset: u64, size: u32) -> Result<Bytes> {
        let mut guard = self.inner.lock();
        let fh: usize = self.fno.into();
        let inner = &mut *guard;
        let offset = Absolute::from(offset);

        {
            let db = &mut inner.db;
            let handle = inner.handles.get_mut(fh).ok_or(Error::NotFound)?;
            if !handle.buffer_empty() {
                db.with_write_tx(|tx| handle.flush(tx))?;
            }
        }

        let ino = {
            let handle = inner.handles.get(fh).ok_or(Error::NotFound)?;
            handle.ino
        };

        inner.db.with_read_tx(|tx| {
            let attr = queries::inode::lookup(tx, ino)?;
            let remaining = Absolute::from(attr.size) - offset;
            let cap = remaining.min(size);
            let mut buf = FixedBuffer::with_capacity(cap);

            queries::block::iter_blocks_from(tx, ino, offset, |block| {
                block.copy_into(&mut buf, offset);
                Ok(buf.remaining_mut() > 0)
            })?;
            assert!(buf.len() <= size as usize);
            Ok(buf.freeze())
        })
    }

    fn write(&self, offset: u64, data: &[u8]) -> Result<()> {
        let mut guard = self.inner.lock();
        let fh: usize = self.fno.into();
        let inner = &mut *guard;
        let db = &mut inner.db;
        let handle = inner.handles.get_mut(fh).ok_or(Error::NotFound)?;

        let offset = Absolute::from(offset);
        let mut data = data;

        if handle.write_offset() != offset {
            log::debug!(
                "seek occured, flushing, old offset = {}, new offset = {}",
                handle.write_offset(),
                offset
            );
            db.with_write_tx(|tx| handle.flush(tx))?;
            handle.seek_to(offset);
        }

        while !data.is_empty() {
            if handle.buffer_full() {
                log::debug!("handle buffer full, flushing");
                db.with_write_tx(|tx| handle.flush(tx))?;
            }
            let consumed = handle.consume_input(data);
            data = &data[consumed..];
        }

        Ok(())
    }

    fn flush(&self) -> Result<()> {
        let mut guard = self.inner.lock();
        let fh: usize = self.fno.into();
        let inner = &mut *guard;
        let db = &mut inner.db;
        let handle = inner.handles.get_mut(fh).ok_or(Error::NotFound)?;
        db.with_write_tx(|tx| handle.flush(tx))
    }
}

#[cfg(test)]
mod tests {
    use std::ffi::OsStr;

    use super::SyncVfs;
    use crate::{
        database::DatabaseOps,
        driver::{attr::FileAttrBuilder, OpenFlags},
        errors::Error,
        offsets::Absolute,
        queries::{self, block::Compression},
        types::FileType,
        vfs::{Ino, Vfs, VfsHandle},
    };
    use rand::{Rng, RngCore};
    use sha1::{Digest, Sha1};
    use test_log::test;

    fn count_blocks(vfs: &SyncVfs, ino: u64) -> anyhow::Result<usize> {
        let mut guard = vfs.0.lock();
        let mut block_count = 0;
        guard.db.with_read_tx(|tx| {
            queries::block::iter_blocks_from(tx, ino, 0.into(), |_| {
                block_count += 1;
                Ok(true)
            })
        })?;
        Ok(block_count)
    }

    #[test]
    fn test_lookup() -> anyhow::Result<()> {
        let db = DatabaseOps::open_in_memory()?;
        let vfs = SyncVfs::new(db, Compression::None);

        let mut root_dir = FileAttrBuilder::new_directory().build();
        let mut node = FileAttrBuilder::new_node(FileType::RegularFile)
            .with_uid(1337)
            .with_gid(1338)
            .build();

        {
            let mut guard = vfs.0.lock();
            guard.db.with_write_tx(|tx| {
                queries::inode::create(tx, &mut root_dir)?;
                queries::inode::create(tx, &mut node)?;
                queries::dir_entry::create(tx, root_dir.ino, OsStr::new("foo.txt"), node.ino)?;
                Ok(())
            })?;
        }

        let attr = vfs.lookup_name(root_dir.ino.into(), OsStr::new("foo.txt"))?;
        assert_eq!(attr.uid, 1337);
        assert_eq!(attr.gid, 1338);

        let res = vfs.lookup_name(root_dir.ino.into(), OsStr::new("not_found.jpg"));
        assert_eq!(res, Err(Error::NotFound));

        Ok(())
    }

    #[test]
    fn test_mknod() -> anyhow::Result<()> {
        let db = DatabaseOps::open_in_memory()?;
        let vfs = SyncVfs::new(db, Compression::LZ4);

        let mut root_dir = FileAttrBuilder::new_directory().build();

        {
            let mut guard = vfs.0.lock();
            guard.db.with_write_tx(|tx| {
                queries::inode::create(tx, &mut root_dir)?;
                Ok(())
            })?;
        }

        let attr = vfs.mknod(
            root_dir.ino.into(),
            OsStr::new("foo.txt"),
            0o644 | libc::S_IFREG,
            0,
            1337,
            0,
            0,
        )?;

        let db_attr = {
            let mut guard = vfs.0.lock();
            guard.db.with_read_tx(|tx| {
                let ino = queries::dir_entry::lookup(tx, root_dir.ino, OsStr::new("foo.txt"))?;
                queries::inode::lookup(tx, ino)
            })?
        };

        assert_eq!(attr.ino, db_attr.ino);
        assert_eq!(attr.perm, db_attr.perm);
        assert_eq!(attr.kind, db_attr.kind);
        assert_eq!(db_attr.kind, fuser::FileType::RegularFile);
        assert_eq!(db_attr.perm, 0o644);

        Ok(())
    }

    #[test]
    fn test_link_unlink() -> anyhow::Result<()> {
        let db = DatabaseOps::open_in_memory()?;
        let vfs = SyncVfs::new(db, Compression::Zstd);

        let mut root_dir = FileAttrBuilder::new_directory().build();
        let mut node = FileAttrBuilder::new_node(FileType::RegularFile)
            .with_uid(1337)
            .with_gid(1338)
            .build();

        {
            let mut guard = vfs.0.lock();
            guard.db.with_write_tx(|tx| {
                queries::inode::create(tx, &mut root_dir)?;
                queries::inode::create(tx, &mut node)?;
                queries::dir_entry::create(tx, root_dir.ino, OsStr::new("foo.txt"), node.ino)?;
                queries::block::create(
                    tx,
                    node.ino,
                    0.into(),
                    b"hello world!",
                    queries::block::Compression::Zstd,
                )?;
                Ok(())
            })?;
        }

        assert_eq!(count_blocks(&vfs, node.ino)?, 1);

        let linked_node = vfs.link(node.ino.into(), root_dir.ino.into(), OsStr::new("foo2.txt"))?;
        let linked_ino = {
            let mut guard = vfs.0.lock();
            guard
                .db
                .with_read_tx(|tx| queries::dir_entry::lookup(tx, root_dir.ino, OsStr::new("foo2.txt")))?
        };
        assert_eq!(linked_node.ino, linked_ino);
        assert_eq!(linked_node.ino, node.ino);
        assert_eq!(linked_node.nlink, 2);

        vfs.unlink(root_dir.ino.into(), OsStr::new("foo.txt"))?;

        let res = {
            let mut guard = vfs.0.lock();
            guard
                .db
                .with_read_tx(|tx| queries::dir_entry::lookup(tx, root_dir.ino, OsStr::new("foo.txt")))
        };
        assert_eq!(res, Err(Error::NotFound));

        let updated_node = {
            let mut guard = vfs.0.lock();
            guard.db.with_read_tx(|tx| queries::inode::lookup(tx, linked_ino))?
        };
        assert_eq!(updated_node.nlink, 1);

        vfs.unlink(root_dir.ino.into(), OsStr::new("foo2.txt"))?;

        let res = {
            let mut guard = vfs.0.lock();
            guard.db.with_read_tx(|tx| queries::inode::lookup(tx, linked_ino))
        };
        assert_eq!(res, Err(Error::NotFound));

        assert_eq!(count_blocks(&vfs, node.ino)?, 0);

        Ok(())
    }

    #[test]
    fn test_mkdir() -> anyhow::Result<()> {
        let db = DatabaseOps::open_in_memory()?;
        let vfs = SyncVfs::new(db, Compression::None);

        let mut root_dir = FileAttrBuilder::new_directory().build();

        {
            let mut guard = vfs.0.lock();
            guard.db.with_write_tx(|tx| {
                queries::inode::create(tx, &mut root_dir)?;
                Ok(())
            })?;
        }

        let attr = vfs.mkdir(root_dir.ino.into(), OsStr::new("foo"), 0o755, 0, 0, 0)?;

        let db_attr = {
            let mut guard = vfs.0.lock();
            guard.db.with_read_tx(|tx| {
                let ino = queries::dir_entry::lookup(tx, root_dir.ino, OsStr::new("foo"))?;
                queries::inode::lookup(tx, ino)
            })?
        };

        assert_eq!(attr.ino, db_attr.ino);
        assert_eq!(attr.perm, db_attr.perm);
        assert_eq!(attr.kind, db_attr.kind);
        assert_eq!(db_attr.kind, fuser::FileType::Directory);
        assert_eq!(db_attr.perm, 0o755);

        Ok(())
    }

    #[test]
    fn test_rmdir() -> anyhow::Result<()> {
        let db = DatabaseOps::open_in_memory()?;
        let vfs = SyncVfs::new(db, Compression::None);

        let mut root_dir = FileAttrBuilder::new_directory().build();
        let mut dir1 = FileAttrBuilder::new_directory().build();
        let mut file1 = FileAttrBuilder::new_node(FileType::RegularFile).build();

        {
            let mut guard = vfs.0.lock();
            guard.db.with_write_tx(|tx| {
                queries::inode::create(tx, &mut root_dir)?;
                queries::inode::create(tx, &mut dir1)?;
                queries::dir_entry::create(tx, root_dir.ino, OsStr::new("dir1"), dir1.ino)?;
                queries::inode::create(tx, &mut file1)?;
                queries::dir_entry::create(tx, dir1.ino, OsStr::new("file1"), file1.ino)?;
                Ok(())
            })?;
        }

        let res = vfs.rmdir(root_dir.ino.into(), OsStr::new("dir1"));
        assert_eq!(res, Err(Error::NotEmpty));

        {
            let mut guard = vfs.0.lock();
            guard.db.with_write_tx(|tx| {
                queries::inode::remove(tx, file1.ino)?;
                Ok(())
            })?;
        }

        vfs.rmdir(root_dir.ino.into(), OsStr::new("dir1"))?;

        Ok(())
    }

    #[test]
    fn test_read_write_cycle() -> anyhow::Result<()> {
        let db = DatabaseOps::open_in_memory()?;
        let vfs = SyncVfs::new(db, Compression::None);

        let mut root_dir = FileAttrBuilder::new_directory().build();
        let mut node = FileAttrBuilder::new_node(FileType::RegularFile)
            .with_uid(1337)
            .with_gid(1338)
            .build();

        {
            let mut guard = vfs.0.lock();
            guard.db.with_write_tx(|tx| {
                queries::inode::create(tx, &mut root_dir)?;
                queries::inode::create(tx, &mut node)?;
                queries::dir_entry::create(tx, root_dir.ino, OsStr::new("foo.txt"), node.ino)?;
                Ok(())
            })?;
        }

        let handle = vfs.open(Ino::from(node.ino), OpenFlags::from(libc::O_RDWR))?;
        handle.write(0, &[1u8; 200])?;
        handle.write(200, &[2u8; 200])?;

        let data = handle.read(0, 400)?;
        assert_eq!(data.len(), 400);
        assert_eq!(&data[..200], &[1u8; 200]);
        assert_eq!(&data[200..], &[2u8; 200]);
        Ok(())
    }

    #[test]
    fn test_rename() -> anyhow::Result<()> {
        let db = DatabaseOps::open_in_memory()?;
        let vfs = SyncVfs::new(db, Compression::None);

        let mut root_dir = FileAttrBuilder::new_directory().build();
        let mut node = FileAttrBuilder::new_node(FileType::RegularFile)
            .with_uid(1337)
            .with_gid(1338)
            .build();

        {
            let mut guard = vfs.0.lock();
            guard.db.with_write_tx(|tx| {
                queries::inode::create(tx, &mut root_dir)?;
                queries::inode::create(tx, &mut node)?;
                queries::dir_entry::create(tx, root_dir.ino, OsStr::new("foo.txt"), node.ino)?;
                Ok(())
            })?;
        }

        vfs.rename(
            root_dir.ino.into(),
            OsStr::new("foo.txt"),
            root_dir.ino.into(),
            OsStr::new("foo2.txt"),
            0,
        )?;

        let res = {
            let mut guard = vfs.0.lock();
            guard
                .db
                .with_read_tx(|tx| queries::dir_entry::lookup(tx, root_dir.ino, OsStr::new("foo.txt")))
        };
        assert_eq!(res, Err(Error::NotFound));

        let db_attr = {
            let mut guard = vfs.0.lock();
            guard.db.with_read_tx(|tx| {
                let ino = queries::dir_entry::lookup(tx, root_dir.ino, OsStr::new("foo2.txt"))?;
                queries::inode::lookup(tx, ino)
            })?
        };
        assert_eq!(db_attr.ino, node.ino);

        Ok(())
    }

    #[test]
    fn test_for_corruption() -> anyhow::Result<()> {
        let mut rng = rand::thread_rng();

        for compression in [Compression::None, Compression::LZ4, Compression::Zstd] {
            let db = DatabaseOps::open_in_memory()?;
            let vfs = SyncVfs::new(db, compression);

            let attr = vfs.mknod(1u64.into(), OsStr::new("foo"), libc::S_IFREG, 0, 0, 0, 0)?;
            let handle = vfs.open(Ino::from(attr.ino), OpenFlags::from(libc::O_RDWR))?;

            let max = Absolute::from(10u64 * 1024 * 1024);
            let mut write_offset = Absolute::from(0u64);

            let mut write_hasher = Sha1::new();
            let mut read_hasher = Sha1::new();

            while write_offset < max {
                let size = rng.gen_range(0..130 * 1024usize);
                let mut buf = vec![0u8; size];
                rng.fill_bytes(&mut buf);

                write_hasher.update(&buf);
                handle.write(write_offset.into(), &buf)?;

                write_offset += buf.len();
            }

            let mut read_offset: u64 = 0;

            while Absolute::from(read_offset) < write_offset {
                let size = rng.gen_range(1..130 * 1024u32);
                let buf = handle.read(read_offset, size)?;

                read_hasher.update(&buf);

                read_offset += size as u64;
            }

            assert_eq!(write_hasher.finalize(), read_hasher.finalize());
        }

        Ok(())
    }
}
