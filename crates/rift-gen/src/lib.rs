//! Deterministic generator of file pairs with a known mutation manifest.
//!
//! Both files stream through the writers in one pass; memory stays constant
//! regardless of the target size. The manifest records every mutation so a
//! diff can be checked against the truth.

#![forbid(unsafe_code)]

mod profile;
mod rng;
pub mod synthetic;

use std::io::{self, Write};

pub use profile::{NOKEY_POOL, Profile, RUN_LINES, Rows};
pub use rng::Rng;

/// Kind of mutation applied to a region. Line numbers are 0-based.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, clap::ValueEnum)]
pub enum Kind {
    /// `count` lines present only in B.
    Insert,
    /// `count` lines present only in A.
    Delete,
    /// `count` lines that differ between A and B.
    Modify,
    /// A block of `count` lines at `line_a` in A appears later in B at `line_b`.
    Move,
    /// `count` copies of one line, identical in both files.
    Dup,
    /// `count` lines from a tiny vocabulary, identical in both files.
    LowUniq,
    /// One line longer than 1 MiB, identical in both files.
    Long,
    /// `count` lines terminated by CRLF, identical in both files.
    Crlf,
    /// The final line has no trailing newline, in both files.
    NoEol,
}

impl Kind {
    const ALL: [Kind; 9] = [
        Kind::Insert,
        Kind::Delete,
        Kind::Modify,
        Kind::Move,
        Kind::Dup,
        Kind::LowUniq,
        Kind::Long,
        Kind::Crlf,
        Kind::NoEol,
    ];

    /// Whether this kind makes A and B differ.
    #[must_use]
    pub fn differs(self) -> bool {
        matches!(
            self,
            Kind::Insert | Kind::Delete | Kind::Modify | Kind::Move
        )
    }

    fn name(self) -> &'static str {
        match self {
            Kind::Insert => "insert",
            Kind::Delete => "delete",
            Kind::Modify => "modify",
            Kind::Move => "move",
            Kind::Dup => "dup",
            Kind::LowUniq => "lowuniq",
            Kind::Long => "long",
            Kind::Crlf => "crlf",
            Kind::NoEol => "noeol",
        }
    }

    fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|k| k.name() == s)
    }
}

/// One row of the manifest.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Mutation {
    /// What happened.
    pub kind: Kind,
    /// First affected line in A, 0-based. For `Insert` the line in A after
    /// which B's lines appear.
    pub line_a: u64,
    /// First affected line in B, 0-based. For `Delete` the line in B after
    /// which A's lines appear.
    pub line_b: u64,
    /// Number of lines involved.
    pub count: u64,
}

/// Generator settings.
#[derive(Debug, Clone)]
pub struct Config {
    /// Seed for every random decision.
    pub seed: u64,
    /// Approximate size of A in bytes; generation stops once reached.
    pub size: u64,
    /// Line content.
    pub profile: Profile,
    /// Row format of the `nokey` and `shuffled` profiles.
    pub rows: Rows,
    /// Different lines of a `distinct` file.
    pub distinct: u64,
    /// Lines in one period of a `periodic` file.
    pub period: u64,
    /// Probability that a mutation starts at a given line.
    pub change_ratio: f64,
    /// Kinds eligible for random selection.
    pub kinds: Vec<Kind>,
}

impl Config {
    /// Settings with every kind enabled, except `Long` below 64 MiB where a
    /// 1 MiB line would dominate the file.
    ///
    /// `nokey` and the low-entropy profiles leave out the kinds that would
    /// add a line found once: `LowUniq`, `Long`, `Crlf` and `NoEol`. `shuffled` keeps the changes
    /// a key can name, `Insert`, `Delete` and `Modify`, and moves blocks on
    /// its own schedule (see [`Profile::Shuffled`]).
    #[must_use]
    pub fn new(seed: u64, size: u64, profile: Profile) -> Self {
        let kinds = Kind::ALL
            .into_iter()
            .filter(|k| *k != Kind::Long || size >= 64 << 20)
            .filter(|k| match profile {
                p if p == Profile::Nokey || p.is_low_entropy() => matches!(
                    k,
                    Kind::Insert | Kind::Delete | Kind::Modify | Kind::Move | Kind::Dup
                ),
                Profile::Shuffled => matches!(k, Kind::Insert | Kind::Delete | Kind::Modify),
                _ => true,
            })
            .collect();
        Self {
            seed,
            size,
            profile,
            rows: Rows::default(),
            distinct: 100,
            period: 1000,
            change_ratio: 0.001,
            kinds,
        }
    }
}

/// Writes A, B and the manifest for `config`.
///
/// # Errors
///
/// Returns the first write error.
pub fn generate(
    config: &Config,
    a: impl Write,
    b: impl Write,
    manifest: impl Write,
) -> io::Result<()> {
    Generator::new(config, a, b, manifest).run()
}

/// Parses a manifest written by [`generate`].
///
/// # Errors
///
/// Returns `InvalidData` on a malformed row.
pub fn parse_manifest(text: &[u8]) -> io::Result<Vec<Mutation>> {
    let bad = |line: &[u8]| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("bad manifest row: {}", String::from_utf8_lossy(line)),
        )
    };
    let mut out = Vec::new();
    for line in text.split(|&c| c == b'\n').skip(1) {
        if line.is_empty() {
            continue;
        }
        let text = str::from_utf8(line).map_err(|_| bad(line))?;
        let mut f = text.split('\t');
        let (Some(kind), Some(la), Some(lb), Some(n), None) =
            (f.next(), f.next(), f.next(), f.next(), f.next())
        else {
            return Err(bad(line));
        };
        let kind = Kind::parse(kind).ok_or_else(|| bad(line))?;
        let num = |s: &str| s.parse::<u64>().map_err(|_| bad(line));
        out.push(Mutation {
            kind,
            line_a: num(la)?,
            line_b: num(lb)?,
            count: num(n)?,
        });
    }
    Ok(out)
}

/// Probability that a `shuffled` block move starts at a line, when none is
/// in flight.
const SHUFFLE_MOVE_RATE: f64 = 0.01;

struct Generator<'c, A, B, M> {
    config: &'c Config,
    rng: Rng,
    lines: profile::Lines,
    a: A,
    b: B,
    manifest: M,
    line_a: u64,
    line_b: u64,
    bytes_a: u64,
    buf: Vec<u8>,
    /// The A side of the line being modified.
    old: Vec<u8>,
    /// A block withheld from B, with the number of B lines still to pass
    /// before it is emitted.
    pending_move: Option<(Mutation, Vec<u8>, u64)>,
}

impl<A: Write, B: Write, M: Write> Generator<'_, A, B, M> {
    fn new(config: &Config, a: A, b: B, manifest: M) -> Generator<'_, A, B, M> {
        let mut rng = Rng::new(config.seed);
        let lines = profile::Lines::new(config, &mut rng);
        Generator {
            config,
            rng,
            lines,
            a,
            b,
            manifest,
            line_a: 0,
            line_b: 0,
            bytes_a: 0,
            buf: Vec::with_capacity(4096),
            old: Vec::new(),
            pending_move: None,
        }
    }

    fn run(mut self) -> io::Result<()> {
        self.manifest.write_all(b"kind\tline_a\tline_b\tcount\n")?;
        let no_eol = self.config.kinds.contains(&Kind::NoEol) && self.rng.chance(0.5);
        if let Some(header) = self.lines.header() {
            self.fixed_line(header)?;
        }
        while self.bytes_a < self.config.size {
            self.flush_move_if_due()?;
            if self.config.profile == Profile::Shuffled
                && self.pending_move.is_none()
                && self.rng.chance(SHUFFLE_MOVE_RATE)
            {
                // At least four times the block's lines pass, in both
                // files, before it lands, so the block, not what it jumps
                // over, is the part out of order.
                let count = self.rng.range(1, 50);
                let (m, block) = self.withhold(count)?;
                let distance = self.rng.range(4 * count, 4 * count + 500);
                self.pending_move = Some((m, block, distance));
                continue;
            }
            let kind = self.pick_kind();
            match kind {
                None => {
                    self.next_line();
                    self.both(b"\n")?;
                }
                Some(kind) => self.mutate(kind)?,
            }
        }
        if let Some((m, block, _)) = self.pending_move.take() {
            self.emit_move(m, &block)?;
        }
        if let Some(footer) = self.lines.footer() {
            self.fixed_line(footer)?;
        }
        if no_eol {
            self.next_line();
            self.record(Kind::NoEol, 1)?;
            self.both(b"")?;
        }
        self.a.flush()?;
        self.b.flush()?;
        self.manifest.flush()
    }

    fn pick_kind(&mut self) -> Option<Kind> {
        if self.config.kinds.is_empty() || !self.rng.chance(self.config.change_ratio) {
            return None;
        }
        let kind = self.config.kinds[self.rng.index(self.config.kinds.len())];
        // A long line is about a thousand ordinary ones; at equal weight they
        // would make up most of the file.
        if kind == Kind::Long && !self.rng.chance(0.01) {
            return None;
        }
        (kind != Kind::NoEol).then_some(kind)
    }

    /// Mostly short runs, occasionally a long one.
    fn pick_count(&mut self) -> u64 {
        if self.rng.chance(0.1) {
            self.rng.range(10, 500)
        } else {
            self.rng.range(1, 10)
        }
    }

    fn mutate(&mut self, kind: Kind) -> io::Result<()> {
        let count = self.pick_count();
        match kind {
            Kind::Insert => {
                self.record(kind, count)?;
                for _ in 0..count {
                    self.next_line();
                    self.only_b(b"\n")?;
                }
            }
            Kind::Delete => {
                self.record(kind, count)?;
                for _ in 0..count {
                    self.next_line();
                    self.only_a(b"\n")?;
                }
            }
            Kind::Modify => {
                self.record(kind, count)?;
                for _ in 0..count {
                    self.next_line();
                    self.only_a(b"\n")?;
                    self.changed_line();
                    self.only_b(b"\n")?;
                }
            }
            Kind::Move => {
                if self.pending_move.is_some() {
                    return Ok(());
                }
                let (m, block) = self.withhold(count)?;
                let distance = self.rng.range(count + 1, 2000);
                self.pending_move = Some((m, block, distance));
            }
            Kind::Dup => {
                self.record(kind, count)?;
                self.next_line();
                for _ in 0..count {
                    self.both(b"\n")?;
                }
            }
            Kind::LowUniq => {
                self.record(kind, count)?;
                for _ in 0..count {
                    self.buf.clear();
                    self.config
                        .profile
                        .repetitive_line(&mut self.rng, &mut self.buf);
                    self.both(b"\n")?;
                }
            }
            Kind::Long => {
                self.record(kind, 1)?;
                self.buf.clear();
                let target = (1 << 20) + self.rng.index(1 << 20);
                while self.buf.len() < target {
                    self.buf.push(b'a' + self.rng.byte(26));
                }
                self.both(b"\n")?;
            }
            Kind::Crlf => {
                self.record(kind, count)?;
                for _ in 0..count {
                    self.next_line();
                    self.both(b"\r\n")?;
                }
            }
            Kind::NoEol => {}
        }
        Ok(())
    }

    /// Writes a block of `count` lines to A only and returns it, for a
    /// move to emit in B later.
    fn withhold(&mut self, count: u64) -> io::Result<(Mutation, Vec<u8>)> {
        let m = Mutation {
            kind: Kind::Move,
            line_a: self.line_a,
            line_b: 0,
            count,
        };
        let mut block = Vec::new();
        for _ in 0..count {
            self.next_line();
            self.buf.push(b'\n');
            block.extend_from_slice(&self.buf);
            self.only_a(b"")?;
        }
        Ok((m, block))
    }

    fn flush_move_if_due(&mut self) -> io::Result<()> {
        let due = match &mut self.pending_move {
            Some((_, _, left)) if *left == 0 => true,
            // Counted down in `both` instead.
            Some(_) if self.config.profile == Profile::Shuffled => false,
            Some((_, _, left)) => {
                *left -= 1;
                false
            }
            None => false,
        };
        if due {
            let (m, block, _) = self.pending_move.take().expect("checked above");
            self.emit_move(m, &block)?;
        }
        Ok(())
    }

    fn emit_move(&mut self, mut m: Mutation, block: &[u8]) -> io::Result<()> {
        m.line_b = self.line_b;
        self.write_manifest(m)?;
        self.b.write_all(block)?;
        self.line_b += m.count;
        Ok(())
    }

    fn next_line(&mut self) {
        self.buf.clear();
        self.lines.next_line(&mut self.rng, &mut self.buf);
    }

    /// Replaces the line in the buffer with one that differs from it,
    /// under the same key where the profile has one.
    fn changed_line(&mut self) {
        self.old.clear();
        self.old.extend_from_slice(&self.buf);
        loop {
            self.buf.clear();
            self.lines.same_key(&mut self.rng, &mut self.buf);
            if self.buf != self.old {
                break;
            }
        }
    }

    /// A header or footer line, in both files and outside the manifest.
    fn fixed_line(&mut self, line: &str) -> io::Result<()> {
        self.buf.clear();
        self.buf.extend_from_slice(line.as_bytes());
        self.both(b"\n")
    }

    fn record(&mut self, kind: Kind, count: u64) -> io::Result<()> {
        self.write_manifest(Mutation {
            kind,
            line_a: self.line_a,
            line_b: self.line_b,
            count,
        })
    }

    fn write_manifest(&mut self, m: Mutation) -> io::Result<()> {
        writeln!(
            self.manifest,
            "{}\t{}\t{}\t{}",
            m.kind.name(),
            m.line_a,
            m.line_b,
            m.count
        )
    }

    fn both(&mut self, end: &[u8]) -> io::Result<()> {
        if self.config.profile == Profile::Shuffled
            && let Some((_, _, left)) = &mut self.pending_move
        {
            *left = left.saturating_sub(1);
        }
        self.only_a(end)?;
        self.only_b(end)
    }

    fn only_a(&mut self, end: &[u8]) -> io::Result<()> {
        self.a.write_all(&self.buf)?;
        self.a.write_all(end)?;
        self.bytes_a += (self.buf.len() + end.len()) as u64;
        self.line_a += 1;
        Ok(())
    }

    fn only_b(&mut self, end: &[u8]) -> io::Result<()> {
        self.b.write_all(&self.buf)?;
        self.b.write_all(end)?;
        self.line_b += 1;
        Ok(())
    }
}

#[cfg(test)]
#[allow(clippy::cast_possible_truncation, reason = "test files are small")]
mod tests {
    use std::collections::{HashMap, HashSet};

    use super::*;

    fn pair(config: &Config) -> (Vec<u8>, Vec<u8>, Vec<Mutation>) {
        let (mut a, mut b, mut m) = (Vec::new(), Vec::new(), Vec::new());
        generate(config, &mut a, &mut b, &mut m).unwrap();
        (a, b, parse_manifest(&m).unwrap())
    }

    fn lines(data: &[u8]) -> Vec<&[u8]> {
        let mut v: Vec<&[u8]> = data.split(|&c| c == b'\n').collect();
        if v.last() == Some(&&b""[..]) {
            v.pop();
        }
        v
    }

    #[test]
    fn same_seed_is_reproducible() {
        let cfg = Config::new(3, 200_000, Profile::Csv);
        assert_eq!(pair(&cfg), pair(&cfg));
    }

    #[test]
    fn no_mutations_gives_identical_files() {
        let mut cfg = Config::new(1, 100_000, Profile::Sql);
        cfg.kinds.clear();
        let (a, b, muts) = pair(&cfg);
        assert_eq!(a, b);
        assert_eq!(muts, [] as [Mutation; 0]);
        assert!(a.len() >= 100_000);
    }

    #[test]
    fn manifest_line_numbers_point_at_mutations() {
        for profile in [Profile::Sql, Profile::Csv, Profile::Ndjson, Profile::Text] {
            let mut cfg = Config::new(11, 2_000_000, profile);
            cfg.change_ratio = 0.01;
            let (a, b, muts) = pair(&cfg);
            let (la, lb) = (lines(&a), lines(&b));
            let mut seen = HashSet::new();
            for mu in &muts {
                seen.insert(mu.kind);
                let n = usize::try_from(mu.count).unwrap();
                match mu.kind {
                    Kind::Insert => {
                        for i in 0..n {
                            assert!(!la.contains(&lb[mu.line_b as usize + i]));
                        }
                    }
                    Kind::Delete => {
                        for i in 0..n {
                            assert!(!lb.contains(&la[mu.line_a as usize + i]));
                        }
                    }
                    Kind::Modify => {
                        for i in 0..n {
                            assert_ne!(la[mu.line_a as usize + i], lb[mu.line_b as usize + i]);
                        }
                    }
                    Kind::Move => {
                        let (ia, ib) = (mu.line_a as usize, mu.line_b as usize);
                        assert_eq!(&la[ia..ia + n], &lb[ib..ib + n]);
                    }
                    Kind::Dup => {
                        let ia = mu.line_a as usize;
                        assert!(la[ia..ia + n].iter().all(|l| *l == la[ia]));
                        assert_eq!(&la[ia..ia + n], &lb[mu.line_b as usize..][..n]);
                    }
                    Kind::LowUniq | Kind::Crlf | Kind::Long => {
                        let ia = mu.line_a as usize;
                        assert_eq!(&la[ia..ia + n], &lb[mu.line_b as usize..][..n]);
                        if mu.kind == Kind::Crlf {
                            assert!(la[ia].ends_with(b"\r"));
                        }
                    }
                    Kind::NoEol => {
                        assert!(!a.ends_with(b"\n"));
                        assert!(!b.ends_with(b"\n"));
                        assert_eq!(mu.line_a as usize, la.len() - 1);
                    }
                }
            }
            for k in [
                Kind::Insert,
                Kind::Delete,
                Kind::Modify,
                Kind::Move,
                Kind::Dup,
            ] {
                assert!(seen.contains(&k), "{profile:?} never produced {k:?}");
            }
        }
    }

    #[test]
    fn long_line_exceeds_one_mebibyte() {
        let mut cfg = Config::new(5, 4 << 20, Profile::Text);
        cfg.kinds = vec![Kind::Long];
        cfg.change_ratio = 0.01;
        let (a, _, muts) = pair(&cfg);
        let long = muts.iter().find(|x| x.kind == Kind::Long).unwrap();
        assert!(lines(&a)[long.line_a as usize].len() > 1 << 20);
    }

    #[test]
    fn nokey_has_no_line_found_once() {
        for rows in [Rows::Csv, Rows::Sql, Rows::Ndjson] {
            let mut cfg = Config::new(7, 2 << 20, Profile::Nokey);
            cfg.rows = rows;
            cfg.change_ratio = 0.01;
            let (a, b, muts) = pair(&cfg);
            assert_ne!(a, b);
            for data in [&a, &b] {
                let mut seen = HashMap::new();
                for l in lines(data) {
                    *seen.entry(l).or_insert(0) += 1;
                }
                assert!(seen.len() <= NOKEY_POOL, "{rows:?}");
                assert!(
                    seen.values().all(|&n| n > 1),
                    "{rows:?} has a line found once"
                );
            }
            let (la, lb) = (lines(&a), lines(&b));
            let mut kinds = HashSet::new();
            for mu in &muts {
                kinds.insert(mu.kind);
                let (ia, ib, n) = (mu.line_a as usize, mu.line_b as usize, mu.count as usize);
                match mu.kind {
                    Kind::Modify => {
                        for i in 0..n {
                            assert_ne!(la[ia + i], lb[ib + i], "{rows:?} {mu:?}");
                        }
                    }
                    Kind::Move | Kind::Dup => assert_eq!(&la[ia..ia + n], &lb[ib..ib + n]),
                    _ => {}
                }
            }
            for k in [Kind::Insert, Kind::Delete, Kind::Modify, Kind::Move] {
                assert!(kinds.contains(&k), "{rows:?} never produced {k:?}");
            }
        }
    }

    /// The key of a `shuffled` row.
    fn key(rows: Rows, line: &[u8]) -> &[u8] {
        let field = |sep: u8| line.split(|&c| c == sep).next().unwrap();
        match rows {
            Rows::Csv => field(b','),
            Rows::Sql => field(b'\t'),
            Rows::Ndjson => {
                let rest = &line[b"{\"id\":\"".len()..];
                &rest[..rest.iter().position(|&c| c == b'"').unwrap()]
            }
        }
    }

    /// Rows of a `shuffled` file by key, without the header and footer.
    fn by_key(rows: Rows, data: &[u8]) -> HashMap<&[u8], &[u8]> {
        let mut all = lines(data);
        assert!(all.len() > 2);
        all.remove(0);
        if rows == Rows::Sql {
            assert_eq!(all.pop(), Some(&b"\\."[..]));
        }
        let mut out = HashMap::new();
        for l in all {
            let k = key(rows, l);
            assert!(out.insert(k, l).is_none(), "duplicate key {k:?}");
        }
        out
    }

    #[test]
    fn shuffled_changes_by_key_are_the_manifest() {
        for rows in [Rows::Csv, Rows::Sql, Rows::Ndjson] {
            let mut cfg = Config::new(7, 1 << 20, Profile::Shuffled);
            cfg.rows = rows;
            cfg.change_ratio = 0.01;
            let (a, b, muts) = pair(&cfg);
            let (la, lb) = (lines(&a), lines(&b));
            assert_eq!(la[0], lb[0], "{rows:?} header");
            let (ka, kb) = (by_key(rows, &a), by_key(rows, &b));
            let at_b: HashMap<&[u8], usize> = lb.iter().enumerate().map(|(i, l)| (*l, i)).collect();
            let in_a: HashSet<&[u8]> = la.iter().copied().collect();

            let (mut added, mut removed, mut changed) = (vec![], vec![], vec![]);
            let mut moves = 0;
            for mu in &muts {
                let (ia, ib, n) = (mu.line_a as usize, mu.line_b as usize, mu.count as usize);
                match mu.kind {
                    Kind::Insert => added.extend(lb[ib..ib + n].iter().map(|l| key(rows, l))),
                    Kind::Delete => removed.extend(la[ia..ia + n].iter().map(|l| key(rows, l))),
                    Kind::Modify => {
                        for i in 0..n {
                            assert_eq!(key(rows, la[ia + i]), key(rows, lb[ib + i]));
                            changed.push(key(rows, la[ia + i]));
                        }
                    }
                    Kind::Move => {
                        assert_eq!(&la[ia..ia + n], &lb[ib..ib + n]);
                        // Lines in both files that the block jumps over in
                        // B. One still in flight when A ends lands early.
                        let start = (0..ia)
                            .rev()
                            .find_map(|i| at_b.get(la[i]))
                            .map_or(0, |&p| p + 1);
                        let crossed = lb[start..ib].iter().filter(|l| in_a.contains(*l)).count();
                        let footer = usize::from(rows == Rows::Sql);
                        let last = ib + n + footer == lb.len();
                        assert!(
                            last || crossed >= 4 * n,
                            "{rows:?} {mu:?} crosses {crossed}"
                        );
                        moves += 1;
                    }
                    k => panic!("{rows:?} produced {k:?}"),
                }
            }
            let keys = |from: &HashMap<&[u8], &[u8]>, pick: &dyn Fn(&[u8], &[u8]) -> bool| {
                let mut v: Vec<Vec<u8>> = from
                    .iter()
                    .filter(|(k, l)| pick(k, l))
                    .map(|(k, _)| k.to_vec())
                    .collect();
                v.sort_unstable();
                v
            };
            let sorted = |v: Vec<&[u8]>| {
                let mut v: Vec<Vec<u8>> = v.into_iter().map(<[u8]>::to_vec).collect();
                v.sort_unstable();
                v
            };
            assert_eq!(sorted(added), keys(&kb, &|k, _| !ka.contains_key(k)));
            assert_eq!(sorted(removed), keys(&ka, &|k, _| !kb.contains_key(k)));
            assert_eq!(
                sorted(changed),
                keys(&ka, &|k, l| kb.get(k).is_some_and(|m| *m != l))
            );
            assert!(moves > 10, "{rows:?} moved {moves} blocks");
        }
    }

    #[test]
    fn shuffled_without_changes_only_reorders() {
        let mut cfg = Config::new(3, 1 << 20, Profile::Shuffled);
        cfg.change_ratio = 0.0;
        let (a, b, muts) = pair(&cfg);
        assert_ne!(a, b);
        let sorted = |d: &[u8]| {
            let mut v = lines(d);
            v.sort_unstable();
            v.into_iter().map(<[u8]>::to_vec).collect::<Vec<_>>()
        };
        assert_eq!(sorted(&a), sorted(&b));
        assert!(muts.iter().all(|m| m.kind == Kind::Move));
    }

    /// A low-entropy config: `k` is `--distinct` or `--period`.
    fn low_entropy(profile: Profile, k: u64, seed: u64) -> Config {
        let mut cfg = Config::new(seed, 1 << 20, profile);
        cfg.distinct = k;
        cfg.period = k;
        cfg
    }

    fn distinct_lines(data: &[u8]) -> usize {
        lines(data).into_iter().collect::<HashSet<_>>().len()
    }

    #[test]
    fn low_entropy_profiles_without_changes_have_their_shape() {
        for k in [1, 2, 10, 100] {
            let mut cfg = low_entropy(Profile::Distinct, k, 5);
            cfg.kinds.clear();
            let (a, b, _) = pair(&cfg);
            assert_eq!(a, b);
            assert_eq!(distinct_lines(&a) as u64, k);
        }
        let mut cfg = low_entropy(Profile::Periodic, 1000, 5);
        cfg.kinds.clear();
        let (a, _, _) = pair(&cfg);
        let la = lines(&a);
        assert!(la.len() > 3000);
        assert_eq!(distinct_lines(&a), 1000);
        assert!(la.iter().zip(&la[1000..]).all(|(x, y)| x == y));

        let mut cfg = low_entropy(Profile::Alternating, 0, 5);
        cfg.kinds.clear();
        let (a, _, _) = pair(&cfg);
        let la = lines(&a);
        assert_eq!(distinct_lines(&a), 2);
        assert!(la.windows(2).all(|w| w[0] != w[1]));
        assert!(la.iter().zip(&la[2..]).all(|(x, y)| x == y));

        let mut cfg = low_entropy(Profile::Empty, 0, 5);
        cfg.kinds.clear();
        let (a, _, _) = pair(&cfg);
        assert!(a.len() >= 1 << 20 && a.iter().all(|&c| c == b'\n'));

        let mut cfg = low_entropy(Profile::Runs, 0, 5);
        cfg.size = 8 << 20;
        cfg.kinds.clear();
        let (a, _, _) = pair(&cfg);
        let la = lines(&a);
        let runs: Vec<usize> = la.chunk_by(|x, y| x == y).map(<[_]>::len).collect();
        assert!(runs.len() > 2, "{} runs", runs.len());
        let (lo, hi) = (RUN_LINES.0 as usize, RUN_LINES.1 as usize);
        assert!(runs[..runs.len() - 1].iter().all(|n| (lo..=hi).contains(n)));
        assert_eq!(distinct_lines(&a), runs.len());
    }

    #[test]
    fn low_entropy_changes_follow_the_manifest() {
        let points = [
            (Profile::Distinct, 1),
            (Profile::Distinct, 2),
            (Profile::Distinct, 10),
            (Profile::Distinct, 100),
            (Profile::Distinct, 10_000),
            (Profile::Periodic, 1000),
            (Profile::Runs, 0),
            (Profile::Alternating, 0),
            (Profile::Empty, 0),
        ];
        for (profile, k) in points {
            let mut cfg = low_entropy(profile, k, 11);
            cfg.change_ratio = 0.01;
            assert_eq!(pair(&cfg), pair(&cfg), "{profile:?} {k} is reproducible");
            let (a, b, muts) = pair(&cfg);
            let (la, lb) = (lines(&a), lines(&b));
            let mut kinds = HashSet::new();
            for mu in &muts {
                kinds.insert(mu.kind);
                let (ia, ib, n) = (mu.line_a as usize, mu.line_b as usize, mu.count as usize);
                match mu.kind {
                    Kind::Modify => {
                        for i in 0..n {
                            assert_eq!(lb[ib + i], [la[ia + i], b"~"].concat(), "{profile:?}");
                        }
                    }
                    Kind::Move | Kind::Dup => assert_eq!(&la[ia..ia + n], &lb[ib..ib + n]),
                    Kind::Insert | Kind::Delete => {}
                    k => panic!("{profile:?} produced {k:?}"),
                }
            }
            for kind in [Kind::Insert, Kind::Delete, Kind::Modify, Kind::Move] {
                assert!(kinds.contains(&kind), "{profile:?} never produced {kind:?}");
            }
            // A modify appends `~`, so B has at most twice A's lines.
            assert!(distinct_lines(&b) <= 2 * distinct_lines(&a), "{profile:?}");
            if profile == Profile::Distinct {
                assert!(distinct_lines(&a) as u64 <= k);
            }
        }
    }

    #[test]
    fn manifest_roundtrip() {
        let text = b"kind\tline_a\tline_b\tcount\ninsert\t3\t4\t5\nnoeol\t9\t9\t1\n";
        let muts = parse_manifest(text).unwrap();
        assert_eq!(muts[0].kind, Kind::Insert);
        assert_eq!((muts[0].line_a, muts[0].line_b, muts[0].count), (3, 4, 5));
        assert_eq!(muts[1].kind, Kind::NoEol);
        assert!(parse_manifest(b"h\nbogus\t1\t2\t3\n").is_err());
    }
}
