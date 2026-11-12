import { ChildProcess, spawn } from 'child_process';
import { InitializeResult, Limits, Notification, RpcErrorObject, Code } from './protocol';

export class RpcError extends Error implements RpcErrorObject {
  constructor(
    readonly code: number,
    message: string,
    readonly data?: Record<string, unknown>,
  ) {
    super(message);
  }
}

// What Server needs of one server process.
export interface Link {
  readonly startedAt: number;
  readonly pid?: number;
  request<T>(method: string, params: object): Promise<T>;
  kill(): void;
}

// One server process: Content-Length framed JSON-RPC over its stdio.
class Connection implements Link {
  private proc: ChildProcess;
  private nextId = 0;
  private waiting = new Map<number, { resolve: (v: never) => void; reject: (e: Error) => void }>();
  private buf: Buffer = Buffer.alloc(0);
  readonly startedAt = Date.now();
  // Set when we end the process because of what it sent: not a crash.
  private aborted?: string;

  constructor(
    command: string,
    args: string[],
    private onNotification: (n: Notification) => void,
    onClose: (reason: string, deliberate: boolean) => void,
  ) {
    this.proc = spawn(command, args, { stdio: ['pipe', 'pipe', 'ignore'] });
    this.proc.stdout!.on('data', (chunk: Buffer) => this.read(chunk));
    // A write to a dead process raises EPIPE here; the exit event reports it.
    this.proc.stdin!.on('error', () => {});
    let closed = false;
    const close = (reason: string) => {
      if (closed) return;
      closed = true;
      for (const w of this.waiting.values()) w.reject(new Error(reason));
      this.waiting.clear();
      onClose(reason, this.aborted !== undefined);
    };
    this.proc.on('error', (e) => close(`cannot start ${command}: ${e.message}`));
    this.proc.on('exit', (code, signal) => close(this.aborted ?? `rift server exited (${signal ?? code})`));
  }

  get pid(): number | undefined {
    return this.proc.pid;
  }

  request<T>(method: string, params: object): Promise<T> {
    const id = ++this.nextId;
    return new Promise<T>((resolve, reject) => {
      this.waiting.set(id, { resolve: resolve as (v: never) => void, reject });
      const body = Buffer.from(JSON.stringify({ jsonrpc: '2.0', id, method, params }), 'utf8');
      this.proc.stdin!.write(`Content-Length: ${body.length}\r\n\r\n`);
      this.proc.stdin!.write(body);
    });
  }

  kill(): void {
    this.proc.kill();
  }

  // Ends the process because its stream cannot be trusted; waiting requests fail with `reason`.
  private abort(reason: string): void {
    this.aborted ??= reason;
    this.kill();
  }

  private read(chunk: Buffer): void {
    this.buf = Buffer.concat([this.buf, chunk]);
    for (;;) {
      const end = this.buf.indexOf('\r\n\r\n');
      if (end < 0) return;
      const m = /^content-length:\s*(\d+)\s*$/im.exec(this.buf.subarray(0, end).toString('ascii'));
      if (!m) {
        this.abort('the server sent a header without Content-Length');
        return;
      }
      const len = Number(m[1]);
      if (this.buf.length < end + 4 + len) return;
      const body = this.buf.subarray(end + 4, end + 4 + len).toString('utf8');
      this.buf = this.buf.subarray(end + 4 + len);
      let msg;
      try {
        msg = JSON.parse(body);
      } catch {
        msg = undefined;
      }
      if (typeof msg !== 'object' || msg === null || Array.isArray(msg)) {
        this.abort('the server sent a reply that is not JSON');
        return;
      }
      this.dispatch(msg);
    }
  }

  private dispatch(msg: { id?: number; method?: string; params?: unknown; result?: unknown; error?: RpcErrorObject }): void {
    if (msg.id === undefined || msg.id === null) {
      if (msg.method) this.onNotification({ method: msg.method, params: msg.params } as Notification);
      return;
    }
    const w = this.waiting.get(msg.id);
    if (!w) return;
    this.waiting.delete(msg.id);
    if (msg.error) w.reject(new RpcError(msg.error.code, msg.error.message, msg.error.data));
    else w.resolve(msg.result as never);
  }
}

// A request that was not sent, or whose retries stopped, because its caller no longer wants it.
export class Superseded extends Error {
  constructor() {
    super('superseded by a newer request');
  }
}

export interface ServerOptions {
  command: () => string;
  args: string[];
  // Waits between attempts after a `busy` answer; the request fails after the last.
  busyDelaysMs?: number[];
  // A process that dies sooner than this after starting is not restarted, so a broken binary does not loop.
  minUptimeMs?: number;
  // `epoch` is the process that sent it.
  onNotification: (n: Notification, epoch: number) => void;
  // The process died, or was stopped by us because of what it sent (`deliberate`, never restarted).
  // `restarting` is false when it died right after starting.
  onCrash: (reason: string, restarting: boolean, deliberate: boolean) => void;
  onRestarted?: () => void;
  // The restarted process could not be used, an initialize that was refused included.
  onRestartFailed?: (error: Error) => void;
  // Starts a process in place of spawning `command`; tests use it to control delivery order.
  link?: (onNotification: (n: Notification) => void, onClose: (reason: string, deliberate: boolean) => void) => Link;
}

// One server process for the window. It starts on the first request and
// restarts after a crash; requests in flight when it dies are rejected.
export class Server {
  private conn?: Promise<Link>;
  private disposed = false;
  // Counts the processes started. Session ids restart with each process, so a session is
  // named by (epoch, id), and a request for one of an older epoch is never sent.
  private started = 0;
  limits?: Limits;

  constructor(private opts: ServerOptions) {}

  // The epoch of the process that answers now, or of the last one if none runs.
  get epoch(): number {
    return this.started;
  }

  // A process of this epoch still answers.
  alive(epoch: number): boolean {
    return this.conn !== undefined && epoch === this.started;
  }

  private connect(): Promise<Link> {
    this.conn ??= this.start();
    return this.conn;
  }

  private start(): Promise<Link> {
    const epoch = ++this.started;
    const conn = new Promise<Link>((resolve, reject) => {
      let rejected = false;
      const onNotification = (n: Notification) => this.opts.onNotification(n, epoch);
      const onClose = (reason: string, deliberate: boolean) => {
        reject(new Error(reason));
        // Killed below after a failed initialize: not a crash.
        if (this.disposed || rejected) return;
        this.conn = undefined;
        // Its sessions are gone either way. We ended it ourselves, so the next request starts a new one.
        const restarting = !deliberate && Date.now() - c.startedAt >= (this.opts.minUptimeMs ?? 2000);
        this.opts.onCrash(reason, restarting, deliberate);
        if (restarting) this.connect().then(() => this.opts.onRestarted?.(), (e) => this.opts.onRestartFailed?.(e));
      };
      const c = this.opts.link?.(onNotification, onClose) ?? new Connection(this.opts.command(), this.opts.args, onNotification, onClose);
      c.request<InitializeResult>('initialize', { protocol: 1, client: { name: 'rift-vscode', version: '0.3.2' } }).then((r) => {
        this.limits = r.limits;
        resolve(c);
      }, (e) => {
        // Without this the rejection stays cached and the process runs on.
        rejected = true;
        if (this.conn === conn) this.conn = undefined;
        c.kill();
        reject(e);
      });
    });
    // The failure reaches callers through connect(); this only stops an unhandled rejection.
    conn.catch(() => {});
    return conn;
  }

  // `wanted` is asked right before each attempt to send, after a start or a backoff
  // included; once it says no, nothing more is sent and the request fails with `Superseded`.
  // A request about a session passes the `epoch` that issued it: it fails the same way when
  // that process is gone, and never starts a process. `before` is awaited at the start of
  // each attempt.
  async request<T>(method: string, params: object, wanted: () => boolean = () => true, epoch?: number, before?: () => Promise<unknown>): Promise<T> {
    const delays = this.opts.busyDelaysMs ?? [150, 300, 600, 1200];
    const gone = () => epoch !== undefined && !this.alive(epoch);
    for (let attempt = 0; ; attempt++) {
      await before?.();
      if (!wanted() || gone()) throw new Superseded();
      try {
        const conn = await this.connect();
        if (!wanted() || gone()) throw new Superseded();
        return await conn.request<T>(method, params);
      } catch (e) {
        if (!(e instanceof RpcError && e.code === Code.busy) || attempt >= delays.length) throw e;
        await new Promise((r) => setTimeout(r, delays[attempt]));
      }
    }
  }

  async pid(): Promise<number | undefined> {
    return (await this.connect()).pid;
  }

  dispose(): void {
    this.disposed = true;
    this.conn?.then((c) => c.kill(), () => {});
  }
}
