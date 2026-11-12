import { RpcError, Server } from './client';
import {
  Code, HunkResult, MoveBlock, MovesResult, Notification, OpenResult, Progress, Range,
  RpcErrorObject, Segment, SegmentsResult, Summary,
} from './protocol';

export const PAGE = 100;
const MAX_CRASHES = 2;

export type ComparisonState = 'opening' | 'scanning' | 'ready' | 'failed';

export interface Events {
  change(): void;
  // The scan failed; the same error stays in `error`.
  failed(error: RpcErrorObject): void;
  // Awaited before each attempt to send `open`, a retry after `busy` included.
  opening?(): Promise<unknown>;
  // The server answered `open` with `session`, whether or not this comparison is still current.
  opened?(): void;
}

// One open of a pair of files and the session the server gave it. A comparison
// opens once; a rerun or a reopen is a new comparison.
export class Comparison {
  state: ComparisonState = 'opening';
  session?: number;
  // The server process that issued `session`; ids restart with each process.
  epoch?: number;
  summary?: Summary;
  progress?: Progress;
  error?: RpcErrorObject;
  segments: Segment[] = [];
  segmentsTotal = 0;
  moves: MoveBlock[] = [];
  movesTotal = 0;
  // Position in `segments` of the difference last shown, -1 for none.
  current = -1;
  // Reruns made after a budget failure of this pair.
  reruns = 0;
  // Crashes since the refinement of this pair last ended, carried into its reopen.
  crashes = 0;
  // Asked before every request, the open included. Nothing is sent and nothing changes once it says no.
  live: () => boolean = () => true;
  // Notices that came before the open answered, the latest per kind, session and process:
  // a `progress` holds the whole count, and a `segment_ready` names a segment not loaded yet,
  // so it is dropped.
  private early = new Map<string, { n: Notification; epoch?: number }>();
  private loadingSegments?: Promise<void>;
  private loadingMoves?: Promise<void>;

  constructor(
    readonly server: Server,
    readonly a: string,
    readonly b: string,
    public mem: string,
    private events: Events,
  ) {}

  // Opens the pair. Throws what `open` throws.
  async open(mem = this.mem): Promise<void> {
    this.reset();
    this.mem = mem;
    let r: OpenResult;
    try {
      r = await this.send<OpenResult>('open', { a: this.a, b: this.b, mem }, undefined, () => this.events.opening?.() ?? Promise.resolve());
    } catch (e) {
      if (!this.live()) throw e;
      // The pair has no session now; show why instead of a spinner.
      this.state = 'failed';
      this.error = e instanceof RpcError ? { code: e.code, message: e.message, data: e.data } : { code: -32603, message: String(e) };
      this.events.change();
      throw e;
    }
    // Nothing can intervene between the answer and this read, so it is the process that answered.
    this.session = r.session;
    this.epoch = this.server.epoch;
    this.events.opened?.();
    if (!this.live()) return;
    this.state = r.state;
    const early = [...this.early.values()];
    this.early.clear();
    for (const e of early) this.handle(e.n, e.epoch);
    if (r.state === 'ready') await this.ready();
    this.events.change();
  }

  private reset(): void {
    this.session = undefined;
    this.epoch = undefined;
    this.state = 'opening';
    this.summary = this.progress = this.error = undefined;
    this.segments = [];
    this.moves = [];
    this.segmentsTotal = this.movesTotal = 0;
    this.current = -1;
    this.early.clear();
  }

  // The session is gone with its server process: wait for a reopen, or show `error`.
  gone(error?: RpcErrorObject): void {
    this.reset();
    if (error) {
      this.state = 'failed';
      this.error = error;
    }
    this.events.change();
  }

  // The server process died. Returns whether the pair is to be reopened: not after it
  // took the server down twice without its refinement ending in between, which a
  // restart would only repeat.
  lost(message: string, restarting: boolean): boolean {
    const again = restarting && ++this.crashes < MAX_CRASHES;
    this.gone(again ? undefined : { code: -32603, message });
    return again;
  }

  // `epoch` is the server process that sent `n`; ids restart with each process.
  handle(n: Notification, epoch?: number): void {
    if (this.session === undefined) {
      // Deleted first, so the replay keeps the order in which the kept ones came.
      const key = `${n.method}:${epoch}:${n.params.session}`;
      this.early.delete(key);
      if (n.method !== 'segment_ready') this.early.set(key, { n, epoch });
      return;
    }
    if (n.params.session !== this.session || (epoch !== undefined && epoch !== this.epoch)) return;
    switch (n.method) {
      case 'progress':
        this.progress = n.params;
        if (n.params.phase === 'refine' && n.params.done === n.params.total) {
          this.crashes = 0;
          void this.refreshSummary();
        }
        break;
      case 'session_state':
        if (n.params.state === 'failed') {
          this.state = 'failed';
          this.error = n.params.error;
          if (n.params.error) this.events.failed(n.params.error);
        } else {
          void this.ready().then(() => this.events.change());
        }
        break;
      case 'segment_ready': {
        const seg = this.segments.find((s) => s.index === n.params.index);
        if (seg) seg.status = n.params.status;
        break;
      }
    }
    this.events.change();
  }

  private async ready(): Promise<void> {
    this.state = 'ready';
    this.progress = undefined;
    try {
      await this.refreshSummary();
      await this.loadSegments();
      if (this.summary?.moves === 'found' && (this.summary.moved_blocks ?? 0) > 0) await this.loadMoves();
    } catch (e) {
      if (!(e instanceof RpcError)) throw e;
      this.state = 'failed';
      this.error = { code: e.code, message: e.message, data: e.data };
      this.events.failed(this.error);
    }
  }

  private async refreshSummary(): Promise<void> {
    const session = this.session;
    const s = await this.call<Summary>('summary', {});
    if (s && session === this.session) {
      this.summary = s;
      if (s.progress) this.progress = s.progress;
      this.events.change();
    }
  }

  loadSegments(): Promise<void> {
    this.loadingSegments ??= (async () => {
      const r = await this.call<SegmentsResult>('segments', { offset: this.segments.length, limit: PAGE });
      if (r) {
        this.segments.push(...r.segments);
        this.segmentsTotal = r.total;
        if (r.segments.length) this.setVisible(span(r.segments));
      }
    })().finally(() => (this.loadingSegments = undefined));
    return this.loadingSegments;
  }

  loadMoves(): Promise<void> {
    this.loadingMoves ??= (async () => {
      const r = await this.call<MovesResult>('moves', { offset: this.moves.length, limit: PAGE });
      if (r) {
        this.moves.push(...r.blocks);
        this.movesTotal = r.total;
      }
    })().finally(() => (this.loadingMoves = undefined));
    return this.loadingMoves;
  }

  hunk(target: { segment: number } | { line_a: number }, context: number): Promise<HunkResult | undefined> {
    return this.call<HunkResult>('hunk', { ...target, context });
  }

  setVisible(a: Range): void {
    void this.call('set_visible', { a });
  }

  // Both hold the same session of the same server process.
  sameSession(o: Comparison | undefined): boolean {
    return o !== undefined && this.session !== undefined && this.session === o.session && this.epoch === o.epoch;
  }

  // Every request of this comparison goes through here, so none is sent once it is not live.
  private send<T>(method: string, params: object, epoch?: number, before?: () => Promise<unknown>): Promise<T> {
    return this.server.request<T>(method, params, () => this.live(), epoch, before);
  }

  // A request on this session. `not_ready` and `unknown_session` mean the
  // session moved on (still scanning, closed, restarted): not an error to show.
  private async call<T>(method: string, params: object): Promise<T | undefined> {
    const { session, epoch } = this;
    if (session === undefined) return undefined;
    try {
      return await this.send<T>(method, { session, ...params }, epoch);
    } catch (e) {
      if (!(e instanceof RpcError) || !this.live()) return undefined;
      if (e.code === Code.unknownSession && session === this.session) {
        this.dropped(e);
        return undefined;
      }
      if (e.code === Code.notReady || e.code === Code.unknownSession) return undefined;
      throw e;
    }
  }

  // The server forgot this session while it was shown, as a full server does for the one
  // idle longest. Without a word every click would do nothing.
  private dropped(e: RpcError): void {
    if (this.state === 'failed') return;
    this.state = 'failed';
    this.error = { code: e.code, message: 'The server no longer has this comparison. Compare the files again.' };
    this.events.failed(this.error);
    this.events.change();
  }
}

export interface WindowEvents {
  change(): void;
  // A scan of the current comparison failed.
  failed(error: RpcErrorObject, c: Comparison): void;
  // The current compare could not open.
  error(e: unknown): void;
}

// The window's comparisons. `shown` is on screen; `pending` is the newest compare, opening
// or loading. Only these two are current: a comparison that is neither sends nothing and
// changes nothing. Each compare, rerun and reopen is a new comparison, so its generation is
// its identity, and a replaced one is simply not current any more.
//
// Every session an open was given, current or not, is noted here, and only `settle` closes
// one: the server keeps one session per pair of files, whatever their paths, and may hand
// the same one to several opens.
export class Compares {
  shown?: Comparison;
  pending?: Comparison;
  private sessions = new Map<string, { id: number; epoch: number }>();
  // The closes on the wire, and the pending compare while its open is. The server matches a
  // pair by file identity, not by path, so an open may be answered with any session it knows,
  // and it runs requests in parallel: a close and the pending open are never on the wire together.
  private closing: Promise<unknown> = Promise.resolve();
  private wire?: Comparison;

  constructor(
    private server: Server,
    private events: WindowEvents,
  ) {}

  handle(n: Notification, epoch: number): void {
    this.pending?.handle(n, epoch);
    this.shown?.handle(n, epoch);
  }

  // Opens the pair and shows it once loaded, unless a newer compare came meanwhile.
  // `from` is the comparison a rerun or a reopen continues.
  async compare(a: string, b: string, mem: string, from?: Comparison): Promise<void> {
    const next: Comparison = new Comparison(this.server, a, b, mem, {
      change: () => this.events.change(),
      failed: (e) => {
        if (next.live()) this.events.failed(e, next);
      },
      opening: async () => {
        // A replaced compare is about to be refused by `live`; it must not touch `wire`.
        if (next !== this.pending) return;
        this.wire = undefined;
        this.settle();
        // A close sent while this waits must be answered too.
        for (let c; c !== this.closing; ) await (c = this.closing);
        if (next === this.pending) this.wire = next;
      },
      opened: () => {
        if (this.wire === next) this.wire = undefined;
        this.sessions.set(`${next.epoch}:${next.session}`, { id: next.session!, epoch: next.epoch! });
        this.settle();
      },
    });
    next.live = () => next === this.pending || next === this.shown;
    if (from) {
      next.reruns = from.reruns;
      next.crashes = from.crashes;
    }
    this.pending = next;
    this.settle();
    try {
      await next.open();
    } catch (e) {
      if (this.wire === next) this.wire = undefined;
      if (next !== this.pending) return;
      this.pending = undefined;
      this.events.error(e);
      // A shown comparison waiting for its reopen waits for this compare, which failed.
      if (this.shown?.state === 'opening') this.shown.gone(next.error);
      this.settle();
      return;
    }
    if (next !== this.pending) return;
    this.pending = undefined;
    const prev = this.shown;
    // The server reuses a session and does not send its progress and segment_ready again, so
    // `prev` knows more and stays; unless it failed, which must not keep its replacement out.
    if (!(next.sameSession(prev) && prev?.state !== 'failed')) this.shown = next;
    this.settle();
    this.events.change();
  }

  // Opens the shown pair again with `mem`.
  rerun(mem: string): Promise<void> {
    const c = this.shown!;
    c.reruns++;
    return this.compare(c.a, c.b, mem, c);
  }

  close(): Promise<void> {
    this.shown = this.pending = undefined;
    this.events.change();
    return this.settle();
  }

  // The server process died, or was stopped for what it sent (`deliberate`). Its sessions are
  // gone. The current pair is reopened after a crash that restarts the server, up to its crash
  // limit, and after a stop that cut short a compare still loading. Returns whether it is.
  lost(reason: string, restarting: boolean, deliberate: boolean): boolean {
    const target = this.pending ?? this.shown;
    if (!target) return restarting;
    // A stop counts toward the crash limit too: a reply the client cannot read comes back on a reopen.
    const again = deliberate ? target === this.pending && target.session !== undefined && ++target.crashes < MAX_CRASHES : target.lost(reason, restarting);
    const error = { code: -32603, message: deliberate ? `${reason}. Compare the files again.` : reason };
    if (deliberate && !again) target.gone(error);
    if (this.shown && this.shown !== target) this.shown.gone(again ? undefined : error);
    if (again) void this.compare(target.a, target.b, target.mem, target);
    this.settle();
    return again;
  }

  // Closes every noted session that no current comparison holds, unless the pending open is
  // on the wire: the server may answer it with any of them. Sessions of a process that is
  // gone are forgotten, never sent.
  settle(): Promise<void> {
    const holds = (c: Comparison | undefined, s: { id: number; epoch: number }) => c?.session === s.id && c.epoch === s.epoch;
    const p = this.pending;
    const sending = p !== undefined && this.wire === p;
    const closes: Promise<unknown>[] = [];
    for (const [key, s] of this.sessions) {
      if (this.server.alive(s.epoch)) {
        if (sending || holds(this.shown, s) || holds(p, s)) continue;
        closes.push(this.server.request('close', { session: s.id }, undefined, s.epoch).catch(() => {}));
      }
      this.sessions.delete(key);
    }
    const done = Promise.all(closes).then(() => {});
    this.closing = Promise.all([this.closing, done]);
    return done;
  }
}

export type RerunOffer = { kind: 'rerun'; mem: string } | { kind: 'limit'; limit: number } | { kind: 'guide' };

export const MEMORY_GUIDE = 'https://rubtsov-stan.github.io/rift/guide/memory/';

// Offers a rerun twice; after that the memory guide, so a pair that keeps
// failing does not loop.
const MAX_RERUNS = 2;
const MIN_MEM = 16 << 20;

function memArg(bytes: number): string {
  for (const [unit, size] of [['G', 2 ** 30], ['M', 2 ** 20], ['K', 1024]] as const) {
    if (bytes % size === 0) return `${bytes / size}${unit}`;
  }
  return String(bytes);
}

// What to offer after a scan failed on the budget: the server's advice plus
// 25%, rounded up to a power of two from 16 MiB and capped at the server's
// per-session limit. Over the limit and no larger than what failed, there is
// nothing to offer. `reruns` counts the reruns already made for this pair.
export function rerunOffer(error: RpcErrorObject | undefined, maxSessionMem: number | undefined, reruns: number): RerunOffer | undefined {
  if (error?.code !== Code.budgetExceeded) return undefined;
  const suggested = error.data?.suggested_mem_bytes;
  if (typeof suggested !== 'number') return undefined;
  if (reruns >= MAX_RERUNS) return { kind: 'guide' };
  let bytes = MIN_MEM;
  while (bytes < Math.ceil(suggested * 1.25)) bytes *= 2;
  if (maxSessionMem !== undefined) bytes = Math.min(bytes, maxSessionMem);
  const failed = error.data?.mem_bytes;
  if (maxSessionMem !== undefined && bytes <= (typeof failed === 'number' ? failed : 0)) return { kind: 'limit', limit: maxSessionMem };
  return { kind: 'rerun', mem: memArg(bytes) };
}

// The A lines from the first to the last of some segments.
function span(segs: Segment[]): Range {
  const start = Math.min(...segs.map((s) => s.a.start));
  const end = Math.max(...segs.map((s) => s.a.start + Math.max(s.a.count, 1)));
  return { start, count: end - start };
}
