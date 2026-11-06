//! Records and their keys, read from a [`Source`] through one buffer.
//!
//! A record is one line, except in CSV, where a quoted field can hold line
//! breaks. Its bytes leave out the line terminator (`\n` or `\r\n`), so a
//! change of line ending alone is not a change of the record. Blank lines
//! are not records, and in a `pg_dump` only the rows of `COPY` blocks that
//! have the key column are.

use std::io;
use std::ops::Range;

use memchr::{memchr, memchr2, memchr3};
use xxhash_rust::xxh3::{xxh3_64, xxh3_64_with_seed};

use super::{CsvColumn, KeyError, KeySpec, Side};
use crate::budget::{Budget, Half, Reservation};
use crate::error::{Error, Result};
use crate::source::Source;
use crate::types::{ByteOffset, Bytes, LineNo};

/// One record in the reader's buffer.
#[derive(Debug)]
pub(crate) struct Record<'b> {
    /// The record without its line terminator.
    pub bytes: &'b [u8],
    /// Where the key is in `bytes`.
    pub key: Range<usize>,
    /// The record's first line.
    pub line: u64,
    /// The `COPY` block's table, 0 outside `pg_dump` files.
    pub table: u32,
    /// The key hash, see [`key_hash`].
    pub hash: u64,
}

/// A 63-bit key hash that is never 0, so a table slot can use 0 for empty
/// and the top bit for its own flag. Keys of different `COPY` tables hash
/// apart through the table's seed.
pub(crate) fn key_hash(key: &[u8], seed: u64) -> u64 {
    (xxh3_64_with_seed(key, seed) >> 1) | 1
}

/// Names of the `COPY` tables seen so far, shared by both files so a name
/// gets the same id in each.
#[derive(Debug)]
pub(crate) struct Tables {
    names: Vec<(Box<[u8]>, u64)>,
    /// Refine half: the names and the list.
    res: Reservation,
}

impl Tables {
    pub(crate) fn new(budget: &Budget) -> Result<Self> {
        Ok(Self {
            names: Vec::new(),
            res: budget.try_reserve(Half::Refine, Bytes(0))?,
        })
    }

    /// The id and hash seed of a table name, added if new.
    fn id(&mut self, name: &[u8]) -> Result<(u32, u64)> {
        if let Some(i) = self.names.iter().position(|(n, _)| **n == *name) {
            return Ok((id(i), self.names[i].1));
        }
        // The list doubles, so two slots per name cover its capacity.
        let slot = size_of::<(Box<[u8]>, u64)>() as u64;
        self.res.grow(Bytes(name.len() as u64 + 2 * slot))?;
        self.names.push((name.into(), xxh3_64(name)));
        let last = self.names.len() - 1;
        Ok((id(last), self.names[last].1))
    }
}

#[allow(
    clippy::cast_possible_truncation,
    reason = "a dump has far fewer than 2^32 tables"
)]
fn id(i: usize) -> u32 {
    i as u32 + 1
}

/// What the reader knows about the file's layout.
#[derive(Debug)]
enum Format {
    /// CSV; `col` is `None` until the header names the column.
    Csv {
        col: Option<u32>,
        name: Option<Box<str>>,
    },
    /// NDJSON with a field path.
    Ndjson(Box<[Box<str>]>),
    /// `pg_dump` text, keyed by a `COPY` column.
    Copy { name: Box<str>, block: Block },
}

/// Where a `pg_dump` reader is.
#[derive(Debug, Clone, Copy)]
enum Block {
    /// Outside any `COPY` block.
    Outside,
    /// In a block; its rows are records when the block has the column.
    Rows(Option<(u32, u32, u64)>),
}

/// A record split off the front of the buffer, as offsets into it.
#[derive(Debug, PartialEq, Eq)]
struct Split {
    /// Record length without the terminator.
    len: usize,
    /// Bytes consumed, terminator included.
    next: usize,
    /// Line breaks consumed.
    lines: u64,
    /// The key column, if the record has it (CSV only).
    key: Option<Range<usize>>,
}

/// Reads one file's records, front to back, any number of times.
#[derive(Debug)]
pub(crate) struct Reader<'s, S: ?Sized> {
    src: &'s S,
    side: Side,
    format: Format,
    buf: Vec<u8>,
    /// Refine half: the buffer.
    res: Reservation,
    start: usize,
    end: usize,
    /// File offset of `buf[end]`.
    pos: u64,
    /// Line of `buf[start]`.
    line: u64,
    eof: bool,
    /// `COPY` blocks seen with the key column.
    blocks: u64,
}

impl<'s, S: Source + ?Sized> Reader<'s, S> {
    /// A reader with a `buf`-byte buffer from the refine half.
    pub(crate) fn new(
        src: &'s S,
        side: Side,
        spec: &KeySpec,
        budget: &Budget,
        buf: usize,
    ) -> Result<Self> {
        let res = budget.try_reserve(Half::Refine, Bytes(buf as u64))?;
        let format = match spec {
            KeySpec::Csv(CsvColumn::Index(i)) => Format::Csv {
                col: Some(*i),
                name: None,
            },
            KeySpec::Csv(CsvColumn::Header(name)) => Format::Csv {
                col: None,
                name: Some(name.as_str().into()),
            },
            KeySpec::Ndjson(path) => {
                Format::Ndjson(path.iter().map(|s| s.as_str().into()).collect())
            }
            KeySpec::Copy(name) => Format::Copy {
                name: name.as_str().into(),
                block: Block::Outside,
            },
        };
        Ok(Self {
            src,
            side,
            format,
            buf: vec![0; buf],
            res,
            start: 0,
            end: 0,
            pos: 0,
            line: 0,
            eof: false,
            blocks: 0,
        })
    }

    /// Which file this is.
    pub(crate) fn side(&self) -> Side {
        self.side
    }

    /// Goes back to the start of the file.
    pub(crate) fn rewind(&mut self) {
        self.start = 0;
        self.end = 0;
        self.pos = 0;
        self.line = 0;
        self.eof = false;
        self.blocks = 0;
        if let Format::Copy { block, .. } = &mut self.format {
            *block = Block::Outside;
        }
    }

    /// Lines in the file, extrapolated from the first buffer. Reads it, so
    /// call before the first [`next`](Self::next).
    pub(crate) fn estimate_lines(&mut self) -> Result<u64> {
        self.refill()?;
        let lines = memchr::memchr_iter(b'\n', &self.buf[..self.end]).count() as u64 + 1;
        let read = (self.end as u64).max(1);
        Ok(
            u64::try_from(u128::from(self.src.len().0) * u128::from(lines) / u128::from(read))
                .unwrap_or(u64::MAX)
                .max(lines),
        )
    }

    /// Bytes of the file read and split into records so far.
    pub(crate) fn consumed(&self) -> u64 {
        self.pos - (self.end - self.start) as u64
    }

    /// `COPY` blocks with the key column since the last rewind.
    pub(crate) fn blocks(&self) -> u64 {
        self.blocks
    }

    /// The next record, or `None` at the end of the file.
    ///
    /// # Errors
    ///
    /// A record without the key, a CSV header without the named column, a
    /// record that does not fit in the buffer, or a read error.
    pub(crate) fn next(&mut self, tables: &mut Tables) -> Result<Option<Record<'_>>> {
        let (at, len, key, line, table, seed) = loop {
            if self.start == self.end && self.eof {
                return Ok(None);
            }
            let avail = &self.buf[self.start..self.end];
            let split = match &self.format {
                Format::Csv { col, .. } => csv_record(avail, col.unwrap_or(u32::MAX), self.eof),
                _ => line_record(avail, self.eof),
            };
            let Some(split) = split else {
                self.refill()?;
                continue;
            };
            let at = self.start;
            let line = self.line;
            self.start += split.next;
            self.line += split.lines;
            let rec = &self.buf[at..at + split.len];
            if rec.is_empty() {
                continue;
            }
            let no_key = || {
                Error::Keyed(KeyError::NoKey {
                    side: self.side,
                    line: LineNo(line),
                })
            };
            match &mut self.format {
                Format::Csv { col: Some(_), .. } => {
                    let key = split.key.ok_or_else(no_key)?;
                    break (at, split.len, key, line, 0, 0);
                }
                Format::Csv { col, name } => {
                    let name = name.as_deref().unwrap_or_default();
                    let found = (0..u32::MAX)
                        .map_while(|i| csv_field(rec, i).map(|r| (i, r)))
                        .find(|(_, r)| unquoted_eq(&rec[r.clone()], name.as_bytes()));
                    let Some((i, key)) = found else {
                        return Err(Error::Keyed(KeyError::NoColumn {
                            side: self.side,
                            name: name.into(),
                        }));
                    };
                    *col = Some(i);
                    break (at, split.len, key, line, 0, 0);
                }
                Format::Ndjson(path) => {
                    let key = json_key(rec, path).ok_or_else(no_key)?;
                    break (at, split.len, key, line, 0, 0);
                }
                Format::Copy { name, block } => match *block {
                    Block::Outside => {
                        if let Some((table, cols)) = copy_header(rec) {
                            let cols = copy_column(cols, name.as_bytes());
                            *block = Block::Rows(match cols {
                                Some(col) => {
                                    let (id, seed) = tables.id(table)?;
                                    self.blocks += 1;
                                    Some((col, id, seed))
                                }
                                None => None,
                            });
                        }
                    }
                    Block::Rows(_) if rec == b"\\." => *block = Block::Outside,
                    Block::Rows(None) => {}
                    Block::Rows(Some((col, table, seed))) => {
                        let key = tab_field(rec, col).ok_or_else(no_key)?;
                        break (at, split.len, key, line, table, seed);
                    }
                },
            }
        };
        let bytes = &self.buf[at..at + len];
        Ok(Some(Record {
            hash: key_hash(&bytes[key.clone()], seed),
            bytes,
            key,
            line,
            table,
        }))
    }

    /// Moves the unread bytes to the front and reads more, growing the
    /// buffer when one record fills it.
    fn refill(&mut self) -> Result<()> {
        if self.start > 0 {
            self.buf.copy_within(self.start..self.end, 0);
            self.end -= self.start;
            self.start = 0;
        }
        if self.end == self.buf.len() {
            let more = self.buf.len();
            if self.res.grow(Bytes(more as u64)).is_err()
                || self.buf.try_reserve_exact(more).is_err()
            {
                return Err(Error::LineTooLong {
                    line: LineNo(self.line),
                    cap: Bytes(self.buf.len() as u64),
                });
            }
            self.buf.resize(self.buf.len() + more, 0);
        }
        loop {
            match self
                .src
                .read_at(&mut self.buf[self.end..], ByteOffset(self.pos))
            {
                Ok(0) => self.eof = true,
                Ok(n) => {
                    self.end += n;
                    self.pos += n as u64;
                }
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(e) => return Err(e.into()),
            }
            return Ok(());
        }
    }
}

/// The line at the front of `buf`, or `None` if it may go on past it.
fn line_record(buf: &[u8], eof: bool) -> Option<Split> {
    let (end, next) = match memchr(b'\n', buf) {
        Some(nl) => (nl, nl + 1),
        None if eof => (buf.len(), buf.len()),
        None => return None,
    };
    Some(Split {
        len: trim_cr(buf, end),
        next,
        lines: 1,
        key: None,
    })
}

fn trim_cr(buf: &[u8], end: usize) -> usize {
    if end > 0 && buf[end - 1] == b'\r' {
        end - 1
    } else {
        end
    }
}

/// The CSV record at the front of `buf` (RFC 4180) and its field `col`, or
/// `None` if the record may go on past `buf`.
///
/// Quote parity decides what a comma or line break means: inside quotes
/// it is data. A doubled quote flips the parity twice, so it needs no case
/// of its own, and a stray quote in an unquoted field is read the same
/// lenient way.
fn csv_record(buf: &[u8], col: u32, eof: bool) -> Option<Split> {
    let (mut field, mut field_start, mut quoted) = (0u32, 0, false);
    let (mut key, mut lines, mut i) = (None, 0, 0);
    let end = loop {
        let hit = if key.is_some() {
            memchr2(b'"', b'\n', &buf[i..])
        } else {
            memchr3(b',', b'"', b'\n', &buf[i..])
        };
        let Some(j) = hit.map(|off| i + off) else {
            if !eof {
                return None;
            }
            break buf.len();
        };
        i = j + 1;
        match buf[j] {
            b'"' => quoted = !quoted,
            b',' if !quoted => {
                if field == col {
                    key = Some(field_start..j);
                }
                field += 1;
                field_start = j + 1;
            }
            b'\n' if !quoted => break j,
            b'\n' => lines += 1,
            _ => {}
        }
    };
    let len = trim_cr(buf, end);
    if key.is_none() && field == col {
        key = Some(field_start..len.max(field_start));
    }
    Some(Split {
        len,
        next: (end + 1).min(buf.len()),
        lines: lines + 1,
        key,
    })
}

/// Field `col` of a whole CSV record.
fn csv_field(rec: &[u8], col: u32) -> Option<Range<usize>> {
    csv_record(rec, col, true).and_then(|s| s.key)
}

/// Whether a CSV field, without its quotes, is `name`.
fn unquoted_eq(field: &[u8], name: &[u8]) -> bool {
    let Some(inner) = field
        .strip_prefix(b"\"")
        .and_then(|f| f.strip_suffix(b"\""))
    else {
        return field == name;
    };
    let mut rest = name;
    let mut i = 0;
    while i < inner.len() {
        let c = inner[i];
        i += if c == b'"' && inner.get(i + 1) == Some(&b'"') {
            2
        } else {
            1
        };
        match rest.split_first() {
            Some((&n, tail)) if n == c => rest = tail,
            _ => return false,
        }
    }
    rest.is_empty()
}

/// Field `col` of a tab-separated `COPY` row.
fn tab_field(rec: &[u8], col: u32) -> Option<Range<usize>> {
    let mut start = 0;
    for _ in 0..col {
        start += memchr(b'\t', &rec[start..])? + 1;
    }
    let end = memchr(b'\t', &rec[start..]).map_or(rec.len(), |e| start + e);
    Some(start..end)
}

/// The table name and the column list of a `COPY name (cols) FROM stdin;`
/// line.
fn copy_header(line: &[u8]) -> Option<(&[u8], &[u8])> {
    let rest = line.strip_prefix(b"COPY ")?.strip_suffix(b" FROM stdin;")?;
    let open = memchr(b'(', rest)?;
    let cols = rest[open + 1..].strip_suffix(b")")?;
    Some((rest[..open].trim_ascii_end(), cols))
}

/// The position of `name` in a `COPY` column list, quoted or not.
fn copy_column(cols: &[u8], name: &[u8]) -> Option<u32> {
    let pos = cols
        .split(|&c| c == b',')
        .position(|c| unquoted_eq(c.trim_ascii(), name))?;
    u32::try_from(pos).ok()
}

/// The value of the field at `path` in a JSON object, raw.
fn json_key(line: &[u8], path: &[Box<str>]) -> Option<Range<usize>> {
    let mut at = skip_ws(line, 0);
    let mut found = None;
    for name in path {
        let r = json_field(line, at, name.as_bytes())?;
        at = r.start;
        found = Some(r);
    }
    found
}

fn skip_ws(s: &[u8], mut i: usize) -> usize {
    while s.get(i).is_some_and(u8::is_ascii_whitespace) {
        i += 1;
    }
    i
}

/// The value of member `name` of the object starting at `s[i]`.
fn json_field(s: &[u8], i: usize, name: &[u8]) -> Option<Range<usize>> {
    if s.get(i) != Some(&b'{') {
        return None;
    }
    let mut i = skip_ws(s, i + 1);
    loop {
        if s.get(i) != Some(&b'"') {
            return None;
        }
        let key_end = string_end(s, i)?;
        let is_name = &s[i + 1..key_end - 1] == name;
        i = skip_ws(s, key_end);
        if s.get(i) != Some(&b':') {
            return None;
        }
        let start = skip_ws(s, i + 1);
        let end = value_end(s, start)?;
        if is_name {
            return Some(start..end);
        }
        i = skip_ws(s, end);
        match s.get(i) {
            Some(b',') => i = skip_ws(s, i + 1),
            _ => return None,
        }
    }
}

/// One past the closing quote of the string starting at `s[i]`.
fn string_end(s: &[u8], mut i: usize) -> Option<usize> {
    i += 1;
    loop {
        i += memchr2(b'"', b'\\', s.get(i..)?)?;
        if s[i] == b'"' {
            return Some(i + 1);
        }
        i += 2;
    }
}

/// One past the JSON value starting at `s[i]`.
fn value_end(s: &[u8], mut i: usize) -> Option<usize> {
    match *s.get(i)? {
        b'"' => string_end(s, i),
        b'{' | b'[' => {
            let mut depth = 0usize;
            loop {
                match *s.get(i)? {
                    b'"' => {
                        i = string_end(s, i)?;
                        continue;
                    }
                    b'{' | b'[' => depth += 1,
                    b'}' | b']' => {
                        depth -= 1;
                        if depth == 0 {
                            return Some(i + 1);
                        }
                    }
                    _ => {}
                }
                i += 1;
            }
        }
        _ => {
            let start = i;
            while s
                .get(i)
                .is_some_and(|c| !matches!(c, b',' | b'}' | b']') && !c.is_ascii_whitespace())
            {
                i += 1;
            }
            (i > start).then_some(i)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn csv(buf: &[u8], col: u32) -> Option<(usize, usize, u64, Option<&[u8]>)> {
        csv_record(buf, col, false).map(|s| (s.len, s.next, s.lines, s.key.map(|k| &buf[k])))
    }

    #[test]
    fn csv_records_respect_quotes() {
        assert_eq!(csv(b"1,a,b\n2", 0), Some((5, 6, 1, Some(&b"1"[..]))));
        assert_eq!(csv(b"1,a,b\n", 2), Some((5, 6, 1, Some(&b"b"[..]))));
        assert_eq!(csv(b"1,a,b\r\n", 2), Some((5, 7, 1, Some(&b"b"[..]))));
        assert_eq!(csv(b"1,a\n", 2), Some((3, 4, 1, None)));
        assert_eq!(
            csv(b"\"x,\ny\",2\nz", 0),
            Some((8, 9, 2, Some(&b"\"x,\ny\""[..])))
        );
        assert_eq!(
            csv(b"\"a\"\"b\",c\n", 0),
            Some((8, 9, 1, Some(&b"\"a\"\"b\""[..])))
        );
        assert_eq!(csv(b"\"open,\n", 0), None, "quote still open");
        assert_eq!(csv(b"1,2", 1), None, "no terminator yet");
        let last = csv_record(b"1,2", 1, true).unwrap();
        assert_eq!((last.len, last.next, last.key), (3, 3, Some(2..3)));
        let empty = csv_record(b"\n", 0, false).unwrap();
        assert_eq!((empty.len, empty.next), (0, 1));
    }

    #[test]
    fn header_names_compare_without_quotes() {
        assert!(unquoted_eq(b"id", b"id"));
        assert!(unquoted_eq(b"\"id\"", b"id"));
        assert!(unquoted_eq(b"\"a\"\"b\"", b"a\"b"));
        assert!(!unquoted_eq(b"\"idx\"", b"id"));
        assert!(!unquoted_eq(b"\"i\"", b"id"));
    }

    #[test]
    fn json_paths_find_raw_values() {
        let path = |p: &str| p.split('.').map(Into::into).collect::<Vec<Box<str>>>();
        let line = br#"{"a":{"x":[1,{"id":0}],"id":"7\"q"}, "id" : 12 ,"z":"}"}"#;
        let get = |p: &str| json_key(line, &path(p)).map(|r| &line[r]);
        assert_eq!(get("id"), Some(&b"12"[..]));
        assert_eq!(get("a.id"), Some(&br#""7\"q""#[..]));
        assert_eq!(get("z"), Some(&br#""}""#[..]));
        assert_eq!(get("a.x"), Some(&br#"[1,{"id":0}]"#[..]));
        assert_eq!(get("missing"), None);
        assert_eq!(get("id.deeper"), None);
        assert_eq!(json_key(b"[1]", &path("id")), None);
        assert_eq!(json_key(br#"{"id":"#, &path("id")), None);
    }

    #[test]
    fn copy_headers_and_rows() {
        let (table, cols) =
            copy_header(b"COPY public.orders (id, \"Name\", note) FROM stdin;").unwrap();
        assert_eq!(table, b"public.orders");
        assert_eq!(copy_column(cols, b"Name"), Some(1));
        assert_eq!(copy_column(cols, b"note"), Some(2));
        assert_eq!(copy_column(cols, b"other"), None);
        assert_eq!(copy_header(b"COPY t FROM stdin;"), None);
        assert_eq!(copy_header(b"SELECT 1;"), None);
        let row = b"7\tx\t\\N";
        assert_eq!(tab_field(row, 0).map(|r| &row[r]), Some(&b"7"[..]));
        assert_eq!(tab_field(row, 2).map(|r| &row[r]), Some(&b"\\N"[..]));
        assert_eq!(tab_field(row, 3), None);
    }

    fn records(spec: &str, data: &[u8], buf: usize) -> Result<Vec<(u64, String, u32)>> {
        let budget = Budget::new(Bytes::mib(1));
        let mut tables = Tables::new(&budget)?;
        let spec: KeySpec = spec.parse().unwrap();
        let mut r = Reader::new(data, Side::A, &spec, &budget, buf)?;
        let mut out = Vec::new();
        for _ in 0..2 {
            out.clear();
            r.rewind();
            while let Some(rec) = r.next(&mut tables)? {
                let key = String::from_utf8_lossy(&rec.bytes[rec.key.clone()]).into_owned();
                out.push((rec.line, key, rec.table));
            }
            assert_eq!(r.consumed(), data.len() as u64);
        }
        Ok(out)
    }

    #[test]
    fn readers_number_records_by_their_first_line() {
        let data = b"id,v\n1,\"two\nlines\"\n\n2,x\r\n3,y";
        let want = |k: &[&str]| -> Vec<(u64, String, u32)> {
            [0, 1, 4, 5]
                .iter()
                .zip(k)
                .map(|(&l, k)| (l, (*k).into(), 0))
                .collect()
        };
        for buf in [4, 64] {
            assert_eq!(
                records("csv:1", data, buf).unwrap(),
                want(&["id", "1", "2", "3"])
            );
            assert_eq!(
                records("csv:v", data, buf).unwrap(),
                want(&["v", "\"two\nlines\"", "x", "y"])
            );
        }
        assert!(matches!(
            records("csv:w", data, 64),
            Err(Error::Keyed(KeyError::NoColumn { side: Side::A, .. }))
        ));
        assert!(matches!(
            records("csv:3", data, 64),
            Err(Error::Keyed(KeyError::NoKey {
                line: LineNo(0),
                ..
            }))
        ));
    }

    #[test]
    fn copy_rows_outside_keyed_blocks_are_skipped() {
        let dump = b"SET x;\nCOPY a (id, v) FROM stdin;\n1\tx\n\\.\nCOPY b (v) FROM stdin;\n9\n\\.\nCOPY c (v, id) FROM stdin;\ny\t1\n\\.\n";
        let got = records("copy:id", dump, 16).unwrap();
        assert_eq!(got, [(2, "1".into(), 1), (8, "1".into(), 2)]);
    }

    #[test]
    fn ndjson_without_the_field_is_an_error() {
        let data = b"{\"id\":1}\n{\"x\":2}\n";
        assert!(matches!(
            records("json:id", data, 64),
            Err(Error::Keyed(KeyError::NoKey {
                line: LineNo(1),
                ..
            }))
        ));
    }
}
