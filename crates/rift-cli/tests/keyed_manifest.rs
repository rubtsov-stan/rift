//! Key mode on `shuffled` pairs against the generator's manifest.

use rift_core::keyed::{ChangeKind, KeySpec, Report, compare};
use rift_core::{Budget, Bytes};
use rift_gen::{Config, Kind, Profile, Rows};

type Change = (ChangeKind, Vec<u8>, Option<u64>, Option<u64>);

fn generate(rows: Rows, seed: u64, ratio: f64) -> (Vec<u8>, Vec<u8>, Vec<rift_gen::Mutation>) {
    let mut config = Config::new(seed, 2 << 20, Profile::Shuffled);
    config.rows = rows;
    config.change_ratio = ratio;
    let (mut a, mut b, mut m) = (Vec::new(), Vec::new(), Vec::new());
    rift_gen::generate(&config, &mut a, &mut b, &mut m).unwrap();
    (a, b, rift_gen::parse_manifest(&m).unwrap())
}

fn lines(data: &[u8]) -> Vec<&[u8]> {
    data.split_inclusive(|&c| c == b'\n')
        .map(|l| l.strip_suffix(b"\n").unwrap_or(l))
        .collect()
}

/// The key of a `shuffled` row, as the file has it.
fn key(rows: Rows, line: &[u8]) -> Vec<u8> {
    match rows {
        Rows::Csv => line.split(|&c| c == b',').next().unwrap().to_vec(),
        Rows::Sql => line.split(|&c| c == b'\t').next().unwrap().to_vec(),
        Rows::Ndjson => {
            let rest = &line[b"{\"id\":".len()..];
            rest[..rest[1..].iter().position(|&c| c == b'"').unwrap() + 2].to_vec()
        }
    }
}

fn spec(rows: Rows) -> KeySpec {
    match rows {
        Rows::Csv => "csv:id",
        Rows::Sql => "copy:id",
        Rows::Ndjson => "json:id",
    }
    .parse()
    .unwrap()
}

/// Every change the manifest names, by key.
fn expected(rows: Rows, a: &[u8], b: &[u8], manifest: &[rift_gen::Mutation]) -> Vec<Change> {
    let (la, lb) = (lines(a), lines(b));
    let mut out = Vec::new();
    for m in manifest {
        for i in 0..m.count {
            let (ia, ib) = (m.line_a + i, m.line_b + i);
            let at = |l: &[&[u8]], n: u64| key(rows, l[usize::try_from(n).unwrap()]);
            match m.kind {
                Kind::Insert => out.push((ChangeKind::Added, at(&lb, ib), None, Some(ib))),
                Kind::Delete => out.push((ChangeKind::Removed, at(&la, ia), Some(ia), None)),
                Kind::Modify => out.push((ChangeKind::Changed, at(&la, ia), Some(ia), Some(ib))),
                Kind::Move => {}
                k => panic!("shuffled produced {k:?}"),
            }
        }
    }
    out.sort_unstable_by(|x, y| x.1.cmp(&y.1));
    out
}

fn run(rows: Rows, a: &[u8], b: &[u8], mem: Bytes) -> (Vec<Change>, Report) {
    let mut out = Vec::new();
    let report = compare(a, b, &spec(rows), &Budget::new(mem), |c| {
        out.push((c.kind, c.key.clone(), c.a.map(|l| l.0), c.b.map(|l| l.0)));
        Ok(())
    })
    .unwrap();
    out.sort_unstable_by(|x, y| x.1.cmp(&y.1));
    (out, report)
}

#[test]
fn every_change_in_the_manifest_is_reported_and_nothing_else() {
    for rows in [Rows::Csv, Rows::Sql, Rows::Ndjson] {
        let (a, b, manifest) = generate(rows, 11, 0.002);
        let want = expected(rows, &a, &b, &manifest);
        assert!(want.len() > 20, "{rows:?}: {} changes", want.len());
        assert!(manifest.iter().filter(|m| m.kind == Kind::Move).count() > 10);
        for mem in [Bytes::mib(64), Bytes::mib(1), Bytes(256 << 10)] {
            let (got, report) = run(rows, &a, &b, mem);
            assert_eq!(got, want, "{rows:?} under {mem} bytes");
            if mem < Bytes::mib(1) {
                assert!(report.passes > 1, "{rows:?}: {report:?}");
            }
        }
    }
}

#[test]
fn reordering_alone_reports_nothing() {
    for rows in [Rows::Csv, Rows::Sql, Rows::Ndjson] {
        let (a, b, manifest) = generate(rows, 5, 0.0);
        assert_ne!(a, b);
        assert!(manifest.iter().all(|m| m.kind == Kind::Move));
        let (got, report) = run(rows, &a, &b, Bytes::mib(16));
        assert_eq!(got, [], "{rows:?}");
        assert_eq!(report.passes, 1);
        assert_eq!(report.records_a, report.records_b);
    }
}
