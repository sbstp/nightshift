use std::ffi::OsStr;

use bytes::Bytes;
use derive_more::{From, Into};
use fuser::FileAttr;

pub mod write_behind;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Error {
    NotEmpty,
    NotFound,
    InvalidArgument,
    Overflow,
    Other(String),
    InvalidCompression,
}

impl Error {
    pub fn errno(self) -> libc::c_int {
        match self {
            Error::NotEmpty => libc::ENOTEMPTY,
            Error::NotFound => libc::ENOENT,
            Error::InvalidArgument => libc::EINVAL,
            Error::Overflow => libc::EOVERFLOW,
            Error::InvalidCompression => libc::EINVAL,
            Error::Other(_) => libc::ENOTSUP, // Need better code
        }
    }
}

#[derive(Debug, Clone, Copy, From, Into, Hash, PartialEq, Eq, PartialOrd, Ord)]
pub struct Ino(u64);

#[derive(Debug, Clone, Copy, From, Into, Hash, PartialEq, Eq, PartialOrd, Ord)]
pub struct Fno(u64);

pub trait Vfs {
    type Handle: VfsHandle;

    fn ensure_root(&self) -> Result<(), Error>;
    fn lookup_name(&self, parent: Ino, name: &OsStr) -> Result<FileAttr, Error>;
    fn lookup_ino(&self, ino: Ino) -> Result<FileAttr, Error>;

    fn open(&self, ino: Ino, flags: usize) -> Result<Self::Handle, Error>;
    fn close(&self, fno: Fno) -> Result<(), Error>;
    fn handle(&self, fno: Fno) -> Option<Self::Handle>;
}

pub trait VfsHandle: Clone + Sized {
    fn fno(&self) -> Fno;
    fn read(&self, offset: u64, size: u32) -> Result<Bytes, Error>;
    fn write(&self, offset: u64, data: &[u8]) -> Result<(), Error>;
    fn flush(&self) -> Result<(), Error>;
}
