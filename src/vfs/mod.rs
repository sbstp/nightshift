use std::ffi::OsStr;

use bytes::Bytes;
use derive_more::{From, Into};
use fuser::FileAttr;

use crate::driver::OpenFlags;
use crate::queries::dir_entry::ListDirEntry;
use crate::time::TimeSpec;

pub mod sync_vfs;
pub mod write_behind;

#[derive(Debug, Clone, Copy, From, Into, Hash, PartialEq, Eq, PartialOrd, Ord)]
pub struct Ino(u64);

#[derive(Debug, Clone, Copy, From, Into, Hash, PartialEq, Eq, PartialOrd, Ord)]
pub struct Fno(u64);

impl From<usize> for Fno {
    fn from(value: usize) -> Self {
        Self(value.try_into().expect("fno out of range"))
    }
}

impl From<Fno> for usize {
    fn from(value: Fno) -> Self {
        value.0.try_into().expect("fno out of range")
    }
}

#[derive(Debug, Clone, Copy, From, Into, Hash, PartialEq, Eq, PartialOrd, Ord)]
pub struct Bno(u64);

pub trait Vfs: Clone + Send + Sync {
    type Handle: VfsHandle;
    type Error;

    fn ensure_root(&self) -> Result<(), Self::Error>;
    fn lookup_name(&self, parent: Ino, name: &OsStr) -> Result<FileAttr, Self::Error>;
    fn lookup_ino(&self, ino: Ino) -> Result<FileAttr, Self::Error>;

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
        fh: Option<u64>,
        crtime: Option<TimeSpec>,
        chgtime: Option<TimeSpec>,
        bkuptime: Option<TimeSpec>,
        flags: Option<u32>,
    ) -> Result<FileAttr, Self::Error> {
        let _ = (
            ino, mode, uid, gid, size, atime, mtime, ctime, fh, crtime, chgtime, bkuptime, flags,
        );
        unimplemented!()
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
    ) -> Result<FileAttr, Self::Error>;
    fn link(&self, ino: Ino, newparent: Ino, newname: &OsStr) -> Result<FileAttr, Self::Error>;
    fn unlink(&self, parent: Ino, name: &OsStr) -> Result<(), Self::Error>;
    fn rename(&self, parent: Ino, name: &OsStr, newparent: Ino, newname: &OsStr, flags: u32)
        -> Result<(), Self::Error>;

    fn mkdir(
        &self,
        parent: Ino,
        name: &OsStr,
        mode: u32,
        umask: u32,
        uid: u32,
        gid: u32,
    ) -> Result<FileAttr, Self::Error>;
    fn rmdir(&self, parent: Ino, name: &OsStr) -> Result<(), Self::Error>;
    fn readdir(&self, ino: Ino, offset: i64, f: &mut dyn FnMut(ListDirEntry) -> bool) -> Result<(), Self::Error>;

    fn open(&self, ino: Ino, flags: OpenFlags) -> Result<Self::Handle, Self::Error>;
    fn close(&self, fno: Fno) -> Result<(), Self::Error>;
    fn handle(&self, fno: Fno) -> Option<Self::Handle>;
}

pub trait VfsHandle: Clone + Send + Sync {
    type Error;

    fn fno(&self) -> Fno;
    fn read(&self, offset: u64, size: u32) -> Result<Bytes, Self::Error>;
    fn write(&self, offset: u64, data: &[u8]) -> Result<(), Self::Error>;
    fn flush(&self) -> Result<(), Self::Error>;
}
