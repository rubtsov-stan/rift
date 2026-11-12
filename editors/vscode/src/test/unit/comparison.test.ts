import * as assert from 'assert';
import { Server, Superseded } from '../../client';
import { Comparison, Compares, rerunOffer } from '../../comparison';
import { OpenResult, RpcErrorObject } from '../../protocol';
import * as fs from 'fs';
import * as os from 'os';
import * as path from 'path';
import { mockBinary, mockLog, until } from '../mock';

function setup(a = '/x/a.sql', mem = '256M') {
  const ref: { cmp?: Comparison } = {};
  const failures: RpcErrorObject[] = [];
  const server = new Server({
    command: mockBinary,
    args: [],
    onNotification: (n) => ref.cmp?.handle(n),
    onCrash: () => {},
    onRestarted: () => {},
  });
  const cmp = new Comparison(server, a, '/x/b.sql', mem, { change() {}, failed: (e) => failures.push(e) });
  ref.cmp = cmp;
  return { server, cmp, failures };
}

// A window's comparisons on the mock, logging what it is sent.
function window(env: Record<string, string> = {}, onRestarted?: () => void) {
  const log = path.join(fs.mkdtempSync(path.join(os.tmpdir(), 'rift-log-')), 'log');
  const errors: unknown[] = [];
  const server: Server = new Server({
    command: () => mockBinary({ MOCK_LOG: log, ...env }),
    args: [],
    minUptimeMs: 0,
    onNotification: (n, epoch) => w.handle(n, epoch),
    onCrash: () => {},
    onRestarted,
  });
  const w: Compares = new Compares(server, { change() {}, failed() {}, error: (e) => errors.push(e) });
  return { server, w, errors, log: () => mockLog(log) };
}

describe('Comparison', () => {
  it('goes from scanning to ready and loads the first page and the moves', async () => {
    const { server, cmp } = setup();
    await cmp.open();
    assert.ok(cmp.state === 'scanning' || cmp.state === 'ready');
    await until(() => cmp.state === 'ready' && cmp.movesTotal === 1);
    assert.deepStrictEqual(cmp.segments.map((s) => s.index), [1, 3, 5, 7, 9]);
    assert.strictEqual(cmp.summary?.differing_segments, 5);
    server.dispose();
  });

  it('fails a scan that does not fit and keeps the suggestion', async () => {
    const { server, cmp, failures } = setup('/x/a.sql', '512K');
    await cmp.open();
    await until(() => cmp.state === 'failed');
    assert.strictEqual(failures[0].code, -32003);
    assert.ok((failures[0].data as { suggested_mem?: string }).suggested_mem);
    // Reopening the same pair replaces the failed session.
    await cmp.open('64M');
    await until(() => cmp.state === 'ready');
    server.dispose();
  });

  it('marks a segment approx when its refinement ends so', async () => {
    const { server, cmp } = setup();
    await cmp.open();
    // `ready` is set before the first page of segments has arrived.
    await until(() => cmp.state === 'ready' && cmp.segments.length > 0);
    await cmp.hunk({ segment: 1 }, 3);
    await until(() => cmp.segments[0].status === 'approx');
    server.dispose();
  });

  it('says so when the server no longer has the session', async () => {
    const { server, cmp, failures } = setup();
    await cmp.open();
    await until(() => cmp.state === 'ready' && cmp.segments.length > 0);
    // Another open evicted it, as a full server does.
    await server.request('close', { session: cmp.session });
    assert.strictEqual(await cmp.hunk({ segment: 1 }, 3), undefined);
    assert.strictEqual(cmp.state, 'failed');
    assert.strictEqual(failures[0].code, -32006);
    assert.match(failures[0].message, /Compare the files again/);
    server.dispose();
  });

  it('asks nothing of the session once replaced while it loads', async () => {
    const log = path.join(fs.mkdtempSync(path.join(os.tmpdir(), 'rift-log-')), 'log');
    const ref: { cmp?: Comparison } = {};
    const server = new Server({ command: () => mockBinary({ MOCK_LOG: log }), args: [], onNotification: (n) => ref.cmp?.handle(n), onCrash: () => {}, onRestarted: () => {} });
    const cmp = new Comparison(server, '/x/lagsum.sql', '/x/b.sql', '256M', { change() {}, failed() {} });
    ref.cmp = cmp;
    await cmp.open();
    // Ready, and its summary is on the way for 150 ms: replaced now, it must not go on to load segments.
    await until(() => cmp.state === 'ready');
    cmp.live = () => false;
    await new Promise((r) => setTimeout(r, 400));
    assert.deepStrictEqual(mockLog(log).filter((r) => ['segments', 'moves', 'set_visible'].includes(r.method)), []);
    server.dispose();
  });

  it('closes the session a replaced open was given', async () => {
    const { server, w, log } = window();
    const opening = w.compare('/x/lag-c.sql', '/x/b.sql', '256M');
    await until(() => log().some((r) => r.method === 'open'));
    // The answer to `open` is still on its way, and nothing is known to close yet.
    await w.close();
    await opening;
    await until(() => log().some((r) => r.method === 'close'));
    server.dispose();
  });

  it('does not open when the compare is no longer wanted', async () => {
    const { server, cmp } = setup('/x/busy-live.sql');
    cmp.live = () => false;
    await assert.rejects(cmp.open(), Superseded);
    server.dispose();
  });

  it('closes its session on the server', async () => {
    const { server, w } = window();
    await w.compare('/x/a.sql', '/x/b.sql', '256M');
    const session = w.shown!.session;
    await w.close();
    await assert.rejects(server.request('summary', { session }), (e: { code: number }) => e.code === -32006);
    server.dispose();
  });
});

describe('Compares', () => {
  const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));
  const closes = (log: { method: string; params: Record<string, unknown> }[]) => log.filter((r) => r.method === 'close').map((r) => r.params.session);

  it('closes what the shown comparison does not use', async () => {
    const { server, w, log } = window();
    await w.compare('/x/a.sql', '/x/b.sql', '256M');
    await w.compare('/x/c.sql', '/x/b.sql', '256M');
    await until(() => closes(log()).length > 0);
    assert.deepStrictEqual([closes(log()), w.shown!.session], [[1], 2]);
    server.dispose();
  });

  it('keeps the session of a pair that a compare is opening', async () => {
    const { server, w, log } = window();
    await w.compare('/x/a.sql', '/x/b.sql', '256M');
    // The older open makes session 2 at once and answers 100 ms late, after the newer is sent.
    const older = w.compare('/x/lag-k.sql', '/x/b.sql', '256M');
    await sleep(10);
    await Promise.all([older, w.compare('/x/lag-k.sql', '/x/b.sql', '256M')]);
    await sleep(50);
    assert.deepStrictEqual([closes(log()), w.shown!.session], [[1], 2]);
    server.dispose();
  });

  it('keeps the shown comparison when a newer compare is handed its session', async () => {
    const { server, w } = window();
    await w.compare('/x/a.sql', '/x/b.sql', '256M');
    const first = w.shown;
    // The server reuses the session and sends no segment_ready again, so the first knows more.
    await w.compare('/x/a.sql', '/x/b.sql', '256M');
    assert.strictEqual(w.shown, first);
    server.dispose();
  });

  it('leaves a reused session to the comparison that holds it', async () => {
    const { server, w, log } = window();
    await w.compare('/x/a.sql', '/x/b.sql', '256M');
    await w.compare('/x/a.sql', '/x/b.sql', '256M');
    await sleep(50);
    assert.deepStrictEqual(closes(log()), []);
    server.dispose();
  });

  it('names a session by server process and id', () => {
    const mk = (session: number, epoch: number) => Object.assign(new Comparison(undefined as unknown as Server, 'a', 'b', '1M', { change() {}, failed() {} }), { session, epoch });
    assert.ok(mk(1, 1).sameSession(mk(1, 1)));
    assert.ok(!mk(1, 1).sameSession(mk(1, 2)));
    assert.ok(!mk(1, 1).sameSession(mk(2, 1)));
    assert.ok(!mk(1, 1).sameSession(undefined));
  });
});

describe('Comparison notices before its open answers', () => {
  it('keeps the latest one per kind and session', async () => {
    let answer!: (r: OpenResult) => void;
    const server = {
      epoch: 1,
      request: (method: string) => (method === 'open' ? new Promise((r) => (answer = r)) : Promise.resolve(method === 'segments' ? { total: 0, offset: 0, segments: [] } : undefined)),
    } as unknown as Server;
    const cmp = new Comparison(server, '/x/a.sql', '/x/b.sql', '1M', { change() {}, failed() {} });
    const opening = cmp.open();
    for (let i = 1; i <= 10_000; i++) cmp.handle({ method: 'progress', params: { session: 7, phase: 'scan', done: i, total: 10_000 } }, 1);
    cmp.handle({ method: 'session_state', params: { session: 7, state: 'ready' } }, 1);
    // A burst for another session, which must not push out this one's state.
    for (let i = 1; i <= 10_000; i++) {
      cmp.handle({ method: 'progress', params: { session: 9, phase: 'scan', done: i, total: 10_000 } }, 1);
      cmp.handle({ method: 'segment_ready', params: { session: 7, index: i, status: 'differs' } }, 1);
    }
    cmp.handle({ method: 'progress', params: { session: 7, phase: 'refine', done: 3, total: 5 } }, 1);
    assert.strictEqual((cmp as unknown as { early: Map<string, unknown> }).early.size, 3);
    answer({ session: 7, reused: false, mem_bytes: 1, state: 'scanning' });
    await opening;
    assert.strictEqual(cmp.state, 'ready');
    assert.deepStrictEqual(cmp.progress, { session: 7, phase: 'refine', done: 3, total: 5 });
  });
});

describe('Comparison and a server that restarted', () => {
  it('sends no close for a session of the process that died', async () => {
    let restarted = false;
    const { server, w, log } = window({}, () => (restarted = true));
    await w.compare('/x/a.sql', '/x/b.sql', '256M');
    process.kill((await server.pid())!, 'SIGKILL');
    await until(() => restarted);
    // The new process numbers sessions from 1 as well; this id is not its to close.
    await w.close();
    assert.deepStrictEqual(log().filter((r) => r.method === 'close'), []);
    server.dispose();
  });

  it('takes notices only from the process that issued its session', async () => {
    const cmp = Object.assign(new Comparison(undefined as unknown as Server, '/x/a.sql', '/x/b.sql', '256M', { change() {}, failed() {} }), { session: 5, epoch: 2, state: 'scanning' as const });
    const progress = { method: 'progress', params: { session: 5, phase: 'scan', done: 1, total: 2 } } as const;
    const done = () => cmp.progress?.done;
    cmp.handle(progress, 1);
    assert.strictEqual(done(), undefined);
    cmp.handle(progress, 2);
    assert.strictEqual(done(), 1);
  });
});

describe('rerunOffer', () => {
  const M = 1 << 20;
  const err = (data: Record<string, unknown>, code = -32003) => ({ code, message: 'm', data });
  const adv = (bytes: number, failed = M) => err({ mem_bytes: failed, suggested_mem_bytes: bytes, suggested_mem: 'x' });
  const mem = (bytes: number, max = 1 << 30, reruns = 0) => rerunOffer(adv(bytes), max, reruns);

  it('adds 25% and rounds up to a power of two from 16M', () => {
    assert.deepStrictEqual(mem(5 * M), { kind: 'rerun', mem: '16M' });
    assert.deepStrictEqual(mem(1), { kind: 'rerun', mem: '16M' });
    assert.deepStrictEqual(mem(16 * M), { kind: 'rerun', mem: '32M' });
    assert.deepStrictEqual(mem(30 * M), { kind: 'rerun', mem: '64M' });
  });
  it('keeps 16M when the headroom lands exactly on it, and steps up just above', () => {
    assert.deepStrictEqual(mem(13421772), { kind: 'rerun', mem: '16M' });
    assert.deepStrictEqual(mem(13421773), { kind: 'rerun', mem: '32M' });
  });
  it('reaches 1G and names it in G', () => {
    assert.deepStrictEqual(mem(858993459, 4 * 2 ** 30), { kind: 'rerun', mem: '1G' });
    assert.deepStrictEqual(mem(858993460, 4 * 2 ** 30), { kind: 'rerun', mem: '2G' });
  });
  it('caps at the server limit', () => {
    assert.deepStrictEqual(mem(900 * M, 1 << 30), { kind: 'rerun', mem: '1G' });
    assert.deepStrictEqual(rerunOffer(adv(2 ** 31, 256 * M), 1 << 30, 0), { kind: 'rerun', mem: '1G' });
  });
  it('offers nothing when the capped value is not above the mem that failed', () => {
    assert.deepStrictEqual(rerunOffer(adv(2 ** 31, 1 << 30), 1 << 30, 0), { kind: 'limit', limit: 1 << 30 });
    assert.deepStrictEqual(rerunOffer(adv(900 * M, 64 * M), 32 * M, 0), { kind: 'limit', limit: 32 * M });
  });
  it('does not cap when the limit is unknown', () => {
    assert.deepStrictEqual(rerunOffer(adv(2 ** 31), undefined, 0), { kind: 'rerun', mem: '4G' });
  });
  it('offers nothing without a suggestion or for another error', () => {
    assert.strictEqual(rerunOffer(err({ mem_bytes: 1 }), 1 << 30, 0), undefined);
    assert.strictEqual(rerunOffer(err({ suggested_mem_bytes: 1 }, -32001), 1 << 30, 0), undefined);
    assert.strictEqual(rerunOffer(undefined, 1 << 30, 0), undefined);
  });
  it('offers twice, then points to the memory guide', () => {
    assert.strictEqual(mem(5 * M, 1 << 30, 0)?.kind, 'rerun');
    assert.strictEqual(mem(5 * M, 1 << 30, 1)?.kind, 'rerun');
    assert.deepStrictEqual(mem(5 * M, 1 << 30, 2), { kind: 'guide' });
    assert.deepStrictEqual(mem(5 * M, 1 << 30, 3), { kind: 'guide' });
  });
});
