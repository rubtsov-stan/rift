//! [`Source`] backed by a file, read through `pread`.

use std::fs::{File, OpenOptions};
use std::io;
use std::os::unix::fs::{FileExt, OpenOptionsExt};
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
    /// Symbolic links are followed. Anything but a regular file is refused,
    /// except `/dev/null`, which is an empty file: a pipe or FIFO has no
    /// length and cannot be read at an offset.
    ///
    /// # Errors
    ///
    /// Returns the error from `open` or `metadata`, or `InvalidInput` for a
    /// file that is not regular. A failed read-ahead hint is ignored: it only
    /// affects speed.
    pub fn open(path: impl AsRef<Path>) -> io::Result<Self> {
        let path = path.as_ref();
        // O_NONBLOCK keeps `open` from waiting for a writer on a FIFO. It
        // changes nothing for a regular file, the only kind kept.
        let file = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NONBLOCK)
            .open(path)?;
        let meta = file.metadata()?;
        if !meta.is_file() && path != Path::new("/dev/null") {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "not a regular file; Rift needs random access, so write the input to a file first",
            ));
        }
        let len = meta.len();
        // Linux /proc files are regular with a length of 0 yet hold bytes;
        // read as empty, they would make a wrong diff.
        if len == 0 && file.read_at(&mut [0u8; 1], 0)? > 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "reports no length but has content; Rift needs random access, so write the input to a file first",
            ));
        }
        hints::sequential(&file);
        Ok(Self { file, len })
    }
}

impl Source for FileSource {
    fn len(&self) -> Bytes {
        Bytes(self.len)
    }

    fn read_at(&self, buf: &mut [u8], offset: ByteOffset) -> io::Result<usize> {
        let left = self.len.saturating_sub(offset.0);
        let n = buf.len().min(usize::try_from(left).unwrap_or(usize::MAX));
        let got = self.file.read_at(&mut buf[..n], offset.0)?;
        if got == 0 && n > 0 {
            return Err(shrank());
        }
        Ok(got)
    }
}

/// Nothing came back before the length taken at open: the file was cut
/// while Rift read it, and taking that as the end would make a wrong diff.
fn shrank() -> io::Error {
    io::Error::new(
        io::ErrorKind::UnexpectedEof,
        "the file shrank while Rift was reading it",
    )
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

    #[test]
    fn reads_stop_at_the_length_taken_at_open() {
        let mut tmp = tempfile::NamedTempFile::new().unwrap();
        tmp.write_all(b"x\ny\n").unwrap();
        let src = FileSource::open(tmp.path()).unwrap();
        tmp.write_all(b"ZZ\n").unwrap();
        let mut buf = [0u8; 16];
        assert_eq!(src.read_at(&mut buf, ByteOffset(0)).unwrap(), 4);
        assert_eq!(&buf[..4], b"x\ny\n");
        assert_eq!(src.read_at(&mut buf, ByteOffset(4)).unwrap(), 0);
    }

    #[test]
    fn a_file_that_shrinks_after_open_is_an_error_not_an_end() {
        let mut tmp = tempfile::NamedTempFile::new().unwrap();
        tmp.write_all(b"one\ntwo\nthree\n").unwrap();
        let src = FileSource::open(tmp.path()).unwrap();
        tmp.as_file().set_len(4).unwrap();
        let mut buf = [0u8; 16];
        let err = src.read_at(&mut buf, ByteOffset(4)).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::UnexpectedEof);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn a_proc_file_that_reports_no_length_is_refused() {
        let err = FileSource::open("/proc/self/status").unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
    }

    #[test]
    fn an_empty_regular_file_is_still_a_file() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let src = FileSource::open(tmp.path()).unwrap();
        assert_eq!(src.len(), Bytes(0));
    }

    #[test]
    fn a_directory_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let err = FileSource::open(dir.path()).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
    }

    #[test]
    fn scans_a_file_like_memory() {
        use rift_core::scan::{ScanConfig, scan};
        use rift_core::{Budget, Half, LineAccess, LineBuf, MemLines};

        let data: Vec<u8> = (0..5000)
            .flat_map(|i| format!("row {i} {}\n", i % 97).into_bytes())
            .collect();
        let mut tmp = tempfile::NamedTempFile::new().unwrap();
        tmp.write_all(&data).unwrap();
        let src = FileSource::open(tmp.path()).unwrap();
        let budget = Budget::new(Bytes::mib(64));
        let cfg = ScanConfig {
            level: 2,
            sample_cap: 1 << 12,
            checkpoint_interval: 64,
            checkpoint_cap: 1 << 10,
            chunk: Bytes(4096),
            threads: 3,
        };
        let from_file = scan(&src, &budget, &cfg).unwrap();
        let from_mem = scan(&data[..], &budget, &cfg).unwrap();
        assert_eq!(from_file.index().line_count(), 5000);
        assert_eq!(from_file.samples(), from_mem.samples());
        assert_eq!(
            from_file.index().checkpoints(),
            from_mem.index().checkpoints()
        );

        let access = rift_core::index::IndexedLines::new(
            &src,
            from_file.index(),
            &budget,
            Half::Refine,
            Bytes(4096),
        )
        .unwrap();
        let mem = MemLines::new(&data);
        let mut buf = LineBuf::new();
        access
            .read_lines(rift_core::LineNo(4321)..rift_core::LineNo(4325), &mut buf)
            .unwrap();
        for (k, line) in buf.iter().enumerate() {
            assert_eq!(line, mem.line(4321 + k as u64).unwrap());
        }
    }
}
