use std::ffi::OsStr;

use bytes::Bytes;
use derive_more::{From, Into};
use fuser::FileAttr;

use crate::driver::OpenFlags;

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
