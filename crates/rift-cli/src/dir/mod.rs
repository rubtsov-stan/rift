//! `rift A B` on two directories: lines in the form of `diff -r`, a unified
//! diff per changed file, renames by content, or with `--summary` one
//! status per path and the counts.
//!
//! Three walks of both trees: the first counts one-sided files by size,
//! the second collects the rename candidates (skipped when no size can
//! match), the third prints. Each changed pair is diffed in turn with the
//! same budget, its `Diff` dropped before the walk goes on.
//!
//! Memory: the walk's listings, path buffers, output and compare buffers,
//! the size counters and the rename table share half of the Skeleton half
//! (see [`take`]). A file diff runs under them with the other half of the
//! Skeleton half at least, and the whole Refine half.

mod renames;
mod walk;

use std::ffi::OsStr;
use std::fs;
use std::io::{self, BufWriter, StdoutLock, Write};
use std::os::unix::ffi::OsStrExt;
use std::path::Path;
use std::process::ExitCode;
use std::sync::Arc;
use std::time::Instant;

use rift_core::unified::{DEFAULT_CONTEXT, Label, Options, quote_diff_name};
use rift_core::{Budget, BudgetExceeded, ByteOffset, Bytes, Half, Reservation, Source};
use xxhash_rust::xxh3::Xxh3;

use crate::full;
use crate::source::{FileSource, shown};
use crate::summary::peak_heap;
use renames::{Cand, Counts, Table};
use walk::{At, Kind, Side, Visit, Walk};

/// Standard output's buffer.
const OUT_BUF: usize = 32 << 10;
/// Each of the two buffers that compare and hash files.
const CMP_BUF: usize = 32 << 10;
/// A file is binary when the first this many bytes of it hold a NUL, as
/// GNU diff and git decide.
const BINARY_PROBE: usize = 8 << 10;

/// Reserves `n` bytes for the walk or the rename table from the Skeleton
/// half. Together they may hold half of it, B/4, so a file diff run below
/// them keeps the other half for its skeleton. A refusal reports its
/// figures doubled, as if the share were the whole half, so the `--mem` it
/// advises gives the share what it asked for.
fn take(budget: &Budget, n: u64) -> Result<Reservation, BudgetExceeded> {
    let cap = budget.capacity(Half::Skeleton).0;
    let used = budget.used(Half::Skeleton).0;
    if used.saturating_add(n) > cap / 2 {
        return Err(BudgetExceeded {
            half: Half::Skeleton,
            requested: Bytes(n.saturating_mul(2)),
            available: Bytes(cap.saturating_sub(used.saturating_mul(2))),
        });
    }
    budget.try_reserve(Half::Skeleton, Bytes(n))
}

/// What the command line asked for.
pub struct Opts {
    pub summary: bool,
    pub moves: bool,
    pub stats: bool,
}

/// `path` without trailing slashes, `/` for the root.
fn root(path: &Path) -> Vec<u8> {
    let b = path.as_os_str().as_bytes();
    let end = b.iter().rposition(|&c| c != b'/').map_or(0, |i| i + 1);
    if end == 0 && !b.is_empty() {
        return b"/".to_vec();
    }
    b[..end].to_vec()
}

fn join(root: &[u8], rel: &[u8]) -> Vec<u8> {
    let mut p = root.to_vec();
    if !p.ends_with(b"/") {
        p.push(b'/');
    }
    p.extend_from_slice(rel);
    p
}

fn os(path: &[u8]) -> &Path {
    Path::new(OsStr::from_bytes(path))
}

/// Fills `buf` from `src` at `at`.
fn read_full(src: &FileSource, buf: &mut [u8], at: u64) -> io::Result<()> {
    let mut done = 0;
    while done < buf.len() {
        done += src.read_at(&mut buf[done..], ByteOffset(at + done as u64))?;
    }
    Ok(())
}

/// A read that failed, and the side whose file it was.
type Failed = (Side, io::Error);

/// Whether the files hold the same bytes, read through both buffers.
fn same_bytes(a: &FileSource, b: &FileSource, bufs: &mut [Vec<u8>; 2]) -> Result<bool, Failed> {
    if a.len() != b.len() {
        return Ok(false);
    }
    let len = a.len().0;
    let [ba, bb] = bufs;
    let mut at = 0;
    while at < len {
        let n = usize::try_from(len - at).unwrap_or(usize::MAX).min(CMP_BUF);
        read_full(a, &mut ba[..n], at).map_err(|e| (Side::A, e))?;
        read_full(b, &mut bb[..n], at).map_err(|e| (Side::B, e))?;
        if ba[..n] != bb[..n] {
            return Ok(false);
        }
        at += n as u64;
    }
    Ok(true)
}

fn same_files(a: &[u8], b: &[u8], bufs: &mut [Vec<u8>; 2]) -> io::Result<bool> {
    same_bytes(&FileSource::open(os(a))?, &FileSource::open(os(b))?, bufs).map_err(|(_, e)| e)
}

fn hash_file(path: &[u8], buf: &mut [u8]) -> io::Result<u64> {
    let src = FileSource::open(os(path))?;
    let mut h = Xxh3::new();
    let len = src.len().0;
    let mut at = 0;
    while at < len {
        let n = usize::try_from(len - at)
            .unwrap_or(usize::MAX)
            .min(buf.len());
        read_full(&src, &mut buf[..n], at)?;
        h.update(&buf[..n]);
        at += n as u64;
    }
    Ok(h.digest())
}

/// A regular file on `this` side that the other side has no regular file
/// for: a candidate to be renamed.
fn lone(this: Option<Kind>, other: Option<Kind>) -> bool {
    this == Some(Kind::File) && other != Some(Kind::File)
}

/// The size of a regular file, 0 when it cannot be read.
fn size(path: &[u8]) -> u64 {
    fs::symlink_metadata(os(path)).map_or(0, |m| m.len())
}

/// First walk: the size counters.
struct Census<'c>(&'c mut Counts);

impl Visit for Census<'_> {
    fn entry(
        &mut self,
        at: &At<'_>,
        a: Option<Kind>,
        b: Option<Kind>,
        _: Option<Side>,
    ) -> anyhow::Result<()> {
        for (side, path, this, other) in [(Side::A, at.a, a, b), (Side::B, at.b, b, a)] {
            if lone(this, other) {
                let n = size(path);
                if n > 0 {
                    self.0.add(side, n);
                }
            }
        }
        Ok(())
    }

    fn trouble(&mut self, _: &[u8], _: &io::Error) {}
}

/// Second walk: the candidates, hashed unless their bucket holds one file
/// per tree.
struct Collect<'c, 'b> {
    counts: &'c Counts,
    tables: &'c mut [Table<'b>; 2],
    buf: &'c mut [u8],
}

impl Visit for Collect<'_, '_> {
    fn entry(
        &mut self,
        at: &At<'_>,
        a: Option<Kind>,
        b: Option<Kind>,
        _: Option<Side>,
    ) -> anyhow::Result<()> {
        for (side, path, this, other) in [(Side::A, at.a, a, b), (Side::B, at.b, b, a)] {
            let table = &mut self.tables[side as usize];
            if !lone(this, other) || table.full {
                continue;
            }
            let n = size(path);
            let (ca, cb) = self.counts.get(n);
            if n == 0 || ca == 0 || cb == 0 {
                continue;
            }
            let hash = if ca == 1 && cb == 1 {
                0
            } else {
                // An unreadable file is no candidate; the third walk
                // lists it as only in its tree.
                let Ok(h) = hash_file(path, self.buf) else {
                    continue;
                };
                h
            };
            table.push(Cand::new(n, hash), at.rel);
        }
        Ok(())
    }

    fn trouble(&mut self, _: &[u8], _: &io::Error) {}
}

/// Counts for `--summary`, of entries that are not directories.
#[derive(Default)]
struct Tally {
    added: u64,
    removed: u64,
    renamed: u64,
    changed: u64,
    type_changed: u64,
    same: u64,
}

/// Third walk: the output.
struct Print<'p, 'b> {
    out: &'p mut BufWriter<StdoutLock<'static>>,
    opts: &'p Opts,
    mem: Bytes,
    budget: &'b Budget,
    tables: &'p [Table<'b>; 2],
    root_a: &'p [u8],
    bufs: &'p mut [Vec<u8>; 2],
    tally: Tally,
    differ: bool,
    trouble: bool,
    approx: (u64, u64),
}

fn q(name: &[u8]) -> std::borrow::Cow<'_, [u8]> {
    quote_diff_name(name)
}

impl Print<'_, '_> {
    /// Writes `parts` and a newline: a difference.
    fn line(&mut self, parts: &[&[u8]]) -> anyhow::Result<()> {
        self.differ = true;
        let mut write = || -> io::Result<()> {
            for p in parts {
                self.out.write_all(p)?;
            }
            self.out.write_all(b"\n")
        };
        write().map_err(rift_core::Error::Write)?;
        Ok(())
    }

    fn complain(&mut self, path: &[u8], msg: &dyn std::fmt::Display) {
        self.trouble = true;
        // Keep the message after the lines printed before it; a failed
        // flush shows again at the next write.
        let _ = self.out.flush();
        eprintln!("rift: {}: {msg}", shown(os(path)));
    }

    fn renamed(&mut self, at: &At<'_>, from: &[u8]) -> anyhow::Result<()> {
        self.tally.renamed += 1;
        if self.opts.summary {
            self.line(&[b"R  ", &q(from), b" -> ", &q(at.rel)])
        } else {
            let from = join(self.root_a, from);
            self.line(&[b"Renamed: ", &q(&from), b" -> ", &q(at.b)])
        }
    }

    fn one_sided(
        &mut self,
        at: &At<'_>,
        side: Side,
        kind: Kind,
        under: Option<Side>,
    ) -> anyhow::Result<()> {
        if kind == Kind::File {
            let [a, b] = self.tables;
            let found = match side {
                Side::A => a.peer(at.rel, b),
                Side::B => b.peer(at.rel, a),
            };
            if let Some(from) = found {
                // A renamed file shows once, at its new path.
                return if side == Side::B {
                    self.renamed(at, from)
                } else {
                    Ok(())
                };
            }
        }
        if kind != Kind::Dir {
            match side {
                Side::A => self.tally.removed += 1,
                Side::B => self.tally.added += 1,
            }
        }
        if under.is_some() {
            return Ok(());
        }
        if self.opts.summary {
            let mut rel = at.rel.to_vec();
            if kind == Kind::Dir {
                rel.push(b'/');
            }
            let status: &[u8] = if side == Side::A { b"D  " } else { b"A  " };
            return self.line(&[status, &q(&rel)]);
        }
        let path = if side == Side::A { at.a } else { at.b };
        let cut = path.len() - at.name_len;
        let parent = if cut > 1 {
            &path[..cut - 1]
        } else {
            &path[..cut]
        };
        self.line(&[b"Only in ", &q(parent), b": ", &q(&path[cut..])])
    }

    fn links(&mut self, at: &At<'_>) -> anyhow::Result<()> {
        let targets = fs::read_link(os(at.a)).and_then(|a| Ok((a, fs::read_link(os(at.b))?)));
        match targets {
            Err(e) => self.complain(at.a, &e),
            Ok((x, y)) if x == y => self.tally.same += 1,
            Ok(_) => {
                self.tally.changed += 1;
                return if self.opts.summary {
                    self.line(&[b"M  ", &q(at.rel)])
                } else {
                    self.line(&[b"Symbolic links ", &q(at.a), b" and ", &q(at.b), b" differ"])
                };
            }
        }
        Ok(())
    }

    fn files(&mut self, at: &At<'_>) -> anyhow::Result<()> {
        let open = |side| FileSource::open(os(at.on(side))).map_err(|e| (side, e));
        let (fa, fb) = match open(Side::A).and_then(|a| Ok((a, open(Side::B)?))) {
            Ok(f) => f,
            Err((side, e)) => {
                self.complain(at.on(side), &e);
                return Ok(());
            }
        };
        match same_bytes(&fa, &fb, self.bufs) {
            Ok(true) => {
                self.tally.same += 1;
                return Ok(());
            }
            Ok(false) => {}
            Err((side, e)) => {
                self.complain(at.on(side), &e);
                return Ok(());
            }
        }
        self.tally.changed += 1;
        if self.opts.summary {
            return self.line(&[b"M  ", &q(at.rel)]);
        }
        match self.binary(&fa, &fb) {
            Ok(true) => self.line(&[b"Binary files ", &q(at.a), b" and ", &q(at.b), b" differ"]),
            Ok(false) => self.diff(at, fa, fb),
            Err((side, e)) => {
                self.complain(at.on(side), &e);
                Ok(())
            }
        }
    }

    fn binary(&mut self, a: &FileSource, b: &FileSource) -> Result<bool, Failed> {
        let [ba, bb] = &mut *self.bufs;
        for (side, src, buf) in [(Side::A, a, ba), (Side::B, b, bb)] {
            let n = usize::try_from(src.len().0)
                .unwrap_or(usize::MAX)
                .min(BINARY_PROBE);
            read_full(src, &mut buf[..n], 0).map_err(|e| (side, e))?;
            if memchr::memchr(0, &buf[..n]).is_some() {
                return Ok(true);
            }
        }
        Ok(false)
    }

    fn diff(&mut self, at: &At<'_>, fa: FileSource, fb: FileSource) -> anyhow::Result<()> {
        let files = full::Files {
            a: Arc::new(fa),
            b: Arc::new(fb),
            path_a: os(at.a),
            path_b: os(at.b),
            moves: self.opts.moves,
        };
        let diff = full::build(&files, self.budget, self.mem)?;
        let (old_name, old_time) = full::label(files.path_a)?;
        let (new_name, new_time) = full::label(files.path_b)?;
        self.line(&[b"diff -r ", &q(at.a), b" ", &q(at.b)])?;
        let opts = Options {
            context: DEFAULT_CONTEXT,
            old: Label {
                name: &old_name,
                mtime: Some(old_time),
            },
            new: Label {
                name: &new_name,
                mtime: Some(new_time),
            },
        };
        let written = diff.write_unified(&opts, &mut *self.out)?;
        self.approx.0 += written.approx_segments;
        self.approx.1 += written.approx_lines;
        Ok(())
    }
}

impl Visit for Print<'_, '_> {
    fn entry(
        &mut self,
        at: &At<'_>,
        a: Option<Kind>,
        b: Option<Kind>,
        under: Option<Side>,
    ) -> anyhow::Result<()> {
        match (a, b) {
            (Some(x), Some(y)) if x == y => match x {
                Kind::Dir => Ok(()),
                Kind::File => self.files(at),
                Kind::Link => self.links(at),
                _ => {
                    self.complain(at.a, &format_args!("not compared: {}", x.name()));
                    Ok(())
                }
            },
            (Some(x), Some(y)) => {
                self.tally.type_changed += 1;
                if self.opts.summary {
                    self.line(&[b"T  ", &q(at.rel)])?;
                } else {
                    self.line(&[
                        b"File ",
                        &q(at.a),
                        b" is a ",
                        x.name().as_bytes(),
                        b" while file ",
                        &q(at.b),
                        b" is a ",
                        y.name().as_bytes(),
                    ])?;
                }
                match self.tables[1].peer(at.rel, &self.tables[0]) {
                    Some(from) if y == Kind::File => self.renamed(at, from),
                    _ => Ok(()),
                }
            }
            (Some(x), None) => self.one_sided(at, Side::A, x, under),
            (None, Some(y)) => self.one_sided(at, Side::B, y, under),
            (None, None) => Ok(()),
        }
    }

    fn trouble(&mut self, path: &[u8], err: &io::Error) {
        self.complain(path, err);
    }
}

/// Compares the trees at `a` and `b` under `mem`. Exit code 0 when they
/// are the same, 1 when they differ, 2 when something could not be
/// compared; the rest of the output is complete then.
pub fn run(a: &Path, b: &Path, mem: Bytes, opts: &Opts) -> anyhow::Result<ExitCode> {
    let start = Instant::now();
    let budget = crate::budget(mem, true);
    let (root_a, root_b) = (root(a), root(b));
    let _out_held = take(&budget, OUT_BUF as u64)?;
    let _bufs_held = take(&budget, 2 * CMP_BUF as u64)?;
    let mut bufs = [vec![0; CMP_BUF], vec![0; CMP_BUF]];
    let mut walk = Walk::new(&budget, &root_a, &root_b)?;
    let mut counts = Counts::new(&budget)?;
    walk.run(&mut Census(&mut counts))?;
    // The later walks hold what the first one did; the table gets the rest.
    let room = (budget.capacity(Half::Skeleton).0 / 2).saturating_sub(walk.peak) / 2;
    let mut tables = [Table::new(&budget, room), Table::new(&budget, room)];
    if counts.shared() {
        walk.run(&mut Collect {
            counts: &counts,
            tables: &mut tables,
            buf: &mut bufs[0],
        })?;
    }
    drop(counts);
    let [ta, tb] = &mut tables;
    let cut = renames::pair(ta, tb, |x, y| {
        same_files(&join(&root_a, x), &join(&root_b, y), &mut bufs).unwrap_or(false)
    });
    let full = tables.iter().any(|t| t.full) || cut;
    let mut out = BufWriter::with_capacity(OUT_BUF, io::stdout().lock());
    if full {
        let twice = Bytes(mem.0.saturating_mul(2));
        writeln!(
            out,
            "Renames approximate: the rename table is full at --mem {}; renamed files past it are listed as only in one tree; try --mem {}",
            mem.to_mem_arg(),
            twice.to_mem_arg()
        )
        .map_err(rift_core::Error::Write)?;
    }
    let mut print = Print {
        out: &mut out,
        opts,
        mem,
        budget: &budget,
        tables: &tables,
        root_a: &root_a,
        bufs: &mut bufs,
        tally: Tally::default(),
        differ: full,
        trouble: false,
        approx: (0, 0),
    };
    walk.run(&mut print)?;
    let (tally, differ, trouble, approx) = (print.tally, print.differ, print.trouble, print.approx);
    let elapsed = start.elapsed();
    let mut tail = || -> io::Result<()> {
        if opts.summary {
            let t = &tally;
            writeln!(
                out,
                "added {}, removed {}, renamed {}, changed {}, type changed {}, same {}",
                t.added, t.removed, t.renamed, t.changed, t.type_changed, t.same
            )?;
            writeln!(out, "{}", peak_heap(mem))?;
            writeln!(out, "time {:.2} s", elapsed.as_secs_f64())?;
        }
        out.flush()
    };
    tail().map_err(rift_core::Error::Write)?;
    if opts.stats && !opts.summary {
        eprintln!("{}", peak_heap(mem));
        eprintln!("time {:.2} s", elapsed.as_secs_f64());
        eprintln!(
            "approx {} segments, {} lines in a and b",
            approx.0, approx.1
        );
    }
    Ok(match (trouble, differ) {
        (true, _) => ExitCode::from(2),
        (false, true) => ExitCode::from(1),
        (false, false) => ExitCode::SUCCESS,
    })
}
