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
    /// Rows in the `--rows` format with no id column, drawn from a pool of
    /// [`NOKEY_POOL`] rows, so a line rarely occurs only once.
    Nokey,
    /// Rows in the `--rows` format keyed by a unique id in the first
    /// column (`id` field for NDJSON), with B's rows reordered by block
    /// moves. Changes keep the key: an insert adds keys, a delete removes
    /// them, a modify rewrites a row under the same key.
    Shuffled,
}

/// Row format of the [`Profile::Nokey`] and [`Profile::Shuffled`] profiles.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, clap::ValueEnum)]
pub enum Rows {
    /// Comma-separated, with a header row when keyed.
    #[default]
    Csv,
    /// `INSERT` statements without ids for `nokey`; one `COPY ... FROM
    /// stdin` block for `shuffled`.
    Sql,
    /// One JSON object per line.
    Ndjson,
}

/// Distinct rows of a [`Profile::Nokey`] file.
pub const NOKEY_POOL: usize = 1024;

const WORDS: usize = 2048;

/// Stateful line producer for one profile.
#[derive(Debug)]
pub struct Lines {
    profile: Profile,
    rows: Rows,
    id: u64,
    vocab: String,
    vocab_at: Vec<(u32, u32)>,
    /// The rows of a `nokey` file, back to back, and where each ends.
    pool: Vec<u8>,
    pool_ends: Vec<usize>,
}

impl Lines {
    /// Builds the producer; the text vocabulary is derived from `rng`.
    /// `rows` matters only to the `nokey` and `shuffled` profiles.
    pub fn new(profile: Profile, rows: Rows, rng: &mut Rng) -> Self {
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
        let mut lines = Self {
            profile,
            rows,
            id: 1,
            vocab,
            vocab_at,
            pool: Vec::new(),
            pool_ends: Vec::new(),
        };
        if profile == Profile::Nokey {
            let mut pool = Vec::new();
            for _ in 0..NOKEY_POOL {
                lines.unkeyed_row(rng, &mut pool);
                lines.pool_ends.push(pool.len());
            }
            lines.pool = pool;
        }
        lines
    }

    /// The first line of both files, if the profile has one.
    pub fn header(&self) -> Option<&'static str> {
        match (self.profile, self.rows) {
            (Profile::Shuffled, Rows::Csv) => Some(
                "id,pickup_at,passengers,pickup_lat,pickup_lon,dropoff_lat,dropoff_lon,fare,payment",
            ),
            (Profile::Shuffled, Rows::Sql) => {
                Some("COPY public.orders (id, customer_id, status, amount, note) FROM stdin;")
            }
            _ => None,
        }
    }

    /// The last line of both files, if the profile has one.
    pub fn footer(&self) -> Option<&'static str> {
        (self.profile == Profile::Shuffled && self.rows == Rows::Sql).then_some("\\.")
    }

    /// Appends, without a terminator, a row with the key of the row
    /// [`next_line`](Self::next_line) produced last and new values. Only
    /// `shuffled` rows have a key; other profiles get their next line.
    pub fn same_key(&mut self, rng: &mut Rng, out: &mut Vec<u8>) {
        if self.profile == Profile::Shuffled {
            self.keyed_row(self.id - 1, rng, out);
        } else {
            self.next_line(rng, out);
        }
    }

    /// Appends one line without a line terminator to `out`.
    pub fn next_line(&mut self, rng: &mut Rng, out: &mut Vec<u8>) {
        let id = self.id;
        self.id += 1;
        match self.profile {
            Profile::Sql => {
                if id.is_multiple_of(2) {
                    self.insert_row(id, rng, out);
                } else {
                    self.copy_row(id, rng, out);
                }
            }
            Profile::Csv => csv_row(id, rng, out),
            Profile::Ndjson => self.ndjson_row(id, rng, out),
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
            Profile::Nokey => {
                let i = rng.index(NOKEY_POOL);
                let start = if i == 0 { 0 } else { self.pool_ends[i - 1] };
                out.extend_from_slice(&self.pool[start..self.pool_ends[i]]);
            }
            Profile::Shuffled => self.keyed_row(id, rng, out),
        }
    }

    fn keyed_row(&self, id: u64, rng: &mut Rng, out: &mut Vec<u8>) {
        match self.rows {
            Rows::Csv => csv_row(id, rng, out),
            Rows::Sql => self.copy_row(id, rng, out),
            Rows::Ndjson => self.ndjson_row(id, rng, out),
        }
    }

    /// A row with no id and few distinct values per column.
    fn unkeyed_row(&self, rng: &mut Rng, out: &mut Vec<u8>) {
        match self.rows {
            Rows::Csv => {
                let _ = write!(
                    out,
                    "{},{},{},{}.{:02},{}",
                    rng.range(1, 3),
                    rng.range(1, 6),
                    rng.range(1, 4),
                    rng.below(60),
                    rng.below(4) * 25,
                    rng.below(40),
                );
            }
            Rows::Sql => {
                let _ = write!(
                    out,
                    "INSERT INTO public.events (kind, status, amount) VALUES ('{}', '{}', {});",
                    EVENTS[rng.index(4)],
                    STATUS[rng.index(4)],
                    rng.below(100),
                );
            }
            Rows::Ndjson => {
                let _ = write!(
                    out,
                    "{{\"type\":\"{}\",\"status\":\"{}\",\"amount\":{}}}",
                    EVENTS[rng.index(4)],
                    STATUS[rng.index(4)],
                    rng.below(100),
                );
            }
        }
    }

    fn insert_row(&self, id: u64, rng: &mut Rng, out: &mut Vec<u8>) {
        let _ = write!(
            out,
            "INSERT INTO public.orders (id, customer_id, status, amount, note) VALUES ({id}, {}, '{}', {}.{:02}, '{}');",
            rng.below(1_000_000),
            STATUS[rng.index(4)],
            rng.below(10_000),
            rng.below(100),
            self.word(rng),
        );
    }

    fn copy_row(&self, id: u64, rng: &mut Rng, out: &mut Vec<u8>) {
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

    fn ndjson_row(&self, id: u64, rng: &mut Rng, out: &mut Vec<u8>) {
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

    fn word(&self, rng: &mut Rng) -> &str {
        let (s, e) = self.vocab_at[rng.index(WORDS)];
        &self.vocab[s as usize..e as usize]
    }
}

fn csv_row(id: u64, rng: &mut Rng, out: &mut Vec<u8>) {
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
            Profile::Text | Profile::Nokey | Profile::Shuffled => {
                let _ = write!(out, "{}", STATUS[(v % 4) as usize]);
            }
        }
    }
}
