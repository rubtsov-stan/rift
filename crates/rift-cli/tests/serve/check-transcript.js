#!/usr/bin/env node
// Validates a transcript of `rift serve` against the protocol's schemas,
// with the validator of editors/vscode/test/check-mock.js, loaded from its
// source so there is one validator. No dependencies.
//
// Usage: node check-transcript.js TRANSCRIPT. The transcript has one JSON
// object per line: {"from": "client" | "server", "method": M, "message": MSG}.
// M is the method of a request, of the request a response answers, or of a
// notification; it is null for a response to a malformed request. Every
// message validates against message.json; params of a request or
// notification against M.params.json (client messages marked "bad": true
// are left out: they test the server's refusals); results against
// M.result.json. Each request id is answered exactly once.
'use strict';

const assert = require('assert');
const fs = require('fs');
const path = require('path');

const ROOT = path.join(__dirname, '..', '..', '..', '..');
const SCHEMAS = path.join(ROOT, 'docs', 'protocol');
const source = fs.readFileSync(path.join(ROOT, 'editors', 'vscode', 'test', 'check-mock.js'), 'utf8');
const start = source.indexOf('// --- a JSON Schema subset');
const end = source.indexOf('// --- 1 and 2');
assert(start >= 0 && end > start, 'check-mock.js has moved its validator');
// eslint-disable-next-line no-new-func
const load = new Function('assert', 'fs', 'path', 'SCHEMAS', `${source.slice(start, end)}\nreturn { schemas, validate, checkKeywords };`);
const { schemas, validate, checkKeywords } = load(assert, fs, path, SCHEMAS);
for (const [f, s] of schemas) checkKeywords(s, f);

function valid(file, value, what) {
  assert(schemas.has(file), `${what}: no schema ${file}`);
  const errs = validate(schemas.get(file), value, file);
  assert.deepStrictEqual(errs, [], `${what} against ${file}:\n${errs.join('\n')}\n${JSON.stringify(value).slice(0, 400)}`);
}

const file = process.argv[2];
const lines = fs.readFileSync(file, 'utf8').split('\n').filter(Boolean);
const answered = new Map();
const asked = new Set();
let counts = { client: 0, server: 0 };
for (const [n, line] of lines.entries()) {
  const { from, method, message, bad } = JSON.parse(line);
  const what = `line ${n + 1} (${from} ${method})`;
  counts[from] += 1;
  if (from === 'client') {
    if (bad) continue;
    valid('message.json', message, what);
    if (message.id !== undefined) asked.add(JSON.stringify(message.id));
    const name = method === '$/cancelRequest' ? 'cancel_request' : method;
    if (schemas.has(`${name}.params.json`)) valid(`${name}.params.json`, message.params ?? {}, what);
    continue;
  }
  valid('message.json', message, what);
  if (message.id === undefined) {
    valid(`${method}.params.json`, message.params, what);
  } else {
    if (message.id !== null) {
      const id = JSON.stringify(message.id);
      answered.set(id, (answered.get(id) || 0) + 1);
    }
    if (message.result !== undefined) valid(`${method}.result.json`, message.result, what);
  }
}
for (const [id, times] of answered) assert.strictEqual(times, 1, `request ${id} answered ${times} times`);
for (const id of asked) assert(answered.has(id), `request ${id} never answered`);
console.log(`ok: ${counts.client} client and ${counts.server} server messages`);
