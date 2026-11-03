//! Deterministic generator of file pairs with a known mutation manifest.
//!
//! Both files stream through the writers in one pass; memory stays constant
//! regardless of the target size. The manifest records every mutation so a
//! diff can be checked against the truth.

#![forbid(unsafe_code)]

mod profile;
mod rng;

use std::io::{self, Write};

pub use profile::Profile;
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
    /// Probability that a mutation starts at a given line.
    pub change_ratio: f64,
    /// Kinds eligible for random selection.
    pub kinds: Vec<Kind>,
}

impl Config {
    /// Settings with every kind enabled, except `Long` below 64 MiB where a
    /// 1 MiB line would dominate the file.
    #[must_use]
    pub fn new(seed: u64, size: u64, profile: Profile) -> Self {
        let kinds = Kind::ALL
            .into_iter()
            .filter(|k| *k != Kind::Long || size >= 64 << 20)
            .collect();
        Self {
            seed,
            size,
            profile,
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
    /// A block withheld from B, with the number of B lines still to pass
    /// before it is emitted.
    pending_move: Option<(Mutation, Vec<u8>, u64)>,
}

impl<A: Write, B: Write, M: Write> Generator<'_, A, B, M> {
    fn new(config: &Config, a: A, b: B, manifest: M) -> Generator<'_, A, B, M> {
        let mut rng = Rng::new(config.seed);
        let lines = profile::Lines::new(config.profile, &mut rng);
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
            pending_move: None,
        }
    }

    fn run(mut self) -> io::Result<()> {
        self.manifest.write_all(b"kind\tline_a\tline_b\tcount\n")?;
        let no_eol = self.config.kinds.contains(&Kind::NoEol) && self.rng.chance(0.5);
        while self.bytes_a < self.config.size {
            self.flush_move_if_due()?;
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
                    self.next_line();
                    self.only_b(b"\n")?;
                }
            }
            Kind::Move => {
                if self.pending_move.is_some() {
                    return Ok(());
                }
                let m = Mutation {
                    kind,
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

    fn flush_move_if_due(&mut self) -> io::Result<()> {
        let due = match &mut self.pending_move {
            Some((_, _, left)) if *left == 0 => true,
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
        assert!(muts.is_empty());
        assert!(a.len() >= 100_000);
    }

    #[test]
    fn manifest_line_numbers_point_at_mutations() {
        for profile in [Profile::Sql, Profile::Csv, Profile::Ndjson, Profile::Text] {
            let mut cfg = Config::new(11, 2_000_000, profile);
            cfg.change_ratio = 0.01;
            let (a, b, muts) = pair(&cfg);
            let (la, lb) = (lines(&a), lines(&b));
            let mut seen = std::collections::HashSet::new();
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
    fn manifest_roundtrip() {
        let text = b"kind\tline_a\tline_b\tcount\ninsert\t3\t4\t5\nnoeol\t9\t9\t1\n";
        let muts = parse_manifest(text).unwrap();
        assert_eq!(muts[0].kind, Kind::Insert);
        assert_eq!((muts[0].line_a, muts[0].line_b, muts[0].count), (3, 4, 5));
        assert_eq!(muts[1].kind, Kind::NoEol);
        assert!(parse_manifest(b"h\nbogus\t1\t2\t3\n").is_err());
    }
}
