//! Content profiles: what a single line looks like.

use std::io::Write;

use crate::rng::Rng;

/// Shape of the generated lines.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum Profile {
    /// `pg_dump`-like `INSERT` statements and `COPY` rows.
    Sql,
    /// Comma-separated trip records with numeric columns.
    Csv,
    /// One JSON object per line, event-feed style.
    Ndjson,
    /// Prose-like lines of random words.
    Text,
}

const WORDS: usize = 2048;

/// Stateful line producer for one profile.
#[derive(Debug)]
pub struct Lines {
    profile: Profile,
    id: u64,
    vocab: String,
    vocab_at: Vec<(u32, u32)>,
}

impl Lines {
    /// Builds the producer; the text vocabulary is derived from `rng`.
    pub fn new(profile: Profile, rng: &mut Rng) -> Self {
        let mut vocab = String::with_capacity(WORDS * 8);
        let mut vocab_at = Vec::with_capacity(WORDS);
        for _ in 0..WORDS {
            let start = vocab.len();
            let len = rng.range(2, 11);
            for _ in 0..len {
                vocab.push(char::from(b'a' + rng.byte(26)));
            }
            #[allow(clippy::cast_possible_truncation, reason = "vocab is under 32 KiB")]
            vocab_at.push((start as u32, vocab.len() as u32));
        }
        Self {
            profile,
            id: 1,
            vocab,
            vocab_at,
        }
    }

    /// Appends one line without a line terminator to `out`.
    pub fn next_line(&mut self, rng: &mut Rng, out: &mut Vec<u8>) {
        let id = self.id;
        self.id += 1;
        match self.profile {
            Profile::Sql => {
                if id.is_multiple_of(2) {
                    let _ = write!(
                        out,
                        "INSERT INTO public.orders (id, customer_id, status, amount, note) VALUES ({id}, {}, '{}', {}.{:02}, '{}');",
                        rng.below(1_000_000),
                        STATUS[rng.index(4)],
                        rng.below(10_000),
                        rng.below(100),
                        self.word(rng),
                    );
                } else {
                    let _ = write!(
                        out,
                        "{id}\t{}\t{}\t{}.{:02}\t{}",
                        rng.below(1_000_000),
                        STATUS[rng.index(4)],
                        rng.below(10_000),
                        rng.below(100),
                        self.word(rng),
                    );
                }
            }
            Profile::Csv => {
                let _ = write!(
                    out,
                    "{id},2024-{:02}-{:02} {:02}:{:02}:{:02},{},40.{:06},-73.{:06},40.{:06},-73.{:06},{}.{:02},{}",
                    rng.range(1, 12),
                    rng.range(1, 28),
                    rng.below(24),
                    rng.below(60),
                    rng.below(60),
                    rng.range(1, 6),
                    rng.below(1_000_000),
                    rng.below(1_000_000),
                    rng.below(1_000_000),
                    rng.below(1_000_000),
                    rng.below(200),
                    rng.below(100),
                    rng.range(1, 4),
                );
            }
            Profile::Ndjson => {
                let _ = write!(
                    out,
                    "{{\"id\":\"{}\",\"type\":\"{}\",\"actor\":{{\"id\":{},\"login\":\"{}\"}},\"repo\":{{\"id\":{},\"name\":\"{}/{}\"}},\"created_at\":\"2024-01-{:02}T{:02}:{:02}:{:02}Z\"}}",
                    10_000_000_000u64 + id,
                    EVENTS[rng.index(4)],
                    rng.below(100_000_000),
                    self.word(rng),
                    rng.below(100_000_000),
                    self.word(rng),
                    self.word(rng),
                    rng.range(1, 28),
                    rng.below(24),
                    rng.below(60),
                    rng.below(60),
                );
            }
            Profile::Text => {
                let n = rng.range(5, 15);
                for i in 0..n {
                    if i > 0 {
                        out.push(b' ');
                    }
                    out.extend_from_slice(self.word(rng).as_bytes());
                }
                out.push(b'.');
            }
        }
    }

    fn word(&self, rng: &mut Rng) -> &str {
        let (s, e) = self.vocab_at[rng.index(WORDS)];
        &self.vocab[s as usize..e as usize]
    }
}

const STATUS: [&str; 4] = ["new", "paid", "shipped", "cancelled"];
const EVENTS: [&str; 4] = ["PushEvent", "WatchEvent", "IssuesEvent", "CreateEvent"];

impl Profile {
    /// A line body from a tiny vocabulary, for low-uniqueness regions.
    pub(crate) fn repetitive_line(self, rng: &mut Rng, out: &mut Vec<u8>) {
        let v = rng.below(8);
        match self {
            Profile::Sql => {
                let _ = write!(out, "{v}\t0\tnew\t0.00\tnull");
            }
            Profile::Csv => {
                let _ = write!(out, "{v},,,,,,,,,,");
            }
            Profile::Ndjson => {
                let _ = write!(out, "{{\"type\":\"{}\"}}", EVENTS[(v % 4) as usize]);
            }
            Profile::Text => {
                let _ = write!(out, "{}", STATUS[(v % 4) as usize]);
            }
        }
    }
}
