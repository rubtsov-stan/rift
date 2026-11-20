import { RpcError, Server } from './client';
import {
  Code, HunkResult, MoveBlock, MovesResult, Notification, OpenResult, Progress, Range,
  RpcErrorObject, Segment, SegmentsResult, Summary,
} from './protocol';

export const PAGE = 100;
const MAX_CRASHES = 2;
// The extension sends nothing during a scan, so a change is noticed at ready or after: by the
// summary it loads then, or by a later request. It counts unless the session had been ready and
// unchanged for a minute. A file that changes during every scan is thus counted at each ready, as
// is one that changes again soon after, as a growing log does. Counted from ready, not from the
// reopen, because a scan of a large file outlasts the window. Three full rescans are enough to
// catch one that only changed twice.
const MAX_REOPENS = 3;
const REOPEN_WINDOW_MS = 60_000;
export const KEEPS_CHANGING = 'The file keeps changing; compare again when it settles.';

export type ComparisonState = 'opening' | 'scanning' | 'ready' | 'failed';

export interface Events {
  change(): void;
  // The scan failed; the same error stays in `error`.
  failed(error: RpcErrorObject): void;
  // Awaited before each attempt to send `open`, a retry after `busy` included.
  opening?(): Promise<unknown>;
  // The server answered `open` with `session`, whether or not this comparison is still current.
  opened?(): void;
  // A file changed after the open: the session answers nothing more, and the pair is to be
  // opened again. Called once per comparison. `user` is whether a request the user triggered
  // met the change, rather than one the extension sent for itself.
  changed?(user: boolean): void;
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
  // Reopens of this pair for `changed` since a session of it last stayed unchanged for the
  // window after it was ready, carried into each reopen.
  reopens = 0;
  // The pair was given up because its file keeps changing; the message stays, and no crash reopens it.
  givenUp = false;
  // A user request joined the load in flight: its `changed` is the user's.
  private segmentsUser = false;
  private movesUser = false;
  // When this session became ready, by the clock of the window that opened it.
  readyAt?: number;
  now: () => number = Date.now;
  // Asked before every request, the open included. Nothing is sent and nothing changes once it says no.
  live: () => boolean = () => true;
  // Notices that came before the open answered, the latest per kind, session and process:
  // a `progress` holds the whole count, and a `segment_ready` names a segment not loaded yet,
  // so it is dropped.
  private early = new Map<string, { n: Notification; epoch?: number }>();
  private loadingSegments?: Promise<void>;
  private loadingMoves?: Promise<void>;
  private reopening = false;

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
    // The process that answered is the current one: a process is replaced only after it closed
    // or was stopped, and the replies of either are dropped (see Connection.kill).
    this.session = r.session;
    this.epoch = this.server.epoch;
    this.events.opened?.();
    if (!this.live()) return;
    this.state = r.state;
    const early = [...this.early.values()];
    this.early.clear();
    // Stops once a notice gave the pair up or the compare was replaced: the rest are for a
    // session this comparison no longer has, or has no use for.
    for (const e of early) {
      if (this.session !== r.session || !this.live()) break;
      this.handle(e.n, e.epoch);
    }
    // Given up or replaced in the replay: nothing more to load or to announce.
    if (this.session !== r.session || !this.live()) return;
    // A failure in the replay is final: do not make it ready again.
    if (r.state === 'ready' && this.state === 'ready') await this.ready();
    this.events.change();
  }

  private reset(): void {
    this.session = undefined;
    this.epoch = undefined;
    this.state = 'opening';
    this.summary = this.progress = this.error = undefined;
    this.readyAt = undefined;
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
    if (this.givenUp) return;
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
          void this.refreshStatuses().catch(() => {});
        }
        break;
      case 'session_state':
        if (n.params.error?.code === Code.changed) {
          this.changed(false);
        } else if (n.params.state === 'failed') {
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
    this.readyAt = this.now();
    this.progress = undefined;
    try {
      await this.refreshSummary();
      await this.loadSegments();
      if (this.summary?.moves && this.summary.moves !== 'off' && this.summary.moves !== 'skipped' && (this.summary.moved_blocks ?? 0) > 0) await this.loadMoves();
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

  // Reads the loaded pages again once the refinement is done: `segments` reports
  // approx for a segment whose refinement ended so, whether or not a hunk was asked.
  private async refreshStatuses(): Promise<void> {
    // The page that came with the final notice may not be stored yet.
    await this.loadingSegments?.catch(() => {});
    const session = this.session;
    for (let offset = 0; offset < this.segments.length; offset += PAGE) {
      const r = await this.call<SegmentsResult>('segments', { offset, limit: PAGE });
      if (!r || session !== this.session) return;
      r.segments.forEach((fresh, i) => {
        const seg = this.segments[offset + i];
        if (seg?.index === fresh.index) seg.status = fresh.status;
      });
    }
    this.events.change();
  }

  // `user`: the request answers something the user did, not the opening of the pair.
  loadSegments(user = false): Promise<void> {
    if (user) this.segmentsUser = true;
    this.loadingSegments ??= (async () => {
      const r = await this.call<SegmentsResult>('segments', { offset: this.segments.length, limit: PAGE }, () => this.segmentsUser);
      if (r) {
        this.segments.push(...r.segments);
        this.segmentsTotal = r.total;
        if (r.segments.length) this.setVisible(span(r.segments), this.segmentsUser);
      }
    })().finally(() => {
      this.loadingSegments = undefined;
      this.segmentsUser = false;
    });
    return this.loadingSegments;
  }

  loadMoves(user = false): Promise<void> {
    if (user) this.movesUser = true;
    this.loadingMoves ??= (async () => {
      const r = await this.call<MovesResult>('moves', { offset: this.moves.length, limit: PAGE }, () => this.movesUser);
      if (r) {
        this.moves.push(...r.blocks);
        this.movesTotal = r.total;
      }
    })().finally(() => {
      this.loadingMoves = undefined;
      this.movesUser = false;
    });
    return this.loadingMoves;
  }

  hunk(target: { segment: number } | { line_a: number }, context: number): Promise<HunkResult | undefined> {
    return this.call<HunkResult>('hunk', { ...target, context }, true);
  }

  setVisible(a: Range, user = false): void {
    void this.call('set_visible', { a }, user);
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
  // `user`: whether the request answers something the user did; read when the answer comes, so a
  // user request that joined a load in flight counts.
  private async call<T>(method: string, params: object, user: boolean | (() => boolean) = false): Promise<T | undefined> {
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
      if (e.code === Code.changed && session === this.session) {
        this.changed(typeof user === 'function' ? user() : user);
        return undefined;
      }
      if (e.code === Code.notReady || e.code === Code.unknownSession) return undefined;
      throw e;
    }
  }

  private changed(user: boolean): void {
    if (this.reopening) return;
    this.reopening = true;
    this.events.changed?.(user);
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
  // A pair was given up because its file keeps changing. Once per give-up.
  keepsChanging?(): void;
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
  // The closes on the wire, each marked once its wait ran out, and the pending compare while
  // it waits for them.
  private closes = new Set<{ overdue: boolean }>();
  private gated?: Comparison;

  constructor(
    private server: Server,
    private events: WindowEvents,
    // How long an open waits for a close; past it the process is stopped, its sessions with it.
    // The slowest close measured on `rift serve` took 110 ms (31,000+ closes on an M1 MacBook
    // Air at load 27 to 45, during the scan of a 10 GiB pair; 38.8 ms in another run under
    // load), but the extension host can stall for seconds behind other extensions, and a stop
    // for nothing costs every comparison and a strike against the crash limit.
    private closeWaitMs = 10_000,
    // Wall clock: a monotonic one stops while the machine sleeps. A clock set back makes the
    // window count as over. Injected by tests that must pass minutes.
    private now: () => number = Date.now,
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
        // A shown session the server dropped for the compare that is replacing it is no news: the
        // compare is about to show its own result, and the session is gone because of it.
        if (next.live() && !(next === this.shown && this.pending && e.code === Code.unknownSession)) this.events.failed(e, next);
      },
      opening: async () => {
        // A replaced compare is about to be refused by `live`; it must not touch `wire`.
        if (next !== this.pending) return;
        this.wire = undefined;
        this.settle();
        this.gated = next;
        this.overdue();
        // A close sent while this waits must be answered too.
        for (let c; c !== this.closing; ) await (c = this.closing);
        if (this.gated === next) this.gated = undefined;
        if (next === this.pending) this.wire = next;
      },
      // The compare that replaces it closes the stale session, as for any replaced one. A
      // newer compare of the user's, pending, replaces it anyway and is left alone.
      changed: (user) => {
        if (next !== this.pending && !(next === this.shown && !this.pending)) return;
        // A change met by the user's own request does not count, and ends the run of changes
        // before it: someone is editing and looking, not a writer running ahead of every scan.
        if (user) {
          next.reopens = 0;
        } else if (next.readyAt !== undefined) {
          const age = this.now() - next.readyAt;
          if (age < 0 || age >= REOPEN_WINDOW_MS) next.reopens = 0;
        }
        if (user || next.reopens < MAX_REOPENS) {
          if (!user) next.reopens++;
          void this.compare(next.a, next.b, next.mem, next);
          return;
        }
        // Swapped before `gone`, whose change event is the one that redraws the view with the
        // message, and flagged before it so that redraw reads it as a give-up.
        this.show(next);
        next.givenUp = true;
        next.gone({ code: Code.changed, message: KEEPS_CHANGING });
        this.settle();
        this.events.keepsChanging?.();
      },
      opened: () => {
        if (this.wire === next) this.wire = undefined;
        this.sessions.set(`${next.epoch}:${next.session}`, { id: next.session!, epoch: next.epoch! });
        this.settle();
      },
    });
    // A comparison failed without a session has nothing left to ask: an open of it that still
    // waits for a backoff or for closes is not sent.
    next.live = () => (next === this.pending || next === this.shown) && !(next.state === 'failed' && next.session === undefined);
    next.now = this.now;
    if (from) {
      next.reruns = from.reruns;
      next.crashes = from.crashes;
      next.reopens = from.reopens;
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
    // The kept one answers the user's compare again, which starts the count over.
    else if (!from) prev!.reopens = 0;
    this.settle();
    this.events.change();
  }

  // Opens the shown pair again with `mem`.
  rerun(mem: string): Promise<void> {
    const c = this.shown!;
    c.reruns++;
    c.reopens = 0;
    return this.compare(c.a, c.b, mem, c);
  }

  close(): Promise<void> {
    this.shown = this.pending = undefined;
    this.events.change();
    return this.settle();
  }

  // A target that failed is the one shown, where nothing more is sent for it.
  private show(c: Comparison): void {
    if (c === this.pending) {
      this.pending = undefined;
      this.shown = c;
    }
  }

  // The server process died, or was stopped for what it sent (`deliberate`). Its sessions are
  // gone. The current pair is reopened after a crash that restarts the server, up to its crash
  // limit, and after a stop that cut short a compare still loading. Returns whether it is, and
  // whether the pair was given up for its file, which no crash reopens.
  lost(reason: string, restarting: boolean, deliberate: boolean): { reopened: boolean; givenUp: boolean } {
    const target = this.pending ?? this.shown;
    if (!target) return { reopened: restarting, givenUp: false };
    if (target.givenUp) return { reopened: false, givenUp: true };
    // A stop counts toward the crash limit too: the reopen asks the server the same thing again.
    const again = deliberate ? target === this.pending && ++target.crashes < MAX_CRASHES : target.lost(reason, restarting);
    const error = { code: -32603, message: deliberate ? `${reason}. Compare the files again.` : reason };
    if (this.shown && this.shown !== target) this.shown.gone(again ? undefined : error);
    if (!again) {
      if (deliberate) target.gone(error);
      this.show(target);
    }
    if (again) void this.compare(target.a, target.b, target.mem, target);
    this.settle();
    return { reopened: again, givenUp: false };
  }

  private closeSession(id: number, epoch: number): Promise<void> {
    const c = { overdue: false };
    this.closes.add(c);
    const late = setTimeout(() => {
      c.overdue = true;
      this.overdue();
    }, this.closeWaitMs);
    const done = () => {
      clearTimeout(late);
      this.closes.delete(c);
    };
    return this.server.request('close', { session: id }, undefined, epoch).then(done, done);
  }

  // A close past its wait stops the process, its sessions with it, so that a late close cannot
  // end a session the server hands to the open that waited. Only while an open does wait for
  // it, and only once a turn of I/O has had the chance to bring its answer: a blocked event
  // loop runs a timer late even when the answer is already in the pipe. A close of a process
  // that is gone was rejected and is no longer in `closes`.
  private overdue(): void {
    const stuck = () => this.gated !== undefined && this.gated === this.pending && [...this.closes].some((c) => c.overdue);
    if (!stuck()) return;
    setImmediate(() => {
      if (stuck()) this.server.stop(`the server did not answer close within ${this.closeWaitMs / 1000} s`);
    });
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
        closes.push(this.closeSession(s.id, s.epoch));
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

// The row shown for a failed comparison. A pair given up for `changed` had no scan fail.
export function failureLabel(e: RpcErrorObject | undefined, givenUp: boolean): string {
  return givenUp && e ? e.message : `Scan failed: ${e?.message ?? 'unknown error'}`;
}

// The warning for a server that died. `reopen`: the current pair is opened again with it.
// `givenUp`: the pair was given up because its file keeps changing, so a restart does not
// open it again.
export function crashMessage(reason: string, o: { reopen: boolean; deliberate: boolean; restarting: boolean; givenUp: boolean }): string {
  let text: string;
  if (o.reopen || (o.givenUp && o.restarting && !o.deliberate)) text = `${reason}. Restarting it.`;
  else if (o.deliberate) text = `${reason}. Compare the files again.`;
  else if (o.restarting) text = `${reason}. This comparison crashed it twice, so it is not reopened.`;
  else text = `${reason}. It stopped right after starting, so it is not restarted.`;
  return o.givenUp ? `${text} The pair is not opened again: its file keeps changing.` : text;
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
