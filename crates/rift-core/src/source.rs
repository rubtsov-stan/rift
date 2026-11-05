//! Random-access byte source abstraction.
//!
//! The core never touches the file system. Files, in-memory buffers and test
//! fixtures all arrive through [`Source`], which models `pread`: a positional
//! read into a caller-owned buffer, safe to call from several threads.

use std::io;
use std::sync::Arc;

use crate::types::{ByteOffset, Bytes};

/// A readable sequence of bytes with a known length.
///
/// Implementations must be safe to read from several threads at once, which
/// is why `read_at` takes `&self` and the trait requires `Sync`.
pub trait Source: Sync {
    /// Total length in bytes.
    fn len(&self) -> Bytes;

    /// Reads bytes starting at `offset` into `buf`, returning how many were
    /// read. Like `pread`, a short read is not an error; zero means end of
    /// source.
    ///
    /// # Errors
    ///
    /// Returns the underlying I/O error.
    fn read_at(&self, buf: &mut [u8], offset: ByteOffset) -> io::Result<usize>;

    /// Whether the source holds no bytes.
    fn is_empty(&self) -> bool {
        self.len() == Bytes(0)
    }

    /// Fills `buf` completely from `offset`, retrying short reads.
    ///
    /// # Errors
    ///
    /// Returns `UnexpectedEof` if the source ends before `buf` is full, or
    /// the underlying I/O error.
    fn read_exact_at(&self, mut buf: &mut [u8], mut offset: ByteOffset) -> io::Result<()> {
        while !buf.is_empty() {
            match self.read_at(buf, offset) {
                Ok(0) => {
                    return Err(io::Error::new(
                        io::ErrorKind::UnexpectedEof,
                        "source ended before buffer was filled",
                    ));
                }
                Ok(n) => {
                    buf = &mut buf[n..];
                    offset += n as u64;
                }
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(e) => return Err(e),
            }
        }
        Ok(())
    }
}

impl Source for [u8] {
    fn len(&self) -> Bytes {
        Bytes(<[u8]>::len(self) as u64)
    }

    fn read_at(&self, buf: &mut [u8], offset: ByteOffset) -> io::Result<usize> {
        let Ok(start) = usize::try_from(offset.0) else {
            return Ok(0);
        };
        let Some(rest) = self.get(start..) else {
            return Ok(0);
        };
        let n = rest.len().min(buf.len());
        buf[..n].copy_from_slice(&rest[..n]);
        Ok(n)
    }
}

impl<S: Source + ?Sized> Source for &S {
    fn len(&self) -> Bytes {
        (**self).len()
    }

    fn read_at(&self, buf: &mut [u8], offset: ByteOffset) -> io::Result<usize> {
        (**self).read_at(buf, offset)
    }
}

impl<S: Source + ?Sized + Send> Source for Arc<S> {
    fn len(&self) -> Bytes {
        (**self).len()
    }

    fn read_at(&self, buf: &mut [u8], offset: ByteOffset) -> io::Result<usize> {
        (**self).read_at(buf, offset)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slice_reads_are_positional_and_short_at_end() {
        let src: &[u8] = b"hello world";
        let mut buf = [0u8; 4];
        assert_eq!(src.read_at(&mut buf, ByteOffset(6)).unwrap(), 4);
        assert_eq!(&buf, b"worl");
        assert_eq!(src.read_at(&mut buf, ByteOffset(10)).unwrap(), 1);
        assert_eq!(buf[0], b'd');
        assert_eq!(src.read_at(&mut buf, ByteOffset(11)).unwrap(), 0);
        assert_eq!(src.read_at(&mut buf, ByteOffset(u64::MAX)).unwrap(), 0);
    }

    #[test]
    fn read_exact_at_fails_past_end() {
        let src: &[u8] = b"abc";
        let mut buf = [0u8; 2];
        src.read_exact_at(&mut buf, ByteOffset(1)).unwrap();
        assert_eq!(&buf, b"bc");
        let err = src.read_exact_at(&mut buf, ByteOffset(2)).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::UnexpectedEof);
    }

    #[test]
    fn arc_reads_like_what_it_holds() {
        let src: Arc<[u8]> = Arc::from(&b"shared"[..]);
        let mut buf = [0u8; 3];
        assert_eq!(src.read_at(&mut buf, ByteOffset(3)).unwrap(), 3);
        assert_eq!(&buf, b"red");
        assert_eq!(Source::len(&src), Bytes(6));
    }

    #[test]
    fn empty_slice_is_empty() {
        let src: &[u8] = b"";
        assert!(src.is_empty());
        assert_eq!(Source::len(&src), Bytes(0));
    }
}
