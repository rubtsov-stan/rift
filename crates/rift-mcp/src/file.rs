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
    /// A message for the client that names the path and the reason.
    pub fn open(path: &str) -> Result<(Self, FileId), String> {
        let p = Path::new(path);
        let fail = |why: &dyn std::fmt::Display| format!("{path}: {why}");
        let before = fs::symlink_metadata(p).map_err(|e| fail(&e))?;
        if before.file_type().is_symlink() {
            return Err(fail(&"is a symbolic link, which is not followed"));
        }
        if !before.is_file() {
            return Err(fail(&"is not a regular file"));
        }
        // `File::open` is read-only.
        let file = File::open(p).map_err(|e| fail(&e))?;
        let after = file.metadata().map_err(|e| fail(&e))?;
        if !after.is_file() || (after.dev(), after.ino()) != (before.dev(), before.ino()) {
            return Err(fail(&"changed while it was being opened"));
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
        FileSource::open(path).unwrap_err()
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
    }

    #[test]
    fn a_directory_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        assert!(err(dir.path().to_str().unwrap()).contains("not a regular file"));
    }

    #[test]
    fn a_device_is_refused() {
        assert!(err("/dev/null").contains("not a regular file"));
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
