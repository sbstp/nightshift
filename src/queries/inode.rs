use crate::{
    errors::{Error, Result},
    time::TimeSpec,
    types::FileType,
};
use rusqlite::params;

pub fn lookup(tx: &mut rusqlite::Transaction, ino: u64) -> Result<fuser::FileAttr> {
    let mut stmt = tx.prepare_cached(include_str!("sql/lookup_inode.sql"))?;
    let attr = stmt.query_row(params![ino], |row| {
        let mut rc = RowCounter::default();
        Ok(fuser::FileAttr {
            ino: row.get(rc.next())?,
            size: row.get(rc.next())?,
            blocks: row.get(rc.next())?,
            atime: TimeSpec::new(row.get(rc.next())?, row.get(rc.next())?).into(),
            mtime: TimeSpec::new(row.get(rc.next())?, row.get(rc.next())?).into(),
            ctime: TimeSpec::new(row.get(rc.next())?, row.get(rc.next())?).into(),
            crtime: TimeSpec::new(row.get(rc.next())?, row.get(rc.next())?).into(),
            kind: FileType::import(row.get(rc.next())?),
            perm: row.get(rc.next())?,
            nlink: row.get(rc.next())?,
            uid: row.get(rc.next())?,
            gid: row.get(rc.next())?,
            rdev: row.get(rc.next())?,
            blksize: row.get(rc.next())?,
            flags: row.get(rc.next())?,
        })
    })?;
    Ok(attr)
}

pub fn create(tx: &mut rusqlite::Transaction, attr: &mut fuser::FileAttr) -> Result<()> {
    let atime = TimeSpec::from(attr.atime);
    let mtime = TimeSpec::from(attr.mtime);
    let ctime = TimeSpec::from(attr.ctime);
    let crtime = TimeSpec::from(attr.crtime);

    let mut stmt = tx.prepare_cached(include_str!("sql/create_inode.sql"))?;
    let ino = stmt.insert(params![
        attr.size,
        attr.blocks,
        atime.secs,
        atime.nanos,
        mtime.secs,
        mtime.nanos,
        ctime.secs,
        ctime.nanos,
        crtime.secs,
        crtime.nanos,
        FileType::export(attr.kind),
        attr.perm,
        attr.nlink,
        attr.uid,
        attr.gid,
        attr.rdev,
        attr.blksize,
        attr.flags,
    ])?;

    attr.ino = ino as u64;
    Ok(())
}

pub fn create_with_ino(tx: &mut rusqlite::Transaction, attr: &fuser::FileAttr) -> Result<()> {
    let atime = TimeSpec::from(attr.atime);
    let mtime = TimeSpec::from(attr.mtime);
    let ctime = TimeSpec::from(attr.ctime);
    let crtime = TimeSpec::from(attr.crtime);

    let mut stmt = tx.prepare_cached(include_str!("sql/create_inode_with_ino.sql"))?;
    stmt.insert(params![
        attr.ino,
        attr.size,
        attr.blocks,
        atime.secs,
        atime.nanos,
        mtime.secs,
        mtime.nanos,
        ctime.secs,
        ctime.nanos,
        crtime.secs,
        crtime.nanos,
        FileType::export(attr.kind),
        attr.perm,
        attr.nlink,
        attr.uid,
        attr.gid,
        attr.rdev,
        attr.blksize,
        attr.flags,
    ])?;
    Ok(())
}

pub fn set_attr(
    tx: &mut rusqlite::Transaction,
    ino: u64,
    name: &str,
    value: impl rusqlite::ToSql + std::fmt::Debug,
) -> Result<()> {
    let mut stmt = tx.prepare_cached(&format!("UPDATE inode SET `{name}` = ? WHERE ino = ?"))?;
    let affected = stmt.execute(params![value, ino])?;
    match affected {
        0 => Err(Error::NotFound),
        _ => Ok(()),
    }
}

pub fn remove(tx: &mut rusqlite::Transaction, ino: u64) -> Result<()> {
    let mut stmt = tx.prepare_cached("DELETE FROM inode WHERE ino = ?")?;
    let affected = stmt.execute(params![ino])?;
    match affected {
        0 => Err(Error::NotFound),
        _ => Ok(()),
    }
}

#[derive(Default)]
struct RowCounter {
    c: usize,
}

impl RowCounter {
    fn next(&mut self) -> usize {
        let x = self.c;
        self.c += 1;
        x
    }
}
