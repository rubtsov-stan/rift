//! `rift serve`: the protocol of `docs/protocol.md`, JSON-RPC 2.0 with
//! LSP's framing on standard input and output, over `rift-session`.
//!
//! Threads, no async runtime: the main thread reads and checks requests
//! and answers `initialize` itself; every other request runs on a thread
//! of its own, at most [`MAX_IN_FLIGHT`] at once; one thread writes every
//! message; one sends the notifications (see [`notify`]).
//!
//! Memory. Sessions take their budgets from `--max-total-mem` less
//! [`PROTOCOL_RESERVE`], which pays for the protocol: the input buffer and
//! one request body of at most [`frame::MAX_BODY`] while it is parsed, the
//! checked params of the requests in flight, up to [`QUEUE`] messages
//! waiting for the writer and the writer's buffer. The large answers hold
//! no memory of their own: pages of segments and moved blocks are written
//! straight from the session's map, and a hunk's lines live in the
//! session's response pool until they are written. Every message is
//! written through the writer's fixed buffer, after a first pass that
//! only counts its length.

mod frame;
mod methods;
mod notify;
mod wire;

use std::io::{self, BufReader, BufWriter, Write};
use std::process::ExitCode;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, SyncSender, TryRecvError, sync_channel};
use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::time::Duration;

use rift_core::Bytes;
use rift_diff::ServeArgs;
use rift_session::{
    Limits, MAX_CONTEXT, MAX_LINE_BYTES, MAX_LINES, MAX_PAGE, MAX_SESSIONS, Sessions, code,
};
use serde_json::{Value, json};

use self::frame::Frame;
use self::wire::{Body, Params, RpcError, invalid};

/// The protocol versions the server speaks.
const PROTOCOL: u64 = 1;
/// The memory a session gets when `open` does not say.
const DEFAULT_MEM: Bytes = Bytes::mib(256);
/// Requests answered at once; reading waits for one to finish beyond.
const MAX_IN_FLIGHT: usize = 8;
/// Messages waiting for the writer; senders wait beyond.
const QUEUE: usize = 16;
/// The part of `--max-total-mem` the protocol keeps for itself; see the
/// module docs. Worst cases, by count rather than measured: under 2 MiB to
/// parse one body (a 64 KiB array of zeros is 32 Ki values of 32 bytes,
/// plus the growth of their vector), 512 KiB for the paths of the requests
/// in flight, under 4 MiB for the queue, whose largest messages are file
/// errors that quote a path of a whole body three times, and 128 KiB of
/// input and output buffers.
const PROTOCOL_RESERVE: Bytes = Bytes::mib(8);
/// The input and output buffers.
const IO_BUF: usize = 64 << 10;

/// Runs `rift serve` with `cli`.
pub fn main(cli: &ServeArgs) -> ExitCode {
    match run(cli) {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("rift serve: {err:#}");
            ExitCode::from(2)
        }
    }
}

/// A request being answered.
pub struct Flight {
    id: Value,
    /// Set by whichever answer goes first: the request's or a cancel.
    answered: AtomicBool,
}

/// What the request threads share.
pub struct Server {
    sessions: Sessions,
    max_session_mem: Bytes,
    default_mem: Bytes,
    out: SyncSender<Box<dyn Body>>,
    flights: Mutex<Vec<Arc<Flight>>>,
    room: Condvar,
    notes: Mutex<notify::Notes>,
}

impl Server {
    fn send(&self, body: Box<dyn Body>) {
        // The writer is gone only when the output is closed; there is no
        // one left to tell.
        let _ = self.out.send(body);
    }

    /// Sends the answer to `flight` unless it was answered already.
    fn reply(&self, flight: &Flight, body: Box<dyn Body>) {
        if !flight.answered.swap(true, Ordering::AcqRel) {
            self.send(body);
        }
    }

    fn flights(&self) -> std::sync::MutexGuard<'_, Vec<Arc<Flight>>> {
        self.flights.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Answers the request `id` with `cancelled` if it is still running.
    fn cancel(&self, id: &Value) {
        let flight = self.flights().iter().find(|f| f.id == *id).cloned();
        if let Some(f) = flight {
            let e = RpcError::new(code::CANCELLED, "the request was cancelled");
            self.reply(&f, Box::new(e.answer(&f.id)));
        }
    }

    /// Runs `call` on a thread of its own, once fewer than
    /// [`MAX_IN_FLIGHT`] requests run.
    fn start(self: &Arc<Self>, id: Value, call: methods::Call) {
        let flight = Arc::new(Flight {
            id,
            answered: AtomicBool::new(false),
        });
        {
            let mut flights = self.flights();
            while flights.len() >= MAX_IN_FLIGHT {
                flights = self
                    .room
                    .wait(flights)
                    .unwrap_or_else(PoisonError::into_inner);
            }
            flights.push(Arc::clone(&flight));
        }
        let (server, f) = (Arc::clone(self), Arc::clone(&flight));
        let spawned = std::thread::Builder::new()
            .name("rift-request".into())
            .spawn(move || {
                server.run(&f, call);
                server.land(&f);
            });
        if let Err(e) = spawned {
            let e = RpcError::new(code::INTERNAL_ERROR, format!("cannot start a thread: {e}"));
            self.reply(&flight, Box::new(e.answer(&flight.id)));
            self.land(&flight);
        }
    }

    /// Forgets a request that has ended.
    fn land(&self, flight: &Arc<Flight>) {
        self.flights().retain(|f| !Arc::ptr_eq(f, flight));
        self.room.notify_all();
    }

    fn initialize(&self, params: Option<Value>) -> Result<Value, RpcError> {
        let p = Params::new(params, &["protocol", "client"])?;
        let version = p.need_count("protocol", 1)?;
        if let Some(client) = p.object("client", &["name", "version"])? {
            client.text("name")?.ok_or_else(|| invalid("client"))?;
            client.text("version")?;
        }
        if version != PROTOCOL {
            return Err(RpcError::new(
                code::INVALID_PARAMS,
                format!("protocol {version} is not spoken here"),
            )
            .with(json!({"param": "protocol", "value": version, "supported": [PROTOCOL]})));
        }
        Ok(json!({
            "protocol": PROTOCOL,
            "server": {"name": "rift", "version": env!("CARGO_PKG_VERSION")},
            "capabilities": {
                "methods": methods::METHODS,
                "notifications": ["progress", "session_state", "segment_ready"],
            },
            "limits": {
                "max_sessions": MAX_SESSIONS,
                "max_session_mem": self.max_session_mem.0,
                "max_total_mem": self.sessions.limits().total_mem.0,
                "default_mem": self.default_mem.0,
                "idle_timeout_secs": null,
                "max_page": MAX_PAGE,
                "max_lines": MAX_LINES,
                "max_line_bytes": MAX_LINE_BYTES,
                "max_context": MAX_CONTEXT,
            },
        }))
    }

    /// Handles one message from the client.
    fn handle(self: &Arc<Self>, body: &[u8], initialized: &mut bool) {
        let refuse = |code, message: &str, id: &Value| {
            self.send(Box::new(RpcError::new(code, message).answer(id)));
        };
        let Ok(msg) = serde_json::from_slice::<Value>(body) else {
            return refuse(code::PARSE_ERROR, "the body is not JSON", &Value::Null);
        };
        let Value::Object(mut msg) = msg else {
            return refuse(
                code::INVALID_REQUEST,
                "not a JSON-RPC request; batches are not supported",
                &Value::Null,
            );
        };
        let id = match msg.remove("id") {
            None => None,
            Some(id @ Value::String(_)) => Some(id),
            Some(Value::Number(n)) if n.is_i64() || n.is_u64() => Some(Value::Number(n)),
            Some(_) => {
                return refuse(
                    code::INVALID_REQUEST,
                    "an id is an integer or a string",
                    &Value::Null,
                );
            }
        };
        let method = match (msg.remove("jsonrpc"), msg.remove("method")) {
            (Some(Value::String(v)), Some(Value::String(m))) if v == "2.0" => m,
            _ => {
                return refuse(
                    code::INVALID_REQUEST,
                    "not a JSON-RPC 2.0 request",
                    id.as_ref().unwrap_or(&Value::Null),
                );
            }
        };
        let params = msg.remove("params");
        let Some(id) = id else {
            if method == "$/cancelRequest"
                && let Some(Value::Object(p)) = &params
                && let Some(id) = p.get("id")
            {
                self.cancel(id);
            }
            // Other notifications are ignored.
            return;
        };
        if !methods::METHODS.contains(&method.as_str()) {
            return refuse(code::METHOD_NOT_FOUND, &format!("no method {method}"), &id);
        }
        if method == "initialize" {
            let answer = match self.initialize(params) {
                Ok(result) => {
                    *initialized = true;
                    wire::result(&id, result)
                }
                Err(e) => e.answer(&id),
            };
            return self.send(Box::new(answer));
        }
        if !*initialized {
            return refuse(code::NOT_INITIALIZED, "initialize first", &id);
        }
        match methods::parse(&method, params) {
            Ok(call) => self.start(id, call),
            Err(e) => self.send(Box::new(e.answer(&id))),
        }
    }
}

/// Writes every message it receives, framed, and flushes when none waits.
fn write_all(rx: &Receiver<Box<dyn Body>>) -> io::Result<()> {
    let mut out = BufWriter::with_capacity(IO_BUF, io::stdout().lock());
    let mut next = rx.recv().ok();
    while let Some(body) = next {
        frame::write(&mut out, &mut |w| body.write(w))?;
        drop(body);
        next = match rx.try_recv() {
            Ok(b) => Some(b),
            Err(TryRecvError::Empty) => {
                out.flush()?;
                rx.recv().ok()
            }
            Err(TryRecvError::Disconnected) => None,
        };
    }
    out.flush()
}

fn run(cli: &ServeArgs) -> anyhow::Result<()> {
    let sessions_mem = Bytes(cli.max_total_mem.0.saturating_sub(PROTOCOL_RESERVE.0));
    anyhow::ensure!(
        sessions_mem.0 > 0,
        "--max-total-mem must be above the {} the protocol keeps",
        PROTOCOL_RESERVE.to_mem_arg()
    );
    let (tx, rx) = sync_channel(QUEUE);
    let writer = std::thread::Builder::new()
        .name("rift-writer".into())
        .spawn(move || write_all(&rx))?;
    let server = Arc::new(Server {
        sessions: Sessions::new(Limits {
            sessions: MAX_SESSIONS,
            session_mem: cli.max_session_mem,
            total_mem: sessions_mem,
            idle: Duration::MAX,
        }),
        max_session_mem: cli.max_session_mem,
        default_mem: DEFAULT_MEM.min(cli.max_session_mem),
        out: tx,
        flights: Mutex::default(),
        room: Condvar::new(),
        notes: Mutex::default(),
    });
    let stop = Arc::new(AtomicBool::new(false));
    let ticker = {
        let (server, stop) = (Arc::clone(&server), Arc::clone(&stop));
        std::thread::Builder::new()
            .name("rift-notify".into())
            .spawn(move || {
                while !stop.load(Ordering::Relaxed) {
                    std::thread::sleep(notify::TICK);
                    server.notify();
                }
            })?
    };

    let mut input = BufReader::with_capacity(IO_BUF, io::stdin().lock());
    let mut body = Vec::with_capacity(frame::MAX_BODY);
    let mut initialized = false;
    loop {
        match frame::read(&mut input, &mut body) {
            Ok(Frame::Body) => server.handle(&body, &mut initialized),
            Ok(Frame::NoLength) => server.send(Box::new(
                RpcError::new(code::PARSE_ERROR, "a header block without Content-Length")
                    .answer(&Value::Null),
            )),
            Ok(Frame::TooLarge(len)) => server.send(Box::new(
                RpcError::new(
                    code::INVALID_REQUEST,
                    format!(
                        "a body of {len} bytes, over the limit of {}",
                        frame::MAX_BODY
                    ),
                )
                .answer(&Value::Null),
            )),
            Ok(Frame::End) | Err(_) => break,
        }
    }

    // Standard input closed: cancel what runs, drop every session, and
    // let the writer finish what is queued.
    let running = server.flights().clone();
    for f in running {
        server.cancel(&f.id);
    }
    server.sessions.close_all();
    stop.store(true, Ordering::Relaxed);
    ticker.join().ok();
    {
        let mut flights = server.flights();
        while !flights.is_empty() {
            flights = server
                .room
                .wait(flights)
                .unwrap_or_else(PoisonError::into_inner);
        }
    }
    drop(server);
    // A write error means the client closed its end; nothing is left to do.
    let _ = writer.join();
    if cli.stats {
        eprintln!("rift serve: peak heap {} bytes", rift_core::alloc::peak().0);
    }
    Ok(())
}
