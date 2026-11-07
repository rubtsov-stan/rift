import { RpcError, Server } from './client';
import {
  Code, HunkResult, MoveBlock, MovesResult, Notification, OpenResult, Progress, Range,
  RpcErrorObject, Segment, SegmentsResult, Summary,
} from './protocol';

export const PAGE = 100;

export type ComparisonState = 'opening' | 'scanning' | 'ready' | 'failed';

export interface Events {
  change(): void;
  // The scan failed; the same error stays in `error`.
  failed(error: RpcErrorObject): void;
}

// One pair of files and its session on the server.
export class Comparison {
  state: ComparisonState = 'opening';
  session?: number;
  summary?: Summary;
  progress?: Progress;
  error?: RpcErrorObject;
  segments: Segment[] = [];
  segmentsTotal = 0;
  moves: MoveBlock[] = [];
  movesTotal = 0;
  // Position in `segments` of the difference last shown, -1 for none.
  current = -1;
  private closed = false;
  private early: Notification[] = [];
  private loadingSegments?: Promise<void>;
  private loadingMoves?: Promise<void>;

  constructor(
    readonly server: Server,
    readonly a: string,
    readonly b: string,
    public mem: string,
    private events: Events,
  ) {}

  // Opens the pair, or replaces a failed session of it. Throws what `open` throws.
  async open(mem = this.mem): Promise<void> {
    this.reset();
    const r = await this.server.request<OpenResult>('open', { a: this.a, b: this.b, mem });
    if (this.closed) return;
    this.mem = mem;
    this.session = r.session;
    this.state = r.state;
    const early = this.early;
    this.early = [];
    for (const n of early) this.handle(n);
    if (r.state === 'ready') await this.ready();
    this.events.change();
  }

  private reset(): void {
    this.session = undefined;
    this.state = 'opening';
    this.summary = this.progress = this.error = undefined;
    this.segments = [];
    this.moves = [];
    this.segmentsTotal = this.movesTotal = 0;
    this.current = -1;
    this.early = [];
  }

  // The server process died. Its sessions are gone.
  lost(message: string, restarting: boolean): void {
    this.reset();
    if (!restarting) {
      this.state = 'failed';
      this.error = { code: -32603, message };
    }
    this.events.change();
  }

  handle(n: Notification): void {
    if (this.closed) return;
    if (this.session === undefined) {
      this.early.push(n);
      return;
    }
    if (n.params.session !== this.session) return;
    switch (n.method) {
      case 'progress':
        this.progress = n.params;
        if (n.params.phase === 'refine' && n.params.done === n.params.total) void this.refreshSummary();
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
      this.events.change();
    }
  }

  loadSegments(): Promise<void> {
    this.loadingSegments ??= (async () => {
      const r = await this.call<SegmentsResult>('segments', { offset: this.segments.length, limit: PAGE });
      if (r) {
        this.segments.push(...r.segments);
        this.segmentsTotal = r.total;
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

  async close(): Promise<void> {
    const session = this.session;
    this.closed = true;
    if (session !== undefined) await this.server.request('close', { session }).catch(() => {});
  }

  // A request on this session. `not_ready` and `unknown_session` mean the
  // session moved on (still scanning, closed, restarted): not an error to show.
  private async call<T>(method: string, params: object): Promise<T | undefined> {
    if (this.session === undefined || this.closed) return undefined;
    try {
      return await this.server.request<T>(method, { session: this.session, ...params });
    } catch (e) {
      if (e instanceof RpcError && (e.code === Code.notReady || e.code === Code.unknownSession)) return undefined;
      if (this.closed || !(e instanceof RpcError)) return undefined;
      throw e;
    }
  }
}
