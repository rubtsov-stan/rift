//! Operands that are not regular files: Rift reads by offset, so a pipe,
//! a FIFO or a directory is refused with the path and a way forward, never
//! diffed as empty.

use std::fs;
use std::os::unix::net::UnixListener;
use std::path::Path;
use std::process::{Command, Output, Stdio};

const BIN: &str = env!("CARGO_BIN_EXE_rift");

/// Runs `rift`, killing it after 20 s: a diff that blocks on a FIFO is a
/// failure, not a hung test run.
fn finish(mut cmd: Command) -> Output {
    let child = cmd
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let pid = child.id();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || drop(tx.send(child.wait_with_output())));
    if let Ok(out) = rx.recv_timeout(std::time::Duration::from_secs(20)) {
        return out.unwrap();
    }
    let _ = Command::new("kill").args(["-9", &pid.to_string()]).status();
    panic!("rift did not finish within 20 s");
}

fn rift(args: &[&Path]) -> Output {
    let mut cmd = Command::new(BIN);
    cmd.args(args);
    finish(cmd)
}

fn mkfifo(path: &Path) {
    assert!(Command::new("mkfifo").arg(path).status().unwrap().success());
}

fn refused(out: &Output, path: &Path) {
    let err = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(2), "stderr: {err}");
    assert_eq!(out.stdout, b"");
    assert!(err.contains(&path.display().to_string()), "{err}");
    assert!(err.contains("random access"), "{err}");
    assert!(err.contains("write the input to a file first"), "{err}");
}

#[test]
fn a_fifo_is_refused_without_blocking() {
    let dir = tempfile::tempdir().unwrap();
    let (fifo, b) = (dir.path().join("fifo"), dir.path().join("b"));
    mkfifo(&fifo);
    fs::write(&b, "x\n").unwrap();
    refused(&rift(&[&fifo, &b]), &fifo);
    refused(&rift(&[&b, &fifo]), &fifo);
}

#[test]
fn a_pipe_from_process_substitution_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let b = dir.path().join("b");
    fs::write(&b, "x\n").unwrap();
    let mut cmd = Command::new("bash");
    cmd.arg("-c")
        .arg(format!("{BIN} <(sleep .2; echo one) {}", b.display()));
    let out = finish(cmd);
    let err = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(2), "stderr: {err}");
    assert_eq!(out.stdout, b"");
    assert!(err.contains("/dev/fd/"), "{err}");
    assert!(err.contains("write the input to a file first"), "{err}");
}

#[test]
fn a_directory_names_its_path() {
    let dir = tempfile::tempdir().unwrap();
    let b = dir.path().join("b");
    fs::write(&b, "x\n").unwrap();
    refused(&rift(&[dir.path(), &b]), dir.path());
}

#[test]
fn a_socket_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let (sock, b) = (dir.path().join("s"), dir.path().join("b"));
    let _listener = UnixListener::bind(&sock).unwrap();
    fs::write(&b, "x\n").unwrap();
    // The kernel refuses to open a socket, so there is no advice to add.
    let out = rift(&[&sock, &b]);
    let err = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(2), "{err}");
    assert!(err.contains(&sock.display().to_string()), "{err}");
}

#[test]
fn a_symlink_to_a_file_is_followed() {
    let dir = tempfile::tempdir().unwrap();
    let (a, link, b) = (
        dir.path().join("a"),
        dir.path().join("link"),
        dir.path().join("b"),
    );
    fs::write(&a, "x\n").unwrap();
    fs::write(&b, "x\n").unwrap();
    std::os::unix::fs::symlink(&a, &link).unwrap();
    assert_eq!(rift(&[&link, &b]).status.code(), Some(0));
}

#[test]
fn dev_null_is_an_empty_file() {
    let dir = tempfile::tempdir().unwrap();
    let b = dir.path().join("b");
    fs::write(&b, "x\n").unwrap();
    assert_eq!(rift(&[Path::new("/dev/null"), &b]).status.code(), Some(1));
}

/// An operand of 120 KB (a Linux argument stops at 128 KiB) that names no file: the error names its start and
/// end, not all of it, because the command line is the one thing the
/// budget does not hold and a message must not copy it two or three times.
#[test]
fn a_very_long_operand_is_named_by_its_ends() {
    let long = format!("{}end", "d/".repeat(60_000));
    let ok = tempfile::NamedTempFile::new().unwrap();
    let ok = ok.path().to_str().unwrap();
    for args in [[long.as_str(), ok], [ok, long.as_str()]] {
        let mut cmd = Command::new(BIN);
        cmd.args(["--mem", "64K"]).args(args);
        let out = finish(cmd);
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert_eq!(out.status.code(), Some(2), "{args:.1?}");
        assert!(stderr.len() < 1024, "{} bytes of error", stderr.len());
        assert!(stderr.contains("d/d/"), "{stderr}");
        assert!(stderr.contains("end"), "{stderr}");
    }
}
