#!/usr/bin/env node
// Self-check of the protocol's schemas, the fixture and the mock server.
// Run: node editors/vscode/test/check-mock.js. Exits non-zero on the first
// failure. No dependencies.
//
// 1. Every schema under docs/protocol/ parses and uses only the keywords
//    the validator below knows, so a keyword it would skip cannot hide a
//    hole.
// 2. The fixture's answers validate against the result schemas.
// 3. The mock server, driven over Content-Length framing, answers a
//    scripted session; every message it sends validates against
//    message.json and its method's schema, and the scenarios the
//    extension relies on behave as documented in mock-server.js.
'use strict';

const assert = require('assert');
const fs = require('fs');
const path = require('path');
const { spawn } = require('child_process');

const SCHEMAS = path.join(__dirname, '..', '..', '..', 'docs', 'protocol');
const fixture = JSON.parse(fs.readFileSync(path.join(__dirname, 'fixture.json'), 'utf8'));

// --- a JSON Schema subset ------------------------------------------------

const schemas = new Map();
for (const f of fs.readdirSync(SCHEMAS).filter((f) => f.endsWith('.json'))) {
  schemas.set(f, JSON.parse(fs.readFileSync(path.join(SCHEMAS, f), 'utf8')));
}

const ANNOTATIONS = new Set(['$schema', '$id', '$defs', 'title', 'description']);
const KEYWORDS = new Set([
  '$ref', 'type', 'properties', 'required', 'additionalProperties', 'items',
  'enum', 'const', 'minimum', 'maximum', 'minLength', 'oneOf',
]);

function checkKeywords(schema, where) {
  if (typeof schema !== 'object' || schema === null) throw new Error(`${where}: not a schema`);
  for (const [k, v] of Object.entries(schema)) {
    if (!KEYWORDS.has(k) && !ANNOTATIONS.has(k)) throw new Error(`${where}: unsupported keyword ${k}`);
    if (k === 'properties' || k === '$defs') {
      for (const [name, sub] of Object.entries(v)) checkKeywords(sub, `${where}/${k}/${name}`);
    } else if (k === 'items') {
      checkKeywords(v, `${where}/items`);
    } else if (k === 'oneOf') {
      v.forEach((sub, i) => checkKeywords(sub, `${where}/oneOf/${i}`));
    } else if (k === 'additionalProperties' && v !== false) {
      throw new Error(`${where}: additionalProperties must be false`);
    }
  }
}

function resolve(ref, file) {
  const [target, pointer = ''] = ref.split('#');
  const name = target || file;
  let node = schemas.get(name);
  if (!node) throw new Error(`${file}: $ref to unknown ${ref}`);
  for (const part of pointer.split('/').filter(Boolean)) {
    node = node[part];
    if (node === undefined) throw new Error(`${file}: $ref ${ref} does not resolve`);
  }
  return { schema: node, file: name };
}

const typeOf = (v) =>
  v === null ? 'null' : Array.isArray(v) ? 'array' : Number.isInteger(v) ? 'integer' : typeof v;

// Errors as a list of "path: message"; empty means valid.
function validate(schema, value, file, at = '$') {
  const errs = [];
  if (schema.$ref) {
    const r = resolve(schema.$ref, file);
    errs.push(...validate(r.schema, value, r.file, at));
  }
  if (schema.type !== undefined) {
    const types = [].concat(schema.type);
    const t = typeOf(value);
    if (!types.includes(t) && !(t === 'integer' && types.includes('number'))) {
      return [...errs, `${at}: ${t} is not ${types.join('|')}`];
    }
  }
  if ('const' in schema && JSON.stringify(schema.const) !== JSON.stringify(value)) {
    errs.push(`${at}: not ${JSON.stringify(schema.const)}`);
  }
  if (schema.enum && !schema.enum.some((e) => JSON.stringify(e) === JSON.stringify(value))) {
    errs.push(`${at}: ${JSON.stringify(value)} not in ${JSON.stringify(schema.enum)}`);
  }
  if (typeof value === 'number') {
    if (schema.minimum !== undefined && value < schema.minimum) errs.push(`${at}: below ${schema.minimum}`);
    if (schema.maximum !== undefined && value > schema.maximum) errs.push(`${at}: above ${schema.maximum}`);
  }
  if (typeof value === 'string' && schema.minLength !== undefined && value.length < schema.minLength) {
    errs.push(`${at}: shorter than ${schema.minLength}`);
  }
  if (typeOf(value) === 'object') {
    for (const k of schema.required || []) if (!(k in value)) errs.push(`${at}: missing ${k}`);
    for (const [k, v] of Object.entries(value)) {
      const sub = schema.properties && schema.properties[k];
      if (sub) errs.push(...validate(sub, v, file, `${at}.${k}`));
      else if (schema.additionalProperties === false) errs.push(`${at}: unexpected ${k}`);
    }
  }
  if (Array.isArray(value) && schema.items) {
    value.forEach((v, i) => errs.push(...validate(schema.items, v, file, `${at}[${i}]`)));
  }
  if (schema.oneOf) {
    const ok = schema.oneOf.filter((s) => validate(s, value, file, at).length === 0).length;
    if (ok !== 1) errs.push(`${at}: matches ${ok} of oneOf, not 1`);
  }
  return errs;
}

function valid(file, value, what) {
  const errs = validate(schemas.get(file), value, file);
  assert.deepStrictEqual(errs, [], `${what} against ${file}:\n${errs.join('\n')}\n${JSON.stringify(value).slice(0, 400)}`);
}

// --- 1 and 2: schemas and fixture --------------------------------------------

for (const [f, s] of schemas) checkKeywords(s, f);
const NOTIFICATIONS = ['progress', 'session_state', 'segment_ready', 'cancel_request'];
for (const n of NOTIFICATIONS) assert(schemas.has(`${n}.params.json`), `schema of ${n}`);
const METHODS = ['initialize', 'open', 'summary', 'segments', 'hunk', 'moves', 'set_visible', 'close'];
for (const m of METHODS) {
  assert(schemas.has(`${m}.params.json`) && schemas.has(`${m}.result.json`), `schemas of ${m}`);
}
valid('initialize.result.json', fixture.initialize, 'fixture initialize');
valid('summary.result.json', fixture.summary, 'fixture summary');
valid('session_state.params.json', { session: 1, state: 'ready' }, 'session_state');
valid('segments.result.json', { total: fixture.segments.length, offset: 0, segments: fixture.segments }, 'fixture segments');
valid('moves.result.json', { total: 1, offset: 0, moves: 'found', blocks: fixture.moves }, 'fixture moves');
for (const h of Object.values(fixture.hunks)) valid('hunk.result.json', h, `fixture hunk ${h.segment}`);
for (const r of fixture.refined) valid('segment_ready.params.json', { session: 1, ...r }, 'fixture refined');

// A schema that accepts anything checks nothing: spot-check rejections.
assert.notDeepStrictEqual(validate(schemas.get('hunk.params.json'), { session: 1, segment: 0, line_a: 3 }, 'hunk.params.json'), []);
assert.notDeepStrictEqual(validate(schemas.get('hunk.params.json'), { session: 1 }, 'hunk.params.json'), []);
assert.notDeepStrictEqual(validate(schemas.get('summary.result.json'), { ...fixture.summary, extra: 1 }, 'summary.result.json'), []);
assert.notDeepStrictEqual(validate(schemas.get('error.json'), { code: -32003, message: 'x' }, 'error.json'), []);

// --- 3: the mock server --------------------------------------------------------

const server = spawn(process.execPath, [path.join(__dirname, 'mock-server.js')], { stdio: ['pipe', 'pipe', 'inherit'] });
let buf = Buffer.alloc(0);
const notes = [];
const answers = new Map();
let wake = null;
server.stdout.on('data', (chunk) => {
  buf = Buffer.concat([buf, chunk]);
  for (;;) {
    const end = buf.indexOf('\r\n\r\n');
    if (end < 0) break;
    const len = Number(/content-length:\s*(\d+)/i.exec(buf.subarray(0, end).toString())[1]);
    if (buf.length < end + 4 + len) break;
    const msg = JSON.parse(buf.subarray(end + 4, end + 4 + len).toString('utf8'));
    buf = buf.subarray(end + 4 + len);
    valid('message.json', msg, 'message');
    if (msg.id === undefined) {
      valid(`${msg.method}.params.json`, msg.params, `notification ${msg.method}`);
      notes.push(msg);
    } else {
      answers.set(msg.id, msg);
    }
    if (wake) wake();
  }
});

function write(body) {
  const b = Buffer.from(body, 'utf8');
  server.stdin.write(`Content-Length: ${b.length}\r\n\r\n`);
  server.stdin.write(b);
}

async function until(pred, what) {
  const deadline = Date.now() + 5000;
  while (!pred()) {
    if (Date.now() > deadline) throw new Error(`timed out waiting for ${what}`);
    await new Promise((r) => {
      wake = r;
      setTimeout(r, 50);
    });
  }
}

let nextId = 1;
async function call(method, params) {
  const id = nextId++;
  write(JSON.stringify({ jsonrpc: '2.0', id, method, params }));
  await until(() => answers.has(id), `${method} answer`);
  const msg = answers.get(id);
  if (msg.result !== undefined && METHODS.includes(method)) {
    valid(`${method}.result.json`, msg.result, `${method} result`);
  }
  return msg;
}

async function ok(method, params) {
  if (METHODS.includes(method)) valid(`${method}.params.json`, params, `${method} params`);
  const msg = await call(method, params);
  assert(msg.error === undefined, `${method}: ${JSON.stringify(msg.error)}`);
  return msg.result;
}

async function err(method, params, code) {
  const msg = await call(method, params);
  assert(msg.error, `${method} should fail with ${code}`);
  assert.strictEqual(msg.error.code, code, JSON.stringify(msg.error));
  return msg.error;
}

// The session_state notification of a session, once it comes.
async function stateOf(session) {
  const find = () => notes.find((n) => n.method === 'session_state' && n.params.session === session);
  await until(find, `session_state of ${session}`);
  return find().params;
}

async function main() {
  await err('summary', { session: 1 }, -32002);
  const init = await ok('initialize', { protocol: 1, client: { name: 'check' } });
  assert.strictEqual(init.protocol, 1);
  await err('initialize', { protocol: 2 }, -32602);
  await err('nope', {}, -32601);

  // Errors open itself reports.
  const file = await err('open', { a: '/x/a.sql', b: '/x/missing.sql' }, -32001);
  assert.deepStrictEqual(file.data, { side: 'b', path: '/x/missing.sql', reason: 'not_found' });
  await err('open', { a: '/x/a.sql', b: '/x/b.sql', mem: '100G' }, -32004);
  await err('open', { a: '/x/a.sql', b: '/x/b.sql', mem: 'lots' }, -32602);

  // A scan that fails: open answers, the session fails, reopening with
  // the suggested budget replaces it.
  const small = await ok('open', { a: '/x/a.sql', b: '/x/small.sql', mem: '512K' });
  assert.strictEqual(small.state, 'scanning');
  const failedNote = await stateOf(small.session);
  assert.strictEqual(failedNote.state, 'failed');
  const budget = failedNote.error;
  assert.strictEqual(budget.code, -32003);
  assert.strictEqual(budget.data.suggested_mem_bytes, 2 * (256 * 1024 + 1024 * 1024));
  assert.strictEqual(budget.data.suggested_mem, '2560K');
  assert.deepStrictEqual((await ok('summary', { session: small.session })).error, budget);
  const notReady = await err('segments', { session: small.session }, -32008);
  assert.deepStrictEqual(notReady.data, { session: small.session, state: 'failed', error: budget });
  const retry = await ok('open', { a: '/x/a.sql', b: '/x/small.sql', mem: budget.data.suggested_mem });
  assert(retry.session !== small.session && !retry.reused, 'a failed session is replaced');
  await err('summary', { session: small.session }, -32006);
  assert.strictEqual((await stateOf(retry.session)).state, 'ready');

  // A slow scan: not_ready with progress, the same session for the same
  // pair, and close stops it.
  const slow = await ok('open', { a: '/x/slow.sql', b: '/x/b.sql' });
  await until(() => notes.some((n) => n.params.session === slow.session && n.params.phase === 'scan'), 'scan progress');
  const scanning = await err('hunk', { session: slow.session, segment: 1 }, -32008);
  assert.strictEqual(scanning.data.state, 'scanning');
  assert.strictEqual(scanning.data.progress.phase, 'scan');
  const scanSum = await ok('summary', { session: slow.session });
  assert.strictEqual(scanSum.state, 'scanning');
  const again = await ok('open', { a: '/x/slow.sql', b: '/x/b.sql', mem: '1G' });
  assert.deepStrictEqual(again, { session: slow.session, reused: true, mem_bytes: 256 << 20, state: 'scanning' });
  assert.deepStrictEqual(await ok('close', { session: slow.session }), { closed: true });
  await err('summary', { session: slow.session }, -32006);
  const heard = notes.length;
  await new Promise((r) => setTimeout(r, 1200));
  assert(!notes.slice(heard).some((n) => n.params.session === slow.session), 'nothing after close');

  // A cancel that comes after the answer does nothing.
  write(JSON.stringify({ jsonrpc: '2.0', method: '$/cancelRequest', params: { id: 1 } }));

  const opened = await ok('open', { a: '/x/a.sql', b: '/x/b.sql', mem: '256M' });
  assert.deepStrictEqual(opened, { session: opened.session, reused: false, mem_bytes: 256 << 20, state: 'scanning' });
  const s = opened.session;
  assert.strictEqual((await stateOf(s)).state, 'ready');
  assert.deepStrictEqual(await ok('open', { a: '/x/a.sql', b: '/x/b.sql' }), { ...opened, reused: true, state: 'ready' });
  const scans = notes.filter((n) => n.method === 'progress' && n.params.session === s && n.params.phase === 'scan');
  assert.strictEqual(scans.length, fixture.scan_steps);
  assert.strictEqual(scans.at(-1).params.done, scans.at(-1).params.total);

  const sum = await ok('summary', { session: s });
  assert.strictEqual(sum.state, 'ready');
  assert.strictEqual(sum.differing_segments, fixture.segments.length);

  // Paging.
  const p1 = await ok('segments', { session: s, offset: 0, limit: 2 });
  const p2 = await ok('segments', { session: s, offset: 2, limit: 100 });
  assert.strictEqual(p1.segments.length + p2.segments.length, p1.total);
  assert(p1.segments.concat(p2.segments).some((x) => x.status === 'approx'), 'an approx segment');

  // Hunks.
  const long = await ok('hunk', { session: s, segment: 7 });
  const cutLine = long.a.lines.find((l) => l.truncated);
  assert(cutLine && Buffer.byteLength(cutLine.text) <= 4096 && cutLine.bytes > 4096, 'a cut line');
  assert(long.b.lines.some((l) => l.lossy), 'a lossy line');
  assert.strictEqual(long.pending, true);
  await until(() => notes.some((n) => n.method === 'segment_ready' && n.params.index === 7), 'segment_ready 7');
  assert.strictEqual((await ok('hunk', { session: s, segment: 7 })).pending, false);

  const big = await ok('hunk', { session: s, segment: 9, max_lines: 5000 });
  assert(big.truncated && big.b.lines.length < big.b.total, 'a truncated hunk');
  const few = await ok('hunk', { session: s, segment: 9, max_lines: 4, context: 0 });
  assert.strictEqual(few.b.lines.length, 4);
  assert.strictEqual(few.b.start, few.range.b.start);
  assert(few.b.lines.some((l) => l.moved), 'moved lines on B');

  const byLine = await ok('hunk', { session: s, line_a: 22 });
  assert.strictEqual(byLine.segment, 3);
  assert(byLine.a.lines.filter((l) => l.moved).length === 5, 'moved lines on A');
  const equal = await ok('hunk', { session: s, line_a: 30, context: 1, max_lines: 2 });
  assert.strictEqual(equal.status, 'equal');
  assert.strictEqual(equal.a.start, 29);
  assert.strictEqual(equal.a.lines[1].text, equal.b.lines[1].text);
  await err('hunk', { session: s, segment: 99 }, -32602);
  await err('hunk', { session: s, segment: 1, line_a: 3 }, -32602);

  const moves = await ok('moves', { session: s });
  assert.strictEqual(moves.blocks.length, 1);

  assert.strictEqual(await ok('set_visible', { session: s, a: { start: 8, count: 5 } }), null);
  await until(() => notes.some((n) => n.method === 'segment_ready' && n.params.index === 1), 'segment_ready 1');
  // The progress that follows a segment_ready can arrive in a later read.
  const lastProgress = () => notes.filter((n) => n.method === 'progress' && n.params.session === s).pop().params;
  await until(() => lastProgress().done === lastProgress().total, 'refinement progress at its total');
  assert.strictEqual(lastProgress().phase, 'refine');

  // Malformed input gets an error, not a crash.
  write('{not json');
  await until(() => answers.has(null), 'parse error');
  assert.strictEqual(answers.get(null).error.code, -32700);
  answers.delete(null);
  write(JSON.stringify([{ jsonrpc: '2.0', id: 99, method: 'summary' }]));
  await until(() => answers.has(null), 'batch refusal');
  assert.strictEqual(answers.get(null).error.code, -32600);

  assert.deepStrictEqual(await ok('close', { session: s }), { closed: true });
  assert.deepStrictEqual(await ok('close', { session: s }), { closed: false });
  const unknown = await err('summary', { session: s }, -32006);
  assert.deepStrictEqual(unknown.data, { session: s });
  const reopened = await ok('open', { a: '/x/a.sql', b: '/x/b.sql' });
  assert(reopened.session > s && !reopened.reused, 'ids are not reused');

  server.stdin.end();
  await new Promise((r) => server.on('exit', r));
  assert.strictEqual(server.exitCode, 0, 'a closed stdin ends the server cleanly');
  console.log(`ok: ${schemas.size} schemas, fixture, ${nextId - 1} requests, ${notes.length} notifications`);
}

main().catch((e) => {
  console.error(e.stack || e);
  server.kill();
  process.exit(1);
});
