import * as assert from 'assert';
import { Link, RpcError, Server, Superseded } from '../../client';
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
function window(env: Record<string, string> = {}, crash?: (c: Compares, reason: string, restarting: boolean) => void, onRestarted?: () => void) {
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
