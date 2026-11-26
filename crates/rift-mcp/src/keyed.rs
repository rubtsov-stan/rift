//! Key mode for the MCP tools `keyed_summary` and `keyed_changes`.
//!
//! Records are compared by a key, as `rift --key` does, through the same
//! `rift-core` passes. Nothing is kept between calls: a keyed run holds
//! tables sized from its budget, and a cache of them would outlive the
//! call. Each call runs the whole comparison, so a duplicate or missing
//! key is an error whatever page was asked for, and pages come from one
//! deterministic order: by pass, then by line within a pass. The order
//! depends on `mem`, `fields`, `key` and the files, so a page's cursor
//! carries a fingerprint of them and a cursor of another call is refused.
//!
//! # Memory
//!
//! - the run: a [`rift_session::Lease`] of `mem` counts in the server's
//!   total and takes a slot, and the core's own [`Budget`] of that size
//!   pays for tables, windows and record buffers, as `rift --key` does;
//! - the answer under construction: `ANSWER_RESERVE` bytes of the refine
//!   half, held for the whole run. A page weighs at most
//!   [`render::MAX_TEXT_BYTES`] by the count in `weigh`, which is at
//!   least its size as JSON, so it fits in a reserve twice that;
//! - the worker threads: the core builds a pool of its own for each run, with
//!   as many threads as its refine half pays for (at most one per shard), so
//!   they are accounted there and never start rayon's global pool.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

use rift_core::keyed::{
    ChangeKind, FieldChange, FieldName, KeyError, KeySpec, KeyedChange, Side, compare,
    compare_fields,
};
use rift_core::{Budget, Bytes, Error, Half};
use rift_session::Lease;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::Pair;
use crate::file::{FileId, FileSource};
use crate::render;

/// Keys listed per kind by `keyed_summary`.
pub const FIRST_KEYS: usize = 10;
/// Bytes of a key, a table name, a field name or a field value kept.
pub const MAX_VALUE_BYTES: usize = 256;
/// Bytes of one record kept.
pub const MAX_RECORD_BYTES: usize = 1024;
/// Changes in a `keyed_changes` answer when `limit` is not set.
pub const DEFAULT_CHANGES: u64 = 20;
/// Most changes in one answer.
pub const MAX_CHANGES: u64 = 100;
/// Fields listed for one changed key; `fields_more` counts the rest.
pub const MAX_FIELDS: usize = 32;
/// Weight of an entry and of a field beyond their text.
const ENTRY_WEIGHT: usize = 256;
const FIELD_WEIGHT: usize = 64;
/// Refine-half bytes held for the answer.
const ANSWER_RESERVE: u64 = 2 * render::MAX_TEXT_BYTES as u64;

/// Which two files to compare, and by what key.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct KeyedPair {
    /// The files and the memory budget.
    #[serde(flatten)]
    pub pair: Pair,
    /// Where the key is, as `--key`: `csv:NAME` (header name), `csv:N`
    /// (column N, from 1), `json:PATH` (NDJSON field, dotted for nested)
    /// or `copy:NAME` (column of a `pg_dump` COPY block).
    pub key: String,
}

/// Arguments of `keyed_changes`.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct KeyedChanges {
    /// The files and the key.
    #[serde(flatten)]
    pub keyed: KeyedPair,
    /// Where the page starts: absent for the first page, else the `next` of
    /// the last answer. A cursor is good only for the same files, `key`,
    /// `mem` and `fields` it was issued for; any other call is refused,
    /// and the client starts again without a cursor. `limit` may differ.
    pub cursor: Option<String>,
    /// Most changes to return, default 20, at most 100. An answer also
    /// stops at 64 KiB.
    pub limit: Option<u64>,
    /// Compare only these fields, as `--fields`: a changed key whose
    /// listed fields are equal is not returned and not counted. Names are
    /// header cells, member names or COPY columns; for `csv:N` keys, column
    /// numbers. An empty list means all fields.
    pub fields: Option<Vec<String>>,
}

/// A key and where it is.
#[derive(Debug, Serialize, JsonSchema)]
pub struct KeyRef {
    /// The `COPY` table as its header names it, unescaped; null outside
    /// `pg_dump` files.
    pub table: Option<String>,
    /// The key as the file has it, quotes included, cut at 256 bytes.
    pub key: String,
    /// `key` or `table` was cut.
    pub key_truncated: bool,
    /// The record's first line in A, 1-based; null for an added key.
    pub a_line: Option<u64>,
    /// The record's first line in B; null for a removed key.
    pub b_line: Option<u64>,
}

/// Answer of `keyed_summary`.
#[derive(Debug, Serialize, JsonSchema)]
pub struct KeyedSummary {
    /// Records in A.
    pub a_records: u64,
    /// Records in B.
    pub b_records: u64,
    /// Passes over the files. More than 1 means the budget was tight.
    pub passes: u32,
    /// Keys only B has.
    pub added: u64,
    /// Keys only A has.
    pub removed: u64,
    /// Keys in both with different record bytes.
    pub changed: u64,
    /// The first added keys, at most 10.
    pub first_added: Vec<KeyRef>,
    /// The first removed keys, at most 10.
    pub first_removed: Vec<KeyRef>,
    /// The first changed keys, at most 10.
    pub first_changed: Vec<KeyRef>,
    /// Memory budget of the run, in bytes.
    pub mem_budget_bytes: u64,
}

/// One field that differs.
#[derive(Debug, Serialize, JsonSchema)]
pub struct FieldEntry {
    /// Header cell, member name or COPY column; the column number, from
    /// 1, for `csv:N` keys.
    pub name: String,
    /// The value in A, cut at 256 bytes; null when A's record lacks it.
    pub a: Option<String>,
    /// The value in B; null when B's record lacks it.
    pub b: Option<String>,
    /// A name or a value was cut.
    pub truncated: bool,
}

/// One changed key.
#[derive(Debug, Serialize, JsonSchema)]
pub struct Entry {
    /// `added`, `removed` or `changed`.
    pub kind: String,
    /// The key and its table.
    #[serde(flatten)]
    pub key: KeyRef,
    /// A's record without its line terminator, cut at 1024 bytes; null for
    /// an added key.
    pub a_record: Option<String>,
    /// B's record; null for a removed key.
    pub b_record: Option<String>,
    /// A record was cut.
    pub records_truncated: bool,
    /// For a changed key, the fields that differ, at most 32, in A's order.
    /// Empty when the records differ only in layout. Null for an added or
    /// removed key, and for a record that does not split into fields (an
    /// NDJSON line that is not one JSON object).
    pub fields: Option<Vec<FieldEntry>>,
    /// Differing fields beyond those listed.
    pub fields_more: u64,
}

/// Answer of `keyed_changes`.
#[derive(Debug, Serialize, JsonSchema)]
pub struct KeyedPage {
    /// All changes, those of other pages included.
    pub total: u64,
    /// Keys only B has.
    pub added: u64,
    /// Keys only A has.
    pub removed: u64,
    /// Keys in both with different record bytes.
    pub changed: u64,
    /// The `cursor` of the next page; null after the last change.
    pub next: Option<String>,
    /// The changes, in the order of the comparison.
    pub changes: Vec<Entry>,
    /// Memory budget of the run, in bytes.
    pub mem_budget_bytes: u64,
}

/// A run's files, key and budget.
struct Run<'s> {
    a: FileSource,
    b: FileSource,
    ids: [FileId; 2],
    spec: KeySpec,
    paths: [String; 2],
    lease: Lease<'s>,
}

impl Run<'_> {
    /// The message a client gets for `err`: the CLI's wording, with the
    /// file named next to its side and, for a budget refusal, the `mem`
    /// to call again with.
    fn describe(&self, err: &Error) -> String {
        let path = |side: Side| &self.paths[usize::from(side == Side::B)];
        let msg = match err {
            Error::Keyed(KeyError::NoColumn { side, name }) => {
                format!("{side} ({}) has no column {name:?}", path(*side))
            }
            Error::Keyed(KeyError::NoKey { side, line }) => {
                format!("line {} of {side} ({}) has no key", line.0 + 1, path(*side))
            }
            Error::Keyed(KeyError::TooLong { side, line, cap }) => format!(
                "line {} of {side} ({}) starts a record longer than {cap} bytes, the most key mode holds",
                line.0 + 1,
                path(*side)
            ),
            e => e.to_string(),
        };
        if let Error::Budget(b) = err
            && let Some(m) = b.mem_advice(self.lease.mem())
        {
            return format!("{msg}; call again with mem {}", m.to_mem_arg());
        }
        msg
    }
}

/// Most names `fields` may list, and the longest one: the list is held
/// outside the budget, so it is bounded here.
pub const MAX_FIELD_NAMES: usize = 100;

fn check_fields(fields: Option<&[String]>) -> Result<(), String> {
    let list = fields.unwrap_or_default();
    if list.len() > MAX_FIELD_NAMES || list.iter().any(|f| f.len() > MAX_VALUE_BYTES) {
        return Err(format!(
            "fields: at most {MAX_FIELD_NAMES} names of at most {MAX_VALUE_BYTES} bytes"
        ));
    }
    Ok(())
}

const MALFORMED: &str = "cursor: not a cursor of keyed_changes; start again without a cursor";
const STALE: &str = "cursor: the files or arguments changed since that cursor was issued; start again without a cursor";
const CURSOR_TAG: &str = "r1";
/// Tag, then three 16-digit hex numbers.
const CURSOR_LEN: usize = CURSOR_TAG.len() + 3 * 16;

/// What a call's pages have to share: `key`, the budget, `fields` and, for
/// each file, its device, inode, size and modification time. A page that
/// starts from a cursor of another call would be a different slice of a
/// different comparison.
///
/// The hash only guards paging against a wrong page. It never decides that
/// two files, records or keys are equal: the comparison itself confirms
/// every match byte by byte.
fn fingerprint(key: &str, run: &Run<'_>, fields: Option<&[String]>) -> u64 {
    let mut h = DefaultHasher::new();
    (key, run.lease.mem().0, fields, run.ids).hash(&mut h);
    h.finish()
}

/// Where a page starts, and the call it belongs to. Written as `r1`, the
/// position, the fingerprint and a checksum of both, in lowercase hex: a
/// cursor is opaque to the client, and the checksum only catches a cursor
/// that was cut or mistyped.
#[derive(Debug, Clone, Copy)]
struct Cursor {
    at: u64,
    print: u64,
}

impl Cursor {
    fn check(at: u64, print: u64) -> u64 {
        let mut h = DefaultHasher::new();
        (CURSOR_TAG, at, print).hash(&mut h);
        h.finish()
    }

    fn parse(s: &str) -> Result<Self, String> {
        let hex = s.strip_prefix(CURSOR_TAG).filter(|r| {
            s.len() == CURSOR_LEN && r.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
        });
        let num =
            |i: usize| hex.and_then(|r| u64::from_str_radix(&r[16 * i..16 * (i + 1)], 16).ok());
        match (num(0), num(1), num(2)) {
            (Some(at), Some(print), Some(sum)) if sum == Self::check(at, print) => {
                Ok(Self { at, print })
            }
            _ => Err(MALFORMED.into()),
        }
    }
}

impl std::fmt::Display for Cursor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let Self { at, print } = *self;
        write!(
            f,
            "{CURSOR_TAG}{at:016x}{print:016x}{:016x}",
            Self::check(at, print)
        )
    }
}

impl crate::Server {
    fn keyed_run(&self, p: &KeyedPair) -> Result<Run<'_>, String> {
        let spec: KeySpec = p.key.parse().map_err(|e| format!("key: {e}"))?;
        let mem = p
            .pair
            .mem
            .as_deref()
            .map(Bytes::parse_budget)
            .transpose()
            .map_err(|e| format!("mem: {e}"))?;
        let a = FileSource::open(&p.pair.a).map_err(|e| format!("a: {e}"))?;
        let b = FileSource::open(&p.pair.b).map_err(|e| format!("b: {e}"))?;
        let lease = self.sessions.lease(mem).map_err(|e| e.to_string())?;
        Ok(Run {
            a: a.0,
            b: b.0,
            ids: [a.1, b.1],
            spec,
            paths: [p.pair.a.clone(), p.pair.b.clone()],
            lease,
        })
    }
}

/// Text and its cut flag: `bytes` as UTF-8 text, cut at `max` bytes on a
/// character boundary when it is UTF-8.
fn cut(bytes: &[u8], max: usize) -> (String, bool) {
    if bytes.len() <= max {
        return (String::from_utf8_lossy(bytes).into_owned(), false);
    }
    // Back up over continuation bytes, so a character is not split.
    let mut end = max;
    while end > 0 && bytes[end] & 0xC0 == 0x80 {
        end -= 1;
    }
    (String::from_utf8_lossy(&bytes[..end]).into_owned(), true)
}

fn key_ref(c: &KeyedChange) -> KeyRef {
    let (key, key_cut) = cut(&c.key, MAX_VALUE_BYTES);
    let table = c.table.as_deref().map(|t| cut(t, MAX_VALUE_BYTES));
    KeyRef {
        key_truncated: key_cut || table.as_ref().is_some_and(|t| t.1),
        table: table.map(|t| t.0),
        key,
        a_line: c.a.map(|l| l.0 + 1),
        b_line: c.b.map(|l| l.0 + 1),
    }
}

fn kind(k: ChangeKind) -> &'static str {
    match k {
        ChangeKind::Added => "added",
        ChangeKind::Removed => "removed",
        ChangeKind::Changed => "changed",
    }
}

/// Bytes `s` takes between the quotes of a JSON string.
fn json_len(s: &str) -> usize {
    s.chars()
        .map(|c| match c {
            '"' | '\\' | '\n' | '\r' | '\t' | '\u{8}' | '\u{c}' => 2,
            c if c < ' ' => 6,
            c => c.len_utf8(),
        })
        .sum()
}

/// What an entry costs against [`render::MAX_TEXT_BYTES`]: its text as
/// JSON escapes it, plus a fixed weight for the entry and for each field
/// that covers the names and punctuation around the text and the structs
/// that hold it.
fn weigh(e: &Entry) -> usize {
    let opt = |s: &Option<String>| s.as_deref().map_or(0, json_len);
    let fields = e
        .fields
        .iter()
        .flatten()
        .map(|f| FIELD_WEIGHT + json_len(&f.name) + opt(&f.a) + opt(&f.b));
    ENTRY_WEIGHT
        + json_len(&e.key.key)
        + opt(&e.key.table)
        + opt(&e.a_record)
        + opt(&e.b_record)
        + fields.sum::<usize>()
}

fn entry(c: &FieldChange<'_>) -> Entry {
    let ch = &c.change.change;
    let rec = |r: Option<&[u8]>| r.map(|r| cut(r, MAX_RECORD_BYTES));
    let (a_record, b_record) = (rec(c.a_record), rec(c.b_record));
    let mut records_truncated = false;
    let mut text = |r: Option<(String, bool)>| {
        r.map(|(s, t)| {
            records_truncated |= t;
            s
        })
    };
    let (a_record, b_record) = (text(a_record), text(b_record));
    let (mut fields, mut more) = (None, 0);
    if ch.kind == ChangeKind::Changed
        && let Some(diffs) = c.fields()
    {
        let mut list = Vec::new();
        for f in diffs {
            if list.len() == MAX_FIELDS {
                more += 1;
                continue;
            }
            let (name, mut truncated) = match f.name {
                FieldName::Number(n) => (n.to_string(), false),
                FieldName::Bytes(n) => cut(n, MAX_VALUE_BYTES),
            };
            let mut value = |v: Option<&[u8]>| {
                v.map(|v| {
                    let (s, t) = cut(v, MAX_VALUE_BYTES);
                    truncated |= t;
                    s
                })
            };
            let (a, b) = (value(f.a), value(f.b));
            list.push(FieldEntry {
                name,
                a,
                b,
                truncated,
            });
        }
        fields = Some(list);
    }
    Entry {
        kind: kind(ch.kind).into(),
        key: key_ref(ch),
        a_record,
        b_record,
        records_truncated,
        fields,
        fields_more: more,
    }
}

/// Changes of one kind: how many, and the first few.
#[derive(Default)]
struct Kind {
    n: u64,
    first: Vec<KeyRef>,
}

#[derive(Default)]
struct Counts([Kind; 3]);

impl Counts {
    fn of(&mut self, k: ChangeKind) -> &mut Kind {
        &mut self.0[k as usize]
    }
}

impl crate::Server {
    /// The budget of a run, with the answer's share already taken.
    fn keyed_budget(run: &Run<'_>) -> Result<(Budget, rift_core::Reservation), String> {
        let budget = Budget::new(run.lease.mem());
        let answer = budget
            .try_reserve(Half::Refine, Bytes(ANSWER_RESERVE))
            .map_err(|e| run.describe(&Error::Budget(e)))?;
        Ok((budget, answer))
    }

    pub(crate) fn keyed_summary_now(&self, p: &KeyedPair) -> Result<KeyedSummary, String> {
        let run = self.keyed_run(p)?;
        let (budget, _answer) = Self::keyed_budget(&run)?;
        let mut counts = Counts::default();
        let report = compare(&run.a, &run.b, &run.spec, &budget, |c| {
            let k = counts.of(c.kind);
            if k.first.len() < FIRST_KEYS {
                k.first.push(key_ref(c));
            }
            k.n += 1;
            Ok(())
        })
        .map_err(|e| run.describe(&e))?;
        let [added, removed, changed] = counts.0;
        Ok(KeyedSummary {
            a_records: report.records_a,
            b_records: report.records_b,
            passes: report.passes,
            added: added.n,
            removed: removed.n,
            changed: changed.n,
            first_added: added.first,
            first_removed: removed.first,
            first_changed: changed.first,
            mem_budget_bytes: run.lease.mem().0,
        })
    }

    pub(crate) fn keyed_changes_now(&self, a: &KeyedChanges) -> Result<KeyedPage, String> {
        let limit = a.limit.unwrap_or(DEFAULT_CHANGES).clamp(1, MAX_CHANGES);
        check_fields(a.fields.as_deref())?;
        let cursor = a.cursor.as_deref().map(Cursor::parse).transpose()?;
        let run = self.keyed_run(&a.keyed)?;
        // An empty list names no field to compare; it means all of them.
        let fields = a.fields.as_deref().filter(|f| !f.is_empty());
        let print = fingerprint(&a.keyed.key, &run, fields);
        let from = match cursor {
            Some(c) if c.print != print => return Err(STALE.into()),
            Some(c) => c.at,
            None => 0,
        };
        let (budget, _answer) = Self::keyed_budget(&run)?;
        let mut counts = Counts::default();
        let (mut index, mut weight) = (0u64, 0usize);
        let mut page = Vec::with_capacity(limit as usize);
        let mut full = false;
        compare_fields(&run.a, &run.b, &run.spec, fields, &budget, |c| {
            counts.of(c.change.change.kind).n += 1;
            let at = index;
            index += 1;
            if at < from || full || page.len() as u64 == limit {
                return Ok(());
            }
            let mut e = entry(c);
            let mut w = weigh(&e);
            // One change alone can weigh more than the bound (bytes that
            // are not UTF-8 grow threefold as text): its last fields go.
            while page.is_empty() && w > render::MAX_TEXT_BYTES {
                let Some(list) = e.fields.as_mut().filter(|l| !l.is_empty()) else {
                    break;
                };
                list.pop();
                e.fields_more += 1;
                w = weigh(&e);
            }
            // An entry that would cross the bound waits for the next page,
            // unless it is the only one: then the page is that entry alone.
            if !page.is_empty() && weight + w > render::MAX_TEXT_BYTES {
                full = true;
                return Ok(());
            }
            weight += w;
            page.push(e);
            Ok(())
        })
        .map_err(|e| run.describe(&e))?;
        let end = from + page.len() as u64;
        let [added, removed, changed] = counts.0.map(|k| k.n);
        Ok(KeyedPage {
            total: index,
            added,
            removed,
            changed,
            next: (end < index).then(|| Cursor { at: end, print }.to_string()),
            changes: page,
            mem_budget_bytes: run.lease.mem().0,
        })
    }
}
