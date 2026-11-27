//! `rift A B2 --experimental-from R` prints what `rift A B2` prints, with R
//! saved by `rift A B --save R` for an earlier B.

use std::ffi::OsStr;
use std::fs;
use std::process::{Command, Output};

use rift_gen::Profile;

fn rift(args: &[&OsStr]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_rift"))
        .args(args)
        .output()
        .unwrap()
}

#[test]
fn a_guided_run_prints_what_a_fresh_run_prints() {
    let (mut a, mut b, mut manifest) = (Vec::new(), Vec::new(), Vec::new());
    let config = rift_gen::Config::new(7, 4 << 20, Profile::Sql);
    rift_gen::generate(&config, &mut a, &mut b, &mut manifest).unwrap();
    let dir = tempfile::tempdir().unwrap();
    let at = |name: &str| dir.path().join(name);
    let (pa, pb, pb2, pr) = (at("a"), at("b"), at("b2"), at("r"));
    let mut b2 = b.clone();
    // Lines that repeat lines of A and B.
    b2.extend_from_slice(&a[..a.len() / 10]);
    let cut = b2.iter().rposition(|&c| c == b'\n').unwrap() + 1;
    b2.truncate(cut);
    fs::write(&pa, &a).unwrap();
    fs::write(&pb, &b).unwrap();
    fs::write(&pb2, &b2).unwrap();
    let (a, b, b2, saved_at) = (
        pa.as_os_str(),
        pb.as_os_str(),
        pb2.as_os_str(),
        pr.as_os_str(),
    );
    let saved = rift(&[a, b, OsStr::new("--save"), saved_at]);
    assert_eq!(
        saved.status.code(),
        Some(1),
        "{}",
        String::from_utf8_lossy(&saved.stderr)
    );

    let fresh = rift(&[a, b2]);
    let guided = rift(&[a, b2, OsStr::new("--experimental-from"), saved_at]);
    let err = String::from_utf8_lossy(&guided.stderr);
    assert_eq!(guided.status.code(), Some(1), "{err}");
    assert!(err.is_empty(), "{err}");
    assert_eq!(guided.stdout, fresh.stdout);
}
