//! The `---` and `+++` lines of a plain diff name files the way GNU diff
//! does. The expected names were recorded from `diff -u` of GNU diffutils
//! 3.10 on `x86_64` (Debian stable in a podman container started with the
//! platform `linux/amd64`; `LC_ALL=C` and `C.UTF-8` alike), cut at the tab
//! before the timestamp. The same diffutils on `aarch64` prints the é raw:
//! the architecture decides, not the locale. The timestamp is not
//! compared: rift's format matches GNU's for plain names already and the
//! times differ between runs.

use std::fs;
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_rift");

/// (file name, the `--- ` line without the timestamp), as GNU diff 3.10
/// on `x86_64` prints them. Names that no file system here may hold (`bad\xff\x01x`,
/// recorded as `"bad\377\001x"`) are covered by a unit test in rift-core.
const CASES: [(&str, &str); 6] = [
    ("sp ace", "\"sp ace\""),
    ("ta\tb", "\"ta\\tb\""),
    ("new\nline", "\"new\\nline\""),
    ("q\"uote", "\"q\\\"uote\""),
    ("back\\slash", "\"back\\\\slash\""),
    ("caf\u{e9}", "\"caf\\303\\251\""),
];

#[test]
fn names_are_printed_as_gnu_diff_prints_them() {
    for (name, expected) in CASES {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join(name), "a\nb\nc\n").unwrap();
        fs::write(dir.path().join("B"), "a\nX\nc\n").unwrap();
        let out = Command::new(BIN)
            .current_dir(dir.path())
            .args([name, "B"])
            .output()
            .unwrap();
        let text = out.stdout;
        let first = text.split(|b| *b == b'\n').next().unwrap();
        // The timestamp follows the first tab that is not escaped.
        let cut = first.iter().position(|b| *b == b'\t').unwrap();
        assert_eq!(
            String::from_utf8_lossy(&first[..cut]),
            format!("--- {expected}"),
            "{name:?}"
        );
        assert!(text.windows(5).any(|w| w == b"\n+++ "), "{name:?}");
    }
}
