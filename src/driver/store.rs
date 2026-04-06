use std::ffi::OsStr;

use bytes::BufMut;
use fuser::FileAttr;

use crate::{
    buffer::FixedBuffer,
    database::DatabaseOps,
    errors::{Error, Result},
    offsets::Absolute,
    queries::{
        self,
        block::{Block, Compression},
        dir_entry::ListDirEntry,
    },
    time::TimeSpec,
};

use super::{attr::FileAttrBuilder, handle::FileHandle};

pub struct SetAttr {
    pub mode: Option<u32>,
    pub uid: Option<u32>,
    pub gid: Option<u32>,
    pub size: Option<u64>,
    pub atime: Option<TimeSpec>,
    pub mtime: Option<TimeSpec>,
    pub ctime: Option<TimeSpec>,
    pub crtime: Option<TimeSpec>,
    pub flags: Option<u32>,
}

pub trait Store {
    fn ensure_root(&mut self) -> Result<()>;
    fn lookup_entry(&mut self, parent: u64, name: &OsStr) -> Result<FileAttr>;
    fn get_inode(&mut self, ino: u64) -> Result<FileAttr>;
    fn set_inode_attrs(&mut self, ino: u64, changes: SetAttr, compression: Compression) -> Result<FileAttr>;
    fn create_node(&mut self, attr: &mut FileAttr, parent: u64, name: &OsStr) -> Result<()>;
    fn create_link(&mut self, ino: u64, newparent: u64, newname: &OsStr) -> Result<FileAttr>;
    fn unlink(&mut self, parent: u64, name: &OsStr) -> Result<()>;
    fn create_dir(&mut self, attr: &mut FileAttr, parent: u64, name: &OsStr) -> Result<()>;
    fn rmdir(&mut self, parent: u64, name: &OsStr) -> Result<()>;
    fn list_dir<F>(&mut self, ino: u64, offset: i64, iter: F) -> Result<()>
    where
        F: FnMut(ListDirEntry) -> bool;
    fn flush_handle(&mut self, handle: &mut FileHandle) -> Result<()>;
    fn read_file(&mut self, ino: u64, offset: Absolute, size: u32) -> Result<FixedBuffer>;
    fn rename_entry(&mut self, parent: u64, name: &OsStr, newparent: u64, newname: &OsStr) -> Result<()>;
}

impl Store for DatabaseOps {
    fn ensure_root(&mut self) -> Result<()> {
        self.with_write_tx(|tx| match queries::inode::lookup(tx, 1) {
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

    fn lookup_entry(&mut self, parent: u64, name: &OsStr) -> Result<FileAttr> {
        self.with_read_tx(|tx| {
            let ino = queries::dir_entry::lookup(tx, parent, name)?;
            queries::inode::lookup(tx, ino)
        })
    }

    fn get_inode(&mut self, ino: u64) -> Result<FileAttr> {
        self.with_read_tx(|tx| queries::inode::lookup(tx, ino))
    }

    fn set_inode_attrs(&mut self, ino: u64, changes: SetAttr, compression: Compression) -> Result<FileAttr> {
        self.with_write_tx(|tx| {
            if let Some(mode) = changes.mode {
                queries::inode::set_attr(tx, ino, "perm", mode)?;
            }
            if let Some(uid) = changes.uid {
                queries::inode::set_attr(tx, ino, "uid", uid)?;
            }
            if let Some(gid) = changes.gid {
                queries::inode::set_attr(tx, ino, "gid", gid)?;
            }
            if let Some(size) = changes.size.map(Absolute::from) {
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
            if let Some(atime) = changes.atime {
                queries::inode::set_attr(tx, ino, "atime_secs", atime.secs)?;
                queries::inode::set_attr(tx, ino, "atime_nanos", atime.nanos)?;
            }
            if let Some(mtime) = changes.mtime {
                queries::inode::set_attr(tx, ino, "mtime_secs", mtime.secs)?;
                queries::inode::set_attr(tx, ino, "mtime_nanos", mtime.nanos)?;
            }
            if let Some(ctime) = changes.ctime {
                queries::inode::set_attr(tx, ino, "ctime_secs", ctime.secs)?;
                queries::inode::set_attr(tx, ino, "ctime_nanos", ctime.nanos)?;
            }
            if let Some(crtime) = changes.crtime {
                queries::inode::set_attr(tx, ino, "crtime_secs", crtime.secs)?;
                queries::inode::set_attr(tx, ino, "crtime_nanos", crtime.nanos)?;
            }
            if let Some(flags) = changes.flags {
                queries::inode::set_attr(tx, ino, "flags", flags)?;
            }
            queries::inode::lookup(tx, ino)
        })
    }

    fn create_node(&mut self, attr: &mut FileAttr, parent: u64, name: &OsStr) -> Result<()> {
        self.with_write_tx(|tx| {
            queries::inode::create(tx, attr)?;
            queries::dir_entry::create(tx, parent, name, attr.ino)?;
            Ok(())
        })
    }

    fn create_link(&mut self, ino: u64, newparent: u64, newname: &OsStr) -> Result<FileAttr> {
        self.with_write_tx(|tx| {
            let mut attr = queries::inode::lookup(tx, ino)?;
            attr.nlink += 1;
            queries::dir_entry::create(tx, newparent, newname, ino)?;
            queries::inode::set_attr(tx, ino, "nlink", attr.nlink)?;
            Ok(attr)
        })
    }

    fn unlink(&mut self, parent: u64, name: &OsStr) -> Result<()> {
        self.with_write_tx(|tx| {
            let ino = queries::dir_entry::lookup(tx, parent, name)?;
            let mut attr = queries::inode::lookup(tx, ino)?;
            attr.nlink -= 1;
            if attr.nlink > 0 {
                queries::inode::set_attr(tx, ino, "nlink", attr.nlink)?;
                queries::dir_entry::remove(tx, parent, name)?;
            } else {
                // If nlink == 0, the inode removal will remove the dir_entry through CASCADE.
                // The blocks will also be removed through CASCADE.
                queries::inode::remove(tx, ino)?;
            }
            Ok(())
        })
    }

    fn create_dir(&mut self, attr: &mut FileAttr, parent: u64, name: &OsStr) -> Result<()> {
        self.with_write_tx(|tx| {
            queries::inode::create(tx, attr)?;
            queries::dir_entry::create(tx, parent, name, attr.ino)?;
            Ok(())
        })
    }

    fn rmdir(&mut self, parent: u64, name: &OsStr) -> Result<()> {
        self.with_write_tx(|tx| {
            let ino = queries::dir_entry::lookup(tx, parent, name)?;
            let empty = queries::dir_entry::is_dir_empty(tx, ino)?;
            if !empty {
                return Err(Error::NotEmpty);
            }
            queries::inode::remove(tx, ino)?; // CASCADE will remove dir_entry
            Ok(())
        })
    }

    fn list_dir<F>(&mut self, ino: u64, offset: i64, iter: F) -> Result<()>
    where
        F: FnMut(ListDirEntry) -> bool,
    {
        self.with_read_tx(|tx| {
            queries::dir_entry::list_dir(tx, ino, offset, iter)?;
            Ok(())
        })
    }

    fn flush_handle(&mut self, handle: &mut FileHandle) -> Result<()> {
        self.with_write_tx(|tx| handle.flush(tx))
    }

    fn read_file(&mut self, ino: u64, offset: Absolute, size: u32) -> Result<FixedBuffer> {
        self.with_read_tx(|tx| {
            let attr = queries::inode::lookup(tx, ino)?;
            let remaining = Absolute::from(attr.size) - offset;
            let cap = remaining.min(size);
            let mut buf = FixedBuffer::with_capacity(cap);

            queries::block::iter_blocks_from(tx, ino, offset, |block| {
                block.copy_into(&mut buf, offset);
                Ok(buf.has_remaining_mut())
            })?;
            assert!(buf.len() <= size as usize);
            Ok(buf)
        })
    }

    fn rename_entry(&mut self, parent: u64, name: &OsStr, newparent: u64, newname: &OsStr) -> Result<()> {
        self.with_write_tx(|tx| queries::dir_entry::rename(tx, parent, name, newparent, newname))
    }
}
