#!/usr/bin/env node
// A stand-in for `rift serve` that answers the protocol of docs/protocol.md
// from the one diff in fixture.json, so the extension can be built and
// tested without the Rust server. No dependencies.
//
// Paths are not read: every pair opens the fixture's diff, one session per
// pair. `open` answers at once in state scanning; the scan is played as
// fixture.scan_steps progress notifications (phase scan) 10 ms apart, then
// session_state ready and a refine progress. Behaviour a test can trigger:
//   open, a path containing "missing"   -> file error (not_found) for that side
//   open, a path containing "slow"      -> the scan steps are 500 ms apart, time to see
//                                          not_ready or to close the session mid-scan
//   open, a path containing "busy"     -> busy (what: sessions) for that pair's first two opens, then it opens
//   open, a path containing "stuck"    -> busy every time
//   open, mem below 1M                  -> the scan fails: session_state failed with
//                                          budget_exceeded and suggested_mem
//   open, mem above limits.max_session_mem -> too_large
//   hunk on segment 1 or 7              -> pending; segment_ready follows (also via set_visible)
//   hunk on segment 9                   -> B side stored cut at 300 lines: truncated whatever max_lines says
//   hunk on segment 7                   -> a 10 KB line on A, cut at max_line_bytes; a lossy line on B
// Hunks are stored with context 3; a smaller context trims them, a larger
// one is served as 3. An equal segment reached by line_a starts at that
// line, as in the server.
'use strict';

const fs = require('fs');
const path = require('path');

const fixture = JSON.parse(fs.readFileSync(path.join(__dirname, 'fixture.json'), 'utf8'));
const limits = fixture.initialize.limits;

const CODE = {
  parse_error: -32700,
  invalid_request: -32600,
  method_not_found: -32601,
  invalid_params: -32602,
  not_initialized: -32002,
  file: -32001,
  budget_exceeded: -32003,
  too_large: -32004,
  busy: -32005,
  unknown_session: -32006,
  not_ready: -32008,
};

class RpcError extends Error {
  constructor(code, message, data) {
    super(message);
    this.code = code;
    this.data = data;
  }
}

const invalid = (param, extra) =>
  new RpcError(CODE.invalid_params, `invalid ${param}`, { param, ...extra });

// --- framing -----------------------------------------------------------

function send(msg) {
  const body = Buffer.from(JSON.stringify(msg), 'utf8');
  process.stdout.write(`Content-Length: ${body.length}\r\n\r\n`);
  process.stdout.write(body);
}

let pending = Buffer.alloc(0);
process.stdin.on('data', (chunk) => {
  pending = Buffer.concat([pending, chunk]);
  for (;;) {
    const end = pending.indexOf('\r\n\r\n');
    if (end < 0) return;
    const header = pending.subarray(0, end).toString('ascii');
    const m = /^content-length:\s*(\d+)\s*$/im.exec(header);
    if (!m) {
      // No length means the stream cannot be resynchronised.
      send({ jsonrpc: '2.0', id: null, error: { code: CODE.parse_error, message: 'missing Content-Length' } });
      process.exit(1);
    }
    const len = Number(m[1]);
    if (pending.length < end + 4 + len) return;
    const body = pending.subarray(end + 4, end + 4 + len).toString('utf8');
    pending = pending.subarray(end + 4 + len);
    handle(body);
  }
});
process.stdin.on('end', () => process.exit(0));

// --- state ---------------------------------------------------------------

let initialized = false;
let lastId = 0;
// id -> { key, state, mem, scanned, timer, error, done, refined }, where
// refined maps a segment index to its status once segment_ready was sent.
const sessions = new Map();
const busyTries = new Map();
const scanTotal = 2 * (fixture.summary.a.bytes + fixture.summary.b.bytes);
const refineTotal = fixture.summary.progress.total;

// --- dispatch --------------------------------------------------------------

function handle(body) {
  let msg;
  try {
    msg = JSON.parse(body);
  } catch {
    send({ jsonrpc: '2.0', id: null, error: { code: CODE.parse_error, message: 'the body is not JSON' } });
    return;
  }
  if (Array.isArray(msg) || typeof msg !== 'object' || msg === null || msg.jsonrpc !== '2.0' || typeof msg.method !== 'string') {
    send({ jsonrpc: '2.0', id: msg && msg.id !== undefined ? msg.id : null, error: { code: CODE.invalid_request, message: 'not a JSON-RPC request' } });
    return;
  }
  const params = msg.params === undefined ? {} : msg.params;
  // Every request is answered before the next is read, so a
  // $/cancelRequest always comes too late and does nothing.
  if (msg.id === undefined) return;
  const reply = (result) => send({ jsonrpc: '2.0', id: msg.id, result });
  const fail = (e) => send({ jsonrpc: '2.0', id: msg.id, error: errorObject({ code: e.code ?? -32603, message: e.message, data: e.data }) });
  try {
    if (!METHODS[msg.method]) throw new RpcError(CODE.method_not_found, `no method ${msg.method}`);
    if (!initialized && msg.method !== 'initialize') {
      throw new RpcError(CODE.not_initialized, 'initialize first');
    }
    if (typeof params !== 'object' || params === null || Array.isArray(params)) throw invalid('params');
    reply(METHODS[msg.method](params));
  } catch (e) {
    fail(e);
  }
}

const METHODS = {
  initialize(p) {
    if (p.protocol !== 1) throw invalid('protocol', { supported: [1] });
    initialized = true;
    return fixture.initialize;
  },

  open(p) {
    for (const side of ['a', 'b']) {
      if (typeof p[side] !== 'string' || p[side] === '') throw invalid(side);
    }
    for (const side of ['a', 'b']) {
      if (p[side].includes('missing')) {
        throw new RpcError(CODE.file, `${side}: ${p[side]}: No such file or directory (os error 2)`, {
          side,
          path: p[side],
          reason: 'not_found',
        });
      }
    }
    const pair = `${p.a}\0${p.b}`;
    const tries = (busyTries.get(pair) ?? 0) + 1;
    busyTries.set(pair, tries);
    if (pair.includes('stuck') || (pair.includes('busy') && tries <= 2)) {
      throw new RpcError(CODE.busy, 'no room for another session; close one and retry', { what: 'sessions' });
    }
    const mem = p.mem === undefined ? limits.default_mem : parseMem(p.mem);
    if (mem > limits.max_session_mem) {
      throw new RpcError(CODE.too_large, `mem ${mem} is over the limit of ${limits.max_session_mem} per diff`, {
        mem_bytes: mem,
        max_mem_bytes: limits.max_session_mem,
      });
    }
    const key = `${p.a}\0${p.b}`;
    for (const [id, s] of sessions) {
      if (s.key !== key) continue;
      if (s.state !== 'failed') return { session: id, reused: true, mem_bytes: s.mem, state: s.state };
      sessions.delete(id);
    }
    const id = ++lastId;
    const s = { key, state: 'scanning', mem, scanned: 0, timer: null, error: null, done: fixture.summary.progress.done, refined: new Map() };
    sessions.set(id, s);
    const every = p.a.includes('slow') || p.b.includes('slow') ? 500 : 10;
    let step = 0;
    const tick = () => {
      step += 1;
      s.scanned = Math.floor((scanTotal * step) / fixture.scan_steps);
      notify('progress', { session: id, phase: 'scan', done: s.scanned, total: scanTotal });
      if (step < fixture.scan_steps) {
        s.timer = setTimeout(tick, every);
      } else if (mem < 1 << 20) {
        s.state = 'failed';
        s.error = errorObject(budgetError(mem));
        notify('session_state', { session: id, state: 'failed', error: s.error });
      } else {
        s.state = 'ready';
        notify('session_state', { session: id, state: 'ready' });
        notify('progress', { session: id, phase: 'refine', done: s.done, total: refineTotal });
      }
    };
    s.timer = setTimeout(tick, every);
    return { session: id, reused: false, mem_bytes: mem, state: 'scanning' };
  },

  summary(p) {
    const s = session(p);
    if (s.state === 'failed') return { state: 'failed', error: s.error };
    if (s.state === 'scanning') {
      return {
        state: 'scanning',
        a: { bytes: fixture.summary.a.bytes },
        b: { bytes: fixture.summary.b.bytes },
        mem_bytes: s.mem,
        progress: { phase: 'scan', done: s.scanned, total: scanTotal },
      };
    }
    return { ...fixture.summary, mem_bytes: s.mem, progress: { phase: 'refine', done: s.done, total: refineTotal } };
  },

  segments(p) {
    readySession(p);
    const { offset, limit } = page(p);
    return {
      total: fixture.segments.length,
      offset,
      segments: fixture.segments.slice(offset, offset + limit),
    };
  },

  hunk(p) {
    const [id, s] = readySession(p);
    const hasSeg = p.segment !== undefined;
    if (hasSeg === (p.line_a !== undefined)) throw invalid('segment', {});
    const count = Object.keys(fixture.hunks).length;
    let index;
    if (hasSeg) {
      if (!Number.isInteger(p.segment) || p.segment < 0) throw invalid('segment');
      if (p.segment >= count) throw invalid('segment', { value: p.segment, limit: count });
      index = p.segment;
    } else {
      const lines = fixture.summary.a.lines;
      if (!Number.isInteger(p.line_a) || p.line_a < 1) throw invalid('line_a');
      if (p.line_a > lines) throw invalid('line_a', { value: p.line_a, limit: lines });
      index = Object.values(fixture.hunks).findIndex(
        (h) => h.range.a.start <= p.line_a && p.line_a < h.range.a.start + h.range.a.count,
      );
    }
    const ctx = Math.min(num(p, 'context', 3, 0), 3);
    const maxLines = Math.min(num(p, 'max_lines', 1000, 1), limits.max_lines);
    const maxBytes = Math.min(num(p, 'max_line_bytes', 4096, 1), limits.max_line_bytes);
    const h = fixture.hunks[String(index)];
    // An equal segment reached by line is shown from that line.
    const skip = !hasSeg && h.status === 'equal' ? p.line_a - h.range.a.start : 0;
    const pre = Math.min(ctx, h.range.a.start + skip - 1, h.range.b.start + skip - 1);
    const storedPost = h.a.start + h.a.total - (h.range.a.start + h.range.a.count);
    const post = Math.min(ctx, storedPost);
    const col = (c, range) => {
      const start = range.start + skip - pre;
      const totalLines = range.start + range.count + post - start;
      const from = start - c.start;
      const lines = c.lines.slice(from, from + Math.min(totalLines, maxLines)).map((l) => cut(l, maxBytes));
      return { start, total: totalLines, lines };
    };
    const a = col(h.a, h.range.a);
    const b = col(h.b, h.range.b);
    const isPending = h.pending && !s.refined.has(index);
    const result = {
      segment: index,
      status: s.refined.get(index) ?? h.status,
      pending: isPending,
      range: h.range,
      a,
      b,
      truncated: a.lines.length < a.total || b.lines.length < b.total,
    };
    if (isPending) setImmediate(() => ready(id, index));
    return result;
  },

  moves(p) {
    readySession(p);
    const { offset, limit } = page(p);
    return {
      total: fixture.moves.length,
      offset,
      moves: fixture.summary.moves,
      blocks: fixture.moves.slice(offset, offset + limit),
    };
  },

  set_visible(p) {
    const [id] = readySession(p);
    const r = p.a;
    if (typeof r !== 'object' || r === null || !Number.isInteger(r.start) || r.start < 1 || !Number.isInteger(r.count) || r.count < 0) {
      throw invalid('a');
    }
    const end = r.start + Math.max(r.count, 1);
    const shown = fixture.segments.filter((seg) => seg.a.start < end && r.start <= seg.a.start + seg.a.count);
    setImmediate(() => {
      for (const seg of shown) {
        const h = fixture.hunks[String(seg.index)];
        if (h.pending) ready(id, seg.index);
      }
    });
    return null;
  },

  close(p) {
    const id = sessionId(p);
    const s = sessions.get(id);
    if (s) clearTimeout(s.timer);
    return { closed: sessions.delete(id) };
  },
};

// --- helpers ---------------------------------------------------------------

function sessionId(p) {
  if (!Number.isInteger(p.session) || p.session < 1) throw invalid('session');
  return p.session;
}

function session(p) {
  const id = sessionId(p);
  const s = sessions.get(id);
  if (!s) {
    throw new RpcError(CODE.unknown_session, `no session ${id}; it was closed or dropped, open the files again`, {
      session: id,
    });
  }
  return s;
}

// The session, which must have its map: [id, session].
function readySession(p) {
  const s = session(p);
  const id = p.session;
  if (s.state === 'scanning') {
    throw new RpcError(CODE.not_ready, `session ${id} is scanning; wait for it to be ready`, {
      session: id,
      state: 'scanning',
      progress: { phase: 'scan', done: s.scanned, total: scanTotal },
    });
  }
  if (s.state === 'failed') {
    throw new RpcError(CODE.not_ready, `session ${id} is failed; wait for it to be ready`, {
      session: id,
      state: 'failed',
      error: s.error,
    });
  }
  return [id, s];
}

function errorObject(e) {
  const error = { code: e.code, message: e.message };
  if (e.data !== undefined) error.data = e.data;
  return error;
}

function notify(method, params) {
  send({ jsonrpc: '2.0', method, params });
}

function num(p, name, dflt, min) {
  if (p[name] === undefined) return dflt;
  if (!Number.isInteger(p[name]) || p[name] < min) throw invalid(name);
  return p[name];
}

function page(p) {
  return { offset: num(p, 'offset', 0, 0), limit: Math.min(num(p, 'limit', 100, 1), limits.max_page) };
}

// A line cut at max bytes of UTF-8, dropping a character the cut splits.
function cut(line, max) {
  const bytes = Buffer.from(line.text, 'utf8');
  if (bytes.length <= max) return line;
  let end = max;
  while (end > 0 && (bytes[end] & 0xc0) === 0x80) end--;
  return { ...line, text: bytes.subarray(0, end).toString('utf8'), truncated: true };
}

function parseMem(s) {
  const m = typeof s === 'string' ? /^\s*(\d+)\s*(k|kib|m|mib|g|gib|t|tib)?\s*$/i.exec(s) : null;
  if (!m) throw invalid('mem');
  const shift = { k: 10, m: 20, g: 30, t: 40 }[(m[2] || '').charAt(0).toLowerCase()] || 0;
  const n = Number(m[1]) * 2 ** shift;
  if (n === 0 || !Number.isSafeInteger(n)) throw invalid('mem');
  return n;
}

// What a server says when the scan needs 1 MiB more than the Refine half
// has: the same formula as BudgetExceeded::mem_advice in rift-core.
function budgetError(mem) {
  const half = Math.floor(mem / 2);
  const requested = 1 << 20;
  const available = 0;
  const need = 2 * (half - available + requested);
  const data = {
    mem_bytes: mem,
    half: 'refine',
    requested_bytes: requested,
    available_bytes: available,
  };
  if (need > mem) {
    data.suggested_mem_bytes = need;
    data.suggested_mem = memArg(need);
  }
  const text = `cannot diff within ${mem}: refine budget exceeded: requested ${requested} bytes, ${available} available`;
  return new RpcError(CODE.budget_exceeded, text, data);
}

// Bytes::to_mem_arg: rounded up to the largest unit that keeps the
// rounding under 1/64.
function memArg(bytes) {
  for (const [shift, suffix] of [[40, 'T'], [30, 'G'], [20, 'M']]) {
    const unit = 2 ** shift;
    if (bytes >= 64 * unit) return `${Math.ceil(bytes / unit)}${suffix}`;
  }
  return `${Math.ceil(bytes / 1024)}K`;
}

function ready(id, index) {
  const s = sessions.get(id);
  if (!s || s.refined.has(index)) return;
  const ev = fixture.refined.find((r) => r.index === index);
  s.refined.set(index, ev.status);
  s.done += 1;
  notify('segment_ready', { session: id, index, status: ev.status });
  notify('progress', { session: id, phase: 'refine', done: s.done, total: refineTotal });
}
