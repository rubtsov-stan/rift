import * as assert from 'assert';
import * as fs from 'fs';
import * as os from 'os';
import * as path from 'path';
import { Link, Server, RpcError, Superseded } from '../../client';
import { Notification, OpenResult, Summary } from '../../protocol';
import { mockBinary, mockLog, until } from '../mock';

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

  it('stops retrying busy when the caller stops wanting it during the wait', async () => {
    const log = path.join(fs.mkdtempSync(path.join(os.tmpdir(), 'rift-log-')), 'log');
    const { server } = make({ command: () => mockBinary({ MOCK_LOG: log }) });
    // Wanted for the first attempt, not any more after its wait; the pair answers busy twice.
    let asked = 0;
    await assert.rejects(server.request('open', { a: '/x/busy-w.sql', b: '/x/b.sql' }, () => ++asked <= 2), Superseded);
    assert.strictEqual(mockLog(log).filter((r) => r.method === 'open').length, 1);
    server.dispose();
  });

  it('never sends for a session of a process that is gone, and does not start one for it', async () => {
    const log = path.join(fs.mkdtempSync(path.join(os.tmpdir(), 'rift-log-')), 'log');
    const { server, stops } = make({ command: () => mockBinary({ MOCK_LOG: log }) });
    await server.request('open', { a: '/x/a.sql', b: '/x/b.sql' });
    const epoch = server.epoch;
    await assert.rejects(server.request('open', { a: '/x/garbage.sql', b: '/x/b.sql' }), /not JSON/);
    await until(() => stops.length === 1);
    await assert.rejects(server.request('summary', { session: 1 }, undefined, epoch), Superseded);
    // No process was started for it.
    assert.strictEqual(server.epoch, epoch);
    // A process that came later numbers its sessions from 1 as well, but is another epoch.
    await server.request('open', { a: '/x/a.sql', b: '/x/b.sql' });
    assert.notStrictEqual(server.epoch, epoch);
    await assert.rejects(server.request('summary', { session: 1 }, undefined, epoch), Superseded);
    assert.deepStrictEqual(mockLog(log).filter((r) => r.method === 'summary'), []);
    server.dispose();
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

  it('does not send once the caller stops wanting it while the server starts', async () => {
    const log = path.join(fs.mkdtempSync(path.join(os.tmpdir(), 'rift-log-')), 'log');
    const { server } = make({ command: () => mockBinary({ MOCK_LOG: log }) });
    let asked = 0;
    await assert.rejects(server.request('open', { a: '/x/a.sql', b: '/x/b.sql' }, () => ++asked < 2), Superseded);
    assert.deepStrictEqual(mockLog(log).filter((r) => r.method === 'open'), []);
    server.dispose();
  });

  it('fails with busy after the last retry', async () => {
    const { server } = make();
    await assert.rejects(server.request('open', { a: '/x/stuck.sql', b: '/x/b.sql' }), (e: RpcError) => e.code === -32005);
    server.dispose();
  });

  // A link whose open answers busy with `data` every time, and a clock the waits move.
  async function busyRun(data: Record<string, unknown> | undefined, extra: Partial<ConstructorParameters<typeof Server>[0]> = {}) {
    const waits: number[] = [];
    let opens = 0;
    const link = (): Link => ({
      startedAt: 0,
      request: <T>(method: string) => {
        if (method === 'initialize') return Promise.resolve({ protocol: 1, limits: {} } as T);
        opens++;
        return Promise.reject(new RpcError(-32005, 'no room for another session', data));
      },
      kill() {},
    });
    const server = new Server({ command: () => 'fake', args: [], busyDelaysMs: [7, 7, 7, 7], link, onNotification: () => {}, onCrash: () => {}, ...extra });
    const realTimeout = global.setTimeout;
    const realNow = Date.now;
    let clock = 0;
    Date.now = () => clock;
    global.setTimeout = ((f: () => void, ms: number) => {
      waits.push(ms);
      clock += ms;
      return realTimeout(f, 0);
    }) as typeof setTimeout;
    let error: unknown;
    try {
      await server.request('open', { a: '/x/a', b: '/x/b' });
    } catch (e) {
      error = e;
    } finally {
      global.setTimeout = realTimeout;
      Date.now = realNow;
    }
    return { waits, opens, error: error as RpcError };
  }

  it('waits as long as a busy answer says, in place of its own backoff', async () => {
    const { waits, opens } = await busyRun({ what: 'sessions', retry_after_ms: 120 });
    assert.deepStrictEqual(waits, [120, 120, 120, 120]);
    assert.strictEqual(opens, 5);
  });

  it('keeps a wait to 50 ms at least and 5 s at most', async () => {
    assert.deepStrictEqual((await busyRun({ retry_after_ms: 0 })).waits, [50, 50, 50, 50]);
    assert.deepStrictEqual((await busyRun({ retry_after_ms: 600000 })).waits, [5000]);
  });

  it('stops once the waits would pass the total bound, and says how long to wait', async () => {
    const { opens, error } = await busyRun({ what: 'sessions', retry_after_ms: 4000 }, { busyTotalMs: 9000 });
    // Waits of 4 s: the second ends at 8 s, the third would end at 12 s.
    assert.strictEqual(opens, 3);
    assert.strictEqual(error.code, -32005);
    assert.match(error.message, /no room for another session/);
    assert.match(error.message, /try again in 4 s/);
    assert.deepStrictEqual(error.data, { what: 'sessions', retry_after_ms: 4000 });
  });

  it('keeps its own backoff and the server message when the answer has no time', async () => {
    const { waits, error } = await busyRun(undefined);
    assert.deepStrictEqual(waits, [7, 7, 7, 7]);
    assert.strictEqual(error.message, 'no room for another session');
  });

  it('declares in initialize that it honours retry_after_ms', async () => {
    const sent: object[] = [];
    const link = (): Link => ({
      startedAt: 0,
      request: <T>(_m: string, params: object) => {
        sent.push(params);
        return Promise.resolve({ protocol: 1, limits: {} } as T);
      },
      kill() {},
    });
    const server = new Server({ command: () => 'fake', args: [], link, onNotification: () => {}, onCrash: () => {} });
    await server.pid();
    assert.deepStrictEqual((sent[0] as { capabilities: unknown }).capabilities, { retry_after: true });
  });

  // A link that refuses `capabilities` in initialize as rift serve 0.3.x does, and answers busy
  // with a time after it.
  function oldServerLink(sent: object[]): () => Link {
    return () => ({
      startedAt: 0,
      request: <T>(method: string, params: object) => {
        if (method === 'initialize') {
          sent.push(params);
          if ('capabilities' in params) return Promise.reject(new RpcError(-32602, 'invalid capabilities', { param: 'capabilities' }));
          return Promise.resolve({ protocol: 1, limits: {} } as T);
        }
        return Promise.reject(new RpcError(-32005, 'busy', { what: 'sessions', retry_after_ms: 120 }));
      },
      kill() {},
    });
  }

  it('asks again without capabilities when the server refuses them, and still honours retry_after_ms', async () => {
    const sent: object[] = [];
    const server = new Server({ command: () => 'fake', args: [], busyDelaysMs: [7, 7], link: oldServerLink(sent), onNotification: () => {}, onCrash: () => {} });
    const waits: number[] = [];
    const realTimeout = global.setTimeout;
    global.setTimeout = ((f: () => void, ms: number) => {
      waits.push(ms);
      return realTimeout(f, 0);
    }) as typeof setTimeout;
    try {
      await assert.rejects(server.request('open', { a: '/x/a', b: '/x/b' }), (e: RpcError) => e.code === -32005);
    } finally {
      global.setTimeout = realTimeout;
    }
    assert.strictEqual(sent.length, 2);
    assert.ok(!('capabilities' in sent[1]));
    assert.deepStrictEqual(waits, [120, 120]);
  });

  it('does not ask again when initialize is refused for another reason', async () => {
    let initializes = 0;
    const link = (): Link => ({
      startedAt: 0,
      request: <T>() => {
        initializes++;
        return Promise.reject(new RpcError(-32602, 'invalid protocol', { param: 'protocol' })) as Promise<T>;
      },
      kill() {},
    });
    const server = new Server({ command: () => 'fake', args: [], link, onNotification: () => {}, onCrash: () => {} });
    await assert.rejects(server.request('open', { a: '/x/a', b: '/x/b' }), /invalid protocol/);
    assert.strictEqual(initializes, 1);
  });

  it('works against a server that behaves as 0.3.x and refuses capabilities', async () => {
    const { server } = make({ command: () => mockBinary({ MOCK_OLD_SERVER: '1' }) });
    const open = await server.request<OpenResult>('open', { a: '/x/busy.sql', b: '/x/b.sql' });
    assert.strictEqual(open.state, 'scanning');
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

  it('reports the stop for a reply that is not JSON before what waits fails', async () => {
    const { server, stops } = make();
    // The handler of the failure must find the stop already reported, or it takes it for its own error.
    let reported = -1;
    await server.request('open', { a: '/x/garbage.sql', b: '/x/b.sql' }).catch(() => (reported = stops.length));
    assert.strictEqual(reported, 1);
    server.dispose();
  });

  it('dispatches nothing of a read that holds a frame that is not JSON', async () => {
    const { server, stops } = make();
    // The answer comes in the same write as the junk: taken, its handler would run after the stop.
    let reported = -1;
    await assert.rejects(
      server.request('open', { a: '/x/openjunk.sql', b: '/x/b.sql' }).catch((e) => {
        reported = stops.length;
        throw e;
      }),
      /not JSON/,
    );
    assert.strictEqual(reported, 1);
    server.dispose();
  });

  it('fails the request when the server sends JSON that is not a message', async () => {
    const { server, crashes, stops } = make();
    await assert.rejects(server.request('open', { a: '/x/nullbody.sql', b: '/x/b.sql' }), /not JSON/);
    await until(() => stops.length === 1);
    assert.deepStrictEqual([crashes, stops], [[], [false]]);
    server.dispose();
  });

  it('reports a stop as deliberate, and none once disposed', async () => {
    const { server, crashes, stops } = make();
    await server.request('open', { a: '/x/a.sql', b: '/x/b.sql' });
    server.stop('it stopped answering');
    assert.deepStrictEqual([crashes, stops], [[], [false]]);
    await server.request('open', { a: '/x/a.sql', b: '/x/b.sql' });
    // Closing the window must not raise a warning about the server.
    server.dispose();
    server.stop('it stopped answering');
    assert.deepStrictEqual(stops, [false]);
  });

  it('drops what a stopped process still had in the pipe', async () => {
    const { server } = make();
    const open = await server.request<OpenResult>('open', { a: '/x/a.sql', b: '/x/b.sql' });
    const summary = server.request('summary', { session: open.session }, undefined, server.epoch);
    await new Promise((r) => setImmediate(r));
    // The answer reaches the pipe while the window is busy, and the process is stopped first.
    const end = Date.now() + 300;
    while (Date.now() < end);
    server.stop('it stopped answering');
    await assert.rejects(summary, /stopped/);
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
