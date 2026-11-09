//! Git LFS pointers: what git stores for a large file, and where the real
//! content sits in the local store.

use std::fs::OpenOptions;
use std::io::Read;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, bail};

const MAGIC: &[u8] = b"version https://git-lfs.github.com/spec/v1\n";

/// The spec caps a pointer at 1024 bytes.
const MAX_POINTER: u64 = 1024;

/// The oid and size a pointer file names.
#[derive(Debug, PartialEq, Eq)]
pub struct Pointer {
    pub oid: String,
    pub size: u64,
}

impl Pointer {
    /// Parses pointer text; `None` when it is anything else.
    fn parse(text: &[u8]) -> Option<Self> {
        let rest = text.strip_prefix(MAGIC)?;
        let (mut oid, mut size) = (None, None);
        for line in std::str::from_utf8(rest).ok()?.lines() {
            if let Some(hex) = line.strip_prefix("oid sha256:") {
                let ok = hex.len() == 64 && hex.bytes().all(|b| b.is_ascii_hexdigit());
                oid = ok.then(|| hex.to_owned());
            } else if let Some(n) = line.strip_prefix("size ") {
                size = n.parse().ok();
            }
        }
        Some(Self {
            oid: oid?,
            size: size?,
        })
    }

    /// Reads `path` and parses it if it is small enough to be a pointer.
    /// A file over a KiB is never read.
    pub fn read(path: &Path) -> anyhow::Result<Option<Self>> {
        // O_NONBLOCK keeps a FIFO from blocking the open; anything but a
        // regular file is no pointer, and the diff itself refuses it.
        let file = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NONBLOCK)
            .open(path)
            .with_context(|| format!("open {}", path.display()))?;
        let meta = file.metadata()?;
        let len = meta.len();
        if !meta.is_file() || len > MAX_POINTER || len < MAGIC.len() as u64 {
            return Ok(None);
        }
        let mut text = Vec::new();
        file.take(MAX_POINTER).read_to_end(&mut text)?;
        Ok(Self::parse(&text))
    }

    /// Where the object is kept below the git directory.
    fn object_in(&self, git_common_dir: &Path) -> PathBuf {
        git_common_dir
            .join("lfs/objects")
            .join(&self.oid[0..2])
            .join(&self.oid[2..4])
            .join(&self.oid)
    }

    /// The local object for this pointer, checked against the pointer's
    /// size. `name` is the path in the repository, for the messages.
    pub fn resolve(&self, name: &str) -> anyhow::Result<PathBuf> {
        let object = self.object_in(&git_common_dir()?);
        let len = match std::fs::metadata(&object) {
            Ok(m) => m.len(),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => bail!(
                "{name} is a Git LFS pointer and object {} is not in {}; \
                 run `git lfs fetch`",
                self.oid,
                object.display()
            ),
            Err(e) => return Err(e).with_context(|| format!("stat {}", object.display())),
        };
        if len != self.size {
            bail!(
                "LFS object {} for {name} is {len} bytes, the pointer says {}; \
                 run `git lfs fetch` after removing {}",
                self.oid,
                self.size,
                object.display()
            );
        }
        Ok(object)
    }
}

/// The directory that holds `lfs/objects`: the shared git directory, which
/// is not `.git` inside a linked work tree.
fn git_common_dir() -> anyhow::Result<PathBuf> {
    let out = Command::new("git")
        .args(["rev-parse", "--git-common-dir"])
        .output()
        .context("run git to find the LFS store")?;
    if !out.status.success() {
        bail!(
            "git rev-parse --git-common-dir failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    let dir = String::from_utf8_lossy(&out.stdout);
    Ok(PathBuf::from(dir.trim_end_matches('\n')))
}

#[cfg(test)]
mod tests {
    use super::*;

    const OID: &str = "4d7a214614ab2935c943f9e0ff69d22eadbb8f32b1258daaa5e2ca24d17e2393";

    #[test]
    fn a_fifo_is_not_read_as_a_pointer_and_does_not_block() {
        let dir = tempfile::tempdir().unwrap();
        let fifo = dir.path().join("fifo");
        assert!(
            Command::new("mkfifo")
                .arg(&fifo)
                .status()
                .unwrap()
                .success()
        );
        let (tx, rx) = std::sync::mpsc::channel();
        let path = fifo.clone();
        std::thread::spawn(move || {
            let _ = tx.send(Pointer::read(&path).map_err(|e| e.to_string()));
        });
        let got = rx
            .recv_timeout(std::time::Duration::from_secs(10))
            .expect("reading a FIFO as a pointer blocked");
        assert_eq!(got.unwrap(), None);
        // Unblock the reader thread if it is still waiting in open.
        drop(
            OpenOptions::new()
                .write(true)
                .custom_flags(libc::O_NONBLOCK)
                .open(&fifo),
        );
    }

    #[test]
    fn parses_a_pointer_and_nothing_else() {
        let text = format!(
            "{}oid sha256:{OID}\nsize 12345\n",
            "version https://git-lfs.github.com/spec/v1\n"
        );
        assert_eq!(
            Pointer::parse(text.as_bytes()),
            Some(Pointer {
                oid: OID.into(),
                size: 12345
            })
        );
        assert_eq!(Pointer::parse(b"just some text\nsize 3\n"), None);
        let short = "version https://git-lfs.github.com/spec/v1\noid sha256:abc\nsize 3\n";
        assert_eq!(Pointer::parse(short.as_bytes()), None);
        let no_size = format!("version https://git-lfs.github.com/spec/v1\noid sha256:{OID}\n");
        assert_eq!(Pointer::parse(no_size.as_bytes()), None);
    }

    #[test]
    fn the_object_path_fans_out_by_oid() {
        let p = Pointer {
            oid: OID.into(),
            size: 1,
        };
        assert_eq!(
            p.object_in(Path::new(".git")),
            Path::new(".git/lfs/objects/4d/7a").join(OID)
        );
    }
}
