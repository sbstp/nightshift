#![allow(clippy::too_many_arguments)]

mod attr;
mod flags;
mod handle;
mod request_info;
mod store;

use std::{
    ffi::OsStr,
    fs,
    os::unix::fs::MetadataExt,
    path::Path,
    time::{Duration, SystemTime},
};

use attr::FileAttrBuilder;
use fuser::FileAttr;
use slab::Slab;

use crate::{buffer::FixedBuffer, types::FileType};
use crate::{errors::{Error, Result}, queries::block::Compression};
use crate::{offsets::Absolute, queries::dir_entry::ListDirEntry};
use store::{SetAttr, Store};
pub use flags::OpenFlags;
pub use handle::Handle;
pub use request_info::RequestInfo;

const DURATION: Duration = Duration::from_secs(0);

pub struct FuseDriver<S: Store> {
    pub db: S,
    compression: Compression,
    handles: Slab<S::Handle>,
    mount_uid: u32,
    mount_gid: u32,
}

impl<S: Store> FuseDriver<S> {
    pub fn new(db: S, compression: Compression, mount_path: &Path) -> anyhow::Result<Self> {
        let md = fs::metadata(mount_path)?;
        Ok(Self {
            db,
            compression,
            handles: Slab::new(),
            mount_uid: md.uid(),
            mount_gid: md.gid(),
        })
    }

    #[cfg(test)]
    pub fn new_no_io(db: S, compression: Compression) -> Self {
        Self {
            db,
            compression,
            handles: Slab::new(),
            mount_uid: 0,
            mount_gid: 0,
        }
    }

    fn ensure_root_exists(&mut self) -> Result<()> {
        self.db.ensure_root()
    }

    fn lookup_impl(&mut self, _req: RequestInfo, parent: u64, name: &OsStr) -> Result<FileAttr> {
        let mut attr = self.db.lookup_entry(parent, name)?;
        if attr.ino == 1 {
            attr.uid = self.mount_uid;
            attr.gid = self.mount_gid;
        }
        Ok(attr)
    }

    fn getattr_impl(&mut self, _req: RequestInfo, ino: u64) -> Result<FileAttr> {
        let mut attr = self.db.get_inode(ino)?;
        if attr.ino == 1 {
            attr.uid = self.mount_uid;
            attr.gid = self.mount_gid;
        }
        Ok(attr)
    }

    fn setattr_impl(&mut self, _req: RequestInfo, ino: u64, changes: SetAttr) -> Result<FileAttr> {
        self.db.set_inode_attrs(ino, changes, self.compression)
    }

    fn mknod_impl(
        &mut self,
        req: RequestInfo,
        parent: u64,
        name: &OsStr,
        mode: u32,
        umask: u32,
        rdev: u32,
    ) -> Result<FileAttr> {
        let kind = FileType::from_mode(mode).ok_or(Error::InvalidArgument)?;
        let mut attr = FileAttrBuilder::new_node(kind)
            .with_uid(req.uid)
            .with_gid(req.gid)
            .with_mode_umask(mode, umask)
            .with_rdev(rdev)
            .build();
        self.db.create_node(&mut attr, parent, name)?;
        Ok(attr)
    }

    fn link_impl(&mut self, _req: RequestInfo, ino: u64, newparent: u64, newname: &OsStr) -> Result<FileAttr> {
        self.db.create_link(ino, newparent, newname)
    }

    fn unlink_impl(&mut self, _req: RequestInfo, parent: u64, name: &OsStr) -> Result<()> {
        self.db.unlink(parent, name)
    }

    fn mkdir_impl(&mut self, req: RequestInfo, parent: u64, name: &OsStr, mode: u32, umask: u32) -> Result<FileAttr> {
        let mut attr = FileAttrBuilder::new_directory()
            .with_mode_umask(mode, umask)
            .with_uid(req.uid)
            .with_gid(req.gid)
            .build();
        self.db.create_dir(&mut attr, parent, name)?;
        Ok(attr)
    }

    fn rmdir_impl(&mut self, _req: RequestInfo, parent: u64, name: &OsStr) -> Result<()> {
        self.db.rmdir(parent, name)
    }

    fn readdir_impl<F>(&mut self, _req: RequestInfo, ino: u64, _fh: u64, offset: i64, iter: F) -> Result<()>
    where
        F: FnMut(ListDirEntry) -> bool,
    {
        self.db.list_dir(ino, offset, iter)
    }

    fn open_impl(&mut self, _req: RequestInfo, ino: u64, flags: OpenFlags) -> Result<(u64, u32)> {
        let handle = self.db.open_handle(ino, flags, self.compression)?;
        let fh = self.handles.insert(handle);
        let fh = u64::try_from(fh).map_err(|_| Error::Overflow)?;
        Ok((fh, flags.bits as u32))
    }

    fn release_impl(
        &mut self,
        _req: RequestInfo,
        _ino: u64,
        fh: u64,
        _flags: i32,
        _lock_owner: Option<u64>,
        _flush: bool,
    ) -> Result<()> {
        let fh = usize::try_from(fh).map_err(|_| Error::Overflow)?;
        let mut handle = self.handles.try_remove(fh).ok_or(Error::NotFound)?;
        self.db.flush_handle(&mut handle)
    }

    fn read_impl(
        &mut self,
        _req: RequestInfo,
        ino: u64,
        fh: u64,
        offset: Absolute,
        size: u32,
        _flags: i32,
        _lock_owner: Option<u64>,
    ) -> Result<FixedBuffer> {
        let fh = usize::try_from(fh).map_err(|_| Error::Overflow)?;
        let handle = self.handles.get_mut(fh).ok_or(Error::NotFound)?;

        // If any data is left in the write buffer, flush it before reading.
        if !handle.buffer_empty() {
            self.db.flush_handle(handle)?;
        }

        self.db.read_file(ino, offset, size)
    }

    fn write_impl(
        &mut self,
        _req: RequestInfo,
        _ino: u64,
        fh: u64,
        offset: Absolute,
        mut data: &[u8],
        _write_flags: u32,
        _flags: i32,
        _lock_owner: Option<u64>,
    ) -> Result<u32> {
        let fh = usize::try_from(fh).map_err(|_| Error::Overflow)?;
        let handle = self.handles.get_mut(fh).ok_or(Error::NotFound)?;
        let start_size = data.len();

        // Detect if seek happened. If it did flush whatever is in the buffer
        // where it belongs and then update the offset where to write to.
        if handle.write_offset() != offset {
            log::debug!(
                "seek occured, flushing, old offset = {}, new offset = {}",
                handle.write_offset(),
                offset
            );
            self.db.flush_handle(handle)?;
            handle.seek_to(offset);
        }

        while !data.is_empty() {
            if handle.buffer_full() {
                log::debug!("handle buffer full, flushing");
                self.db.flush_handle(handle)?;
            }
            let consumed = handle.consume_input(data);
            data = &data[consumed..];
        }
        Ok(start_size as u32)
    }

    fn flush_impl(&mut self, _req: RequestInfo, _ino: u64, fh: u64, _lock_owner: u64) -> Result<()> {
        let fh = usize::try_from(fh).map_err(|_| Error::Overflow)?;
        let handle = self.handles.get_mut(fh).ok_or(Error::NotFound)?;
        self.db.flush_handle(handle)
    }

    fn rename_impl(
        &mut self,
        _req: RequestInfo,
        parent: u64,
        name: &OsStr,
        newparent: u64,
        newname: &OsStr,
        _flags: u32,
    ) -> Result<()> {
        self.db.rename_entry(parent, name, newparent, newname)
    }
}

impl<S: Store> fuser::Filesystem for FuseDriver<S> {
    fn init(
        &mut self,
        _req: &fuser::Request<'_>,
        config: &mut fuser::KernelConfig,
    ) -> std::result::Result<(), libc::c_int> {
        config.set_max_write(128 * 1024).expect("unable to set max_write");
        match self.ensure_root_exists() {
            Ok(()) => Ok(()),
            Err(e) => {
                log::error!("init error: {}", e);
                Err(e.errno())
            }
        }
    }

    fn lookup(&mut self, req: &fuser::Request<'_>, parent: u64, name: &std::ffi::OsStr, reply: fuser::ReplyEntry) {
        log::trace!("lookup(parent={}, name={:?})", parent, name.to_string_lossy());
        let res = self.lookup_impl(req.into(), parent, name);
        log::trace!("lookup: {:?}", res);

        match res {
            Ok(attr) => reply.entry(&DURATION, &attr, 0),
            Err(e) => reply.error(e.errno()),
        }
    }

    fn getattr(&mut self, req: &fuser::Request<'_>, ino: u64, reply: fuser::ReplyAttr) {
        log::trace!("getattr(ino={})", ino);
        let res = self.getattr_impl(req.into(), ino);
        log::trace!("getattr: {:?}", res);

        match res {
            Ok(attr) => reply.attr(&DURATION, &attr),
            Err(e) => reply.error(e.errno()),
        }
    }

    fn setattr(
        &mut self,
        req: &fuser::Request<'_>,
        ino: u64,
        mode: Option<u32>,
        uid: Option<u32>,
        gid: Option<u32>,
        size: Option<u64>,
        atime: Option<fuser::TimeOrNow>,
        mtime: Option<fuser::TimeOrNow>,
        ctime: Option<SystemTime>,
        fh: Option<u64>,
        crtime: Option<SystemTime>,
        chgtime: Option<SystemTime>,
        bkuptime: Option<SystemTime>,
        flags: Option<u32>,
        reply: fuser::ReplyAttr,
    ) {
        log::trace!(
            "setattr(ino={}, mode={:#?}, uid={:?}, gid={:?}, size={:?})",
            ino,
            mode,
            uid,
            gid,
            size,
        );
        let changes = SetAttr {
            mode,
            uid,
            gid,
            size,
            atime: atime.map(Into::into),
            mtime: mtime.map(Into::into),
            ctime: ctime.map(Into::into),
            crtime: crtime.map(Into::into),
            flags,
        };
        let _ = (fh, chgtime, bkuptime);
        let res = self.setattr_impl(req.into(), ino, changes);
        log::trace!("setattr: {:?}", res);

        match res {
            Ok(attr) => reply.attr(&DURATION, &attr),
            Err(e) => reply.error(e.errno()),
        }
    }

    fn mknod(
        &mut self,
        req: &fuser::Request<'_>,
        parent: u64,
        name: &OsStr,
        mode: u32,
        umask: u32,
        rdev: u32,
        reply: fuser::ReplyEntry,
    ) {
        log::trace!(
            "mknod(parent={}, name={:?}, mode={}, umask={:#o}, rdev={})",
            parent,
            name.to_string_lossy(),
            mode,
            umask,
            rdev
        );
        let res = self.mknod_impl(req.into(), parent, name, mode, umask, rdev);
        log::trace!("mknod: {:?}", res);

        match res {
            Ok(attr) => reply.entry(&DURATION, &attr, 0),
            Err(e) => reply.error(e.errno()),
        }
    }

    fn link(&mut self, req: &fuser::Request<'_>, ino: u64, newparent: u64, newname: &OsStr, reply: fuser::ReplyEntry) {
        log::trace!("link(ino={}, newparent={}, newname={:?})", ino, newparent, newname);
        let res = self.link_impl(req.into(), ino, newparent, newname);
        log::trace!("link: {:?}", res);

        match res {
            Ok(attr) => reply.entry(&DURATION, &attr, 0),
            Err(e) => reply.error(e.errno()),
        }
    }

    fn unlink(&mut self, req: &fuser::Request<'_>, parent: u64, name: &OsStr, reply: fuser::ReplyEmpty) {
        log::trace!("unlink(parent={}, name={:?})", parent, name);
        let res = self.unlink_impl(req.into(), parent, name);
        log::trace!("unlink: {:?}", res);

        match res {
            Ok(_) => reply.ok(),
            Err(e) => reply.error(e.errno()),
        }
    }

    fn mkdir(
        &mut self,
        req: &fuser::Request<'_>,
        parent: u64,
        name: &OsStr,
        mode: u32,
        umask: u32,
        reply: fuser::ReplyEntry,
    ) {
        log::trace!(
            "mkdir(parent={}, name={:?}, mode={}, umask={:#o})",
            parent,
            name.to_string_lossy(),
            mode,
            umask,
        );
        let res = self.mkdir_impl(req.into(), parent, name, mode, umask);
        log::trace!("mkdir: {:?}", res);

        match res {
            Ok(attr) => reply.entry(&DURATION, &attr, 0),
            Err(e) => reply.error(e.errno()),
        }
    }

    fn rmdir(&mut self, req: &fuser::Request<'_>, parent: u64, name: &OsStr, reply: fuser::ReplyEmpty) {
        log::trace!("rmdir(parent={}, name={:?})", parent, name);
        let res = self.rmdir_impl(req.into(), parent, name);
        log::trace!("rmdir: {:?}", res);

        match res {
            Ok(_) => reply.ok(),
            Err(e) => reply.error(e.errno()),
        }
    }

    fn readdir(&mut self, req: &fuser::Request<'_>, ino: u64, fh: u64, offset: i64, mut reply: fuser::ReplyDirectory) {
        log::trace!("readdir(ino={}, fh={}, offset={})", ino, fh, offset);
        let res = self.readdir_impl(req.into(), ino, fh, offset, |entry| {
            reply.add(entry.ino, entry.offset, entry.kind, entry.name)
        });
        log::trace!("readdir: {:?}", res);

        match res {
            Ok(_) => reply.ok(),
            Err(e) => reply.error(e.errno()),
        }
    }

    fn open(&mut self, req: &fuser::Request<'_>, ino: u64, flags: i32, reply: fuser::ReplyOpen) {
        let flags = OpenFlags::from(flags);
        log::trace!("open(ino={}, flags={:?})", ino, flags);
        let res = self.open_impl(req.into(), ino, flags);
        log::trace!("open: {:?}", res);

        match res {
            Ok((fh, flags)) => reply.opened(fh, flags),
            Err(e) => reply.error(e.errno()),
        }
    }

    fn release(
        &mut self,
        req: &fuser::Request<'_>,
        ino: u64,
        fh: u64,
        flags: i32,
        lock_owner: Option<u64>,
        flush: bool,
        reply: fuser::ReplyEmpty,
    ) {
        log::trace!("release(ino={}, fh={}, flush={})", ino, fh, flush);
        let res = self.release_impl(req.into(), ino, fh, flags, lock_owner, flush);
        log::trace!("release: {:?}", res);

        match res {
            Ok(_) => reply.ok(),
            Err(e) => reply.error(e.errno()),
        }
    }

    fn read(
        &mut self,
        req: &fuser::Request<'_>,
        ino: u64,
        fh: u64,
        offset: i64,
        size: u32,
        flags: i32,
        lock_owner: Option<u64>,
        reply: fuser::ReplyData,
    ) {
        log::trace!("read(ino={}, offset={}, size={})", ino, offset, size);
        let res = self.read_impl(req.into(), ino, fh, offset.into(), size, flags, lock_owner);
        log::trace!("read: {:?}", res.as_ref().map(|d| d.len()));

        match res {
            Ok(data) => reply.data(&data),
            Err(Error::NotFound) => reply.data(&[]),
            Err(e) => reply.error(e.errno()),
        }
    }

    fn write(
        &mut self,
        req: &fuser::Request<'_>,
        ino: u64,
        fh: u64,
        offset: i64,
        data: &[u8],
        write_flags: u32,
        flags: i32,
        lock_owner: Option<u64>,
        reply: fuser::ReplyWrite,
    ) {
        log::trace!("write(ino={}, offset={}, data_len={})", ino, offset, data.len());
        let res = self.write_impl(req.into(), ino, fh, offset.into(), data, write_flags, flags, lock_owner);
        log::trace!("write: {:?}", res);

        match res {
            Ok(written) => reply.written(written),
            Err(e) => reply.error(e.errno()),
        }
    }

    fn flush(&mut self, req: &fuser::Request<'_>, ino: u64, fh: u64, lock_owner: u64, reply: fuser::ReplyEmpty) {
        log::trace!("flush(ino={}, fh={})", ino, fh);
        let res = self.flush_impl(req.into(), ino, fh, lock_owner);
        log::trace!("flush: {:?}", res);

        match res {
            Ok(_) => reply.ok(),
            Err(e) => reply.error(e.errno()),
        }
    }

    fn rename(
        &mut self,
        req: &fuser::Request<'_>,
        parent: u64,
        name: &OsStr,
        newparent: u64,
        newname: &OsStr,
        flags: u32,
        reply: fuser::ReplyEmpty,
    ) {
        log::trace!(
            "rename(parent={}, name={:?}, newparent={}, newname={:?}",
            parent,
            name,
            newparent,
            newname
        );
        let res = self.rename_impl(req.into(), parent, name, newparent, newname, flags);
        log::trace!("rename: {:?}", res);

        match res {
            Ok(_) => reply.ok(),
            Err(e) => reply.error(e.errno()),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::ffi::OsStr;

    use super::{attr::FileAttrBuilder, FuseDriver, OpenFlags, RequestInfo};
    use crate::{
        database::DatabaseOps,
        errors::Error,
        offsets::Absolute,
        queries::{self, block::Compression},
        types::FileType,
    };
    use rand::{Rng, RngCore};
    use sha1::{Digest, Sha1};
    use test_log::test;

    fn count_blocks(driver: &mut FuseDriver<DatabaseOps>, ino: u64) -> anyhow::Result<usize> {
        let mut block_count = 0;
        driver.db.with_read_tx(|tx| {
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
        let mut driver = FuseDriver::new_no_io(db, queries::block::Compression::None);

        let mut root_dir = FileAttrBuilder::new_directory().build();
        let mut node = FileAttrBuilder::new_node(FileType::RegularFile)
            .with_uid(1337)
            .with_gid(1338)
            .build();

        driver.db.with_write_tx(|tx| {
            queries::inode::create(tx, &mut root_dir)?;
            queries::inode::create(tx, &mut node)?;
            queries::dir_entry::create(tx, root_dir.ino, OsStr::new("foo.txt"), node.ino)?;
            Ok(())
        })?;

        let attr = driver.lookup_impl(RequestInfo::default(), root_dir.ino, OsStr::new("foo.txt"))?;
        assert_eq!(attr.uid, 1337);
        assert_eq!(attr.gid, 1338);

        // Not found test
        let res = driver.lookup_impl(RequestInfo::default(), root_dir.ino, OsStr::new("not_found.jpg"));
        assert_eq!(res, Err(Error::NotFound));

        Ok(())
    }

    #[test]
    fn test_mknod() -> anyhow::Result<()> {
        let db = DatabaseOps::open_in_memory()?;
        let mut driver = FuseDriver::new_no_io(db, queries::block::Compression::LZ4);

        let mut root_dir = FileAttrBuilder::new_directory().build();

        driver.db.with_write_tx(|tx| {
            queries::inode::create(tx, &mut root_dir)?;
            Ok(())
        })?;

        let attr = driver.mknod_impl(
            RequestInfo::default(),
            root_dir.ino,
            OsStr::new("foo.txt"),
            0o644 | libc::S_IFREG,
            0,
            1337,
        )?;

        let db_attr = driver.db.with_read_tx(|tx| {
            let ino = queries::dir_entry::lookup(tx, root_dir.ino, OsStr::new("foo.txt"))?;
            queries::inode::lookup(tx, ino)
        })?;

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
        let mut driver = FuseDriver::new_no_io(db, Compression::Zstd);

        let mut root_dir = FileAttrBuilder::new_directory().build();
        let mut node = FileAttrBuilder::new_node(FileType::RegularFile)
            .with_uid(1337)
            .with_gid(1338)
            .build();

        driver.db.with_write_tx(|tx| {
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

        assert_eq!(count_blocks(&mut driver, node.ino)?, 1);

        let linked_node = driver.link_impl(RequestInfo::default(), node.ino, root_dir.ino, OsStr::new("foo2.txt"))?;
        let linked_ino = driver
            .db
            .with_read_tx(|tx| queries::dir_entry::lookup(tx, root_dir.ino, OsStr::new("foo2.txt")))?;
        assert_eq!(linked_node.ino, linked_ino);
        assert_eq!(linked_node.ino, node.ino);
        assert_eq!(linked_node.nlink, 2);

        // Unlink foo.txt
        driver.unlink_impl(RequestInfo::default(), root_dir.ino, OsStr::new("foo.txt"))?;

        // Make sure foo.txt is gone
        let res = driver
            .db
            .with_read_tx(|tx| queries::dir_entry::lookup(tx, root_dir.ino, OsStr::new("foo.txt")));
        assert_eq!(res, Err(Error::NotFound));

        // Make sure the inode has updated nlink
        let updated_node = driver.db.with_read_tx(|tx| queries::inode::lookup(tx, linked_ino))?;
        assert_eq!(updated_node.nlink, 1);

        println!("last");
        // Unlink foo2.txt
        driver.unlink_impl(RequestInfo::default(), root_dir.ino, OsStr::new("foo2.txt"))?;
        println!("last 2");

        // Make sure the inode is gone
        let res = driver.db.with_read_tx(|tx| queries::inode::lookup(tx, linked_ino));
        assert_eq!(res, Err(Error::NotFound));

        // Make sure the blocks are gone
        assert_eq!(count_blocks(&mut driver, node.ino)?, 0);

        Ok(())
    }

    #[test]
    fn test_mkdir() -> anyhow::Result<()> {
        let db = DatabaseOps::open_in_memory()?;
        let mut driver = FuseDriver::new_no_io(db, Compression::None);

        let mut root_dir = FileAttrBuilder::new_directory().build();

        driver.db.with_write_tx(|tx| {
            queries::inode::create(tx, &mut root_dir)?;
            Ok(())
        })?;

        let attr = driver.mkdir_impl(RequestInfo::default(), root_dir.ino, OsStr::new("foo"), 0o755, 0)?;

        let db_attr = driver.db.with_read_tx(|tx| {
            let ino = queries::dir_entry::lookup(tx, root_dir.ino, OsStr::new("foo"))?;
            queries::inode::lookup(tx, ino)
        })?;

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
        let mut driver = FuseDriver::new_no_io(db, Compression::None);

        let mut root_dir = FileAttrBuilder::new_directory().build();
        let mut dir1 = FileAttrBuilder::new_directory().build();
        let mut file1 = FileAttrBuilder::new_node(FileType::RegularFile).build();

        driver.db.with_write_tx(|tx| {
            queries::inode::create(tx, &mut root_dir)?;
            queries::inode::create(tx, &mut dir1)?;
            queries::dir_entry::create(tx, root_dir.ino, OsStr::new("dir1"), dir1.ino)?;
            queries::inode::create(tx, &mut file1)?;
            queries::dir_entry::create(tx, dir1.ino, OsStr::new("file1"), file1.ino)?;
            Ok(())
        })?;

        let res = driver.rmdir_impl(RequestInfo::default(), root_dir.ino, OsStr::new("dir1"));
        assert_eq!(res, Err(Error::NotEmpty));

        driver.db.with_write_tx(|tx| {
            queries::inode::remove(tx, file1.ino)?; // should delete dir_entry through CASCADE
            Ok(())
        })?;

        driver.rmdir_impl(RequestInfo::default(), root_dir.ino, OsStr::new("dir1"))?;

        Ok(())
    }

    #[test]
    fn test_read_write_cycle() -> anyhow::Result<()> {
        let db = DatabaseOps::open_in_memory()?;
        let mut driver = FuseDriver::new_no_io(db, Compression::None);

        let mut root_dir = FileAttrBuilder::new_directory().build();
        let mut node = FileAttrBuilder::new_node(FileType::RegularFile)
            .with_uid(1337)
            .with_gid(1338)
            .build();

        driver.db.with_write_tx(|tx| {
            queries::inode::create(tx, &mut root_dir)?;
            queries::inode::create(tx, &mut node)?;
            queries::dir_entry::create(tx, root_dir.ino, OsStr::new("foo.txt"), node.ino)?;
            Ok(())
        })?;

        let (fh, _) = driver.open_impl(RequestInfo::default(), node.ino, OpenFlags::from(libc::O_RDWR))?;
        driver.write_impl(RequestInfo::default(), node.ino, fh, 0.into(), &[1u8; 200], 0, 0, None)?;
        driver.write_impl(
            RequestInfo::default(),
            node.ino,
            fh,
            200.into(),
            &[2u8; 200],
            0,
            0,
            None,
        )?;

        let data = driver.read_impl(RequestInfo::default(), node.ino, fh, 0.into(), 400, 0, None)?;
        assert_eq!(data.len(), 400);
        assert_eq!(&data[..200], &[1u8; 200]);
        assert_eq!(&data[200..], &[2u8; 200]);
        Ok(())
    }

    #[test]
    fn test_rename() -> anyhow::Result<()> {
        let db = DatabaseOps::open_in_memory()?;
        let mut driver = FuseDriver::new_no_io(db, Compression::None);

        let mut root_dir = FileAttrBuilder::new_directory().build();
        let mut node = FileAttrBuilder::new_node(FileType::RegularFile)
            .with_uid(1337)
            .with_gid(1338)
            .build();

        driver.db.with_write_tx(|tx| {
            queries::inode::create(tx, &mut root_dir)?;
            queries::inode::create(tx, &mut node)?;
            queries::dir_entry::create(tx, root_dir.ino, OsStr::new("foo.txt"), node.ino)?;
            Ok(())
        })?;

        driver.rename_impl(
            RequestInfo::default(),
            root_dir.ino,
            OsStr::new("foo.txt"),
            root_dir.ino,
            OsStr::new("foo2.txt"),
            0,
        )?;

        let res = driver
            .db
            .with_read_tx(|tx| queries::dir_entry::lookup(tx, root_dir.ino, OsStr::new("foo.txt")));
        assert_eq!(res, Err(Error::NotFound));

        let db_attr = driver.db.with_read_tx(|tx| {
            let ino = queries::dir_entry::lookup(tx, root_dir.ino, OsStr::new("foo2.txt"))?;
            queries::inode::lookup(tx, ino)
        })?;
        assert_eq!(db_attr.ino, node.ino);

        Ok(())
    }

    #[test]
    fn test_for_corruption() -> anyhow::Result<()> {
        let mut rng = rand::thread_rng();

        for compression in [Compression::None, Compression::LZ4, Compression::Zstd] {
            let db = DatabaseOps::open_in_memory()?;
            let mut driver = FuseDriver::new_no_io(db, compression);

            let attr = driver.mknod_impl(RequestInfo::default(), 1, OsStr::new("foo"), libc::S_IFREG, 0, 0)?;
            let (fh, _) = driver.open_impl(RequestInfo::default(), attr.ino, OpenFlags::from(libc::O_RDWR))?;

            let max = Absolute::from(10u64 * 1024 * 1024);
            let mut write_offset = Absolute::from(0);

            let mut write_hasher = Sha1::new();
            let mut read_hahser = Sha1::new();

            while write_offset < max {
                let size = rng.gen_range(0..130 * 1024);
                let mut buf = vec![0u8; size];
                rng.fill_bytes(&mut buf);

                write_hasher.update(&buf);
                driver.write_impl(RequestInfo::default(), attr.ino, fh, write_offset, &buf, 0, 0, None)?;

                write_offset += buf.len();
            }

            let mut read_offset = Absolute::from(0);

            while read_offset < write_offset {
                let size = rng.gen_range(1..130 * 1024);
                let buf = driver.read_impl(RequestInfo::default(), attr.ino, fh, read_offset, size, 0, None)?;

                read_hahser.update(&buf);

                read_offset += u64::from(size);
            }

            assert_eq!(write_hasher.finalize(), read_hahser.finalize());
        }

        Ok(())
    }
}
