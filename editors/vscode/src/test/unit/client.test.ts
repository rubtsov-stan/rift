import * as assert from 'assert';
import * as path from 'path';
import * as fs from 'fs';
import * as os from 'os';
import { Server, RpcError } from '../../client';
import { Notification, OpenResult, Summary } from '../../protocol';

const MOCK = path.join(__dirname, '..', '..', '..', 'test', 'mock-server.js');

// A wrapper script so the server is one executable, as `rift.path` is.
function wrapper(): string {
  const file = path.join(fs.mkdtempSync(path.join(os.tmpdir(), 'rift-mock-')), 'rift');
  fs.writeFileSync(file, `#!/bin/sh\nELECTRON_RUN_AS_NODE=1 exec "${process.execPath}" "${MOCK}"\n`, { mode: 0o755 });
  return file;
}

function make(extra: Partial<ConstructorParameters<typeof Server>[0]> = {}) {
  const notes: Notification[] = [];
  const crashes: boolean[] = [];
  let restarted = () => {};
  const server = new Server({
    command: wrapper,
    args: ['serve'],
    busyDelaysMs: [5, 5, 5],
    minUptimeMs: 0,
    onNotification: (n) => notes.push(n),
    onCrash: (_r, restarting) => crashes.push(restarting),
    onRestarted: () => restarted(),
    ...extra,
  });
  return { server, notes, crashes, onRestarted: (f: () => void) => (restarted = f) };
}

async function until(f: () => boolean): Promise<void> {
  for (let i = 0; i < 400 && !f(); i++) await new Promise((r) => setTimeout(r, 10));
  assert.ok(f(), 'condition not reached');
}

describe('Server', () => {
  it('opens a pair, scans, and reports ready', async () => {
    const { server, notes } = make();
    const open = await server.request<OpenResult>('open', { a: '/x/a.sql', b: '/x/b.sql' });
    assert.strictEqual(open.state, 'scanning');
    await until(() => notes.some((n) => n.method === 'session_state'));
    const s = await server.request<Summary>('summary', { session: open.session });
    assert.strictEqual(s.state, 'ready');
    assert.ok(notes.some((n) => n.method === 'progress' && n.params.phase === 'scan'));
    server.dispose();
  });

  it('rejects with the server error code', async () => {
    const { server } = make();
    await assert.rejects(server.request('open', { a: '/x/missing.sql', b: '/x/b.sql' }), (e: RpcError) => e.code === -32001);
    server.dispose();
  });

  it('retries busy and then succeeds', async () => {
    const { server } = make();
    const open = await server.request<OpenResult>('open', { a: '/x/busy.sql', b: '/x/b.sql' });
    assert.strictEqual(open.state, 'scanning');
    server.dispose();
  });

  it('fails with busy after the last retry', async () => {
    const { server } = make();
    await assert.rejects(server.request('open', { a: '/x/stuck.sql', b: '/x/b.sql' }), (e: RpcError) => e.code === -32005);
    server.dispose();
  });

  it('restarts after a crash and answers again', async () => {
    const { server, crashes, onRestarted } = make();
    await server.request('open', { a: '/x/a.sql', b: '/x/b.sql' });
    let restarted = false;
    onRestarted(() => (restarted = true));
    process.kill((await server.pid())!, 'SIGKILL');
    await until(() => restarted);
    assert.deepStrictEqual(crashes, [true]);
    const open = await server.request<OpenResult>('open', { a: '/x/a.sql', b: '/x/b.sql' });
    assert.strictEqual(open.session, 1);
    server.dispose();
  });

  it('does not restart a process that dies at once', async () => {
    const { server, crashes } = make({ command: () => '/bin/false', minUptimeMs: 60000 });
    await assert.rejects(server.request('open', { a: 'a', b: 'b' }));
    await until(() => crashes.length === 1);
    assert.deepStrictEqual(crashes, [false]);
  });
});
