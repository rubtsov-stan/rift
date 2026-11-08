//! `rift git-diff`: git's external diff protocol.
//!
//! Git runs `diff.<driver>.command` (or `GIT_EXTERNAL_DIFF`) as
//! `CMD path old-file old-hex old-mode new-file new-hex new-mode`, plus
//! `new-path` and a rename header when it found a rename or copy. Any
//! nonzero exit makes `git diff` stop with status 128, so a diff with
//! changes exits 0.

use std::ffi::OsString;
use std::io::{self, Write};
use std::os::unix::ffi::OsStrExt;
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;

use anyhow::{Context, bail};
use rift_core::Error;
use rift_core::unified::{DEFAULT_CONTEXT, Label, Options};
use rift_diff::GitDiffArgs;

use crate::full::{self, Files};
use crate::lfs::Pointer;
use crate::source::FileSource;

/// Git writes this for the side of an added or deleted file.
const DEV_NULL: &str = "/dev/null";

/// One side of the comparison.
#[derive(Debug)]
struct Side {
    file: PathBuf,
    hex: String,
    /// `None` for `.`, which git gives for a side that does not exist.
    mode: Option<String>,
}

/// What git passed, parsed.
#[derive(Debug)]
struct Call {
    old_path: Vec<u8>,
    new_path: Vec<u8>,
    old: Side,
    new: Side,
    /// The similarity and rename lines, as git wrote them.
    rename: Option<Vec<u8>>,
}

impl Call {
    fn parse(args: &[OsString]) -> anyhow::Result<Self> {
        if args.len() != 7 && args.len() != 9 {
            bail!(
                "git-diff takes the 7 arguments git passes to an external diff, or 9 for a \
                 rename or copy; got {}",
                args.len()
            );
        }
        let text = |i: usize| args[i].to_string_lossy().into_owned();
        let side = |at: usize| Side {
            file: PathBuf::from(&args[at]),
            hex: text(at + 1),
            mode: Some(text(at + 2)).filter(|m| m != "."),
        };
        let path = args[0].as_bytes().to_vec();
        Ok(Self {
            new_path: args
                .get(7)
                .map_or_else(|| path.clone(), |p| p.as_bytes().to_vec()),
            old_path: path,
            old: side(1),
            new: side(4),
            rename: args.get(8).map(|h| h.as_bytes().to_vec()),
        })
    }

    fn added(&self) -> bool {
        self.old.file.as_os_str() == DEV_NULL
    }

    fn deleted(&self) -> bool {
        self.new.file.as_os_str() == DEV_NULL
    }

    /// The lines `git diff` prints before `---`, from `diff --git` to
    /// `index`.
    fn header(&self) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(b"diff --git a/");
        out.extend_from_slice(&self.old_path);
        out.extend_from_slice(b" b/");
        out.extend_from_slice(&self.new_path);
        out.push(b'\n');
        let (old_mode, new_mode) = (self.old.mode.as_deref(), self.new.mode.as_deref());
        let mut line = |s: String| {
            out.extend_from_slice(s.as_bytes());
            out.push(b'\n');
        };
        match (old_mode, new_mode) {
            (None, Some(m)) => line(format!("new file mode {m}")),
            (Some(m), None) => line(format!("deleted file mode {m}")),
            (Some(o), Some(n)) if o != n => {
                line(format!("old mode {o}"));
                line(format!("new mode {n}"));
            }
            _ => {}
        }
        if let Some(rename) = &self.rename {
            // Git's text for a rename already ends with the index line.
            out.extend_from_slice(rename);
        } else if self.old.hex != self.new.hex {
            let abbrev = |hex: &str| {
                if hex.bytes().all(|b| b == b'0' || b == b'.') {
                    "0".repeat(7)
                } else {
                    hex.chars().take(7).collect()
                }
            };
            let mut index = format!("index {}..{}", abbrev(&self.old.hex), abbrev(&self.new.hex));
            if let (Some(o), Some(n)) = (old_mode, new_mode)
                && o == n
            {
                index.push(' ');
                index.push_str(o);
            }
            out.extend_from_slice(index.as_bytes());
            out.push(b'\n');
        }
        out
    }

    fn labels(&self) -> (Vec<u8>, Vec<u8>) {
        let name = |prefix: &[u8], path: &[u8]| [prefix, path].concat();
        (
            if self.added() {
                DEV_NULL.into()
            } else {
                name(b"a/", &self.old_path)
            },
            if self.deleted() {
                DEV_NULL.into()
            } else {
                name(b"b/", &self.new_path)
            },
        )
    }
}

/// Where the content of `side` is: the file git gave, or the LFS object
/// when that file is a pointer.
fn content(side: &Side, name: &[u8]) -> anyhow::Result<PathBuf> {
    if side.file.as_os_str() != DEV_NULL
        && let Some(pointer) = Pointer::read(&side.file)?
    {
        return pointer.resolve(&String::from_utf8_lossy(name));
    }
    Ok(side.file.clone())
}

fn run(cli: &GitDiffArgs) -> anyhow::Result<()> {
    let call = Call::parse(&cli.external)?;
    let old = content(&call.old, &call.old_path)?;
    let new = content(&call.new, &call.new_path)?;
    let open = |p: &PathBuf| {
        FileSource::open(p)
            .map(Arc::new)
            .with_context(|| format!("open {}", p.display()))
    };
    let files = Files {
        a: open(&old)?,
        b: open(&new)?,
        path_a: &old,
        path_b: &new,
        moves: true,
    };
    // Built before anything is printed, so a refusal leaves no half header.
    let diff = full::build(&files, cli.mem)?;
    let (old_label, new_label) = call.labels();
    let opts = Options {
        context: DEFAULT_CONTEXT,
        old: Label {
            name: &old_label,
            mtime: None,
        },
        new: Label {
            name: &new_label,
            mtime: None,
        },
    };
    let mut out = io::stdout().lock();
    out.write_all(&call.header()).map_err(Error::Write)?;
    diff.write_unified(&opts, &mut out)?;
    out.flush().map_err(Error::Write)?;
    Ok(())
}

/// Runs `rift git-diff` with `cli`.
///
/// A reader that quit early (git's pager) is not a failure: any nonzero
/// exit makes git report a dead external diff, so it exits 0 silently.
pub fn main(cli: &GitDiffArgs) -> ExitCode {
    let code = match run(cli) {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) if crate::broken_pipe(&err) => return ExitCode::SUCCESS,
        Err(err) => {
            match crate::mem_advice(&err, cli.mem) {
                Some(advice) => eprintln!("rift: {err:#}; {advice}"),
                None => eprintln!("rift: {err:#}"),
            }
            ExitCode::from(2)
        }
    };
    if cli.stats {
        eprintln!("{}", crate::summary::peak_heap(cli.mem));
    }
    code
}

#[cfg(test)]
mod tests {
    use super::*;

    fn call(args: &[&str]) -> Call {
        let args: Vec<OsString> = args.iter().map(Into::into).collect();
        Call::parse(&args).unwrap()
    }

    fn header(args: &[&str]) -> String {
        String::from_utf8(call(args).header()).unwrap()
    }

    #[test]
    fn a_changed_file_has_one_index_line_with_the_mode() {
        let h = header(&[
            "f",
            "/t/f",
            &"a".repeat(40),
            "100644",
            "f",
            &"b".repeat(40),
            "100644",
        ]);
        assert_eq!(h, "diff --git a/f b/f\nindex aaaaaaa..bbbbbbb 100644\n");
    }

    #[test]
    fn added_and_deleted_files_have_no_mode_on_the_index_line() {
        let added = call(&["f", "/dev/null", ".", ".", "f", &"b".repeat(40), "100644"]);
        assert_eq!(
            String::from_utf8(added.header()).unwrap(),
            "diff --git a/f b/f\nnew file mode 100644\nindex 0000000..bbbbbbb\n"
        );
        assert_eq!(added.labels(), (b"/dev/null".to_vec(), b"b/f".to_vec()));
        let gone = call(&[
            "f",
            "/t/f",
            &"a".repeat(40),
            "100755",
            "/dev/null",
            ".",
            ".",
        ]);
        assert_eq!(
            String::from_utf8(gone.header()).unwrap(),
            "diff --git a/f b/f\ndeleted file mode 100755\nindex aaaaaaa..0000000\n"
        );
        assert_eq!(gone.labels(), (b"a/f".to_vec(), b"/dev/null".to_vec()));
    }

    #[test]
    fn a_mode_change_lists_both_modes_and_leaves_the_mode_off_the_index() {
        let h = header(&[
            "f",
            "/t/f",
            &"a".repeat(40),
            "100644",
            "f",
            &"0".repeat(40),
            "100755",
        ]);
        assert_eq!(
            h,
            "diff --git a/f b/f\nold mode 100644\nnew mode 100755\nindex aaaaaaa..0000000\n"
        );
        let same = header(&[
            "f",
            "/t/f",
            &"a".repeat(40),
            "100644",
            "f",
            &"a".repeat(40),
            "100755",
        ]);
        assert!(!same.contains("index"));
    }

    #[test]
    fn a_rename_uses_both_paths_and_git_s_own_lines() {
        let rename = "similarity index 90%\nrename from g\nrename to h\nindex aaaaaaa..bbbbbbb\n";
        let c = call(&[
            "g",
            "/t/g",
            &"a".repeat(40),
            "100644",
            "h",
            &"b".repeat(40),
            "100644",
            "h",
            rename,
        ]);
        assert_eq!(
            String::from_utf8(c.header()).unwrap(),
            format!("diff --git a/g b/h\n{rename}")
        );
        assert_eq!(c.labels(), (b"a/g".to_vec(), b"b/h".to_vec()));
    }

    #[test]
    fn eight_arguments_are_refused() {
        let args: Vec<OsString> = (0..8).map(|i| i.to_string().into()).collect();
        let err = Call::parse(&args).unwrap_err().to_string();
        assert!(err.contains("got 8"), "{err}");
    }
}
