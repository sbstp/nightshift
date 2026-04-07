use std::{
    collections::{BTreeMap, HashMap},
    ffi::{OsStr, OsString},
    sync::Arc,
};

use fuser::FileAttr;
use parking_lot::Mutex;

use crate::{database::DatabaseOps, queries};

use super::{Fno, Ino, Vfs, VfsHandle};

struct WriteBehindCore {
    db: DatabaseOps, // todo use connection pool
    nodes: BTreeMap<Ino, FileAttr>,
    entries: HashMap<OsString, Ino>,
    handles: BTreeMap<Fno, WriteBehindHandle>,
    read_lock: Mutex<()>, // todo striped lock
}

struct WriteBehind {
    core: Arc<WriteBehindCore>,
}

#[derive(Clone)]
struct WriteBehindHandle {
    fno: Fno,
    core: Arc<WriteBehindCore>,
}

impl WriteBehind {}

impl Vfs for WriteBehind {
    type Handle = WriteBehindHandle;

    fn ensure_root(&self) -> Result<(), super::Error> {
        todo!()
    }

    fn lookup_name(&self, parent: super::Ino, name: &OsStr) -> Result<fuser::FileAttr, super::Error> {
        todo!()
    }

    fn lookup_ino(&self, ino: super::Ino) -> Result<fuser::FileAttr, super::Error> {
        let _read_guard = self.core.read_lock.lock();
        match self.core.nodes.get(&ino) {
            Some(attr) => Ok(attr.clone()),
            None => {
                // Node either does not exist or has not been loaded yet
                // TODO: do DB lookup, place in node cache
                todo!()
            }
        }
    }

    fn open(&self, ino: super::Ino, flags: usize) -> Result<Self::Handle, super::Error> {
        todo!()
    }

    fn close(&self, fno: super::Fno) -> Result<(), super::Error> {
        todo!()
    }

    fn handle(&self, fno: super::Fno) -> Option<Self::Handle> {
        todo!()
    }
}

impl VfsHandle for WriteBehindHandle {
    fn fno(&self) -> Fno {
        self.fno
    }

    fn read(&self, offset: u64, size: u32) -> Result<bytes::Bytes, super::Error> {
        todo!()
    }

    fn write(&self, offset: u64, data: &[u8]) -> Result<(), super::Error> {
        todo!()
    }

    fn flush(&self) -> Result<(), super::Error> {
        todo!()
    }
}
