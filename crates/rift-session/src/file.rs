//! Opening the two inputs: regular files only, read-only, read by `pread`.

use std::fs::{self, File, Metadata};
use std::io;
use std::os::unix::fs::{FileExt, MetadataExt};
use std::path::Path;

use rift_core::{ByteOffset, Bytes, Source};

/// A file opened for positional reads. Its length is taken at open time.
#[derive(Debug)]
pub struct FileSource {
    file: File,
    len: u64,
}

/// What identifies a file's contents for the session cache: a changed file
/// gets a new id, so a stale skeleton is never reused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FileId {
    dev: u64,
    ino: u64,
    len: u64,
    mtime: i64,
    mtime_nsec: i64,
}

impl FileId {
    fn of(m: &Metadata) -> Self {
        Self {
            dev: m.dev(),
            ino: m.ino(),
            len: m.len(),
            mtime: m.mtime(),
            mtime_nsec: m.mtime_nsec(),
        }
    }
}

/// Why a path was refused.
#[derive(Debug)]
pub enum Reason {
    /// `lstat`, `open` or `fstat` failed.
    Io(io::Error),
    /// The last component is a symbolic link.
    Symlink,
    /// A directory, device, FIFO or socket.
    NotRegular,
    /// The path named another file after the open than before it.
    Changed,
}

impl Reason {
    /// The protocol's name for the reason: `not_found`,
    /// `permission_denied`, `symlink`, `not_regular`, `changed` or `io`.
    #[must_use]
    pub fn name(&self) -> &'static str {
        match self {
            Self::Io(e) if e.kind() == io::ErrorKind::NotFound => "not_found",
            Self::Io(e) if e.kind() == io::ErrorKind::PermissionDenied => "permission_denied",
            Self::Io(_) => "io",
            Self::Symlink => "symlink",
            Self::NotRegular => "not_regular",
            Self::Changed => "changed",
        }
    }
}

impl std::fmt::Display for Reason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(e) => e.fmt(f),
            Self::Symlink => f.write_str("is a symbolic link, which is not followed"),
            Self::NotRegular => f.write_str("is not a regular file"),
            Self::Changed => f.write_str("changed while it was being opened"),
        }
    }
}

/// A path that cannot be opened for a diff.
#[derive(Debug, thiserror::Error)]
#[error("{path}: {reason}")]
pub struct FileError {
    /// The path as given.
    pub path: String,
    /// Why.
    pub reason: Reason,
}

impl FileSource {
    /// Opens `path` after checking that it names a regular file.
    ///
    /// A symbolic link in the last component is refused, so the server never
    /// reads a file the caller did not name. Directories, devices, FIFOs and
    /// sockets are refused before the open (a FIFO would block it). After the
    /// open, the descriptor is compared with the earlier `lstat`, which closes
    /// the window in which the path could be swapped for something else.
    ///
    /// # Errors
    ///
    /// [`FileError`], which names the path and the reason.
    pub fn open(path: &str) -> Result<(Self, FileId), FileError> {
        let p = Path::new(path);
        let fail = |reason| FileError {
            path: path.to_owned(),
            reason,
        };
        let before = fs::symlink_metadata(p).map_err(|e| fail(Reason::Io(e)))?;
        if before.file_type().is_symlink() {
            return Err(fail(Reason::Symlink));
        }
        if !before.is_file() {
            return Err(fail(Reason::NotRegular));
        }
        // `File::open` is read-only.
        let file = File::open(p).map_err(|e| fail(Reason::Io(e)))?;
        let after = file.metadata().map_err(|e| fail(Reason::Io(e)))?;
        if !after.is_file() || (after.dev(), after.ino()) != (before.dev(), before.ino()) {
            return Err(fail(Reason::Changed));
        }
        let id = FileId::of(&after);
        Ok((
            Self {
                file,
                len: after.len(),
            },
            id,
        ))
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn err(path: &str) -> String {
        FileSource::open(path).unwrap_err().to_string()
    }

    fn reason(path: &str) -> &'static str {
        FileSource::open(path).unwrap_err().reason.name()
    }

    #[test]
    fn reads_a_regular_file_positionally() {
        let mut tmp = tempfile::NamedTempFile::new().unwrap();
        tmp.write_all(b"one\ntwo\n").unwrap();
        let (src, _) = FileSource::open(tmp.path().to_str().unwrap()).unwrap();
        assert_eq!(src.len(), Bytes(8));
        let mut buf = [0u8; 3];
        assert_eq!(src.read_at(&mut buf, ByteOffset(4)).unwrap(), 3);
        assert_eq!(&buf, b"two");
    }

    #[test]
    fn a_missing_file_is_refused() {
        assert!(err("/nonexistent/rift-mcp-test").contains("No such file"));
        assert_eq!(reason("/nonexistent/rift-mcp-test"), "not_found");
    }

    #[test]
    fn a_directory_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        assert!(err(dir.path().to_str().unwrap()).contains("not a regular file"));
    }

    #[test]
    fn a_device_is_refused() {
        assert!(err("/dev/null").contains("not a regular file"));
        assert_eq!(reason("/dev/null"), "not_regular");
    }

    #[test]
    fn a_fifo_is_refused_without_blocking() {
        let dir = tempfile::tempdir().unwrap();
        let fifo = dir.path().join("fifo");
        let status = std::process::Command::new("mkfifo")
            .arg(&fifo)
            .status()
            .unwrap();
        assert!(status.success());
        assert!(err(fifo.to_str().unwrap()).contains("not a regular file"));
    }

    #[test]
    fn a_symlink_is_refused_even_to_a_regular_file() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("target");
        fs::write(&target, b"x\n").unwrap();
        let link = dir.path().join("link");
        std::os::unix::fs::symlink(&target, &link).unwrap();
        assert!(err(link.to_str().unwrap()).contains("symbolic link"));
        assert_eq!(reason(link.to_str().unwrap()), "symlink");
    }

    #[test]
    fn the_id_changes_when_the_file_does() {
        let mut tmp = tempfile::NamedTempFile::new().unwrap();
        tmp.write_all(b"x\n").unwrap();
        let path = tmp.path().to_str().unwrap().to_owned();
        let (_, before) = FileSource::open(&path).unwrap();
        tmp.write_all(b"y\n").unwrap();
        let (_, after) = FileSource::open(&path).unwrap();
        assert_ne!(before, after);
    }
}
