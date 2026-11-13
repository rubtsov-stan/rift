import * as assert from 'assert';
import { Link, RpcError, Server } from '../../client';
import { Comparison, Compares } from '../../comparison';
import { Notification } from '../../protocol';

// Explores the orderings of compares, replies, notifications, busy retries and server faults
// against a fake server whose every delivery this test schedules, and checks the lifecycle
// invariants after every step and at every request sent:
//   1. a compare that is not current (shown or pending) sends nothing and is never shown;
//   2. a close never hits a session that the shown or pending comparison holds, nor one of
//      the pair a pending open is still waiting for;
//   3. no request names a session of another server process;
//   4. once everything is delivered, the shown comparison is ready on a live session, or
//      failed with a message; "no longer has" only for a session the server dropped;
//   5. a session the client knows of, that no current comparison holds or may be handed,
//      has a close on the way, and is closed before the current compare sends an open.
// The fake server keys a pair on file identity, as the real one does, so one pair may be
// spelled two ways (R is P with a doubled slash).
// Each path is replayed from the start; every choice within the first DEPTH steps is
// enumerated, later steps take the first event.

const DEPTH = Number(process.env.RIFT_ORDER_DEPTH ?? 9);
// The busy backoff and the close wait of the code under test. Only timers of these delays
// are scheduled by the test; any other, such as one left by an earlier test, runs as usual.
const BACKOFF_MS = 1;
const CLOSE_WAIT_MS = 1500;
const FAULTS = ['crash', 'crash-early', 'crash-refuse', 'unreadable-reply', 'reply-and-unreadable'] as const;
const P = { a: '/p/a', b: '/p/b' };
const Q = { a: '/p/b', b: '/p/a' };
const R = { a: '/p//a', b: '/p//b' };
type Action = typeof P | 'click' | 'close';
const identity = (p: Record<string, unknown>) => `${String(p.a).replace('//', '/')}\0${String(p.b).replace('//', '/')}`;
const BUSY = -32005;
const UNKNOWN = -32006;
const NOT_READY = -32008;

interface Req {
  proc: Proc;
  method: string;
  params: Record<string, unknown>;
  processed: boolean;
  answer?: () => void;
  resolve: (v: unknown) => void;
  reject: (e: Error) => void;
}

interface Sess {
  pair: string;
  state: 'scanning' | 'ready';
  refined: boolean;
  used: number;
  // A ready notice of a dead process, still in the pipe, has been delivered.
  late?: boolean;
}

let world: World;

// One fake `rift serve`: one session per pair, room for one session; a new pair drops the
// least recently used ready session, and answers busy when only a scanning one is left.
class Proc implements Link {
  startedAt = Date.now();
  readonly pid = undefined;
  dead = false;
  sessions = new Map<number, Sess>();
  pairs = new Map<number, string>();
  lastId = 0;

  constructor(
    readonly epoch: number,
    private onNotification: (n: Notification) => void,
    private onClose: (reason: string, deliberate: boolean) => void,
  ) {}

  request<T>(method: string, params: object): Promise<T> {
    world.sent(this, method, params as Record<string, unknown>);
    return new Promise<T>((resolve, reject) => {
      world.reqs.push({ proc: this, method, params: params as Record<string, unknown>, processed: false, resolve: resolve as (v: unknown) => void, reject });
    });
  }

  // As Connection.kill: what waits fails now, the exit is seen on a later turn.
  kill(): void {
    this.die('killed', false, false, true);
  }

  // A crash, or the end of a process the client gave up on: its requests fail, and the close is
  // reported in the same turn, or with `later` once the exit is seen.
  die(reason: string, deliberate: boolean, early: boolean, later = false): void {
    if (this.dead) return;
    this.dead = true;
    this.startedAt = early ? Date.now() : 0;
    for (const r of world.reqs.filter((r) => r.proc === this)) r.reject(new Error(reason));
    world.reqs = world.reqs.filter((r) => r.proc !== this);
    if (later) world.exits.push(() => this.onClose(reason, deliberate));
    else this.onClose(reason, deliberate);
  }

  notify(n: Notification): void {
    this.onNotification(n);
  }

  // What the server answers; an open's effect happens here, its answer later.
  run(method: string, p: Record<string, unknown>): unknown {
    if (method === 'initialize') {
      if (world.refuse) {
        world.refuse = false;
        throw new RpcError(-32603, 'initialize failed');
      }
      return { protocol: 1, server: { name: 'fake', version: '0' }, limits: { max_session_mem: 1 << 30 } };
    }
    if (method === 'open') {
      const pair = identity(p);
      for (const [id, s] of this.sessions) if (s.pair === pair) return { session: id, reused: true, mem_bytes: 1, state: s.state };
      if (this.sessions.size >= 1) {
        const ready = [...this.sessions].filter(([, s]) => s.state === 'ready').sort((x, y) => x[1].used - y[1].used);
        if (!ready.length) throw new RpcError(BUSY, 'busy', { what: 'sessions' });
        this.sessions.delete(ready[0][0]);
        world.evicted.add(`${this.epoch}:${ready[0][0]}`);
      }
      const id = ++this.lastId;
      this.sessions.set(id, { pair, state: 'scanning', refined: false, used: ++world.clock });
      this.pairs.set(id, pair);
      return { session: id, reused: false, mem_bytes: 1, state: 'scanning' };
    }
    const id = p.session as number;
    const s = this.sessions.get(id);
    if (method === 'close') return { closed: this.sessions.delete(id) };
    if (!s) throw new RpcError(UNKNOWN, `no session ${id}`, { session: id });
    s.used = ++world.clock;
    if (method === 'summary') return { state: s.state, a: { bytes: 1 }, b: { bytes: 1 }, mem_bytes: 1, moves: 'off', differing_segments: 1 };
    if (s.state !== 'ready') throw new RpcError(NOT_READY, 'scanning', { session: id });
    const seg = { index: 1, a: { start: 1, count: 1 }, b: { start: 1, count: 1 }, status: 'differs' };
    if (method === 'segments') return { total: 1, offset: 0, segments: [seg] };
    if (method === 'hunk') return { segment: 1, status: 'differs', pending: false, range: { a: seg.a, b: seg.b }, a: { start: 1, total: 1, lines: [] }, b: { start: 1, total: 1, lines: [] } };
    return null;
  }
}

class World {
  procs: Proc[] = [];
  reqs: Req[] = [];
  // Busy backoffs and close waits, fired when the test picks them. A close wait running out
  // is a fault: the server did not answer.
  timers: { f: () => void; fault: boolean }[] = [];
  // Exits of killed processes, not seen yet.
  exits: (() => void)[] = [];
  refuse = false;
  clock = 0;
  learned = new Set<string>();
  evicted = new Set<string>();
  closes = new Set<string>();
  // Sessions whose own process said they are ready.
  announced = new Set<string>();
  // The epoch a close was sent for, as its caller gave it.
  claimed = new WeakMap<object, number | undefined>();
  senders = new WeakMap<object, Comparison>();
  truth = new Map<Comparison, string>();
  violation?: string;
  log: string[] = [];
  w!: Compares;
  server!: Server;

  get proc(): Proc | undefined {
    const p = this.procs[this.procs.length - 1];
    return p && !p.dead ? p : undefined;
  }

  fail(what: string): void {
    this.violation ??= `${what}\n  log: ${this.log.join(' ')}`;
  }

  current(c: Comparison | undefined): boolean {
    return c !== undefined && (c === this.w.shown || c === this.w.pending);
  }

  held(key: string): boolean {
    const p = this.w.pending;
    return this.truth.get(this.w.shown!) === key || (p !== undefined && this.truth.get(p) === key);
  }

  // The current open on the wire that the server could answer with session `id`.
  handedTo(proc: Proc, id: number): boolean {
    return this.reqs.some((r) => r.proc === proc && r.method === 'open' && this.senders.get(r.params) === this.w.pending && identity(r.params) === proc.pairs.get(id));
  }

  sent(proc: Proc, method: string, p: Record<string, unknown>): void {
    const sender = this.senders.get(p);
    this.log.push(`${method}${p.session !== undefined ? `#${proc.epoch}:${p.session}` : ''}`);
    if (method === 'initialize') return;
    if (sender) {
      if (!this.current(sender)) this.fail(`1: a compare that is not current sent ${method}`);
      if (p.session !== undefined && this.truth.get(sender) !== `${proc.epoch}:${p.session}`) this.fail(`3: ${method} on a session not issued to its sender by this process`);
      if ((method === 'segments' || method === 'moves') && !this.announced.has(`${proc.epoch}:${p.session}`)) this.fail(`3: ${method} on ${proc.epoch}:${p.session} before its own process said it is ready`);
      if (method === 'open') {
        for (const id of proc.sessions.keys()) {
          const key = `${proc.epoch}:${id}`;
          if (this.learned.has(key) && !this.held(key)) this.fail(`5: open sent while ${key}, held by nobody, is still open`);
        }
      }
    } else if (method === 'close') {
      const id = p.session as number;
      const epoch = this.claimed.get(p);
      const key = `${epoch}:${id}`;
      if (epoch !== proc.epoch) this.fail(`3: close of ${key} sent to process ${proc.epoch}`);
      else if (!this.learned.has(key)) this.fail(`3: close of ${key}, an id this process never gave the client`);
      else if (this.held(key) || this.handedTo(proc, id)) this.fail(`2: close of ${key}, which a current comparison holds or may be handed`);
      this.closes.add(key);
    } else {
      this.fail(`a request without a sender: ${method}`);
    }
  }

  // Every event that may happen next, in a fixed order.
  events(script: Action[], faults: number): { name: string; run: () => void }[] {
    const out: { name: string; run: () => void }[] = [];
    for (const r of this.reqs) {
      const tag = `${r.method}${r.params.session !== undefined ? `#${r.params.session}` : ''}`;
      if (r.method === 'open' && !r.processed) {
        out.push({ name: `process ${tag}`, run: () => this.process(r) });
      } else {
        out.push({
          name: `answer ${tag}`,
          run: () => {
            if (!r.processed) this.process(r);
            this.reqs.splice(this.reqs.indexOf(r), 1);
            r.answer!();
          },
        });
      }
    }
    const proc = this.proc;
    // A dead process's last notices may still be in the pipe after its exit is seen.
    for (const dead of this.procs.filter((d) => d.dead)) {
      for (const [id, s] of dead.sessions) {
        if (s.state !== 'scanning' || s.late) continue;
        out.push({
          name: `late scan #${dead.epoch}:${id}`,
          run: () => {
            s.late = true;
            this.announced.add(`${dead.epoch}:${id}`);
            dead.notify({ method: 'session_state', params: { session: id, state: 'ready' } });
          },
        });
      }
    }
    if (proc) {
      for (const [id, s] of proc.sessions) {
        if (s.state === 'scanning') {
          out.push({
            name: `scan #${id}`,
            run: () => {
              s.state = 'ready';
              this.announced.add(`${proc.epoch}:${id}`);
              proc.notify({ method: 'session_state', params: { session: id, state: 'ready' } });
            },
          });
        } else if (!s.refined) {
          out.push({
            name: `refine #${id}`,
            run: () => {
              s.refined = true;
              proc.notify({ method: 'progress', params: { session: id, phase: 'refine', done: 1, total: 1 } });
            },
          });
        }
      }
    }
    this.exits.forEach((x, i) =>
      out.push({
        name: 'exit',
        run: () => {
          this.exits.splice(i, 1);
          x();
        },
      }),
    );
    this.timers.forEach((t, i) => {
      if (t.fault && faults <= 0) return;
      out.push({
        name: t.fault ? 'close-timeout' : 'backoff',
        run: () => {
          this.timers.splice(i, 1);
          t.f();
        },
      });
    });
    if (proc && faults > 0) {
      for (const f of FAULTS) {
        if (f === 'unreadable-reply' && !this.reqs.some((r) => r.proc === proc)) continue;
        if (f === 'reply-and-unreadable' && !this.reqs.some((r) => r.proc === proc && r.method !== 'initialize')) continue;
        out.push({ name: f, run: () => this.fault(proc, f) });
      }
    }
    if (script.length) out.push({ name: `user ${typeof script[0] === 'string' ? script[0] : `compare ${script[0] === P ? 'P' : script[0] === R ? 'R' : 'Q'}`}`, run: () => this.act(script.shift()!) });
    return out;
  }

  process(r: Req): void {
    r.processed = true;
    try {
      const v = r.proc.run(r.method, r.params);
      r.answer = () => {
        if (r.method === 'open') {
          const res = v as { session: number; state: string };
          const key = `${r.proc.epoch}:${res.session}`;
          this.learned.add(key);
          if (res.state === 'ready') this.announced.add(key);
          const sender = this.senders.get(r.params);
          if (sender) this.truth.set(sender, key);
        }
        r.resolve(v);
      };
    } catch (e) {
      r.answer = () => r.reject(e as Error);
    }
  }

  // One read holding the answer to `r` and then a frame that is not JSON. As Connection.read:
  // nothing of the read is dispatched, and the stop is reported with the failures.
  readWithJunk(r: Req): void {
    if (!r.processed) this.process(r);
    r.proc.die('the server sent a reply that is not JSON', true, false);
  }

  fault(proc: Proc, f: (typeof FAULTS)[number]): void {
    // As Connection.abort: the stop is reported with the failures, the exit comes later.
    if (f === 'unreadable-reply') return proc.die('the server sent a reply that is not JSON', true, false);
    if (f === 'reply-and-unreadable') return this.readWithJunk(this.reqs.find((r) => r.proc === proc && r.method !== 'initialize')!);
    if (f === 'crash-refuse') this.refuse = true;
    proc.die('rift server exited (SIGKILL)', false, f === 'crash-early');
  }

  act(a: Action): void {
    if (a === 'close') {
      void this.w.close();
    } else if (a === 'click') {
      const c = this.w.shown;
      if (c?.state === 'ready') void c.hunk({ segment: 1 }, 3).catch(() => {});
    } else {
      void this.w.compare(a.a, a.b, '256M');
    }
  }

  // After each step: what is shown changed only to the pending compare, and no known session leaks.
  step(before: { shown?: Comparison; pending?: Comparison }, closing: boolean): void {
    const shown = this.w.shown;
    if (shown !== before.shown && !(shown === undefined && closing) && shown !== before.pending) this.fail('1: a comparison that was not pending was shown');
    const proc = this.proc;
    if (!proc) return;
    for (const id of proc.sessions.keys()) {
      const key = `${proc.epoch}:${id}`;
      const p = this.w.pending;
      const deferred = p !== undefined && !this.truth.has(p);
      if (this.learned.has(key) && !this.held(key) && !deferred && !this.closes.has(key)) this.fail(`5: ${key} is held by nobody and not closed`);
    }
  }

  final(): void {
    if (this.w.pending) return this.fail('4: a compare is still pending with nothing left to deliver');
    const c = this.w.shown;
    const proc = this.proc;
    if (proc) for (const id of proc.sessions.keys()) if (this.truth.get(c!) !== `${proc.epoch}:${id}`) this.fail(`5: ${proc.epoch}:${id} is still open and not shown`);
    if (!c) return;
    const key = this.truth.get(c);
    if (c.state === 'failed') {
      if (!c.error?.message) this.fail('4: failed without a message');
      if (/no longer has/.test(c.error?.message ?? '') && !this.evicted.has(key!)) this.fail(`4: "no longer has" for ${key}, which the server did not drop`);
    } else if (c.state !== 'ready' || !proc || `${proc.epoch}:${c.session}` !== key || !proc.sessions.has(c.session!)) {
      this.fail(`4: shows state ${c.state} on ${key}, which the server does not have`);
    }
  }
}

const drain = () => new Promise((r) => setImmediate(r));

// One path: `choices[i]` picks the event at step i, the first one past the end. Returns how
// many events were open at each step, to enumerate the siblings.
async function run(script: Action[], faults: number, choices: number[]): Promise<{ widths: number[]; violation?: string }> {
  world = new World();
  const left = [...script];
  const server = new Server({
    command: () => 'fake',
    args: [],
    busyDelaysMs: [BACKOFF_MS, BACKOFF_MS],
    minUptimeMs: 1000,
    link: (onN, onClose) => {
      const p = new Proc(world.procs.length + 1, onN, onClose);
      world.procs.push(p);
      return p;
    },
    onNotification: (n, epoch) => world.w.handle(n, epoch),
    onCrash: (reason, restarting, deliberate) => world.w.lost(reason, restarting, deliberate),
  });
  world.server = server;
  const request = server.request.bind(server);
  server.request = ((method: string, params: object, ...rest: [(() => boolean)?, number?, (() => Promise<unknown>)?]) => {
    world.claimed.set(params, rest[1]);
    return request(method, params, ...rest);
  }) as typeof server.request;
  world.w = new Compares(server, {
    change() {},
    failed: (_e, c) => {
      if (!world.current(c)) world.fail('1: a compare that is not current reported a failure');
    },
    // A lost connection is the window's to report, as a stop or a crash, never as the error of
    // the compare it cut short.
    error: (e) => {
      if (!(e instanceof RpcError)) world.fail(`4: the compare showed a lost connection as its error: ${String(e)}`);
    },
  }, CLOSE_WAIT_MS);
  const widths: number[] = [];
  let clicked = false;
  for (let i = 0; ; i++) {
    if (i > 300) {
      world.fail('no end: still going after 300 steps');
      break;
    }
    let evs = world.events(left, faults);
    if (!evs.length) {
      if (clicked) break;
      // Everything delivered: a click must work or say why.
      clicked = true;
      world.act('click');
      await drain();
      evs = world.events(left, faults);
      if (!evs.length) break;
    }
    const k = i < choices.length ? choices[i] : 0;
    if (i < DEPTH) widths.push(evs.length);
    const ev = evs[k];
    world.log.push(`[${ev.name}]`);
    if ((FAULTS as readonly string[]).includes(ev.name) || ev.name === 'close-timeout') faults--;
    const before = { shown: world.w.shown, pending: world.w.pending };
    ev.run();
    await drain();
    world.step(before, ev.name === 'user close');
    if (world.violation) return { widths, violation: world.violation };
  }
  world.final();
  return { widths, violation: world.violation };
}

// Every path that differs in the first DEPTH choices, once each.
export async function explore(script: Action[], faults: number): Promise<{ paths: number; violation?: string }> {
  const saved = [globalThis.setTimeout, globalThis.clearTimeout] as const;
  const send = Object.getOwnPropertyDescriptor(Comparison.prototype, 'send')!;
  globalThis.setTimeout = ((f: () => void, ms: number) => {
    if (ms !== BACKOFF_MS && ms !== CLOSE_WAIT_MS) return saved[0](f, ms);
    world.timers.push({ f, fault: ms === CLOSE_WAIT_MS });
    return f;
  }) as unknown as typeof setTimeout;
  globalThis.clearTimeout = ((h: unknown) => {
    const i = world.timers.findIndex((t) => t.f === h);
    if (i >= 0) world.timers.splice(i, 1);
    else saved[1](h as NodeJS.Timeout);
  }) as unknown as typeof clearTimeout;
  // Ties each request to the comparison that sent it.
  Object.defineProperty(Comparison.prototype, 'send', {
    ...send,
    value: function (this: Comparison, method: string, params: object, ...rest: unknown[]) {
      world.senders.set(params, this);
      return send.value.call(this, method, params, ...rest);
    },
  });
  try {
    let choices: number[] = [];
    for (let paths = 1; ; paths++) {
      const { widths, violation } = await run(script, faults, choices);
      if (violation) return { paths, violation };
      let i = Math.min(widths.length, DEPTH) - 1;
      const next = [...choices];
      while (i >= 0 && (next[i] ?? 0) + 1 >= widths[i]) i--;
      if (i < 0) return { paths };
      next[i] = (next[i] ?? 0) + 1;
      choices = next.slice(0, i + 1);
      for (let j = 0; j < i; j++) choices[j] ??= 0;
    }
  } finally {
    [globalThis.setTimeout, globalThis.clearTimeout] = saved;
    Object.defineProperty(Comparison.prototype, 'send', send);
  }
}

describe('Compare lifecycle orderings', function () {
  this.timeout(600_000);
  const scripts: [string, Action[], number][] = [
    ['P, P again', [P, P], 1],
    ['P, Q, P again', [P, Q, P], 1],
    ['P, then P under another path', [P, R], 1],
    ['P, click, Q', [P, 'click', Q], 1],
    ['P, Q, close', [P, Q, 'close'], 1],
    ['P, close, Q', [P, 'close', Q], 1],
    ['P, P again', [P, P], 2],
  ];
  for (const [name, script, faults] of scripts) {
    it(`holds the invariants in every ordering of ${name}, with at most ${faults} fault${faults > 1 ? 's' : ''}`, async () => {
      const r = await explore(script, faults);
      console.log(`      ${name}: ${r.paths} paths, depth ${DEPTH}`);
      assert.strictEqual(r.violation, undefined);
    });
  }
});
