use std::time::SystemTime;

use crate::types::FileType;
use fuser::FileAttr;

const POSIX_BLOCK_SIZE: u32 = 512;

pub struct FileAttrBuilder {
    attr: FileAttr,
}

impl FileAttrBuilder {
    pub fn new_directory() -> FileAttrBuilder {
        let now = SystemTime::now();
        FileAttrBuilder {
            attr: FileAttr {
                ino: 0,
                size: 0,
                blocks: 0,
                atime: now,
                mtime: now,
                ctime: now,
                crtime: now,
                kind: fuser::FileType::Directory,
                perm: 0o755,
                nlink: 2,
                uid: 0,
                gid: 0,
                rdev: 0,
                blksize: POSIX_BLOCK_SIZE,
                flags: 0,
            },
        }
    }

    pub fn new_node(kind: FileType) -> FileAttrBuilder {
        let now = SystemTime::now();
        FileAttrBuilder {
            attr: FileAttr {
                ino: 0,
                size: 0,
                blocks: 0,
                atime: now,
                mtime: now,
                ctime: now,
                crtime: now,
                kind: kind.into(),
                perm: 0o644,
                nlink: 1,
                uid: 0,
                gid: 0,
                rdev: 0,
                blksize: POSIX_BLOCK_SIZE,
                flags: 0,
            },
        }
    }

    pub fn with_uid(mut self, uid: u32) -> FileAttrBuilder {
        self.attr.uid = uid;
        self
    }

    pub fn with_gid(mut self, gid: u32) -> FileAttrBuilder {
        self.attr.gid = gid;
        self
    }

    pub fn with_mode_umask(mut self, mut mode: u32, umask: u32) -> FileAttrBuilder {
        mode &= !libc::S_IFMT; // remove file type from mode
        self.attr.perm = (mode & !umask) as u16;
        self
    }

    pub fn with_ino(mut self, ino: u64) -> FileAttrBuilder {
        self.attr.ino = ino;
        self
    }

    pub fn with_rdev(mut self, rdev: u32) -> FileAttrBuilder {
        self.attr.rdev = rdev;
        self
    }

    pub fn build(self) -> FileAttr {
        self.attr
    }
}
