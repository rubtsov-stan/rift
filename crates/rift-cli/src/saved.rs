//! Saved results: `--save`, `rift open` and `rift info`.

use std::fs::{File, Metadata, OpenOptions};
use std::io;
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::os::unix::fs::{FileExt, MetadataExt};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::Context;
use rift_core::saved::{Identity, SavedFile, Sink};
use rift_core::{ByteOffset, Bytes, Source};
use rift_diff::{InfoArgs, OpenArgs};

use crate::source::{FileSource, shown};

/// A result being written: `FILE.tmp` beside `FILE`, renamed over it once
/// complete and synced, and removed if dropped before that.
#[derive(Debug)]
pub struct Save {
    file: File,
    tmp: PathBuf,
    path: PathBuf,
    ids: [Identity; 2],
    done: bool,
}

impl Save {
    /// Creates `path.tmp` and takes the identity of both inputs.
    pub fn create(path: &Path, inputs: [(&Path, &FileSource); 2]) -> anyhow::Result<Self> {
        let mut tmp = path.as_os_str().to_owned();
        tmp.push(".tmp");
        let tmp = PathBuf::from(tmp);
        // A `.tmp` left by an interrupted save is replaced. O_EXCL after
        // the removal never follows a symbolic link planted in its place.
        let _ = std::fs::remove_file(&tmp);
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(&tmp)
            .with_context(|| format!("create {}", shown(&tmp)))?;
        let id = |(p, s): (&Path, &FileSource)| -> anyhow::Result<Identity> {
            let meta = s.metadata().with_context(|| format!("stat {}", shown(p)))?;
            Ok(identity(p, &meta))
        };
        let ids = [id(inputs[0])?, id(inputs[1])?];
        Ok(Self {
            file,
            tmp,
            path: path.to_owned(),
            ids,
            done: false,
        })
    }

    /// The identities taken at [`create`](Self::create).
    pub fn ids(&self) -> [Identity; 2] {
        self.ids.clone()
    }

    /// Checks that neither input changed while it was read, then syncs
    /// the result and renames it into place.
    pub fn commit(mut self, inputs: [(&Path, &FileSource); 2]) -> anyhow::Result<()> {
        for ((p, s), id) in inputs.into_iter().zip(&self.ids) {
            let now = identity(p, &s.metadata()?);
            anyhow::ensure!(
                (now.size, now.mtime_secs, now.mtime_nanos, now.dev, now.ino)
                    == (id.size, id.mtime_secs, id.mtime_nanos, id.dev, id.ino),
                "{} changed while Rift read it; the result was not saved",
                shown(p)
            );
        }
        self.file
            .sync_all()
            .with_context(|| format!("sync {}", shown(&self.tmp)))?;
        std::fs::rename(&self.tmp, &self.path)
            .with_context(|| format!("rename to {}", shown(&self.path)))?;
        self.done = true;
        Ok(())
    }
}

impl Drop for Save {
    fn drop(&mut self) {
        if !self.done {
            let _ = std::fs::remove_file(&self.tmp);
        }
    }
}

impl Source for Save {
    fn len(&self) -> Bytes {
        Bytes(self.file.metadata().map_or(0, |m| m.len()))
    }

    fn read_at(&self, buf: &mut [u8], offset: ByteOffset) -> io::Result<usize> {
        self.file.read_at(buf, offset.0)
    }
}

impl Sink for Save {
    fn write_at(&self, buf: &[u8], offset: ByteOffset) -> io::Result<()> {
        self.file.write_all_at(buf, offset.0)
    }
}

/// The identity of the file at `path`, whose metadata is `meta`.
fn identity(path: &Path, meta: &Metadata) -> Identity {
    Identity {
        path: path.as_os_str().as_bytes().to_vec(),
        canonical: std::fs::canonicalize(path)
            .map(|p| p.into_os_string().into_vec())
            .unwrap_or_default(),
        size: meta.len(),
        mtime_secs: meta.mtime(),
        mtime_nanos: u32::try_from(meta.mtime_nsec()).unwrap_or(0),
        dev: meta.dev(),
        ino: meta.ino(),
    }
}

/// How a file on disk compares with the one a result describes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    /// Size, modification time, device and inode match.
    Fresh,
    /// The size matches, something else does not: a copy, a touched
    /// file, another machine. Every read is checked against the chunks.
    Unverified,
    /// The size differs: the result no longer describes the file.
    Stale,
}

fn state(id: &Identity, meta: &Metadata) -> State {
    if meta.len() != id.size {
        State::Stale
    } else if (meta.mtime(), meta.mtime_nsec(), meta.dev(), meta.ino())
        == (id.mtime_secs, i64::from(id.mtime_nanos), id.dev, id.ino)
    {
        State::Fresh
    } else {
        State::Unverified
    }
}

impl State {
    fn word(self) -> &'static str {
        match self {
            Self::Fresh => "fresh",
            Self::Unverified => {
                "unverified: the file is not the one saved, or was touched; reads are checked against the saved chunks"
            }
            Self::Stale => "stale: its size changed",
        }
    }
}

/// Where the file a result names for one side is now: `given` when the
/// user passed one, else the canonical path, then the path as given
/// relative to the result's directory, then to the current directory.
/// A path from the result must name a regular file, not a symbolic link:
/// the result may come from anyone.
fn locate(
    result: &Path,
    id: &Identity,
    given: Option<&Path>,
    side: &str,
) -> anyhow::Result<PathBuf> {
    if let Some(p) = given {
        return Ok(p.to_owned());
    }
    let named = Path::new(std::ffi::OsStr::from_bytes(&id.path));
    let canonical = Path::new(std::ffi::OsStr::from_bytes(&id.canonical));
    let beside = result.parent().map(|d| d.join(named));
    let candidates = [Some(canonical.to_owned()), beside, Some(named.to_owned())];
    for p in candidates.into_iter().flatten() {
        if p.as_os_str().is_empty() {
            continue;
        }
        if std::fs::symlink_metadata(&p).is_ok_and(|m| m.file_type().is_file()) {
            return Ok(p);
        }
    }
    anyhow::bail!(
        "{side}: {} is not found as a regular file; give its path with --{side}",
        shown(named)
    )
}

/// `rift info`.
pub fn info(args: &InfoArgs) -> anyhow::Result<ExitCode> {
    let budget = crate::budget(Bytes::mib(16), false);
    let src =
        FileSource::open(&args.file).with_context(|| format!("open {}", shown(&args.file)))?;
    let file = SavedFile::open(src, &budget).with_context(|| shown(&args.file))?;
    let m = file.meta();
    let (major, minor) = file.version();
    println!(
        "format {major}.{minor}, written by rift {}, --mem {}, moves {}",
        m.version,
        Bytes(m.mem).to_mem_arg(),
        if m.moves { "searched" } else { "off" }
    );
    if m.version != env!("CARGO_PKG_VERSION") {
        println!(
            "  this is rift {}: a rerun may give a different script",
            env!("CARGO_PKG_VERSION")
        );
    }
    for (side, id) in ["a", "b"].into_iter().zip(&m.files) {
        let named = Path::new(std::ffi::OsStr::from_bytes(&id.path));
        let at = locate(&args.file, id, None, side).ok();
        let state = match at.as_deref().map(std::fs::metadata) {
            Some(Ok(meta)) => state(id, &meta).word(),
            _ => "missing",
        };
        println!(
            "{side}: {}, {} bytes, {} lines: {state}",
            shown(named),
            id.size,
            m.lines[usize::from(side == "b")]
        );
    }
    let [equal, differs, approx] = m.segments;
    println!("segments {equal} equal, {differs} differs, {approx} approx");
    let partial = if m.refined == m.differing() {
        "complete"
    } else {
        "partial: no scripts for the rest, saved with --summary"
    };
    println!(
        "scripts  {} of {} differing segments, {partial}",
        m.refined,
        m.differing()
    );
    for (reason, n) in rift_core::ApproxReason::ALL.iter().zip(m.approx) {
        if n > 0 {
            println!("  {n} approximate: {}", reason.describe());
        }
    }
    println!("chunk hashes are xxh3: they catch changed files, not files made to collide");
    Ok(ExitCode::SUCCESS)
}

/// Without the `tui` feature there is no terminal interface to open.
#[cfg(not(feature = "tui"))]
pub fn open(_: &OpenArgs) -> anyhow::Result<ExitCode> {
    anyhow::bail!("built without TUI support")
}

/// `rift open`: the terminal interface on a saved result. Exit code as
/// for the diff.
#[cfg(feature = "tui")]
pub fn open(args: &OpenArgs) -> anyhow::Result<ExitCode> {
    use rift_core::saved::SavedDiff;
    use rift_core::{DiffView, SegmentStatus};
    use std::io::IsTerminal;
    anyhow::ensure!(
        io::stdin().is_terminal() && io::stdout().is_terminal(),
        "rift open needs a terminal"
    );
    let budget = crate::budget(args.mem, false);
    let src =
        FileSource::open(&args.file).with_context(|| format!("open {}", shown(&args.file)))?;
    let file = SavedFile::open(src, &budget).with_context(|| shown(&args.file))?;
    let mut inputs = Vec::with_capacity(2);
    for (side, (id, given)) in ["a", "b"]
        .into_iter()
        .zip(file.meta().files.iter().zip([&args.a, &args.b]))
    {
        let path = locate(&args.file, id, given.as_deref(), side)?;
        let src = FileSource::open(&path).with_context(|| format!("open {}", shown(&path)))?;
        match state(id, &src.metadata()?) {
            State::Fresh => {}
            State::Unverified => eprintln!(
                "rift: {side}: {}: {}",
                shown(&path),
                State::Unverified.word()
            ),
            State::Stale => anyhow::bail!(
                "{side}: {} is {} bytes, the result describes {}: the file changed; recompute with rift A B --save {}",
                shown(&path),
                src.len(),
                id.size,
                shown(&args.file)
            ),
        }
        inputs.push(src);
    }
    let b = inputs.pop().expect("two inputs");
    let a = inputs.pop().expect("two inputs");
    let view = SavedDiff::open(file, a, b, &budget)?;
    let identical = view
        .segments()
        .iter()
        .all(|s| s.status == SegmentStatus::Equal);
    let mut app = rift_tui::App::new(&view, view.lines_a(), view.lines_b());
    let mut term = ratatui::init();
    let res = rift_tui::run(&mut term, &mut app, rift_tui::crossterm_events, None);
    ratatui::restore();
    res.context("terminal")?;
    if view.changed() {
        anyhow::bail!(
            "a file changed since the result was saved; nothing from it was shown after that"
        );
    }
    if let Some(e) = view.error() {
        anyhow::bail!("{e}");
    }
    Ok(if identical {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(1)
    })
}
