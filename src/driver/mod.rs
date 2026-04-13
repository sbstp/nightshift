#![allow(clippy::too_many_arguments)]

pub mod attr;
mod flags;
mod handle;
mod request_info;

use std::{ffi::OsStr, time::Duration, time::SystemTime};

use fuser::FileAttr;

use crate::{
    errors::Error,
    vfs::{Fno, Ino, Vfs, VfsHandle},
};
pub use flags::OpenFlags;
pub use handle::FileHandle;

const DURATION: Duration = Duration::from_secs(0);

pub struct FuseDriver<V: Vfs> {
    vfs: V,
    mount_uid: u32,
    mount_gid: u32,
}

impl<V: Vfs<Error = Error>> FuseDriver<V>
where
    V::Handle: VfsHandle<Error = Error>,
{
    pub fn new(vfs: V, mount_uid: u32, mount_gid: u32) -> Self {
        Self { vfs, mount_uid, mount_gid }
    }

    fn apply_root_ownership(&self, mut attr: FileAttr) -> FileAttr {
        if attr.ino == 1 {
            attr.uid = self.mount_uid;
            attr.gid = self.mount_gid;
        }
        attr
    }
}

impl<V: Vfs<Error = Error>> fuser::Filesystem for FuseDriver<V>
where
    V::Handle: VfsHandle<Error = Error>,
{
    fn init(
        &mut self,
        _req: &fuser::Request<'_>,
        config: &mut fuser::KernelConfig,
    ) -> std::result::Result<(), libc::c_int> {
        config.set_max_write(128 * 1024).expect("unable to set max_write");
        match self.vfs.ensure_root() {
            Ok(()) => Ok(()),
            Err(e) => {
                log::error!("init error: {}", e);
                Err(e.errno())
            }
        }
    }

    fn lookup(&mut self, _req: &fuser::Request<'_>, parent: u64, name: &OsStr, reply: fuser::ReplyEntry) {
        log::trace!("lookup(parent={}, name={:?})", parent, name.to_string_lossy());
        let res = self.vfs.lookup_name(Ino::from(parent), name).map(|a| self.apply_root_ownership(a));
        log::trace!("lookup: {:?}", res);

        match res {
            Ok(attr) => reply.entry(&DURATION, &attr, 0),
            Err(e) => reply.error(e.errno()),
        }
    }

    fn getattr(&mut self, _req: &fuser::Request<'_>, ino: u64, reply: fuser::ReplyAttr) {
        log::trace!("getattr(ino={})", ino);
        let res = self.vfs.lookup_ino(Ino::from(ino)).map(|a| self.apply_root_ownership(a));
        log::trace!("getattr: {:?}", res);

        match res {
            Ok(attr) => reply.attr(&DURATION, &attr),
            Err(e) => reply.error(e.errno()),
        }
    }

    fn setattr(
        &mut self,
        _req: &fuser::Request<'_>,
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
        let res = self.vfs.setattr(
            Ino::from(ino),
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
        let res = self.vfs.mknod(Ino::from(parent), name, mode, umask, rdev, req.uid(), req.gid());
        log::trace!("mknod: {:?}", res);

        match res {
            Ok(attr) => reply.entry(&DURATION, &attr, 0),
            Err(e) => reply.error(e.errno()),
        }
    }

    fn link(&mut self, _req: &fuser::Request<'_>, ino: u64, newparent: u64, newname: &OsStr, reply: fuser::ReplyEntry) {
        log::trace!("link(ino={}, newparent={}, newname={:?})", ino, newparent, newname);
        let res = self.vfs.link(Ino::from(ino), Ino::from(newparent), newname);
        log::trace!("link: {:?}", res);

        match res {
            Ok(attr) => reply.entry(&DURATION, &attr, 0),
            Err(e) => reply.error(e.errno()),
        }
    }

    fn unlink(&mut self, _req: &fuser::Request<'_>, parent: u64, name: &OsStr, reply: fuser::ReplyEmpty) {
        log::trace!("unlink(parent={}, name={:?})", parent, name);
        let res = self.vfs.unlink(Ino::from(parent), name);
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
        let res = self.vfs.mkdir(Ino::from(parent), name, mode, umask, req.uid(), req.gid());
        log::trace!("mkdir: {:?}", res);

        match res {
            Ok(attr) => reply.entry(&DURATION, &attr, 0),
            Err(e) => reply.error(e.errno()),
        }
    }

    fn rmdir(&mut self, _req: &fuser::Request<'_>, parent: u64, name: &OsStr, reply: fuser::ReplyEmpty) {
        log::trace!("rmdir(parent={}, name={:?})", parent, name);
        let res = self.vfs.rmdir(Ino::from(parent), name);
        log::trace!("rmdir: {:?}", res);

        match res {
            Ok(_) => reply.ok(),
            Err(e) => reply.error(e.errno()),
        }
    }

    fn readdir(&mut self, _req: &fuser::Request<'_>, ino: u64, _fh: u64, offset: i64, mut reply: fuser::ReplyDirectory) {
        log::trace!("readdir(ino={}, fh={}, offset={})", ino, _fh, offset);
        let res = self.vfs.readdir(Ino::from(ino), offset, &mut |entry| {
            reply.add(entry.ino, entry.offset, entry.kind, entry.name)
        });
        log::trace!("readdir: {:?}", res);

        match res {
            Ok(_) => reply.ok(),
            Err(e) => reply.error(e.errno()),
        }
    }

    fn open(&mut self, _req: &fuser::Request<'_>, ino: u64, flags: i32, reply: fuser::ReplyOpen) {
        let flags = OpenFlags::from(flags);
        log::trace!("open(ino={}, flags={:?})", ino, flags);
        let res = self.vfs.open(Ino::from(ino), flags);
        log::trace!("open: {:?}", res.as_ref().map(|h| h.fno()));

        match res {
            Ok(handle) => reply.opened(handle.fno().into(), flags.bits as u32),
            Err(e) => reply.error(e.errno()),
        }
    }

    fn release(
        &mut self,
        _req: &fuser::Request<'_>,
        ino: u64,
        fh: u64,
        _flags: i32,
        _lock_owner: Option<u64>,
        flush: bool,
        reply: fuser::ReplyEmpty,
    ) {
        log::trace!("release(ino={}, fh={}, flush={})", ino, fh, flush);
        let res = self.vfs.close(Fno::from(fh));
        log::trace!("release: {:?}", res);

        match res {
            Ok(_) => reply.ok(),
            Err(e) => reply.error(e.errno()),
        }
    }

    fn read(
        &mut self,
        _req: &fuser::Request<'_>,
        ino: u64,
        fh: u64,
        offset: i64,
        size: u32,
        _flags: i32,
        _lock_owner: Option<u64>,
        reply: fuser::ReplyData,
    ) {
        log::trace!("read(ino={}, offset={}, size={})", ino, offset, size);
        let res = self
            .vfs
            .handle(Fno::from(fh))
            .ok_or(Error::NotFound)
            .and_then(|h| h.read(offset as u64, size));
        log::trace!("read: {:?}", res.as_ref().map(|d| d.len()));

        match res {
            Ok(data) => reply.data(&data),
            Err(Error::NotFound) => reply.data(&[]),
            Err(e) => reply.error(e.errno()),
        }
    }

    fn write(
        &mut self,
        _req: &fuser::Request<'_>,
        ino: u64,
        fh: u64,
        offset: i64,
        data: &[u8],
        _write_flags: u32,
        _flags: i32,
        _lock_owner: Option<u64>,
        reply: fuser::ReplyWrite,
    ) {
        log::trace!("write(ino={}, offset={}, data_len={})", ino, offset, data.len());
        let written = data.len() as u32;
        let res = self
            .vfs
            .handle(Fno::from(fh))
            .ok_or(Error::NotFound)
            .and_then(|h| h.write(offset as u64, data));
        log::trace!("write: {:?}", res);

        match res {
            Ok(_) => reply.written(written),
            Err(e) => reply.error(e.errno()),
        }
    }

    fn flush(&mut self, _req: &fuser::Request<'_>, ino: u64, fh: u64, _lock_owner: u64, reply: fuser::ReplyEmpty) {
        log::trace!("flush(ino={}, fh={})", ino, fh);
        let res = self
            .vfs
            .handle(Fno::from(fh))
            .ok_or(Error::NotFound)
            .and_then(|h| h.flush());
        log::trace!("flush: {:?}", res);

        match res {
            Ok(_) => reply.ok(),
            Err(e) => reply.error(e.errno()),
        }
    }

    fn rename(
        &mut self,
        _req: &fuser::Request<'_>,
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
        let res = self.vfs.rename(Ino::from(parent), name, Ino::from(newparent), newname, flags);
        log::trace!("rename: {:?}", res);

        match res {
            Ok(_) => reply.ok(),
            Err(e) => reply.error(e.errno()),
        }
    }
}
