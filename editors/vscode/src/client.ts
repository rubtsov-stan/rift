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

// One server process: Content-Length framed JSON-RPC over its stdio.
class Connection {
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

export interface ServerOptions {
  command: () => string;
  args: string[];
  // Waits between attempts after a `busy` answer; the request fails after the last.
  busyDelaysMs?: number[];
  // A process that dies sooner than this after starting is not restarted, so a broken binary does not loop.
  minUptimeMs?: number;
  onNotification: (n: Notification) => void;
  // The process died, or was stopped by us because of what it sent (`deliberate`, never restarted).
  // `restarting` is false when it died right after starting.
  onCrash: (reason: string, restarting: boolean, deliberate: boolean) => void;
  onRestarted: () => void;
  // The restarted process could not be used, an initialize that was refused included.
  onRestartFailed?: (error: Error) => void;
}

// One server process for the window. It starts on the first request and
// restarts after a crash; requests in flight when it dies are rejected.
export class Server {
  private conn?: Promise<Connection>;
  private disposed = false;
  limits?: Limits;

  constructor(private opts: ServerOptions) {}

  private connect(): Promise<Connection> {
    this.conn ??= this.start();
    return this.conn;
  }

  private start(): Promise<Connection> {
    const conn = new Promise<Connection>((resolve, reject) => {
      let rejected = false;
      const c = new Connection(
        this.opts.command(),
        this.opts.args,
        (n) => this.opts.onNotification(n),
        (reason, deliberate) => {
          reject(new Error(reason));
          // Killed below after a failed initialize: not a crash.
          if (this.disposed || rejected) return;
          this.conn = undefined;
          // We ended it ourselves, so the next request starts a new one.
          const restarting = !deliberate && Date.now() - c.startedAt >= (this.opts.minUptimeMs ?? 2000);
          this.opts.onCrash(reason, restarting, deliberate);
          if (restarting) this.connect().then(() => this.opts.onRestarted(), (e) => this.opts.onRestartFailed?.(e));
        },
      );
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

  async request<T>(method: string, params: object): Promise<T> {
    const delays = this.opts.busyDelaysMs ?? [150, 300, 600, 1200];
    for (let attempt = 0; ; attempt++) {
      try {
        return await (await this.connect()).request<T>(method, params);
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
