import * as assert from 'assert';
import { Link, RpcError, Server, Superseded } from '../../client';
import { Comparison, Compares, WindowEvents, crashMessage, failureLabel, rerunOffer } from '../../comparison';
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
function window(env: Record<string, string> = {}, crash?: (c: Compares, reason: string, restarting: boolean) => void, onRestarted?: () => void, failed: (e: RpcErrorObject) => void = () => {}, now?: () => number, extra: Partial<WindowEvents> = {}) {
  const log = path.join(fs.mkdtempSync(path.join(os.tmpdir(), 'rift-log-')), 'log');
  const errors: unknown[] = [];
  const server: Server = new Server({
    command: () => mockBinary({ MOCK_LOG: log, ...env }),
    args: [],
    minUptimeMs: 0,
    onNotification: (n, epoch) => w.handle(n, epoch),
    onCrash: (reason, restarting) => crash?.(w, reason, restarting),
    onRestarted,
  });
  const w: Compares = new Compares(server, { change() {}, failed, error: (e) => errors.push(e), ...extra }, undefined, now);
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

  for (const [name, moves] of [['partial', 'partial'], ['a state it does not know', 'from-the-future']]) {
    it(`loads the moves when the server reports ${name}`, async () => {
      const { server, cmp, failures } = setup(`/x/moves${moves === 'partial' ? 'partial' : 'future'}.sql`);
      await cmp.open();
      await until(() => cmp.state === 'ready' && cmp.movesTotal === 1);
      assert.strictEqual(cmp.summary?.moves, moves);
      assert.deepStrictEqual(failures, []);
      server.dispose();
    });
  }

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

  it('keeps a segment approx after the refinement ends and the pages are read again', async () => {
    const { server, cmp } = setup('/x/settle.sql');
    await cmp.open();
    await until(() => cmp.state === 'ready' && cmp.segments.length > 0);
    // segment_ready marks it, and the re-read after the last progress notice must agree.
    await until(() => cmp.segments[0].status === 'approx');
    await until(() => cmp.progress?.phase === 'refine' && cmp.progress.done === cmp.progress.total);
    // Leave time for the re-read that the last progress notice starts.
    await new Promise((r) => setTimeout(r, 200));
    assert.strictEqual(cmp.segments[0].status, 'approx');
    server.dispose();
  });

  it('shows the approx a page read before the refinement did not have', async () => {
    const { server, cmp } = setup('/x/quiet.sql');
    await cmp.open();
    await until(() => cmp.state === 'ready' && cmp.segments.length > 0);
    assert.strictEqual(cmp.segments[0].status, 'differs');
    await until(() => cmp.segments[0].status === 'approx');
    server.dispose();
  });

  it('reads the page again when the last progress notice comes in the same read as the page', async () => {
    const { server, cmp } = setup('/x/coalesce.sql');
    await cmp.open();
    await until(() => cmp.state === 'ready' && cmp.segments.length > 0);
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

describe('Comparison and a server that keeps crashing', function () {
  // Two mock processes start in it, which takes seconds on a loaded machine.
  this.timeout(10_000);
  it('gives up on a pair after it crashed the server twice, instead of reopening it forever', async () => {
    const { server, w, log } = window({}, (c, reason, restarting) => c.lost(reason, restarting, false));
    await w.compare('/x/crashy.sql', '/x/b.sql', '256M');
    await until(() => w.shown?.state === 'failed');
    // Leave room for a third round, which must not come.
    await new Promise((r) => setTimeout(r, 600));
    const reopens = log().filter((r) => r.method === 'open').length - 1;
    assert.strictEqual(reopens, 1);
    assert.strictEqual(w.shown!.state, 'failed');
    assert.match(w.shown!.error!.message, /exited/);
    server.dispose();
  });
});

describe('Comparison and a server whose replies cannot be read', () => {
  // Every end of a process in these is a stop for a reply that is not JSON.
  const stopped = () => window({}, (c, reason, restarting) => c.lost(reason, restarting, true));

  it('takes no answer that came in one read with a frame that is not JSON: open', async () => {
    const { server, w } = stopped();
    await w.compare('/x/openjunk.sql', '/x/b.sql', '256M');
    await new Promise((r) => setTimeout(r, 500));
    // Given up after two stops, and not left spinning on a session of a stopped process.
    assert.strictEqual(w.pending, undefined);
    assert.strictEqual(w.shown?.state, 'failed');
    assert.strictEqual(w.shown?.session, undefined);
    server.dispose();
  });

  it('takes no answer that came in one read with a frame that is not JSON: segments', async () => {
    const { server, w } = stopped();
    await w.compare('/x/segjunk.sql', '/x/b.sql', '256M');
    await new Promise((r) => setTimeout(r, 800));
    assert.strictEqual(w.shown?.state, 'failed');
    assert.deepStrictEqual(w.shown?.segments, []);
    server.dispose();
  });

  it('stops reopening a pair after its replies stopped the server twice', async () => {
    let starts = 0;
    // Every summary is answered with a body that is not JSON, which stops the process.
    const link = (_n: unknown, onClose: (reason: string, deliberate: boolean) => void): Link => {
      starts++;
      let dead = false;
      const waiting: ((e: Error) => void)[] = [];
      return {
        startedAt: Date.now(),
        pid: undefined,
        kill: () => {},
        request: <T>(method: string) =>
          new Promise<T>((resolve, reject) => {
            waiting.push(reject);
            setImmediate(() => {
              if (dead) return;
              if (method === 'initialize') return resolve({ protocol: 1, limits: { max_session_mem: 1 << 30 } } as T);
              if (method === 'open') return resolve({ session: 1, reused: false, mem_bytes: 1, state: 'ready' } as T);
              dead = true;
              waiting.forEach((r) => r(new Error('the server sent a reply that is not JSON')));
              onClose('the server sent a reply that is not JSON', true);
            });
          }),
      };
    };
    const server: Server = new Server({ command: () => 'fake', args: [], link, onNotification: (n, e) => w.handle(n, e), onCrash: (r, s, d) => w.lost(r, s, d) });
    const w: Compares = new Compares(server, { change() {}, failed() {}, error() {} });
    await w.compare('/a', '/b', '64M');
    await new Promise((r) => setTimeout(r, 300));
    assert.strictEqual(starts, 2);
    assert.strictEqual(w.shown?.state, 'failed');
    assert.match(w.shown!.error!.message, /not JSON/);
  });
});

describe('Comparison crash count', () => {
  const idle = () => new Comparison(undefined as unknown as Server, '/x/a.sql', '/x/b.sql', '256M', { change() {}, failed() {} });

  it('counts a crash that comes long after the open, so a slow loop ends too', () => {
    const cmp = idle();
    const now = Date.now;
    let t = now();
    Date.now = () => t;
    try {
      assert.strictEqual(cmp.lost('exited', true), true);
      t += 12_000;
      assert.strictEqual(cmp.lost('exited', true), false);
    } finally {
      Date.now = now;
    }
  });

  it('starts counting again once the refinement has ended', async () => {
    const { server, cmp } = setup();
    await cmp.open();
    await until(() => cmp.state === 'ready');
    assert.strictEqual(cmp.lost('exited', true), true);
    await cmp.open();
    cmp.handle({ method: 'progress', params: { session: cmp.session!, phase: 'refine', done: 5, total: 5 } });
    assert.strictEqual(cmp.lost('exited', true), true);
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

  it('opens the pair again when a file changed after the open', async () => {
    const { server, w, log } = window();
    await w.compare('/x/changes.sql', '/x/b.sql', '256M');
    const stale = w.shown!;
    await until(() => stale.state === 'ready' && stale.segments.length > 0);
    assert.strictEqual(await stale.hunk({ segment: 1 }, 3), undefined);
    await until(() => w.shown !== stale && w.shown?.state === 'ready');
    assert.notStrictEqual(w.shown!.session, stale.session);
    assert.ok(await w.shown!.hunk({ segment: 1 }, 3));
    await until(() => closes(log()).includes(stale.session));
    assert.strictEqual(log().filter((r) => r.method === 'open').length, 2);
    server.dispose();
  });

  type Log = () => { method: string }[];
  const opens = (log: Log) => log().filter((r) => r.method === 'open').length;
  const ready = (c: Comparison) => until(() => c.state === 'ready' && c.segments.length > 0);
  const CAP_OPENS = 4;
  const gaveUp = (w: Compares) => /keeps changing; compare again when it settles/.test(w.shown?.error?.message ?? '');
  // The server tells the shown session that its file changed, as it does for a request the
  // extension sent for itself; waits for the reopen to replace it or for the give-up. The mock's
  // session is flagged by a request that bypasses the comparison, so the reopen gets a new one.
  async function ownChange(w: Compares, server: Server) {
    const c = w.shown!;
    await ready(c);
    await server.request('hunk', { session: c.session!, segment: 1, context: 3 }).catch(() => {});
    w.handle({ method: 'session_state', params: { session: c.session!, state: 'failed', error: { code: -32009, message: 'changed' } } }, server.epoch!);
    await until(() => w.shown !== c || c.state === 'failed');
  }
  // A click on a hunk, which the server answers with `changed`; waits like ownChange.
  async function userChange(w: Compares) {
    const c = w.shown!;
    await ready(c);
    await c.hunk({ segment: 1 }, 3);
    await until(() => w.shown !== c || c.state === 'failed');
  }
  // Every call of the clock is one step of this many ms, so a reopen's ready and the change it
  // meets, the two reads of the clock, are `step` apart.
  const stepping = (step: number) => {
    let t = 0;
    return () => (t += step);
  };

  it('stops reopening a pair whose file changes under every request of the extension', async () => {
    const { server, w, log } = window();
    await w.compare('/x/rewritten.sql', '/x/b.sql', '256M');
    await until(() => gaveUp(w));
    assert.strictEqual(opens(log), CAP_OPENS);
    server.dispose();
  });

  it('stops reopening when each scan outlasts the window', async () => {
    // The clock passes a minute with every open, as it does during a scan of a large file.
    const ref: { log?: Log } = {};
    const { server, w, log } = window({}, undefined, undefined, undefined, () => opens(ref.log!) * 61_000);
    ref.log = log;
    await w.compare('/x/rewritten.sql', '/x/b.sql', '256M');
    await until(() => gaveUp(w));
    assert.strictEqual(opens(log), CAP_OPENS);
    server.dispose();
  });

  it('reopens freely after a session stayed unchanged for the window', async () => {
    // Three reopens at once, then a change a window after a session was ready.
    let calls = 0;
    const { server, w, log } = window({}, undefined, undefined, undefined, () => (++calls <= 6 ? 0 : calls * 61_000));
    await w.compare('/x/rewritten.sql', '/x/b.sql', '256M');
    await until(() => opens(log) >= CAP_OPENS + 2);
    assert.strictEqual(w.shown!.state === 'failed', false);
    server.dispose();
  });

  it('never gives up a pair whose changes are each a window apart', async () => {
    const { server, w, log } = window({}, undefined, undefined, undefined, stepping(61_000));
    await w.compare('/x/rewritten.sql', '/x/b.sql', '256M');
    await until(() => opens(log) >= 7);
    assert.strictEqual(w.shown!.state === 'failed', false);
    server.dispose();
  });

  it('counts a clock set back as a window that is over', async () => {
    // The change reads a clock a day before the one that stamped the session.
    let calls = 0;
    const { server, w, log } = window({}, undefined, undefined, undefined, () => (calls++ % 2 === 0 ? 86_400_000 : 0));
    await w.compare('/x/rewritten.sql', '/x/b.sql', '256M');
    await until(() => opens(log) >= 7);
    assert.strictEqual(w.shown!.state === 'failed', false);
    server.dispose();
  });

  it('reopens a pair again and again for the changes the user meets', async () => {
    let told = 0;
    const { server, w, log } = window({}, undefined, undefined, undefined, undefined, { keepsChanging: () => told++ });
    await w.compare('/x/changesalways.sql', '/x/b.sql', '256M');
    for (let i = 0; i < 6; i++) await userChange(w);
    assert.deepStrictEqual([opens(log), w.shown!.state === 'failed', told], [7, false, 0]);
    server.dispose();
  });

  it('lets a change the user met end the run before it', async () => {
    const { server, w, log } = window();
    await w.compare('/x/changesalways.sql', '/x/b.sql', '256M');
    await ownChange(w, server);
    await ownChange(w, server);
    await userChange(w);
    for (let i = 0; i < 3; i++) await ownChange(w, server);
    // Three more reopens of the extension's own after the click, and no give-up yet.
    assert.deepStrictEqual([opens(log), w.shown!.state === 'failed'], [7, false]);
    await ownChange(w, server);
    assert.ok(gaveUp(w));
    server.dispose();
  });

  it('tells the window once when it gives a pair up', async () => {
    let told = 0;
    const { server, w } = window({}, undefined, undefined, undefined, undefined, { keepsChanging: () => told++ });
    await w.compare('/x/rewritten.sql', '/x/b.sql', '256M');
    await until(() => gaveUp(w));
    assert.deepStrictEqual([gaveUp(w), told], [true, 1]);
    server.dispose();
  });

  it('shows the message when the cap trips on a compare that is still pending', async () => {
    // The change reaches the reopen before its open is answered and before the slow scan ends, so it is pending when it is given up.
    const seen: { pending?: string; shown?: string }[] = [];
    const { server, w } = window({}, undefined, undefined, undefined, undefined, { change: () => seen.push({ pending: w.pending?.error?.message, shown: w.shown?.error?.message }) });
    const seed = new Comparison(server, '/x/lag-slow-changesalways.sql', '/x/b.sql', '256M', { change() {}, failed() {} });
    seed.reopens = 3;
    const opening = w.compare('/x/lag-slow-changesalways.sql', '/x/b.sql', '256M', seed);
    await until(() => w.pending !== undefined && server.epoch! > 0);
    w.handle({ method: 'session_state', params: { session: 1, state: 'failed', error: { code: -32009, message: 'changed' } } }, server.epoch!);
    await opening;
    assert.ok(gaveUp(w));
    assert.ok(seen.some((e) => e.shown), 'a change event saw the message on the shown comparison');
    assert.deepStrictEqual(seen.filter((e) => e.pending && !e.shown), [], 'no change event sees the message before the swap');
    server.dispose();
  });

  it('leaves nothing of the notices it stopped replaying when it gave the pair up', async () => {
    const { server, w } = window();
    const seed = new Comparison(server, '/x/lag-slow-changesalways.sql', '/x/b.sql', '256M', { change() {}, failed() {} });
    seed.reopens = 3;
    const opening = w.compare('/x/lag-slow-changesalways.sql', '/x/b.sql', '256M', seed);
    await until(() => w.pending !== undefined && server.epoch! > 0);
    const epoch = server.epoch!;
    w.handle({ method: 'session_state', params: { session: 1, state: 'failed', error: { code: -32009, message: 'changed' } } }, epoch);
    w.handle({ method: 'progress', params: { session: 1, phase: 'scan', done: 1, total: 2 } }, epoch);
    await opening;
    assert.ok(gaveUp(w));
    assert.strictEqual(w.shown!['early'].size, 0);
    server.dispose();
  });

  it('does not reopen a pair it gave up when the server crashes', async () => {
    const { server, w, log } = window();
    await w.compare('/x/rewritten.sql', '/x/b.sql', '256M');
    await until(() => gaveUp(w));
    assert.deepStrictEqual(w.lost('exited', true, false), { reopened: false, givenUp: true });
    await new Promise((r) => setTimeout(r, 300));
    assert.strictEqual(opens(log), CAP_OPENS);
    assert.ok(gaveUp(w));
    server.dispose();
  });

  it('reopens a pair that failed with changed outside the cap when the server crashes, and keeps its label', async () => {
    const { server, w, log } = window();
    await w.compare('/x/b.sql', '/x/a.sql', '256M');
    const c = w.shown!;
    await ready(c);
    // Any other path that ends a pair with this code, `open()`'s catch for one.
    const error = { code: -32009, message: 'a: file changed since it was opened; open the files again' };
    c.gone(error);
    assert.strictEqual(failureLabel(c.error, c.givenUp), `Scan failed: ${error.message}`);
    assert.deepStrictEqual(w.lost('exited', true, false), { reopened: true, givenUp: false });
    await until(() => opens(log) === 2);
    server.dispose();
  });

  it('buffers no notice for a pair it gave up', async () => {
    const { server, w } = window();
    await w.compare('/x/rewritten.sql', '/x/b.sql', '256M');
    await until(() => gaveUp(w));
    w.handle({ method: 'progress', params: { session: 99, phase: 'scan', done: 1, total: 2 } }, server.epoch!);
    assert.strictEqual(w.shown!['early'].size, 0);
    server.dispose();
  });

  it('takes a change met by a user request that joined a load the extension started as a user change', async () => {
    const users: boolean[] = [];
    const ref: { cmp?: Comparison } = {};
    const server = new Server({ command: mockBinary, args: [], onNotification: (n) => ref.cmp?.handle(n), onCrash: () => {}, onRestarted: () => {} });
    const cmp = new Comparison(server, '/x/rewritten.sql', '/x/b.sql', '256M', { change() {}, failed() {}, changed: (user) => users.push(user) });
    ref.cmp = cmp;
    await cmp.open();
    // The extension's own load is in flight when the user asks for the next page.
    void cmp.loadSegments();
    void cmp.loadSegments(true);
    await until(() => users.length > 0);
    assert.deepStrictEqual(users, [true]);
    server.dispose();
  });

  it('starts the count again when the user compares the files again and the server reuses the session', async () => {
    const { server, w, log } = window();
    await w.compare('/x/changesalways.sql', '/x/b.sql', '256M');
    await ownChange(w, server);
    await ownChange(w, server);
    const kept = w.shown!;
    await ready(kept);
    // The session of the pair is not flagged: the server answers the open with it.
    await w.compare('/x/changesalways.sql', '/x/b.sql', '256M');
    assert.deepStrictEqual([w.shown === kept, kept.reopens, opens(log)], [true, 0, 4]);
    // A full run of three more reopens, then the give-up.
    for (let i = 0; i < 3; i++) await ownChange(w, server);
    assert.strictEqual(w.shown!.state === 'failed', false);
    await ownChange(w, server);
    assert.ok(gaveUp(w));
    server.dispose();
  });

  it('leaves a replaced comparison alone once the replay found it no longer live', async () => {
    const { server, w } = window();
    // The open is answered ready, by the session the first one made.
    await w.compare('/x/lag-reuse.sql', '/x/b.sql', '256M');
    await until(() => w.shown?.state === 'ready');
    const opening = w.compare('/x/lag-reuse.sql', '/x/b.sql', '256M');
    await until(() => w.pending !== undefined);
    const old = w.pending!;
    // A change below the cap reopens the pending compare, which replaces it.
    w.handle({ method: 'session_state', params: { session: 1, state: 'failed', error: { code: -32009, message: 'changed' } } }, server.epoch!);
    await opening;
    assert.strictEqual(old.readyAt, undefined);
    server.dispose();
  });

  it('compares a pair that kept changing again when the user asks', async () => {
    const { server, w, log } = window();
    await w.compare('/x/rewritten.sql', '/x/b.sql', '256M');
    await until(() => gaveUp(w));
    assert.strictEqual(opens(log), CAP_OPENS);
    await w.compare('/x/rewritten.sql', '/x/b.sql', '256M');
    // A new count: the compare's own open, then three reopens, then the give-up again.
    await until(() => opens(log) === 2 * CAP_OPENS && gaveUp(w));
    server.dispose();
  });

  it('starts the count again on a rerun', async () => {
    const { server, w, log } = window();
    await w.compare('/x/changesalways.sql', '/x/b.sql', '256M');
    // Two reopens made: one more is left before the cap.
    await ownChange(w, server);
    await ownChange(w, server);
    await w.rerun('256M');
    for (let i = 0; i < 3; i++) await ownChange(w, server);
    // The rerun's own open, then a full budget of three reopens.
    assert.deepStrictEqual([opens(log), w.shown!.state === 'failed'], [3 + 1 + 3, false]);
    await ownChange(w, server);
    assert.ok(gaveUp(w));
    server.dispose();
  });

  it('stops replaying notices once the compare was replaced', async () => {
    const { server, w } = window();
    // A change below the cap reopens the pending compare, which replaces it.
    const opening = w.compare('/x/lag-slow-changesalways.sql', '/x/b.sql', '256M');
    await until(() => w.pending !== undefined && server.epoch! > 0);
    const old = w.pending!;
    const epoch = server.epoch!;
    w.handle({ method: 'session_state', params: { session: 1, state: 'failed', error: { code: -32009, message: 'changed' } } }, epoch);
    w.handle({ method: 'progress', params: { session: 1, phase: 'scan', done: 1, total: 2 } }, epoch);
    await opening;
    assert.strictEqual(old.progress, undefined);
    server.dispose();
  });

  it('keeps a give-up that came with the notices before the open answered ready', async () => {
    const { server, w } = window();
    // The second open is answered ready, by the session the first one made.
    await w.compare('/x/lag-reuse.sql', '/x/b.sql', '256M');
    await until(() => w.shown?.state === 'ready');
    const seed = new Comparison(server, '/x/lag-reuse.sql', '/x/b.sql', '256M', { change() {}, failed() {} });
    seed.reopens = 3;
    const opening = w.compare('/x/lag-reuse.sql', '/x/b.sql', '256M', seed);
    await until(() => w.pending !== undefined);
    w.handle({ method: 'session_state', params: { session: 1, state: 'failed', error: { code: -32009, message: 'changed' } } }, server.epoch!);
    await opening;
    assert.ok(gaveUp(w));
    assert.strictEqual(w.shown!.state, 'failed');
    server.dispose();
  });

  it('leaves a newer compare alone when the shown pair changed', async () => {
    const { server, w } = window();
    await w.compare('/x/changes.sql', '/x/b.sql', '256M');
    const stale = w.shown!;
    await until(() => stale.state === 'ready' && stale.segments.length > 0);
    // Its open is answered 100 ms late, so it is still pending when the hunk fails.
    const newer = w.compare('/x/lag-n.sql', '/x/b.sql', '256M');
    await sleep(10);
    assert.strictEqual(await stale.hunk({ segment: 1 }, 3), undefined);
    await newer;
    await sleep(50);
    assert.strictEqual(w.shown?.a, '/x/lag-n.sql');
    assert.strictEqual(w.pending, undefined);
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

  it('shows no failure when the server drops the shown session for a compare that replaces it', async () => {
    const failures: RpcErrorObject[] = [];
    const { server, w, log } = window({}, undefined, undefined, (e) => failures.push(e));
    await w.compare('/x/a.sql', '/x/b.sql', '256M');
    const shown = w.shown!;
    await until(() => shown.state === 'ready' && shown.movesTotal > 0);
    // The evictor's open drops the shown session on the server and answers 100 ms late.
    const older = w.compare('/x/lag-hog-evictor.sql', '/x/b.sql', '256M');
    await until(() => log().some((r) => r.method === 'open' && String(r.params.a).includes('evictor')));
    // A request of the shown comparison that reaches the server after the eviction, as a
    // late notice's refresh does on a slow machine.
    await shown.hunk({ segment: 1 }, 3);
    await older;
    assert.deepStrictEqual(failures, []);
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

// A fake process with room for one session, as the real server: a new pair drops a ready
// session and answers busy while one scans. A pair whose path holds "hold" scans until its
// open is answered, 30 ms late. `close` is answered after `closeAfterMs`, or never; at 0 in
// the next turn of I/O, as an answer already in the pipe would be. The
// ordering test cannot express this: its scheduler answers every request.
function fakeServer(closeWaitMs?: number, closeAfterMs?: number) {
  let procs = 0;
  let closes = 0;
  let crash = () => {};
  const opens: { a: string; at: number }[] = [];
  const closed: number[] = [];
  const link = (_n: unknown, onClose: (reason: string, deliberate: boolean) => void): Link => {
    procs++;
    const sessions = new Map<number, { pair: string; scanning: boolean }>();
    const waiting = new Set<(e: Error) => void>();
    let last = 0;
    let dead = false;
    crash = () => {
      dead = true;
      onClose('rift server exited (SIGKILL)', false);
    };
    return {
      startedAt: Date.now(),
      pid: undefined,
      // As Connection.kill: what waits fails at once.
      kill: () => {
        if (!dead) setImmediate(() => onClose('killed', false));
        dead = true;
        waiting.forEach((r) => r(new Error('the server was stopped')));
      },
      request: <T>(method: string, params: object) =>
        new Promise<T>((resolve, reject) => {
          waiting.add(reject);
          const p = params as { a: string; b: string; session: number };
          const answer = (v: unknown, ms = 0, then = () => {}): void => {
            setTimeout(() => {
              if (dead) return;
              then();
              resolve(v as T);
            }, ms);
          };
          if (method === 'initialize') return answer({ protocol: 1, limits: { max_session_mem: 1 << 30 } });
          if (method === 'close') {
            closes++;
            if (closeAfterMs === 0) {
              setImmediate(() => {
                if (dead) return;
                closed.push(Date.now());
                resolve({ closed: sessions.delete(p.session) } as T);
              });
            } else if (closeAfterMs !== undefined) {
              answer({ closed: sessions.delete(p.session) }, closeAfterMs, () => closed.push(Date.now()));
            }
            return;
          }
          if (method === 'open') {
            opens.push({ a: p.a, at: Date.now() });
            const pair = `${p.a}\0${p.b}`;
            for (const [id, k] of sessions) if (k.pair === pair) return answer({ session: id, reused: true, mem_bytes: 1, state: 'ready' });
            if ([...sessions.values()].some((k) => k.scanning)) {
              setImmediate(() => {
                if (!dead) reject(new RpcError(-32005, 'busy'));
              });
              return;
            }
            sessions.clear();
            const k = { pair, scanning: pair.includes('hold') };
            sessions.set(++last, k);
            return answer({ session: last, reused: false, mem_bytes: 1, state: 'ready' }, k.scanning ? 30 : 0, () => (k.scanning = false));
          }
          if (method === 'summary') return answer({ state: 'ready', a: { bytes: 1 }, b: { bytes: 1 }, mem_bytes: 1, moves: 'off' });
          if (method === 'segments') return answer({ total: 0, offset: 0, segments: [] });
          answer(null);
        }),
    };
  };
  const server: Server = new Server({ command: () => 'fake', args: [], busyDelaysMs: Array(20).fill(5), link, onNotification: (n, e) => w.handle(n, e), onCrash: (r, s, d) => w.lost(r, s, d) });
  const w: Compares = new Compares(server, { change() {}, failed() {}, error() {} }, closeWaitMs);
  return { w, procs: () => procs, closes: () => closes, opens, closed, crash: () => crash() };
}

describe('Compares and a close the server answers late or never', () => {
  const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));

  it('opens a compare after a close on a new process once the close wait runs out', async () => {
    const { w, procs } = fakeServer(50);
    await w.compare('/p/a', '/p/b', '1M');
    // Its close is never answered, and the next open waits for it.
    void w.close();
    void w.compare('/p/b', '/p/a', '1M');
    await sleep(300);
    assert.strictEqual(w.shown?.a, '/p/b');
    assert.strictEqual(w.shown?.state, 'ready');
    assert.strictEqual(procs(), 2);
  });

  it('opens a compare that replaced two others on a new process once the close wait runs out', async () => {
    const { w, procs } = fakeServer(50);
    await w.compare('/p/x', '/p/b', '1M');
    // The close of the first session is never answered.
    await w.compare('/p/y', '/p/b', '1M');
    void w.compare('/p/z', '/p/b', '1M');
    await sleep(300);
    assert.strictEqual(w.shown?.a, '/p/z');
    assert.strictEqual(w.shown?.state, 'ready');
    assert.strictEqual(procs(), 2);
  });

  it('retries a busy open on a new process once the close wait runs out', async () => {
    const { w, procs } = fakeServer(50);
    // The first open scans until it answers late, while the second is retrying busy.
    void w.compare('/p/hold-a', '/p/b', '1M');
    await sleep(10);
    void w.compare('/p/b', '/p/a', '1M');
    await sleep(400);
    assert.strictEqual(w.shown?.a, '/p/b');
    assert.strictEqual(w.shown?.state, 'ready');
    assert.strictEqual(procs(), 2);
  });

  it('waits for a close that answers late, without stopping the server', async () => {
    // 220 ms is twice the slowest close measured on the real server; the wait is the default.
    const { w, procs, opens, closed } = fakeServer(undefined, 220);
    await w.compare('/p/a', '/p/b', '1M');
    void w.close();
    void w.compare('/p/b', '/p/a', '1M');
    await sleep(500);
    assert.strictEqual(w.shown?.a, '/p/b');
    assert.strictEqual(w.shown?.state, 'ready');
    assert.strictEqual(procs(), 1);
    assert.ok(opens[1].at >= closed[0], 'the open went out once the close was answered');
  });

  it('does not stop the server when the window stalls past the wait and the answer is in', async () => {
    const { w, procs, closes } = fakeServer(50, 0);
    await w.compare('/p/a', '/p/b', '1M');
    void w.close();
    void w.compare('/p/b', '/p/a', '1M');
    for (let i = 0; i < 100 && closes() === 0; i++) await null;
    // A GC pause or another extension: the wait runs out before the answer is read.
    const end = Date.now() + 120;
    while (Date.now() < end);
    await sleep(200);
    assert.strictEqual(procs(), 1);
    assert.strictEqual(w.shown?.a, '/p/b');
    assert.strictEqual(w.shown?.state, 'ready');
  });

  it('does not stop the server for a close nothing waits for, until a compare does', async () => {
    const { w, procs } = fakeServer(50);
    await w.compare('/p/x', '/p/b', '1M');
    // The close of the first session is never answered; nothing waits for it.
    await w.compare('/p/y', '/p/b', '1M');
    await sleep(200);
    assert.strictEqual(procs(), 1);
    assert.strictEqual(w.shown?.a, '/p/y');
    assert.strictEqual(w.shown?.state, 'ready');
    // A compare now waits for that close, long past its wait.
    void w.compare('/p/z', '/p/b', '1M');
    await sleep(200);
    assert.strictEqual(procs(), 2);
    assert.strictEqual(w.shown?.a, '/p/z');
    assert.strictEqual(w.shown?.state, 'ready');
  });

  it('sends nothing for a compare it gave up on while it waited between busy retries', async () => {
    const { w, procs, opens, crash } = fakeServer(50);
    void w.compare('/p/hold-a', '/p/b', '1M');
    await sleep(10);
    void w.compare('/p/b', '/p/a', '1M');
    await sleep(10);
    // Dies right after starting: not restarted, and the busy compare is given up.
    crash();
    const sent = opens.length;
    await sleep(100);
    assert.strictEqual(opens.length, sent);
    assert.strictEqual(procs(), 1);
    assert.strictEqual(w.shown?.a, '/p/b');
    assert.strictEqual(w.shown?.state, 'failed');
    assert.match(w.shown!.error!.message, /exited/);
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
    const { server, w, log } = window({}, undefined, () => (restarted = true));
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

describe('crashMessage', () => {
  const base = { reopen: false, deliberate: false, restarting: true, givenUp: false };
  it('keeps its text for a pair that is reopened', () => {
    assert.strictEqual(crashMessage('exited', { ...base, reopen: true }), 'exited. Restarting it.');
  });
  const kept = ' The pair is not opened again: its file keeps changing.';
  it('adds that the pair is not opened again after a give-up, once, to what the server does', () => {
    assert.strictEqual(crashMessage('exited', { ...base, givenUp: true }), `exited. Restarting it.${kept}`);
    for (const restarting of [true, false]) {
      assert.strictEqual(crashMessage('stopped', { ...base, deliberate: true, restarting, givenUp: true }), `stopped. Compare the files again.${kept}`);
    }
    assert.strictEqual(crashMessage('exited', { ...base, restarting: false, givenUp: true }), `exited. It stopped right after starting, so it is not restarted.${kept}`);
  });
});

describe('failureLabel', () => {
  it('says scan failed for a scan error and gives the message alone for a give-up', () => {
    assert.strictEqual(failureLabel({ code: -32603, message: 'boom' }, false), 'Scan failed: boom');
    assert.strictEqual(failureLabel({ code: -32009, message: 'The file keeps changing; compare again when it settles.' }, true), 'The file keeps changing; compare again when it settles.');
    assert.strictEqual(failureLabel({ code: -32009, message: 'a: file changed' }, false), 'Scan failed: a: file changed');
    assert.strictEqual(failureLabel(undefined, false), 'Scan failed: unknown error');
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
