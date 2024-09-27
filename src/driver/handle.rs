use std::cmp;

use crate::driver::OpenFlags;
use crate::errors::Result;
use crate::queries;
use crate::queries::block::{Block, Compression};

const BUFFER_SIZE: usize = 2 * 1024 * 1024;

#[derive(Debug)]
pub struct FileHandle {
    pub ino: u64,
    pub size: u64,
    #[allow(dead_code)]
    pub flags: OpenFlags,
    /// Stores the write position where buf must be written.
    write_offset: u64,
    /// Write data buffer used to optimize writes.
    pub buf: Vec<u8>,
    compression: Compression,
}

impl FileHandle {
    pub fn new(ino: u64, size: u64, flags: OpenFlags, compression: Compression) -> Self {
        FileHandle {
            ino,
            size,
            flags,
            write_offset: 0,
            buf: Vec::with_capacity(BUFFER_SIZE),
            compression,
        }
    }

    fn buffer_remaining(&self) -> usize {
        self.buf.capacity() - self.buf.len()
    }

    pub fn buffer_empty(&self) -> bool {
        self.buf.is_empty()
    }

    pub fn buffer_full(&self) -> bool {
        self.buffer_remaining() == 0
    }

    pub fn write_offset(&self) -> u64 {
        self.write_offset + self.buf.len() as u64
    }

    pub fn seek_to(&mut self, offset: u64) {
        assert_eq!(self.buf.len(), 0);
        self.write_offset = offset;
    }

    pub fn consume_input(&mut self, buf: &[u8]) -> usize {
        let write = cmp::min(buf.len(), self.buffer_remaining());
        self.buf.extend_from_slice(&buf[..write]);
        write
    }

    pub fn flush(&mut self, tx: &mut rusqlite::Transaction) -> Result<()> {
        if self.buf.is_empty() {
            return Ok(());
        }
        log::debug!(
            "Flush called, buf.len()={} buf.capacity()={}",
            self.buf.len(),
            self.buf.capacity()
        );

        let mut attr = queries::inode::lookup(tx, self.ino)?;
        let mut new_offset = self.write_offset;
        let mut data = &self.buf[..];
        let mut modified_blocks = Vec::new();

        // Update blocks if the start offset overrides blocks.
        queries::block::iter_blocks_from(tx, self.ino, new_offset, |mut block| {
            let (written, diff) = block.write_at(new_offset, data);
            log::debug!(
                "Update block {} at offset={}, written={}, diff={}",
                block.bno,
                new_offset,
                written,
                diff
            );
            data = &data[written as usize..];
            new_offset += written;
            attr.size = (attr.size as i64 + diff) as u64;
            if written > 0 {
                modified_blocks.push(block);
            }
            Ok(!data.is_empty())
        })?;

        for block in modified_blocks {
            queries::block::update(tx, &block, self.compression)?;
        }

        // Write the rest of the data in a new block.
        while !data.is_empty() {
            let written = queries::block::create(tx, self.ino, new_offset, data, self.compression)?;
            log::debug!(
                "Create block {} at offset={}, written={}, diff={}",
                Block::offset_to_bno(new_offset),
                new_offset,
                written,
                written
            );
            data = &data[written as usize..];
            new_offset += written;
            attr.size += written;
        }

        attr.blocks = attr.size.div_ceil(attr.blksize as u64);
        queries::inode::set_attr(tx, self.ino, "size", attr.size)?;
        queries::inode::set_attr(tx, self.ino, "blocks", attr.blocks)?;

        self.buf.clear();
        self.write_offset = new_offset;
        self.size = attr.size;

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use crate::driver::attr::FileAttrBuilder;
    use crate::driver::{FileHandle, OpenFlags};
    use crate::queries;
    use crate::queries::block::{Compression, BLOCK_SIZE};
    use test_log::test;

    #[test]
    fn test_file_handle_buffer_remaining() {
        let fh = FileHandle {
            ino: 1,
            size: 10,
            flags: OpenFlags::from(0),
            write_offset: 0,
            buf: Vec::with_capacity(37),
            compression: Compression::None,
        };
        assert_eq!(fh.buffer_remaining(), 37);
    }

    #[test]
    fn test_file_handle_buffer_full() {
        let mut fh = FileHandle {
            ino: 1,
            size: 10,
            flags: OpenFlags::from(0),
            write_offset: 0,
            buf: vec![0; 37],
            compression: Compression::None,
        };
        assert!(fh.buffer_full());
        fh.buf.reserve(10);
        assert!(!fh.buffer_full());
    }

    #[test]
    fn test_file_handle_seek_to() {
        let mut fh = FileHandle {
            ino: 1,
            size: 10,
            flags: OpenFlags::from(0),
            write_offset: 0,
            buf: Vec::with_capacity(1000),
            compression: Compression::None,
        };
        fh.seek_to(500);
        assert_eq!(fh.write_offset(), 500);
    }

    #[test]
    #[should_panic]
    fn test_file_handle_seek_to_buffer_not_flushed() {
        let mut fh = FileHandle {
            ino: 1,
            size: 10,
            flags: OpenFlags::from(0),
            write_offset: 0,
            buf: vec![0; 37],
            compression: Compression::None,
        };
        fh.seek_to(0);
    }

    #[test]
    fn test_file_handle_consume() {
        let mut fh = FileHandle {
            ino: 1,
            size: 10,
            flags: OpenFlags::from(0),
            write_offset: 1000,
            buf: Vec::with_capacity(64),
            compression: Compression::None,
        };
        assert_eq!(5, fh.consume_input(&[5; 5]));
        assert_eq!(59, fh.consume_input(&[5; 100]));
        assert_eq!(1064, fh.write_offset());
    }

    #[test]
    fn test_file_handle_flush() -> anyhow::Result<()> {
        let mut cx = rusqlite::Connection::open_in_memory()?;
        cx.execute_batch(include_str!("../queries/sql/schema.sql"))?;
        let mut tx = cx.transaction()?;

        let mut attr = FileAttrBuilder::new_node(crate::types::FileType::RegularFile).build();
        queries::inode::create(&mut tx, &mut attr)?;
        let mut fh = FileHandle::new(attr.ino, attr.size, OpenFlags::from(0), Compression::None);

        //
        // Simple consecutive write...
        //
        fh.consume_input(&[1u8; (BLOCK_SIZE + 100) as usize]);
        fh.flush(&mut tx)?;

        let mut total_size = 0;
        let mut block_num = 0;

        queries::block::iter_blocks_from(&mut tx, attr.ino, 0, |block| {
            block_num += 1;
            total_size += block.data.len();
            Ok(true)
        })?;

        assert_eq!(total_size, (BLOCK_SIZE + 100) as usize);
        assert_eq!(block_num, 2);

        //
        // Seek and overwrite
        //
        fh.seek_to(BLOCK_SIZE / 2);
        fh.consume_input(&[2u8; (BLOCK_SIZE * 2) as usize]);
        fh.flush(&mut tx)?;

        let mut total_size = 0;
        let mut block_num = 0;

        queries::block::iter_blocks_from(&mut tx, attr.ino, 0, |block| {
            block_num += 1;
            total_size += block.data.len();
            Ok(true)
        })?;

        assert_eq!(total_size, (BLOCK_SIZE * 2 + (BLOCK_SIZE / 2)) as usize);
        assert_eq!(block_num, 3);

        Ok(())
    }
}
