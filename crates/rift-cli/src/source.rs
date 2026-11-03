//! [`Source`] backed by a file, read through `pread`.

use std::fs::File;
use std::io;
use std::os::unix::fs::FileExt;
use std::path::Path;

use rift_core::{ByteOffset, Bytes, Source};

/// A file opened for positional reads.
///
/// The length is taken once at open time; the file is expected not to change
/// while a diff runs.
#[derive(Debug)]
pub struct FileSource {
    file: File,
    len: u64,
}

impl FileSource {
    /// Opens `path` read-only and asks the kernel for sequential read-ahead.
    ///
    /// # Errors
    ///
    /// Returns the error from `open` or `metadata`. A failed read-ahead hint
    /// is ignored: it only affects speed.
    pub fn open(path: impl AsRef<Path>) -> io::Result<Self> {
        let file = File::open(path)?;
        let len = file.metadata()?.len();
        hints::sequential(&file);
        Ok(Self { file, len })
    }
}

impl Source for FileSource {
    fn len(&self) -> Bytes {
        Bytes(self.len)
    }

    fn read_at(&self, buf: &mut [u8], offset: ByteOffset) -> io::Result<usize> {
        self.file.read_at(buf, offset.0)
    }
}

/// Kernel read-ahead hints. The only FFI in the binary.
#[allow(unsafe_code, reason = "fadvise and fcntl have no safe std wrapper")]
mod hints {
    use std::fs::File;
    use std::os::fd::AsRawFd;

    #[cfg(target_os = "linux")]
    pub(super) fn sequential(file: &File) {
        // SAFETY: the descriptor is open and owned by `file` for the duration
        // of the call; fadvise reads no memory and only updates kernel state.
        unsafe {
            libc::posix_fadvise(file.as_raw_fd(), 0, 0, libc::POSIX_FADV_SEQUENTIAL);
        }
    }

    #[cfg(target_os = "macos")]
    pub(super) fn sequential(file: &File) {
        // SAFETY: the descriptor is open and owned by `file` for the duration
        // of the call; F_RDAHEAD takes an integer flag and touches no memory.
        unsafe {
            libc::fcntl(file.as_raw_fd(), libc::F_RDAHEAD, 1);
        }
    }

    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    pub(super) fn sequential(_file: &File) {}
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn reads_file_positionally() {
        let mut tmp = tempfile::NamedTempFile::new().unwrap();
        tmp.write_all(b"line one\nline two\n").unwrap();
        let src = FileSource::open(tmp.path()).unwrap();
        assert_eq!(src.len(), Bytes(18));
        let mut buf = [0u8; 8];
        assert_eq!(src.read_at(&mut buf, ByteOffset(9)).unwrap(), 8);
        assert_eq!(&buf, b"line two");
        assert_eq!(src.read_at(&mut buf, ByteOffset(18)).unwrap(), 0);
    }
}
