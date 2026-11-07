//! JSON-RPC messages: what a request's params may hold, the error object
//! of every refusal, and bodies the writer streams.

use std::io::{self, Write};

use rift_core::Bytes;
use rift_session::{Error, Session, SessionState, code};
use serde::Serialize;
use serde_json::{Map, Value, json};

/// Every count in the protocol stays below this, so a JavaScript number
/// holds it.
pub const MAX_COUNT: u64 = (1 << 53) - 1;

/// A message the writer frames and writes, serializing it as it goes.
pub trait Body: Send {
    /// Writes the JSON text of the message.
    ///
    /// # Errors
    ///
    /// A write error of `out`.
    fn write(&self, out: &mut dyn Write) -> io::Result<()>;
}

impl Body for Value {
    fn write(&self, out: &mut dyn Write) -> io::Result<()> {
        Ok(serde_json::to_writer(out, self)?)
    }
}

/// A result of a request, serialized from `result` by the writer.
pub struct Reply<R> {
    pub id: Value,
    pub result: R,
}

impl<R: Serialize + Send> Body for Reply<R> {
    fn write(&self, out: &mut dyn Write) -> io::Result<()> {
        #[derive(Serialize)]
        struct Envelope<'a, R> {
            jsonrpc: &'static str,
            id: &'a Value,
            result: &'a R,
        }
        let msg = Envelope {
            jsonrpc: "2.0",
            id: &self.id,
            result: &self.result,
        };
        Ok(serde_json::to_writer(out, &msg)?)
    }
}

/// A message of `jsonrpc` 2.0 with one more field after `head`.
fn message(head: (&str, Value), last: (&str, Value)) -> Value {
    let mut m = Map::new();
    m.insert("jsonrpc".into(), "2.0".into());
    m.insert(head.0.into(), head.1);
    m.insert(last.0.into(), last.1);
    Value::Object(m)
}

/// A result small enough to build ahead.
pub fn result(id: &Value, result: Value) -> Value {
    message(("id", id.clone()), ("result", result))
}

/// A notification.
pub fn note(method: &str, params: Value) -> Value {
    message(("method", method.into()), ("params", params))
}

/// A JSON-RPC error object.
#[derive(Debug)]
pub struct RpcError {
    pub code: i32,
    pub message: String,
    pub data: Option<Value>,
}

impl RpcError {
    pub fn new(code: i32, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            data: None,
        }
    }

    #[must_use]
    pub fn with(mut self, data: Value) -> Self {
        self.data = Some(data);
        self
    }

    /// The object as the protocol writes it.
    pub fn object(&self) -> Value {
        let mut e = json!({"code": self.code, "message": self.message});
        if let Some(data) = &self.data {
            e["data"] = data.clone();
        }
        e
    }

    /// The error answer to request `id`, `null` when it had none.
    pub fn answer(&self, id: &Value) -> Value {
        json!({"jsonrpc": "2.0", "id": id, "error": self.object()})
    }
}

/// `invalid_params` naming the parameter.
pub fn invalid(param: &str) -> RpcError {
    RpcError::new(code::INVALID_PARAMS, format!("invalid {param}")).with(json!({"param": param}))
}

/// `invalid_params` for a number past `limit`.
pub fn out_of_range(param: &str, value: u64, limit: u64, message: String) -> RpcError {
    RpcError::new(code::INVALID_PARAMS, message)
        .with(json!({"param": param, "value": value, "limit": limit}))
}

/// The params of a request: an object, holding only the names the method
/// takes.
pub struct Params(Map<String, Value>);

impl Params {
    /// `params` as sent, absent meaning `{}`.
    ///
    /// # Errors
    ///
    /// `invalid_params` for a value that is not an object or holds a name
    /// not in `names`.
    pub fn new(params: Option<Value>, names: &[&str]) -> Result<Self, RpcError> {
        let map = match params {
            None => Map::new(),
            Some(Value::Object(map)) => map,
            Some(_) => return Err(invalid("params")),
        };
        if let Some(name) = map.keys().find(|k| !names.contains(&k.as_str())) {
            return Err(invalid(name));
        }
        Ok(Self(map))
    }

    /// An integer of at least `min`, if present.
    ///
    /// # Errors
    ///
    /// `invalid_params` for anything else.
    pub fn count(&self, name: &str, min: u64) -> Result<Option<u64>, RpcError> {
        match self.0.get(name) {
            None => Ok(None),
            Some(v) => v
                .as_u64()
                .filter(|&n| n >= min && n <= MAX_COUNT)
                .map(Some)
                .ok_or_else(|| invalid(name)),
        }
    }

    /// [`count`](Self::count) that must be present.
    ///
    /// # Errors
    ///
    /// `invalid_params` when absent or not such a number.
    pub fn need_count(&self, name: &str, min: u64) -> Result<u64, RpcError> {
        self.count(name, min)?.ok_or_else(|| invalid(name))
    }

    /// A string, if present.
    ///
    /// # Errors
    ///
    /// `invalid_params` for anything else.
    pub fn text(&self, name: &str) -> Result<Option<&str>, RpcError> {
        match self.0.get(name) {
            None => Ok(None),
            Some(Value::String(s)) => Ok(Some(s)),
            Some(_) => Err(invalid(name)),
        }
    }

    /// An object holding only `names`, if present.
    ///
    /// # Errors
    ///
    /// `invalid_params` for anything else.
    pub fn object(&self, name: &str, names: &[&str]) -> Result<Option<Self>, RpcError> {
        match self.0.get(name) {
            None => Ok(None),
            Some(v @ Value::Object(_)) => Self::new(Some(v.clone()), names)
                .map(Some)
                .map_err(|_| invalid(name)),
            Some(_) => Err(invalid(name)),
        }
    }
}

/// The scan progress of `session`, as `progress` writes it.
pub fn scan_progress(session: &Session) -> Value {
    let (done, total) = session.scan_progress();
    json!({"phase": "scan", "done": done.0, "total": total.0})
}

/// The error object of a session error. `session` fills in a `not_ready`
/// refusal: its scan progress or the error its scan failed with.
pub fn session_error(e: &Error, session: Option<&Session>) -> RpcError {
    let base = RpcError::new(e.code(), e.to_string());
    match e {
        Error::File { side, error } => base.with(json!({
            "side": side.to_string(),
            "path": error.path,
            "reason": error.reason.name(),
        })),
        Error::MemOverLimit { mem, limit } => {
            base.with(json!({"mem_bytes": mem.0, "max_mem_bytes": limit.0}))
        }
        Error::Busy => base.with(json!({"what": "sessions"})),
        Error::ResponsesBusy => base.with(json!({"what": "responses"})),
        Error::UnknownSession(id) => base.with(json!({"session": id.0})),
        Error::NotReady { session: id, state } => {
            let mut data = json!({"session": id.0, "state": state.name()});
            match (state, session) {
                (SessionState::Scanning, Some(s)) => data["progress"] = scan_progress(s),
                (SessionState::Failed, Some(s)) => {
                    if let Some(err) = s.error() {
                        data["error"] = session_error(err, None).object();
                    }
                }
                _ => {}
            }
            base.with(data)
        }
        Error::SegmentOutOfRange { index, count } => {
            out_of_range("segment", *index as u64, *count as u64, e.to_string())
        }
        Error::LineOutOfRange { line, count } => {
            out_of_range("line_a", line.0 + 1, *count, e.to_string())
        }
        Error::Build { .. } | Error::Segment { .. } => match e.budget() {
            Some((mem, refusal)) => {
                let mut data = json!({
                    "mem_bytes": mem.0,
                    "half": refusal.half.to_string(),
                    "requested_bytes": refusal.requested.0,
                    "available_bytes": refusal.available.0,
                });
                if let Some(need) = e.suggested_mem() {
                    data["suggested_mem_bytes"] = json!(need.0);
                    data["suggested_mem"] = json!(need.to_mem_arg());
                }
                base.with(data)
            }
            None => base,
        },
    }
}

/// Parses `mem` as `--mem` takes it.
///
/// # Errors
///
/// `invalid_params` naming `mem`.
pub fn parse_mem(mem: &str) -> Result<Bytes, RpcError> {
    Bytes::parse_budget(mem)
        .ok()
        .filter(|b| b.0 <= MAX_COUNT)
        .ok_or_else(|| {
            RpcError::new(code::INVALID_PARAMS, format!("invalid mem {mem:?}"))
                .with(json!({"param": "mem", "value": mem}))
        })
}
