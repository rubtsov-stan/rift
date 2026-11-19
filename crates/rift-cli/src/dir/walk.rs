//! The walk: both trees in step, depth first, names in byte order inside
//! each directory, so the order depends on the trees alone, not on the
//! order the file system lists them in.
//!
//! Memory: a directory is listed twice, once to count its entries and the
//! bytes of their names and once to fill two vectors of exactly that size,
//! so a listing never reallocates. The listings of both trees at one level
//! are reserved together, so a refusal asks for all that the level needs.
//! A level's listings live while the walk is below it: the walk holds the
//! listings on the path from the roots, 8 bytes per entry plus its name.

use std::cmp::Ordering;
use std::ffi::OsStr;
use std::fs::{self, FileType};
use std::io;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::FileTypeExt;

use rift_core::{Budget, BudgetExceeded, Half, Reservation};

use super::take;

/// The longest relative path the walk builds. Longer paths fail in the
/// system calls anyway (`PATH_MAX` is 4096 on Linux, 1024 on macOS); the
/// limit keeps the two path buffers at a fixed size.
pub const PATH_LIMIT: usize = 4096;

/// What an entry is, from the directory listing: links are not followed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    File,
    Dir,
    Link,
    Fifo,
    Socket,
    Block,
    Char,
}

impl Kind {
    fn of(t: FileType) -> Self {
        if t.is_symlink() {
            Self::Link
        } else if t.is_dir() {
            Self::Dir
        } else if t.is_file() {
            Self::File
        } else if t.is_fifo() {
            Self::Fifo
        } else if t.is_socket() {
            Self::Socket
        } else if t.is_block_device() {
            Self::Block
        } else {
            Self::Char
        }
    }

    /// The kind as GNU diff names it in `File X is a ... while file Y is a ...`.
    pub fn name(self) -> &'static str {
        match self {
            Self::File => "regular file",
            Self::Dir => "directory",
            Self::Link => "symbolic link",
            Self::Fifo => "fifo",
            Self::Socket => "socket",
            Self::Block => "block special file",
            Self::Char => "character special file",
        }
    }
}

/// Which tree.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Side {
    A,
    B,
}

/// Where the walk is: the full path on each side and the relative path.
pub struct At<'w> {
    pub a: &'w [u8],
    pub b: &'w [u8],
    pub rel: &'w [u8],
    /// Bytes of the last component.
    pub name_len: usize,
}

impl At<'_> {
    /// The full path on `side`.
    pub fn on(&self, side: Side) -> &[u8] {
        match side {
            Side::A => self.a,
            Side::B => self.b,
        }
    }
}

/// What the walk calls for each entry, in walk order.
pub trait Visit {
    /// An entry with its kind on each side; `under` names the side of a
    /// directory above it that the other tree does not have, so the entry
    /// exists on that side alone. The walk then descends into every
    /// directory, on one side or both.
    fn entry(
        &mut self,
        at: &At<'_>,
        a: Option<Kind>,
        b: Option<Kind>,
        under: Option<Side>,
    ) -> anyhow::Result<()>;

    /// A directory that could not be listed, or a path too long; the walk
    /// does not descend there on either side.
    fn trouble(&mut self, path: &[u8], err: &io::Error);
}

#[derive(Clone, Copy)]
struct Entry {
    off: u32,
    len: u16,
    kind: Kind,
}

const _: () = assert!(size_of::<Entry>() == 8);

/// One directory's entries, sorted by name.
#[derive(Default)]
struct Listing {
    entries: Vec<Entry>,
    names: Vec<u8>,
}

impl Listing {
    fn name(&self, e: Entry) -> &[u8] {
        &self.names[e.off as usize..e.off as usize + e.len as usize]
    }
}

/// Entries and name bytes in a directory, by a first pass over it.
fn count(path: &[u8]) -> io::Result<(usize, usize)> {
    let mut n = 0;
    let mut bytes = 0;
    for e in fs::read_dir(OsStr::from_bytes(path))? {
        n += 1;
        bytes += e?.file_name().len();
    }
    Ok((n, bytes))
}

/// The second pass: fills a listing of at most `n` entries and `bytes`
/// bytes of names, allocated here at that size.
fn fill(path: &[u8], (n, bytes): (usize, usize)) -> io::Result<Listing> {
    let mut l = Listing {
        entries: Vec::with_capacity(n),
        names: Vec::with_capacity(bytes),
    };
    for e in fs::read_dir(OsStr::from_bytes(path))? {
        let e = e?;
        let name = e.file_name();
        let name = name.as_bytes();
        if l.entries.len() == n || l.names.len() + name.len() > bytes {
            return Err(io::Error::other("changed while Rift listed it"));
        }
        l.entries.push(Entry {
            off: u32::try_from(l.names.len()).map_err(io::Error::other)?,
            len: u16::try_from(name.len()).map_err(io::Error::other)?,
            kind: Kind::of(e.file_type()?),
        });
        l.names.extend_from_slice(name);
    }
    let names = &l.names;
    let key = |e: &Entry| &names[e.off as usize..e.off as usize + e.len as usize];
    // Names are unique in a directory, so the order is total.
    l.entries.sort_unstable_by(|x, y| key(x).cmp(key(y)));
    Ok(l)
}

pub struct Walk<'b> {
    budget: &'b Budget,
    a: Vec<u8>,
    b: Vec<u8>,
    /// Where the relative path starts in `a`.
    rel_a: usize,
    /// The most the Skeleton half held during a walk.
    pub peak: u64,
    _paths: Reservation,
}

/// `root` with a separator after it unless it ends in one.
fn start(root: &[u8], cap: usize) -> Vec<u8> {
    let mut v = Vec::with_capacity(cap);
    v.extend_from_slice(root);
    if !root.ends_with(b"/") {
        v.push(b'/');
    }
    v
}

impl<'b> Walk<'b> {
    /// A walk of `root_a` and `root_b`; the two path buffers come out of
    /// the walk's share.
    pub fn new(budget: &'b Budget, root_a: &[u8], root_b: &[u8]) -> Result<Self, BudgetExceeded> {
        let (cap_a, cap_b) = (root_a.len() + 1 + PATH_LIMIT, root_b.len() + 1 + PATH_LIMIT);
        let paths = take(budget, (cap_a + cap_b) as u64)?;
        let (a, b) = (start(root_a, cap_a), start(root_b, cap_b));
        Ok(Self {
            budget,
            rel_a: a.len(),
            a,
            b,
            peak: budget.used(Half::Skeleton).0,
            _paths: paths,
        })
    }

    /// Walks both trees from the roots, calling `v` for every entry.
    ///
    /// # Errors
    ///
    /// A listing the walk's share cannot hold, or an error from `v`.
    pub fn run(&mut self, v: &mut impl Visit) -> anyhow::Result<()> {
        self.level(v, true, true, None)
    }

    /// The directory at the current path: on side A when `on_a`, on side
    /// B when `on_b`.
    fn level(
        &mut self,
        v: &mut impl Visit,
        on_a: bool,
        on_b: bool,
        under: Option<Side>,
    ) -> anyhow::Result<()> {
        let mut sizes = [(0, 0); 2];
        for (side, on, path) in [(0, on_a, &self.a), (1, on_b, &self.b)] {
            if on {
                match count(path) {
                    Ok(s) => sizes[side] = s,
                    Err(e) => {
                        v.trouble(path, &e);
                        return Ok(());
                    }
                }
            }
        }
        let bytes = sizes
            .iter()
            .map(|&(n, b)| n * size_of::<Entry>() + b)
            .sum::<usize>();
        let _held = take(self.budget, bytes as u64).map_err(|e| {
            let path = if on_a { &self.a } else { &self.b };
            let path = path.strip_suffix(b"/").filter(|p| !p.is_empty()).unwrap_or(path);
            // The refusal's figures are doubled (see `take`); the context
            // gives the real size.
            anyhow::Error::from(rift_core::Error::Budget(e)).context(format!(
                "list {}: {} entries in the two trees take {bytes} bytes, more than is left of the walk's quarter of the budget",
                crate::source::shown(std::path::Path::new(OsStr::from_bytes(path))),
                sizes[0].0 + sizes[1].0
            ))
        })?;
        self.peak = self.peak.max(self.budget.used(Half::Skeleton).0);
        let mut lists = [Listing::default(), Listing::default()];
        for (side, on, path) in [(0, on_a, &self.a), (1, on_b, &self.b)] {
            if on {
                match fill(path, sizes[side]) {
                    Ok(l) => lists[side] = l,
                    Err(e) => {
                        v.trouble(path, &e);
                        return Ok(());
                    }
                }
            }
        }
        let [la, lb] = &lists;
        let (mut ia, mut ib) = (0, 0);
        while ia < la.entries.len() || ib < lb.entries.len() {
            let (ea, eb) = (la.entries.get(ia).copied(), lb.entries.get(ib).copied());
            let order = match (ea, eb) {
                (Some(x), Some(y)) => la.name(x).cmp(lb.name(y)),
                (Some(_), None) => Ordering::Less,
                _ => Ordering::Greater,
            };
            let (ka, kb, name) = match order {
                Ordering::Less => (ea.map(|e| e.kind), None, la.name(ea.unwrap())),
                Ordering::Greater => (None, eb.map(|e| e.kind), lb.name(eb.unwrap())),
                Ordering::Equal => (ea.map(|e| e.kind), eb.map(|e| e.kind), la.name(ea.unwrap())),
            };
            ia += usize::from(order != Ordering::Greater);
            ib += usize::from(order != Ordering::Less);
            self.visit(v, name, ka, kb, under)?;
        }
        Ok(())
    }

    /// Visits `name` under the current path and descends into it if it is
    /// a directory on either side.
    fn visit(
        &mut self,
        v: &mut impl Visit,
        name: &[u8],
        ka: Option<Kind>,
        kb: Option<Kind>,
        under: Option<Side>,
    ) -> anyhow::Result<()> {
        let (len_a, len_b) = (self.a.len(), self.b.len());
        let deeper = len_a > self.rel_a;
        if len_a - self.rel_a + 1 + name.len() > PATH_LIMIT {
            let mut long = self.a.clone();
            long.extend_from_slice(b"/...");
            v.trouble(&long, &io::Error::other("path too long"));
            return Ok(());
        }
        for (p, len) in [(&mut self.a, len_a), (&mut self.b, len_b)] {
            debug_assert_eq!(p.len(), len);
            if deeper {
                p.push(b'/');
            }
            p.extend_from_slice(name);
        }
        let at = At {
            a: &self.a,
            b: &self.b,
            rel: &self.a[self.rel_a..],
            name_len: name.len(),
        };
        v.entry(&at, ka, kb, under)?;
        // An error ends the whole run, so the paths need no repair then.
        match (ka == Some(Kind::Dir), kb == Some(Kind::Dir)) {
            (true, true) => self.level(v, true, true, under)?,
            (true, false) => self.level(v, true, false, under.or(Some(Side::A)))?,
            (false, true) => self.level(v, false, true, under.or(Some(Side::B)))?,
            (false, false) => {}
        }
        self.a.truncate(len_a);
        self.b.truncate(len_b);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A directory that grows between the count and the fill, by an entry
    /// or by a longer name, is an error for that directory: the listing
    /// never grows past what was reserved. One that shrinks lists what is
    /// there.
    #[test]
    fn a_directory_that_changes_between_the_two_passes_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        for name in ["a", "bb", "ccc"] {
            fs::write(dir.path().join(name), "").unwrap();
        }
        let path = dir.path().as_os_str().as_bytes();
        let (n, bytes) = count(path).unwrap();
        assert_eq!((n, bytes), (3, 6));
        for (counted, grown) in [((2, 6), "an entry"), ((3, 5), "a name")] {
            let err = fill(path, counted)
                .err()
                .unwrap_or_else(|| panic!("{grown}"));
            assert_eq!(err.to_string(), "changed while Rift listed it", "{grown}");
        }
        let l = fill(path, (4, 8)).unwrap();
        assert_eq!((l.entries.len(), l.names.len()), (3, 6));
        assert_eq!((l.entries.capacity(), l.names.capacity()), (4, 8));
    }
}
