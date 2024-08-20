#![allow(clippy::too_many_arguments)]

mod attr;
mod flags;
mod handle;
mod request_info;

use std::{
    cmp,
    ffi::OsStr,
    time::{Duration, SystemTime},
};

use attr::FileAttrBuilder;
use fuser::FileAttr;
use slab::Slab;

use crate::types::FileType;
use crate::{database::DatabaseOps, time::TimeSpec};
use crate::{
    errors::{Error, Result},
    models::Block,
};
use crate::{models::ListDirEntry, queries};
pub use flags::OpenFlags;
pub use handle::FileHandle;
pub use request_info::RequestInfo;

const DURATION: Duration = Duration::from_secs(0);

pub struct FuseDriver {
    pub db: DatabaseOps,
    handles: Slab<FileHandle>,
}

impl FuseDriver {
    pub fn new(db: DatabaseOps) -> Self {
        Self {
            db,
            handles: Slab::new(),
        }
    }

    fn ensure_root_exists(&mut self) -> Result<()> {
        self.db.with_write_tx(|tx| {
            match queries::inode::lookup(tx, 1) {
                // If ino is 1, this is the root directory.
                Err(Error::NotFound) => {
                    log::debug!("ino=1 requested, but does not exist yet, will create.");
                    let mut attr = FileAttrBuilder::new_directory().build();
                    queries::inode::create(tx, &mut attr)?;
                    Ok(())
                }
                Err(e) => Err(e),
                Ok(_) => Ok(()),
            }
        })
    }

    fn lookup_impl(&mut self, _req: RequestInfo, parent: u64, name: &OsStr) -> Result<FileAttr> {
        self.db.with_read_tx(|tx| {
            let ino = queries::dir_entry::lookup(tx, parent, name)?;
            let attr = queries::inode::lookup(tx, ino)?;
            Ok(attr)
        })
    }

    fn setattr_impl(
        &mut self,
        _req: RequestInfo,
        ino: u64,
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
        self.db.with_write_tx(|tx| {
            if let Some(mode) = mode {
                queries::inode::set_attr(tx, ino, "perm", mode)?;
            }
            if let Some(uid) = uid {
                queries::inode::set_attr(tx, ino, "uid", uid)?;
            }
            if let Some(gid) = gid {
                queries::inode::set_attr(tx, ino, "gid", gid)?;
            }
            if let Some(size) = size {
                let bno = Block::offset_to_bno(size);
                queries::block::remove_blocks_from(tx, ino, bno + 1)?;
                match queries::block::get_block(tx, ino, bno) {
                    Ok(mut block) => {
                        block.truncate(size);
                        queries::block::update(tx, &block)?;
                    }
                    Err(Error::NotFound) => {}
                    Err(e) => return Err(e),
                }
                queries::inode::set_attr(tx, ino, "size", size)?;
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

        self.db.with_write_tx(|tx| {
            queries::inode::create(tx, &mut attr)?;
            queries::dir_entry::create(tx, parent, name, attr.ino)?;
            Ok(attr)
        })
    }

    fn link_impl(&mut self, _req: RequestInfo, ino: u64, newparent: u64, newname: &OsStr) -> Result<FileAttr> {
        self.db.with_write_tx(|tx| {
            let mut attr = queries::inode::lookup(tx, ino)?;
            attr.nlink += 1;
            queries::dir_entry::create(tx, newparent, newname, ino)?;
            queries::inode::set_attr(tx, ino, "nlink", attr.nlink)?;
            Ok(attr)
        })
    }

    fn unlink_impl(&mut self, _req: RequestInfo, parent: u64, name: &OsStr) -> Result<()> {
        self.db.with_write_tx(|tx| {
            let ino = queries::dir_entry::lookup(tx, parent, name)?;
            let mut attr = queries::inode::lookup(tx, ino)?;
            attr.nlink -= 1;
            if attr.nlink > 0 {
                queries::inode::set_attr(tx, ino, "nlink", attr.nlink)?;
            } else {
                queries::block::remove_blocks_from(tx, ino, 0)?;
                queries::inode::remove(tx, ino)?;
            }
            queries::dir_entry::remove(tx, parent, name)?;
            Ok(())
        })
    }

    fn mkdir_impl(&mut self, req: RequestInfo, parent: u64, name: &OsStr, mode: u32, umask: u32) -> Result<FileAttr> {
        let mut attr = FileAttrBuilder::new_directory()
            .with_mode_umask(mode, umask)
            .with_uid(req.uid)
            .with_gid(req.gid)
            .build();

        self.db.with_write_tx(|tx| {
            queries::inode::create(tx, &mut attr)?;
            queries::dir_entry::create(tx, parent, name, attr.ino)?;
            Ok(attr)
        })
    }

    fn rmdir_impl(&mut self, _req: RequestInfo, parent: u64, name: &OsStr) -> Result<()> {
        self.db.with_write_tx(|tx| {
            let ino = queries::dir_entry::lookup(tx, parent, name)?;
            let empty = queries::dir_entry::is_dir_empty(tx, ino)?;
            if !empty {
                return Err(Error::NotEmpty);
            }
            queries::inode::remove(tx, ino)?;
            queries::dir_entry::remove(tx, parent, name)?;

            Ok(())
        })
    }

    fn readdir_impl<F>(&mut self, _req: RequestInfo, ino: u64, _fh: u64, offset: i64, iter: F) -> Result<()>
    where
        F: FnMut(ListDirEntry) -> bool,
    {
        self.db.with_read_tx(|tx| {
            queries::dir_entry::list_dir(tx, ino, offset, iter)?;
            Ok(())
        })
    }

    fn open_impl(&mut self, _req: RequestInfo, ino: u64, flags: OpenFlags) -> Result<(u64, u32)> {
        let attr = self.db.with_read_tx(|tx| queries::inode::lookup(tx, ino))?;
        let fh = self.handles.insert(FileHandle::new(ino, attr.size, flags));
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
        self.db.with_write_tx(|tx| handle.flush(tx))?;
        Ok(())
    }

    fn read_impl(
        &mut self,
        _req: RequestInfo,
        ino: u64,
        _fh: u64,
        offset: i64,
        size: u32,
        _flags: i32,
        _lock_owner: Option<u64>,
    ) -> Result<Vec<u8>> {
        self.db.with_read_tx(|tx| {
            let attr = queries::inode::lookup(tx, ino)?;
            let offset = offset as u64;
            let remaining = attr.size - offset;
            let cap = cmp::min(size as u64, remaining) as usize;
            let mut buf = Vec::with_capacity(cap);

            queries::block::iter_blocks_from(tx, ino, offset, |block| {
                block.copy_into(&mut buf);
                Ok(buf.len() < buf.capacity())
            })?;

            Ok(buf)
        })
    }

    fn write_impl(
        &mut self,
        _req: RequestInfo,
        _ino: u64,
        fh: u64,
        offset: i64,
        mut data: &[u8],
        _write_flags: u32,
        _flags: i32,
        _lock_owner: Option<u64>,
    ) -> Result<u32> {
        let fh = usize::try_from(fh).map_err(|_| Error::Overflow)?;
        let handle = self.handles.get_mut(fh).ok_or(Error::NotFound)?;
        let start_size = data.len();
        let offset = offset as u64;

        // Detect if seek happened. If it did flush whatever is in the buffer
        // where it belongs and then update the offset where to write to.
        if handle.write_offset() != offset {
            log::debug!(
                "seek occured, flushing, old offset = {}, new offset = {}",
                handle.write_offset(),
                offset
            );
            self.db.with_write_tx(|tx| handle.flush(tx))?;
            handle.seek_to(offset);
        }

        while !data.is_empty() {
            if handle.buffer_full() {
                self.db.with_write_tx(|tx| handle.flush(tx))?;
            }
            let consumed = handle.consume_input(data);
            data = &data[consumed..];
        }
        Ok(start_size as u32)
    }

    fn flush_impl(&mut self, _req: RequestInfo, _ino: u64, fh: u64, _lock_owner: u64) -> Result<()> {
        let fh = usize::try_from(fh).map_err(|_| Error::Overflow)?;
        let handle = self.handles.get_mut(fh).ok_or(Error::NotFound)?;
        self.db.with_write_tx(|tx| handle.flush(tx))
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
        self.db
            .with_write_tx(|tx| queries::dir_entry::rename(tx, parent, name, newparent, newname))
    }
}

impl fuser::Filesystem for FuseDriver {
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

    fn getattr(&mut self, _req: &fuser::Request<'_>, ino: u64, reply: fuser::ReplyAttr) {
        log::trace!("getattr(ino={})", ino);
        let res = self.db.with_read_tx(|tx| queries::inode::lookup(tx, ino));
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
        let res = self.setattr_impl(
            req.into(),
            ino,
            mode,
            uid,
            gid,
            size,
            atime.map(Into::into),
            mtime.map(Into::into),
            ctime.map(Into::into),
            fh,
            crtime.map(Into::into),
            chgtime.map(Into::into),
            bkuptime.map(Into::into),
            flags,
        );
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
        let res = self.read_impl(req.into(), ino, fh, offset, size, flags, lock_owner);
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
        let res = self.write_impl(req.into(), ino, fh, offset, data, write_flags, flags, lock_owner);
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

    use super::{attr::FileAttrBuilder, FuseDriver, RequestInfo};
    use crate::{database::DatabaseOps, errors::Error, queries, types::FileType};

    #[test]
    fn test_lookup() -> anyhow::Result<()> {
        let db = DatabaseOps::open_in_memory()?;
        let mut driver = FuseDriver::new(db);

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
}
