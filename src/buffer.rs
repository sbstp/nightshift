use std::{
    io::Write,
    ops::{Deref, DerefMut},
};

use bytes::{BufMut, Bytes, BytesMut};

#[derive(Debug)]
pub struct FixedBuffer {
    inner: BytesMut,
}

impl FixedBuffer {
    pub fn with_capacity(capacity: usize) -> FixedBuffer {
        Self {
            inner: BytesMut::with_capacity(capacity),
        }
    }

    pub fn zeroed(len: usize) -> FixedBuffer {
        Self {
            inner: BytesMut::zeroed(len),
        }
    }

    pub fn len(&self) -> usize {
        self.inner.len()
    }

    pub fn capacity(&self) -> usize {
        self.inner.capacity()
    }

    pub fn extend_from_slice(&mut self, extend: &[u8]) -> usize {
        let cnt = std::cmp::min(self.remaining_mut(), extend.len());
        self.put_slice(&extend[..cnt]);
        cnt
    }

    pub fn resize(&mut self, new_len: usize) {
        assert!(new_len <= self.inner.capacity());
        self.inner.resize(new_len, 0);
    }

    pub fn truncate(&mut self, len: usize) {
        self.inner.truncate(len);
    }

    pub fn clear(&mut self) {
        self.inner.clear();
    }

    pub fn freeze(self) -> Bytes {
        self.inner.freeze()
    }
}

unsafe impl BufMut for FixedBuffer {
    fn remaining_mut(&self) -> usize {
        self.inner.capacity() - self.inner.len()
    }

    unsafe fn advance_mut(&mut self, cnt: usize) {
        self.inner.advance_mut(cnt);
    }

    fn chunk_mut(&mut self) -> &mut bytes::buf::UninitSlice {
        self.inner.spare_capacity_mut().into()
    }
}

impl Write for FixedBuffer {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let cnt = self.extend_from_slice(buf);
        Ok(cnt)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl Deref for FixedBuffer {
    type Target = [u8];

    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}

impl DerefMut for FixedBuffer {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.inner
    }
}

impl AsRef<[u8]> for FixedBuffer {
    fn as_ref(&self) -> &[u8] {
        self.inner.as_ref()
    }
}

impl AsMut<[u8]> for FixedBuffer {
    fn as_mut(&mut self) -> &mut [u8] {
        self.inner.as_mut()
    }
}

impl From<&[u8]> for FixedBuffer {
    fn from(value: &[u8]) -> Self {
        Self {
            inner: BytesMut::from(value),
        }
    }
}

impl PartialEq for FixedBuffer {
    fn eq(&self, other: &Self) -> bool {
        self.as_ref() == other.as_ref()
    }
}

impl Extend<u8> for FixedBuffer {
    fn extend<T: IntoIterator<Item = u8>>(&mut self, iter: T) {
        for v in iter {
            self.put_u8(v);
        }
    }
}
