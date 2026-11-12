import * as assert from 'assert';
import * as fs from 'fs';
import * as os from 'os';
import * as path from 'path';
import { Server, RpcError } from '../../client';
import { Notification, OpenResult, Summary } from '../../protocol';
import { mockBinary, until } from '../mock';

function make(extra: Partial<ConstructorParameters<typeof Server>[0]> = {}) {
  const notes: Notification[] = [];
  const crashes: boolean[] = [];
  const stops: boolean[] = [];
  let restarted = () => {};
  const server = new Server({
    command: mockBinary,
    args: ['serve'],
    busyDelaysMs: [5, 5, 5],
    minUptimeMs: 0,
    onNotification: (n) => notes.push(n),
    onCrash: (_r, restarting, deliberate) => (deliberate ? stops : crashes).push(restarting),
    onRestarted: () => restarted(),
    ...extra,
  });
  return { server, notes, crashes, stops, onRestarted: (f: () => void) => (restarted = f) };
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

  it('reports a restarted server that refuses to initialize', async () => {
    const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'rift-flag-'));
    const flag = path.join(dir, 'fail');
    const failures: string[] = [];
    const { server } = make({ command: () => mockBinary({ MOCK_FAIL_INIT_FILE: flag }), onRestartFailed: (e) => failures.push(e.message) });
    await server.request('open', { a: '/x/a.sql', b: '/x/b.sql' });
    fs.writeFileSync(flag, '');
    process.kill((await server.pid())!, 'SIGKILL');
    await until(() => failures.length === 1);
    assert.match(failures[0], /initialize failed/);
    server.dispose();
  });

  it('fails the request when the server sends a body that is not JSON', async () => {
    const { server, crashes, stops } = make();
    await assert.rejects(server.request('open', { a: '/x/garbage.sql', b: '/x/b.sql' }), /not JSON/);
    // Our own stop is reported as one, never as a crash, and the server is not started again by itself.
    await until(() => stops.length === 1);
    assert.deepStrictEqual([crashes, stops], [[], [false]]);
    // The connection is replaced, not stuck.
    const open = await server.request<OpenResult>('open', { a: '/x/a.sql', b: '/x/b.sql' });
    assert.strictEqual(open.state, 'scanning');
    server.dispose();
  });

  it('fails the request when the server sends JSON that is not a message', async () => {
    const { server, crashes, stops } = make();
    await assert.rejects(server.request('open', { a: '/x/nullbody.sql', b: '/x/b.sql' }), /not JSON/);
    await until(() => stops.length === 1);
    assert.deepStrictEqual([crashes, stops], [[], [false]]);
    server.dispose();
  });

  it('starts again after initialize was rejected, with the current path', async () => {
    let broken = true;
    const { server, crashes } = make({ command: () => (broken ? mockBinary({ MOCK_FAIL_INIT: '1' }) : mockBinary()) });
    await assert.rejects(server.request('open', { a: '/x/a.sql', b: '/x/b.sql' }), /initialize failed/);
    broken = false;
    const open = await server.request<OpenResult>('open', { a: '/x/a.sql', b: '/x/b.sql' });
    assert.strictEqual(open.state, 'scanning');
    // Killing the failed process is not a crash to report.
    assert.deepStrictEqual(crashes, []);
    server.dispose();
  });
});
