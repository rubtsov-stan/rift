//! A test client for `rift serve`: framing, request ids, notifications, and
//! a transcript checked against the protocol's schemas when it ends.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::process::{Child, ChildStdin, Command, ExitStatus, Stdio};
use std::sync::mpsc::{Receiver, RecvTimeoutError, channel};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

/// How long any wait lasts before the test fails.
pub const PATIENCE: Duration = Duration::from_secs(60);

pub struct Client {
    child: Child,
    stdin: Option<ChildStdin>,
    from_server: Receiver<Value>,
    transcript: Vec<String>,
    pending: HashMap<u64, String>,
    answers: HashMap<u64, Value>,
    /// Answers with a null id: refusals of malformed messages.
    pub nulls: Vec<Value>,
    /// Notifications, with when they came.
    pub notes: Vec<(Instant, Value)>,
    next: u64,
}

/// Reads one framed message; `None` at the end of the stream.
fn read_frame(out: &mut impl BufRead) -> Option<Value> {
    let mut len = None;
    loop {
        let mut line = String::new();
        if out.read_line(&mut line).ok()? == 0 {
            return None;
        }
        let line = line.trim_end();
        if line.is_empty() {
            break;
        }
        let (name, value) = line.split_once(':').expect("a header line");
        assert!(name.eq_ignore_ascii_case("content-length"), "{line}");
        len = Some(value.trim().parse::<usize>().unwrap());
    }
    let mut body = vec![0; len.expect("Content-Length")];
    out.read_exact(&mut body).ok()?;
    Some(serde_json::from_slice(&body).expect("the server sends JSON"))
}

#[allow(
    clippy::needless_pass_by_value,
    reason = "tests pass json! literals; taking them by value reads better"
)]
impl Client {
    /// Starts `rift serve` with `args`.
    pub fn start(args: &[&str]) -> Self {
        let mut child = Command::new(env!("CARGO_BIN_EXE_rift"))
            .arg("serve")
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let mut out = BufReader::new(child.stdout.take().unwrap());
        let (tx, from_server) = channel();
        std::thread::spawn(move || {
            while let Some(msg) = read_frame(&mut out) {
                if tx.send(msg).is_err() {
                    break;
                }
            }
        });
        Self {
            stdin: child.stdin.take(),
            child,
            from_server,
            transcript: Vec::new(),
            pending: HashMap::new(),
            answers: HashMap::new(),
            nulls: Vec::new(),
            notes: Vec::new(),
            next: 1,
        }
    }

    /// Writes `body` framed, as it is.
    pub fn send_body(&mut self, body: &[u8]) {
        let stdin = self.stdin.as_mut().unwrap();
        write!(stdin, "Content-Length: {}\r\n\r\n", body.len()).unwrap();
        stdin.write_all(body).unwrap();
        stdin.flush().unwrap();
    }

    /// Writes raw bytes, framing included.
    pub fn send_raw(&mut self, bytes: &[u8]) {
        let stdin = self.stdin.as_mut().unwrap();
        stdin.write_all(bytes).unwrap();
        stdin.flush().unwrap();
    }

    fn record(&mut self, from: &str, method: &Value, message: &Value, bad: bool) {
        let mut line = json!({"from": from, "method": method, "message": message});
        if bad {
            line["bad"] = json!(true);
        }
        self.transcript.push(line.to_string());
    }

    /// Sends `message`; `bad` keeps it out of the schema check of client
    /// messages.
    pub fn send(&mut self, message: &Value, bad: bool) {
        let method = message.get("method").cloned().unwrap_or(Value::Null);
        self.record("client", &method, message, bad);
        if let (Some(id), Some(m)) = (message["id"].as_u64(), method.as_str()) {
            self.pending.insert(id, m.to_owned());
        }
        self.send_body(message.to_string().as_bytes());
    }

    /// Sends a request and returns its id; `bad` as for [`send`](Self::send).
    fn request_as(&mut self, method: &str, params: &Value, bad: bool) -> u64 {
        let id = self.next;
        self.next += 1;
        let msg = json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params});
        self.send(&msg, bad);
        id
    }

    /// Sends a request and returns its id.
    pub fn request(&mut self, method: &str, params: &Value) -> u64 {
        self.request_as(method, params, false)
    }

    fn take(&mut self, msg: Value) {
        let method = match msg.get("id") {
            Some(Value::Null) => Value::Null,
            Some(id) => self
                .pending
                .get(&id.as_u64().unwrap())
                .map_or(Value::Null, |m| json!(m)),
            None => msg["method"].clone(),
        };
        self.record("server", &method, &msg, false);
        match msg.get("id") {
            Some(Value::Null) => self.nulls.push(msg),
            Some(id) => {
                let id = id.as_u64().unwrap();
                assert!(
                    self.answers.insert(id, msg).is_none(),
                    "request {id} answered twice"
                );
            }
            None => self.notes.push((Instant::now(), msg)),
        }
    }

    /// Takes what the server sent, waiting at most `wait` for the first
    /// message. False when nothing came.
    pub fn pump(&mut self, wait: Duration) -> bool {
        match self.from_server.recv_timeout(wait) {
            Ok(msg) => {
                self.take(msg);
                while let Ok(msg) = self.from_server.try_recv() {
                    self.take(msg);
                }
                true
            }
            Err(RecvTimeoutError::Timeout) => false,
            Err(RecvTimeoutError::Disconnected) => panic!("the server closed its output"),
        }
    }

    /// Waits until `done` holds.
    pub fn until(&mut self, what: &str, mut done: impl FnMut(&Self) -> bool) {
        let began = Instant::now();
        while !done(self) {
            assert!(began.elapsed() < PATIENCE, "timed out waiting for {what}");
            self.pump(Duration::from_millis(50));
        }
    }

    /// The answer to request `id`, once it comes.
    pub fn answer(&mut self, id: u64) -> Value {
        self.until("an answer", |c| c.answers.contains_key(&id));
        self.answers[&id].clone()
    }

    /// Whether request `id` has been answered.
    pub fn answered(&self, id: u64) -> bool {
        self.answers.contains_key(&id)
    }

    /// The whole answer message of a request.
    pub fn call(&mut self, method: &str, params: &Value) -> Value {
        let id = self.request(method, params);
        self.answer(id)
    }

    /// The result of a request that must succeed.
    pub fn ok(&mut self, method: &str, params: Value) -> Value {
        let msg = self.call(method, &params);
        assert!(msg.get("error").is_none(), "{method} {params}: {msg}");
        msg["result"].clone()
    }

    /// The error of a request that must fail with `code`.
    pub fn err(&mut self, method: &str, params: Value, code: i64) -> Value {
        let msg = self.call(method, &params);
        assert_eq!(msg["error"]["code"], json!(code), "{method}: {msg}");
        msg["error"].clone()
    }

    /// The error of a request whose params the schema refuses.
    pub fn err_bad(&mut self, method: &str, params: Value, code: i64) -> Value {
        let id = self.request_as(method, &params, true);
        let msg = self.answer(id);
        assert_eq!(msg["error"]["code"], json!(code), "{method}: {msg}");
        msg["error"].clone()
    }

    /// `initialize` with protocol 1; returns the result.
    pub fn initialize(&mut self) -> Value {
        self.ok(
            "initialize",
            json!({"protocol": 1, "client": {"name": "test"}}),
        )
    }

    /// Notifications of `method` about `session`.
    pub fn notes_of(&self, method: &str, session: u64) -> Vec<Value> {
        self.notes
            .iter()
            .filter(|(_, n)| n["method"] == method && n["params"]["session"] == session)
            .map(|(_, n)| n["params"].clone())
            .collect()
    }

    /// The `session_state` notification of `session`, once it comes.
    pub fn state_of(&mut self, session: u64) -> Value {
        self.until("session_state", |c| {
            !c.notes_of("session_state", session).is_empty()
        });
        self.notes_of("session_state", session).remove(0)
    }

    /// Closes standard input, waits for the server to exit, checks the
    /// transcript against the schemas, and returns the exit status and
    /// what the server wrote to standard error.
    pub fn finish(mut self) -> (ExitStatus, String) {
        drop(self.stdin.take());
        let began = Instant::now();
        let status = loop {
            if let Some(status) = self.child.try_wait().unwrap() {
                break status;
            }
            assert!(began.elapsed() < PATIENCE, "the server did not exit");
            if let Ok(msg) = self.from_server.recv_timeout(Duration::from_millis(20)) {
                self.take(msg);
            }
        };
        while let Ok(msg) = self.from_server.recv_timeout(Duration::from_millis(200)) {
            self.take(msg);
        }
        let mut stderr = String::new();
        self.child
            .stderr
            .take()
            .unwrap()
            .read_to_string(&mut stderr)
            .unwrap();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("transcript.jsonl");
        std::fs::write(&path, self.transcript.join("\n")).unwrap();
        let script = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/serve/check-transcript.js"
        );
        let out = Command::new("node")
            .arg(script)
            .arg(&path)
            .output()
            .expect("node runs the schema check");
        assert!(
            out.status.success(),
            "schema check failed:\n{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        (status, stderr)
    }
}
