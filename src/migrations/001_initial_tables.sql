CREATE TABLE IF NOT EXISTS inode(
    ino INTEGER PRIMARY KEY,
    size INTEGER NOT NULL,
    blocks INTEGER NOT NULL,
    atime_secs INTEGER NOT NULL,
    atime_nanos INTEGER NOT NULL,
    mtime_secs INTEGER NOT NULL,
    mtime_nanos INTEGER NOT NULL,
    ctime_secs INTEGER NOT NULL,
    ctime_nanos INTEGER NOT NULL,
    crtime_secs INTEGER NOT NULL,
    crtime_nanos INTEGER NOT NULL,
    kind INTEGER NOT NULL,
    perm INTEGER NOT NULL,
    nlink INTEGER NOT NULL,
    uid INTEGER NOT NULL,
    gid INTEGER NOT NULL,
    rdev INTEGER NOT NULL,
    blksize INTEGER NOT NULL,
    flags INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS dir_entry (
    parent_ino INTEGER NOT NULL REFERENCES inode(ino) ON UPDATE CASCADE ON DELETE RESTRICT, -- do not allow directory with files to be dropped
    name BLOB NOT NULL,
    ino INTEGER NOT NULL REFERENCES inode(ino) ON UPDATE CASCADE ON DELETE CASCADE -- if inode is deleted, delete dir entry
);

CREATE INDEX IF NOT EXISTS entry_parent_ino_name_idx ON dir_entry (parent_ino, name);

CREATE TABLE IF NOT EXISTS block (
    ino INTEGER NOT NULL REFERENCES inode(ino) ON UPDATE CASCADE ON DELETE CASCADE, -- if inode is deleted, delete all data blocks
    bno INTEGER NOT NULL,
    data BLOB NOT NULL
);

CREATE INDEX IF NOT EXISTS block_bno_idx ON block (bno);
CREATE INDEX IF NOT EXISTS block_ino_bno_idx ON block (ino, bno);
