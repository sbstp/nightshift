#![allow(clippy::too_many_arguments)]
use std::{
    cmp,
    ffi::OsStr,
    time::{Duration, SystemTime},
};

use fuser::FileAttr;
use slab::Slab;

use crate::errors::{Error, Result};
use crate::types::FileType;
use crate::{database::DatabaseOps, time::TimeSpec};
use crate::{models::ListDirEntry, queries};

const DURATION: Duration = Duration::from_secs(0);
const POSIX_BLOCK_SIZE: u32 = 512;

#[derive(Clone, Copy, Debug)]
struct OpenFlags {
    bits: i32,
    read: bool,
    write: bool,
    create: bool,
    append: bool,
    truncate: bool,
    sync: bool,
}

impl From<i32> for OpenFlags {
    fn from(flags: i32) -> Self {
        let read = flags & libc::O_WRONLY == libc::O_RDONLY || flags & libc::O_RDWR == libc::O_RDWR;
        let write = flags & libc::O_WRONLY != 0 || flags & libc::O_RDWR == libc::O_RDWR;
        let create = flags & libc::O_CREAT == libc::O_CREAT;
        let append = flags & libc::O_APPEND == libc::O_APPEND;
        let truncate = flags & libc::O_TRUNC == libc::O_TRUNC;
        let sync = flags & libc::O_SYNC == libc::O_SYNC;
        OpenFlags {
            bits: flags,
            read,
            write,
            create,
            append,
            truncate,
            sync,
        }
    }
}

struct FileHandle {
    ino: u64,
    offset: u64,
    end_offset: u64,
    flags: OpenFlags,
}

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
                    let now = SystemTime::now();

                    let mut attr = FileAttr {
                        ino: 0,
                        size: 0,
                        blocks: 0,
                        atime: now,
                        mtime: now,
                        ctime: now,
                        crtime: now,
                        kind: fuser::FileType::Directory,
                        perm: 0o755u16, // TODO probably bad http://web.deu.edu.tr/doc/oreily/networking/puis/ch05_03.htm
                        nlink: 2,
                        uid: 1000, // TODO get real user
                        gid: 1000, // TODO get real group
                        rdev: 0,
                        blksize: 0,
                        flags: 0,
                    };
                    queries::inode::create(tx, &mut attr)?;
                    Ok(())
                }
                Err(e) => Err(e),
                Ok(_) => Ok(()),
            }
        })
    }

    fn lookup_impl(&mut self, _req: &fuser::Request<'_>, parent: u64, name: &std::ffi::OsStr) -> Result<FileAttr> {
        self.db.with_read_tx(|tx| {
            let ino = queries::dir_entry::lookup(tx, parent, name)?;
            let attr = queries::inode::lookup(tx, ino)?;
            Ok(attr)
        })
    }

    fn setattr_impl(
        &mut self,
        _req: &fuser::Request<'_>,
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
        req: &fuser::Request<'_>,
        parent: u64,
        name: &OsStr,
        mode: u32,
        umask: u32,
        rdev: u32,
    ) -> Result<FileAttr> {
        let kind = FileType::from_mode(mode).ok_or(Error::InvalidArgument)?;
        let now = SystemTime::now();

        let mut attr = FileAttr {
            ino: 0,
            size: 0,
            blocks: 0,
            atime: now,
            mtime: now,
            ctime: now,
            crtime: now,
            kind: kind.into(),
            perm: (mode & !umask) as u16, // TODO probably bad http://web.deu.edu.tr/doc/oreily/networking/puis/ch05_03.htm
            nlink: 1,
            uid: req.uid(),
            gid: req.gid(),
            rdev,
            blksize: POSIX_BLOCK_SIZE,
            flags: 0,
        };

        self.db.with_write_tx(|tx| {
            queries::inode::create(tx, &mut attr)?;
            queries::dir_entry::create(tx, parent, name, attr.ino)?;
            Ok(attr)
        })
    }

    fn link_impl(&mut self, _req: &fuser::Request<'_>, ino: u64, newparent: u64, newname: &OsStr) -> Result<FileAttr> {
        self.db.with_write_tx(|tx| {
            let mut attr = queries::inode::lookup(tx, ino)?;
            attr.nlink += 1;
            queries::dir_entry::create(tx, newparent, newname, ino)?;
            queries::inode::set_attr(tx, ino, "nlink", attr.nlink)?;
            Ok(attr)
        })
    }

    fn unlink_impl(&mut self, _req: &fuser::Request<'_>, parent: u64, name: &OsStr) -> Result<()> {
        self.db.with_write_tx(|tx| {
            let ino = queries::dir_entry::lookup(tx, parent, name)?;
            let mut attr = queries::inode::lookup(tx, ino)?;
            attr.nlink -= 1;
            if attr.nlink > 0 {
                queries::inode::set_attr(tx, ino, "nlink", attr.nlink)?;
            } else {
                queries::block::remove_blocks(tx, ino)?;
                queries::inode::remove(tx, ino)?;
            }
            queries::dir_entry::remove(tx, parent, name)?;
            Ok(())
        })
    }

    fn mkdir_impl(
        &mut self,
        req: &fuser::Request<'_>,
        parent: u64,
        name: &OsStr,
        mode: u32,
        umask: u32,
    ) -> Result<FileAttr> {
        let now = SystemTime::now();
        let mut attr = FileAttr {
            ino: 0,
            size: 0,
            blocks: 0,
            atime: now,
            mtime: now,
            ctime: now,
            crtime: now,
            kind: fuser::FileType::Directory,
            perm: (mode & !umask) as u16, // TODO probably bad
            nlink: 2,
            uid: req.uid(),
            gid: req.gid(),
            rdev: 0, // Not given for directory?
            blksize: 0,
            flags: 0,
        };
        self.db.with_write_tx(|tx| {
            queries::inode::create(tx, &mut attr)?;
            queries::dir_entry::create(tx, parent, name, attr.ino)?;
            Ok(attr)
        })
    }

    fn rmdir_impl(&mut self, _req: &fuser::Request<'_>, parent: u64, name: &OsStr) -> Result<()> {
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

    fn readdir_impl<F>(&mut self, _req: &fuser::Request<'_>, ino: u64, _fh: u64, offset: i64, iter: F) -> Result<()>
    where
        F: FnMut(ListDirEntry) -> bool,
    {
        self.db.with_read_tx(|tx| {
            queries::dir_entry::list_dir(tx, ino, offset, iter)?;
            Ok(())
        })
    }

    fn open_impl(&mut self, _req: &fuser::Request<'_>, ino: u64, flags: OpenFlags) -> Result<(u64, u32)> {
        let attr = self.db.with_read_tx(|tx| queries::inode::lookup(tx, ino))?;
        let fh = self.handles.insert(FileHandle {
            ino,
            offset: 0,
            end_offset: attr.size,
            flags,
        });
        let fh = u64::try_from(fh).map_err(|_| Error::Overflow)?;
        Ok((fh, flags.bits as u32))
    }

    fn release_impl(
        &mut self,
        _req: &fuser::Request<'_>,
        _ino: u64,
        fh: u64,
        _flags: i32,
        _lock_owner: Option<u64>,
        flush: bool,
    ) -> Result<()> {
        // TODO flush
        let fh = usize::try_from(fh).map_err(|_| Error::Overflow)?;
        self.handles.try_remove(fh).ok_or(Error::NotFound)?;
        Ok(())
    }

    fn lseek_impl(&mut self, _req: &fuser::Request<'_>, ino: u64, fh: u64, offset: i64, whence: i32) -> Result<i64> {
        let handle = self.handles.get_mut(fh as usize).ok_or(Error::NotFound)?;
        match whence {
            libc::SEEK_SET => {
                handle.offset = u64::try_from(offset).map_err(|_| Error::InvalidArgument)?;
            }
            libc::SEEK_CUR => {
                let mut current_offset = i64::try_from(handle.offset).map_err(|_| Error::InvalidArgument)?;
                current_offset += offset;
                handle.offset = u64::try_from(current_offset).map_err(|_| Error::InvalidArgument)?;
            }
            libc::SEEK_END => {
                self.db.with_read_tx(|tx| {
                    let attr = queries::inode::lookup(tx, ino)?;
                    let mut end_offset = i64::try_from(attr.size).map_err(|_| Error::InvalidArgument)?;
                    end_offset += offset;
                    handle.offset = u64::try_from(end_offset).map_err(|_| Error::InvalidArgument)?;
                    Ok(())
                })?;
            }
            _ => return Err(Error::InvalidArgument),
        };

        Ok(i64::try_from(handle.offset).map_err(|_| Error::Overflow)?)
    }

    fn read_impl(
        &mut self,
        _req: &fuser::Request<'_>,
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
        _req: &fuser::Request<'_>,
        ino: u64,
        _fh: u64,
        offset: i64,
        mut data: &[u8],
        _write_flags: u32,
        _flags: i32,
        _lock_owner: Option<u64>,
    ) -> Result<u32> {
        let size = data.len();
        let mut offset = offset as u64;

        self.db.with_write_tx(|tx| {
            let mut attr = queries::inode::lookup(tx, ino)?;
            let mut write_offset = offset;

            let mut modified_blocks = Vec::new();

            queries::block::iter_blocks_from(tx, ino, offset, |mut block| {
                let (written, diff) = block.write_at(write_offset, data);
                data = &data[written as usize..];
                write_offset += written;
                attr.size = (attr.size as i64 + diff) as u64;
                if written > 0 {
                    modified_blocks.push(block);
                }
                Ok(!data.is_empty())
            })?;

            for block in modified_blocks {
                queries::block::update(tx, &block)?;
            }

            // // Overwrite existing blocks until we get a NotFound error indicating
            // // that there's no more blocks to overwrite. This usually happens when
            // // seek is used or the block is incomplete.
            // while !data.is_empty() {
            //     match queries::block::update(tx, ino, offset, data) {
            //         Ok((written, bytes_diff)) => {
            //             data = &data[written as usize..];
            //             offset += written;
            //             attr.size = (attr.size as i64 + bytes_diff) as u64;
            //         }
            //         Err(Error::NotFound) => break,
            //         Err(e) => return Err(e),
            //     }
            // }

            // Write the rest of the data in a new block.
            while !data.is_empty() {
                let written = queries::block::create(tx, ino, offset, data)?;
                data = &data[written as usize..];
                offset += written;
                attr.size += written;
            }

            attr.blocks = attr.size.div_ceil(POSIX_BLOCK_SIZE as u64);

            queries::inode::set_attr(tx, ino, "size", attr.size)?;
            queries::inode::set_attr(tx, ino, "blocks", attr.blocks)?;

            Ok(size as u32)
        })
    }

    fn rename_impl(
        &mut self,
        _req: &fuser::Request<'_>,
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
        _config: &mut fuser::KernelConfig,
    ) -> std::result::Result<(), libc::c_int> {
        // config.set_max_write(crate::database::BLOCK_SIZE).unwrap();
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
        let res = self.lookup_impl(req, parent, name);
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
            req,
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
        let res = self.mknod_impl(req, parent, name, mode, umask, rdev);
        log::trace!("mknod: {:?}", res);

        match res {
            Ok(attr) => reply.entry(&DURATION, &attr, 0),
            Err(e) => reply.error(e.errno()),
        }
    }

    fn link(&mut self, req: &fuser::Request<'_>, ino: u64, newparent: u64, newname: &OsStr, reply: fuser::ReplyEntry) {
        log::trace!("link(ino={}, newparent={}, newname={:?}", ino, newparent, newname);
        let res = self.link_impl(req, ino, newparent, newname);
        log::trace!("link: {:?}", res);

        match res {
            Ok(attr) => reply.entry(&DURATION, &attr, 0),
            Err(e) => reply.error(e.errno()),
        }
    }

    fn unlink(&mut self, req: &fuser::Request<'_>, parent: u64, name: &OsStr, reply: fuser::ReplyEmpty) {
        log::trace!("unlink(parent={}, name={:?}", parent, name);
        let res = self.unlink_impl(req, parent, name);
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
        let res = self.mkdir_impl(req, parent, name, mode, umask);
        log::trace!("mkdir: {:?}", res);

        match res {
            Ok(attr) => reply.entry(&DURATION, &attr, 0),
            Err(e) => reply.error(e.errno()),
        }
    }

    fn rmdir(&mut self, req: &fuser::Request<'_>, parent: u64, name: &OsStr, reply: fuser::ReplyEmpty) {
        log::trace!("rmdir(parent={}, name={:?}", parent, name);
        let res = self.rmdir_impl(req, parent, name);
        log::trace!("rmdir: {:?}", res);

        match res {
            Ok(_) => reply.ok(),
            Err(e) => reply.error(e.errno()),
        }
    }

    fn readdir(&mut self, req: &fuser::Request<'_>, ino: u64, fh: u64, offset: i64, mut reply: fuser::ReplyDirectory) {
        log::trace!("readdir(ino={}, fh={}, offset={})", ino, fh, offset);
        let res = self.readdir_impl(req, ino, fh, offset, |entry| {
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
        log::trace!("open(ino={}, flags={:?}", ino, flags);
        let res = self.open_impl(req, ino, flags);
        log::trace!("open: {:?}", res);

        match res {
            Ok((fh, flags)) => reply.opened(fh, flags),
            Err(e) => reply.error(e.errno()),
        }
    }

    fn lseek(
        &mut self,
        req: &fuser::Request<'_>,
        ino: u64,
        fh: u64,
        offset: i64,
        whence: i32,
        reply: fuser::ReplyLseek,
    ) {
        log::trace!("lseek(ino={}, fh={}, offset={}, whence={}", ino, fh, offset, whence);
        let res = self.lseek_impl(req, ino, fh, offset, whence);
        log::trace!("lseek: {:?}", res);

        match res {
            Ok(new_offset) => reply.offset(new_offset),
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
        log::trace!("release(ino={}, fh={}, flush={}", ino, fh, flush);
        let res = self.release_impl(req, ino, fh, flags, lock_owner, flush);
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
        log::trace!("read(ino={}, offset={}, size={}", ino, offset, size);
        let res = self.read_impl(req, ino, fh, offset, size, flags, lock_owner);
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
        log::trace!("write(ino={}, offset={}, data_len={}", ino, offset, data.len());
        let res = self.write_impl(req, ino, fh, offset, data, write_flags, flags, lock_owner);
        log::trace!("write: {:?}", res);

        match res {
            Ok(written) => reply.written(written),
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
        let res = self.rename_impl(req, parent, name, newparent, newname, flags);
        log::trace!("rename: {:?}", res);

        match res {
            Ok(_) => reply.ok(),
            Err(e) => reply.error(e.errno()),
        }
    }
}

#[test]
fn test_open_flags() {
    let flags = OpenFlags::from(libc::O_RDONLY);
    assert_eq!(
        (
            flags.read,
            flags.write,
            flags.create,
            flags.append,
            flags.truncate,
            flags.sync
        ),
        (true, false, false, false, false, false)
    );

    let flags = OpenFlags::from(libc::O_WRONLY);
    assert_eq!(
        (
            flags.read,
            flags.write,
            flags.create,
            flags.append,
            flags.truncate,
            flags.sync
        ),
        (false, true, false, false, false, false)
    );

    let flags = OpenFlags::from(libc::O_RDWR);
    assert_eq!(
        (
            flags.read,
            flags.write,
            flags.create,
            flags.append,
            flags.truncate,
            flags.sync
        ),
        (true, true, false, false, false, false)
    );

    let flags = OpenFlags::from(libc::O_WRONLY | libc::O_CREAT | libc::O_APPEND);
    assert_eq!(
        (
            flags.read,
            flags.write,
            flags.create,
            flags.append,
            flags.truncate,
            flags.sync
        ),
        (false, true, true, true, false, false)
    );

    let flags = OpenFlags::from(libc::O_RDWR | libc::O_TRUNC | libc::O_SYNC);
    assert_eq!(
        (
            flags.read,
            flags.write,
            flags.create,
            flags.append,
            flags.truncate,
            flags.sync
        ),
        (true, true, false, false, true, true)
    );
}
