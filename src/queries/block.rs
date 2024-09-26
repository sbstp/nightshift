use std::cmp;

use crate::errors::Result;
use rusqlite::params;

pub const BLOCK_SIZE: u64 = 128 * 1024;

pub fn get_block(tx: &mut rusqlite::Transaction, ino: u64, bno: u64) -> Result<Block> {
    let mut stmt = tx.prepare_cached("SELECT bno, data, compression FROM block WHERE ino = ? AND bno = ?")?;
    let block = stmt.query_row(params![ino, bno], |row| {
        let data = row.get_ref(1)?.as_blob()?;
        let compression: Option<u8> = row.get(2)?;
        let block = CompressedBlock {
            ino,
            bno,
            compression: compression.try_into().map_err(|_| rusqlite::Error::InvalidQuery)?, // TODO: better error
            data,
        };
        Ok(block.decompress())
    })?;
    Ok(block)
}

pub fn iter_blocks_from(
    tx: &mut rusqlite::Transaction,
    ino: u64,
    offset: u64,
    mut iter: impl FnMut(Block) -> Result<bool>,
) -> Result<()> {
    let bno = Block::offset_to_bno(offset);
    let mut stmt =
        tx.prepare_cached("SELECT bno, data, compression FROM block WHERE ino = ? AND bno >= ? ORDER BY bno")?;
    let mut rows = stmt.query(params![ino, bno])?;
    while let Some(row) = rows.next()? {
        let data = row.get_ref(1)?.as_blob()?;
        let compression: Option<u8> = row.get(2)?;
        let block = CompressedBlock {
            ino,
            bno,
            compression: compression.try_into()?,
            data,
        };
        let more = iter(block.decompress())?;
        if !more {
            break;
        }
    }
    Ok(())
}

pub fn update(tx: &mut rusqlite::Transaction, block: &Block, compression: Compression) -> Result<()> {
    let mut buf = Vec::new();
    let cb = CompressedBlock::compress(block, compression, &mut buf);

    let mut stmt = tx.prepare_cached("UPDATE block SET data = ?, compression = ? WHERE ino = ? AND bno = ?")?;
    stmt.execute(params![cb.data, cb.compression as u8, block.ino, block.bno])?;

    Ok(())
}

pub fn create(
    tx: &mut rusqlite::Transaction,
    ino: u64,
    offset: u64,
    data: &[u8],
    compression: Compression,
) -> Result<u64> {
    let bno = Block::offset_to_bno(offset);
    let mut block = Block::empty(ino, bno);
    let written = block.consume(data);
    let mut buf = Vec::new();
    let cb = CompressedBlock::compress(&block, compression, &mut buf);

    let mut stmt = tx.prepare_cached("INSERT INTO block (ino, bno, data, compression) VALUES (?, ?, ?, ?)")?;
    stmt.execute(params![block.ino, block.bno, cb.data, compression as u8])?;

    Ok(written)
}

pub fn remove_blocks_from(tx: &mut rusqlite::Transaction, ino: u64, bno: u64) -> Result<()> {
    let mut stmt = tx.prepare_cached("DELETE FROM block WHERE ino = ? AND bno >= ?")?;
    stmt.execute(params![ino, bno])?;
    Ok(())
}

#[derive(Clone, Copy, Debug, PartialEq)]
#[repr(u8)]
pub enum Compression {
    None = 0,
    LZ4 = 1,
    Zstd = 2,
}

impl TryFrom<Option<u8>> for Compression {
    type Error = crate::errors::Error;

    fn try_from(value: Option<u8>) -> std::result::Result<Self, Self::Error> {
        match value {
            None | Some(1) => Ok(Compression::LZ4),
            Some(0) => Ok(Compression::None),
            Some(2) => Ok(Compression::Zstd),
            _ => Err(crate::errors::Error::InvalidCompression),
        }
    }
}

pub struct CompressedBlock<'d> {
    // Inode number.
    pub ino: u64,
    /// Block number.
    pub bno: u64,
    /// Compression scheme
    pub compression: Compression,
    // Block data. Always compressed
    pub data: &'d [u8],
}

impl<'d> CompressedBlock<'d> {
    pub fn decompress(self) -> Block {
        let buf = match self.compression {
            Compression::None => {
                let mut buf = self.data.to_owned();
                buf.truncate(buf.len());
                buf
            }
            Compression::LZ4 => {
                let mut buf = vec![0u8; BLOCK_SIZE as usize];
                let n = lz4_flex::decompress_into(self.data, &mut buf).expect("lz4 decompress output too small");
                log::debug!("LZ4 decompress {} result {}", self.data.len(), n);
                buf.truncate(n);
                buf
            }
            Compression::Zstd => {
                let mut buf = vec![0u8; BLOCK_SIZE as usize];
                zstd::stream::copy_decode(self.data, &mut buf).expect("zstd decompress error");
                log::debug!("Zstd decompress {} result {}", self.data.len(), buf.len());
                buf.truncate(buf.len());
                buf
            }
        };
        Block {
            ino: self.ino,
            bno: self.bno,
            data: buf,
        }
    }

    pub fn compress(block: &Block, compression: Compression, scratch: &'d mut Vec<u8>) -> CompressedBlock<'d> {
        scratch.clear();

        match compression {
            Compression::None => scratch.extend_from_slice(&block.data),
            Compression::LZ4 => {
                let max_size = lz4_flex::block::get_maximum_output_size(block.data.len());
                scratch.resize(max_size, 0);
                let written = lz4_flex::compress_into(&block.data, scratch).expect("lz4 compress output too small");
                log::debug!("LZ4 compress {} result {}", block.data.len(), written);
                scratch.truncate(written);
            }
            Compression::Zstd => {
                zstd::stream::copy_encode(&block.data[..], &mut *scratch, 0).expect("");
                log::debug!("Zstd compress {} result {}", block.data.len(), scratch.len());
            }
        }

        CompressedBlock {
            ino: block.ino,
            bno: block.bno,
            compression,
            data: &scratch[..],
        }
    }
}

pub struct Block {
    // Inode number.
    pub ino: u64,
    /// Block number.
    pub bno: u64,
    /// Block data. Always uncompressed.
    pub data: Vec<u8>,
}

impl Block {
    pub fn empty(ino: u64, bno: u64) -> Block {
        Block {
            ino,
            bno,
            data: Vec::new(),
        }
    }

    pub fn offset_to_bno(offset: u64) -> u64 {
        offset / BLOCK_SIZE
    }

    pub fn start_offset(&self) -> u64 {
        self.bno * BLOCK_SIZE
    }

    pub fn end_offset(&self) -> u64 {
        (self.bno + 1) * BLOCK_SIZE
    }

    fn available(&self) -> u32 {
        u32::try_from(BLOCK_SIZE - self.data.len() as u64).expect("block size overflow")
    }

    pub fn consume(&mut self, data: &[u8]) -> u64 {
        let avail = self.available();
        let data_len = u32::try_from(data.len()).expect("data size overflow");
        let max_write = cmp::min(avail, data_len) as usize;
        log::debug!("extend max write {}", max_write);
        self.data.extend_from_slice(&data[..max_write]);
        u64::try_from(max_write).expect("written overflow")
    }

    pub fn write_at(&mut self, inode_offset: u64, data: &[u8]) -> (u64, i64) {
        let start_len = self.data.len();
        let rel_offset = inode_offset - self.start_offset();
        self.data.resize(rel_offset as usize, 0);
        let written = self.consume(data);
        let diff = self.data.len() as i64 - start_len as i64;
        (written, diff)
    }

    pub fn copy_into(&self, dest: &mut Vec<u8>, offset: u64) -> usize {
        let rel_offset = offset.saturating_sub(self.start_offset()) as usize;
        let remaining = dest.capacity() - dest.len();
        dbg!(remaining, self.data.len(), rel_offset);
        let max_write = cmp::min(remaining, self.data.len() - rel_offset);
        dest.extend_from_slice(&self.data[rel_offset..][..max_write]);
        max_write
    }

    pub fn truncate(&mut self, inode_offset: u64) {
        let rel_size = inode_offset - self.start_offset();
        self.data.truncate(rel_size as usize);
    }
}

impl std::fmt::Debug for Block {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Block")
            .field("ino", &self.ino)
            .field("bno", &self.bno)
            .field("start_offset", &self.start_offset())
            .field("end_offset", &self.end_offset())
            .field("data.len()", &self.data.len())
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use test_log::test;

    use super::Block;
    use super::BLOCK_SIZE;

    #[test]
    fn test_block() {
        let b = Block::empty(37, 1);
        assert_eq!(b.ino, 37);
        assert_eq!(b.start_offset(), BLOCK_SIZE);
        assert_eq!(b.end_offset(), BLOCK_SIZE + BLOCK_SIZE);
        assert_eq!(b.available(), BLOCK_SIZE as u32);
    }

    #[test]
    fn test_block_consume() {
        let mut b = Block::empty(37, 0);
        assert_eq!(b.consume(&[0; 100]), 100);
        assert_eq!(b.consume(&[1; BLOCK_SIZE as usize]), BLOCK_SIZE - 100);
        assert!(b.data[..100].iter().all(|&b| b == 0));
        assert!(b.data[100..].iter().all(|&b| b == 1));
    }

    #[test]
    fn test_block_write_at() {
        let mut b = Block::empty(0, 1);
        assert_eq!(b.write_at(BLOCK_SIZE, &[1; 5]), (5, 5));
        assert_eq!(b.data, vec![1; 5]);

        let mut b = Block::empty(0, 1);
        assert_eq!(b.write_at(BLOCK_SIZE + 5, &[1; 5]), (5, 10));
        assert_eq!(b.data, vec![0, 0, 0, 0, 0, 1, 1, 1, 1, 1]);
    }

    #[test]
    fn test_block_copy_into() {
        let mut b = Block::empty(0, 0);
        b.data = (1u8..=10).collect();

        let mut buf = Vec::with_capacity(5);
        assert_eq!(b.copy_into(&mut buf, 0), 5);

        let mut buf = Vec::with_capacity(15);
        assert_eq!(b.copy_into(&mut buf, 0), 10);

        let mut buf = Vec::with_capacity(5);
        assert_eq!(b.copy_into(&mut buf, 5), 5);
        assert_eq!(buf, &[6, 7, 8, 9, 10]);
    }

    #[test]
    fn test_block_offset_to_bno() {
        assert_eq!(Block::offset_to_bno(0), 0);
        assert_eq!(Block::offset_to_bno(BLOCK_SIZE), 1);
    }
}
