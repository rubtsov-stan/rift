//! Key mode: records compared by a key column, independent of row order.
//!
//! Only the types are here so far; the comparison itself is not written.

use std::fmt;
use std::str::FromStr;

use crate::types::LineNo;

/// Where the key of a record is.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum KeySpec {
    /// A column of a CSV file (RFC 4180).
    Csv(CsvColumn),
    /// A field of an NDJSON record: a top-level name, or a dotted path
    /// such as `user.id`, one segment per element.
    Ndjson(Vec<String>),
    /// A column of the `COPY ... FROM stdin` blocks of a `pg_dump`, by the
    /// name in the `COPY` header.
    Copy(String),
}

/// A CSV column.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum CsvColumn {
    /// Zero-based position.
    Index(u32),
    /// Name in the header row.
    Header(String),
}

/// A `--key` value that does not name a key.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ParseKeySpecError {
    /// The value has no `kind:` prefix, or an unknown one.
    #[error("key {0:?} must start with csv:, json: or copy:")]
    Kind(String),
    /// The part after the prefix is empty, or a path has an empty segment.
    #[error("key {0:?} names no column or field")]
    Empty(String),
    /// A CSV column number is 0 or does not fit.
    #[error("key {0:?}: csv columns are numbered from 1")]
    Column(String),
}

impl FromStr for KeySpec {
    type Err = ParseKeySpecError;

    /// Parses `csv:N` (column N, counted from 1), `csv:NAME` (header
    /// name), `json:PATH` (dotted field path) or `copy:NAME`.
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let (kind, rest) = s
            .split_once(':')
            .ok_or_else(|| ParseKeySpecError::Kind(s.into()))?;
        if rest.is_empty() {
            return Err(ParseKeySpecError::Empty(s.into()));
        }
        match kind {
            "csv" if rest.bytes().all(|c| c.is_ascii_digit()) => match rest.parse::<u32>() {
                Ok(n) if n > 0 => Ok(Self::Csv(CsvColumn::Index(n - 1))),
                _ => Err(ParseKeySpecError::Column(s.into())),
            },
            "csv" => Ok(Self::Csv(CsvColumn::Header(rest.into()))),
            "json" => {
                let path: Vec<String> = rest.split('.').map(Into::into).collect();
                if path.iter().any(String::is_empty) {
                    return Err(ParseKeySpecError::Empty(s.into()));
                }
                Ok(Self::Ndjson(path))
            }
            "copy" => Ok(Self::Copy(rest.into())),
            _ => Err(ParseKeySpecError::Kind(s.into())),
        }
    }
}

/// What happened to a key.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ChangeKind {
    /// In B only.
    Added,
    /// In A only.
    Removed,
    /// In both, with different record bytes.
    Changed,
}

/// One key whose record differs between A and B.
///
/// `a` is set unless the kind is [`ChangeKind::Added`], `b` unless it is
/// [`ChangeKind::Removed`].
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct KeyedChange {
    /// The key's bytes as they appear in the file, quotes included.
    pub key: Vec<u8>,
    /// Added, removed or changed.
    pub kind: ChangeKind,
    /// The record's first line in A.
    pub a: Option<LineNo>,
    /// The record's first line in B.
    pub b: Option<LineNo>,
}

/// Why key mode could not compare two files.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum KeyError {
    /// A key occurs more than once in one file.
    #[error("duplicate keys: {}", Keys(.0))]
    Duplicate(Vec<Vec<u8>>),
}

/// The first few keys of a list, for an error message.
struct Keys<'k>(&'k [Vec<u8>]);

impl fmt::Display for Keys<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (i, key) in self.0.iter().enumerate() {
            if i > 0 {
                f.write_str(", ")?;
            }
            write!(f, "{}", String::from_utf8_lossy(key))?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(s: &str) -> Result<KeySpec, ParseKeySpecError> {
        s.parse()
    }

    #[test]
    fn every_kind_of_key_parses() {
        assert_eq!(parse("csv:1"), Ok(KeySpec::Csv(CsvColumn::Index(0))));
        assert_eq!(
            parse("csv:id"),
            Ok(KeySpec::Csv(CsvColumn::Header("id".into())))
        );
        assert_eq!(
            parse("json:user.id"),
            Ok(KeySpec::Ndjson(vec!["user".into(), "id".into()]))
        );
        assert_eq!(
            parse("copy:rental_id"),
            Ok(KeySpec::Copy("rental_id".into()))
        );
    }

    #[test]
    fn malformed_keys_are_refused() {
        assert!(matches!(parse("id"), Err(ParseKeySpecError::Kind(_))));
        assert!(matches!(parse("xml:id"), Err(ParseKeySpecError::Kind(_))));
        assert!(matches!(parse("csv:"), Err(ParseKeySpecError::Empty(_))));
        assert!(matches!(
            parse("json:a..b"),
            Err(ParseKeySpecError::Empty(_))
        ));
        assert!(matches!(parse("csv:0"), Err(ParseKeySpecError::Column(_))));
        assert!(matches!(
            parse("csv:99999999999"),
            Err(ParseKeySpecError::Column(_))
        ));
    }

    #[test]
    fn duplicate_keys_are_listed() {
        let err = KeyError::Duplicate(vec![b"7".to_vec(), b"9".to_vec()]);
        assert_eq!(err.to_string(), "duplicate keys: 7, 9");
    }
}
