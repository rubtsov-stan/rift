import * as assert from 'assert';
import * as fs from 'fs';
import * as os from 'os';
import * as path from 'path';
import * as vscode from 'vscode';
import { Api } from '../../extension';
import { mockBinary, until } from '../mock';
import { api, captureMessages, clearRequests, closeAll, compare, describeMock, diffTab, requests } from './helpers';

const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));

describeMock('after a server crash', () => {
  let x: Api;
  let msg: ReturnType<typeof captureMessages>;
  before(async () => (x = await api()));
  beforeEach(() => (msg = captureMessages()));
  afterEach(async () => {
    msg.restore();
    await vscode.workspace.getConfiguration('rift').update('mem', undefined, vscode.ConfigurationTarget.Global);
    await closeAll(x);
  });

  const kill = async () => process.kill((await x.serverPid())!, 'SIGKILL');

  // A server process that has run long enough to be restarted after a crash, with a.sql as session 1.
  async function fresh(): Promise<void> {
    await compare('a.sql', 'b.sql');
    await until(() => x.comparison()?.state === 'ready');
    await sleep(2100);
    await kill();
    await until(() => x.comparison()?.state === 'ready' && x.comparison()!.session === 1 && x.comparison()!.segments.length > 0);
    await sleep(2100);
  }

  // slowsum.sql is session 2, and answers its summary 600 ms late: the second compare of it
  // is inside its loads when the server dies.
  async function crashWhileLoading(): Promise<{ old: number; sent: number }> {
    await fresh();
    await compare('slowsum.sql', 'b.sql');
    await until(() => x.comparison()?.state === 'ready' && x.comparison()!.segments.length > 0, 3000);
    const old = x.comparison()!.session!;
    clearRequests();
    const again = compare('slowsum.sql', 'b.sql');
    // Its summary is asked and not yet answered.
    await until(() => requests().some((r) => r.method === 'summary'));
    const sent = requests().length;
    await kill();
    await again;
    await until(() => x.comparison()?.state === 'ready' && x.comparison()!.segments.length > 0, 5000);
    return { old, sent };
  }

  it('sends nothing on an id of the dead process, and shows the comparison reopened', async () => {
    const { old, sent } = await crashWhileLoading();
    // The new process knows only the ids it issued itself, and does not issue the old id before an open.
    assert.deepStrictEqual(requests().slice(sent).filter((r) => r.params.session === old && r.method !== 'open'), []);
    assert.strictEqual(x.comparison()!.state, 'ready');
    await vscode.commands.executeCommand('rift.openSegment', 3);
    assert.ok(diffTab(), 'a hunk opens');
  });

  it('does not close the session a restart reopened when a compare was handed it first', async () => {
    await compare('freshlag.sql', 'b.sql');
    await until(() => x.comparison()?.state === 'ready' && x.comparison()!.segments.length > 0);
    await sleep(2100);
    const old = await x.serverPid();
    await kill();
    let pid = old;
    while (pid === old) {
      await sleep(2);
      pid = await x.serverPid();
    }
    // The reopen's open created the session and is answered late; this compare is handed it at once.
    await compare('freshlag.sql', 'b.sql');
    await sleep(400);
    const c = x.comparison()!;
    await until(() => c.state === 'ready' && c.segments.length > 0, 3000);
    await vscode.commands.executeCommand('rift.openSegment', c.segments[0].index);
    assert.ok(diffTab(), 'a hunk opens');
  });

  it('fails visibly when the restarted server refuses to initialize', async () => {
    await fresh();
    const flag = path.join(fs.mkdtempSync(path.join(os.tmpdir(), 'rift-flag-')), 'fail');
    const cfg = vscode.workspace.getConfiguration('rift');
    try {
      await cfg.update('path', mockBinary({ MOCK_FAIL_INIT_FILE: flag }), vscode.ConfigurationTarget.Global);
      await sleep(100);
      fs.writeFileSync(flag, '');
      await kill();
      await until(() => x.comparison()?.state === 'failed', 4000);
      assert.ok(msg.error.some((e) => /initialize failed/.test(e)));
    } finally {
      await api();
    }
  });

  it('points to rift.path when the server cannot start', async () => {
    await compare('a.sql', 'b.sql');
    await until(() => x.comparison()?.state === 'ready');
    try {
      await vscode.workspace.getConfiguration('rift').update('path', '/nonexistent/rift', vscode.ConfigurationTarget.Global);
      await kill();
      // The next compare starts the binary that is not there.
      await until(() => msg.warning.length > 0);
      await compare('c.sql', 'b.sql');
      await until(() => msg.error.some((e) => /cannot start .*Set rift\.path/.test(e)), 4000);
    } finally {
      await api();
    }
  });

  it('shows the next compare after that crash, not the comparison of the same id', async () => {
    await crashWhileLoading();
    await compare('c.sql', 'b.sql');
    await until(() => x.comparison()?.a === '/x/c.sql');
    assert.strictEqual(x.comparison()!.a, '/x/c.sql');
  });

  it('reopens a pair when the server dies during its rerun', async () => {
    await fresh();
    await vscode.workspace.getConfiguration('rift').update('mem', '512K', vscode.ConfigurationTarget.Global);
    await compare('lag-r.sql', 'b.sql');
    await until(() => x.comparison()?.state === 'failed');
    const errors = msg.error.length;
    clearRequests();
    const rerun = vscode.commands.executeCommand('rift.rerunWithMem');
    await sleep(30);
    await kill();
    await rerun;
    assert.ok(msg.warning.some((w) => /Restarting/.test(w)));
    await until(() => x.comparison()!.state !== 'failed', 4000);
    // One message for the crash, and the pair is reopened with the mem the rerun asked for.
    assert.strictEqual(msg.error.length, errors);
    const opens = () => requests().filter((r) => r.method === 'open');
    await until(() => opens().length >= 2);
    assert.strictEqual(opens()[opens().length - 1].params.mem, '16M');
  });

  it('does not reopen a pair it gave up on when the server restarts again', async () => {
    await fresh();
    clearRequests();
    // Dies 2.3 s after each open, so each death is a restart that reopens, until the pair is given up.
    await compare('crashlate.sql', 'b.sql');
    await until(() => x.comparison()?.state === 'failed', 12000);
    await sleep(1500);
    const opens = requests().filter((r) => r.method === 'open' && String(r.params.a).includes('crashlate'));
    assert.strictEqual(opens.length, 2);
  });
});
