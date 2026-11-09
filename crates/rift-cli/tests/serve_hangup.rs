//! `rift serve` when its client goes away in the middle of a response: it
//! must exit on its own, without a panic, as it does when stdin closes.

use std::fmt::Write as _;
use std::io::{BufRead, BufReader, Read, Write};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

/// Lines of 4 KiB in a segment that differs whole: a `hunk` answer of
/// about 40 MB, far more than the pipe and the server's buffers hold.
const LINES: usize = 5000;

fn send(stdin: &mut impl Write, id: u64, method: &str, params: &Value) {
    let body = json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params});
    let body = serde_json::to_vec(&body).unwrap();
    write!(stdin, "Content-Length: {}\r\n\r\n", body.len()).unwrap();
    stdin.write_all(&body).unwrap();
    stdin.flush().unwrap();
}

/// The length from a frame's header, the cursor left at its body.
fn header(out: &mut impl BufRead) -> usize {
    let mut len = 0;
    loop {
        let mut line = String::new();
        assert_ne!(out.read_line(&mut line).unwrap(), 0, "server closed early");
        let line = line.trim_end();
        if line.is_empty() {
            return len;
        }
        if let Some(n) = line.strip_prefix("Content-Length:") {
            len = n.trim().parse().unwrap();
        }
    }
}

fn frame(out: &mut impl BufRead) -> Value {
    let mut body = vec![0; header(out)];
    out.read_exact(&mut body).unwrap();
    serde_json::from_slice(&body).unwrap()
}

#[test]
fn a_client_that_leaves_mid_response_ends_the_server_without_a_panic() {
    let dir = tempfile::tempdir().unwrap();
    let file = |name: &str, fill: char| {
        let path = dir.path().join(name);
        let line = fill.to_string().repeat(4000);
        let mut text = String::new();
        for i in 0..LINES {
            writeln!(text, "{line}{i}").unwrap();
        }
        std::fs::write(&path, text).unwrap();
        path.to_str().unwrap().to_owned()
    };
    let (a, b) = (file("a", 'a'), file("b", 'b'));
    let Server(child) = &mut serve();
    let mut stdin = child.stdin.take().unwrap();
    let mut stdout = BufReader::new(child.stdout.take().unwrap());

    send(
        &mut stdin,
        1,
        "initialize",
        &json!({"protocol": 1, "client": {"name": "test"}}),
    );
    while frame(&mut stdout)["id"] != 1 {}
    send(
        &mut stdin,
        2,
        "open",
        &json!({"a": a, "b": b, "mem": "64M"}),
    );
    let mut session = None;
    loop {
        let m = frame(&mut stdout);
        if m["id"] == 2 {
            session = m["result"]["session"].as_u64();
        }
        if m["method"] == "session_state" && m["params"]["state"] == "ready" {
            break;
        }
    }
    let session = session.expect("open answered before the session was ready");
    send(
        &mut stdin,
        3,
        "hunk",
        &json!({"session": session, "segment": 0, "context": 0,
                "max_lines": LINES, "max_line_bytes": 65536}),
    );
    // The answer has begun; leave before it ends.
    header(&mut stdout);
    drop(stdout);
    drop(stdin);

    exits_cleanly(child);
}

/// `rift serve`, killed and reaped when dropped, so a test that fails
/// leaves no server behind.
struct Server(Child);

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn serve() -> Server {
    let child = Command::new(env!("CARGO_BIN_EXE_rift"))
        .arg("serve")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    Server(child)
}

/// Waits a bounded time for `child` to exit, then checks it exited with 0
/// and did not panic.
fn exits_cleanly(child: &mut Child) {
    let began = Instant::now();
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        assert!(
            began.elapsed() < Duration::from_secs(30),
            "the server did not exit"
        );
        std::thread::sleep(Duration::from_millis(10));
    };
    let mut stderr = String::new();
    child
        .stderr
        .take()
        .unwrap()
        .read_to_string(&mut stderr)
        .unwrap();
    assert!(!stderr.contains("panicked"), "{stderr}");
    assert_eq!(status.code(), Some(0), "{status:?}: {stderr}");
}

/// A client killed while several requests run: an open still scanning,
/// hunks that do not fit the pipe, a summary and a cancel. Its pipes close
/// at once, at a different point of the answers each round.
#[test]
fn a_killed_client_with_requests_in_flight_ends_the_server() {
    const ROUNDS: usize = 50;
    let dir = tempfile::tempdir().unwrap();
    let file = |name: &str, fill: char, lines: usize| {
        let path = dir.path().join(name);
        let line = fill.to_string().repeat(1000);
        let mut text = String::new();
        for i in 0..lines {
            writeln!(text, "{line}{i}").unwrap();
        }
        std::fs::write(&path, text).unwrap();
        path.to_str().unwrap().to_owned()
    };
    let (a, b) = (file("a", 'a', 2000), file("b", 'b', 2000));
    let (big_a, big_b) = (file("big_a", 'c', 20_000), file("big_b", 'd', 20_000));
    for round in 0..ROUNDS {
        let Server(child) = &mut serve();
        let mut stdin = child.stdin.take().unwrap();
        let mut stdout = BufReader::new(child.stdout.take().unwrap());
        send(&mut stdin, 1, "initialize", &json!({"protocol": 1}));
        while frame(&mut stdout)["id"] != 1 {}
        send(
            &mut stdin,
            2,
            "open",
            &json!({"a": a, "b": b, "mem": "64M"}),
        );
        let mut session = None;
        loop {
            let m = frame(&mut stdout);
            if m["id"] == 2 {
                session = m["result"]["session"].as_u64();
            }
            if m["method"] == "session_state" && m["params"]["state"] == "ready" {
                break;
            }
        }
        let session = session.unwrap();
        send(
            &mut stdin,
            3,
            "open",
            &json!({"a": big_a, "b": big_b, "mem": "64M"}),
        );
        for id in 4..8 {
            send(
                &mut stdin,
                id,
                "hunk",
                &json!({"session": session, "segment": 0, "context": 0,
                        "max_lines": 2000, "max_line_bytes": 65536}),
            );
        }
        send(&mut stdin, 8, "summary", &json!({"session": session}));
        let cancel = json!({"jsonrpc": "2.0", "method": "$/cancelRequest",
                            "params": {"id": 3}});
        let cancel = serde_json::to_vec(&cancel).unwrap();
        write!(stdin, "Content-Length: {}\r\n\r\n", cancel.len()).unwrap();
        stdin.write_all(&cancel).unwrap();
        stdin.flush().unwrap();
        // Read a little more of the answers each round before leaving.
        for _ in 0..round % 4 {
            header(&mut stdout);
        }
        drop(stdout);
        drop(stdin);
        exits_cleanly(child);
    }
}
