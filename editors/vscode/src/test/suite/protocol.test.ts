import * as assert from 'assert';
import * as vscode from 'vscode';
import { Api } from '../../extension';
import { until } from '../mock';
import { api, captureMessages, closeAll, compare, itMock, labels, ready, uri } from './helpers';

describe('protocol handling', () => {
  let x: Api;
  let msg: ReturnType<typeof captureMessages>;
  before(async () => (x = await api()));
  beforeEach(() => (msg = captureMessages()));
  afterEach(async () => {
    msg.restore();
    await vscode.workspace.getConfiguration('rift').update('mem', undefined, vscode.ConfigurationTarget.Global);
    await closeAll(x);
  });

  itMock('shows scanning, then ready, and no error for requests made too early', async () => {
    await compare('slow-a.sql', 'b.sql');
    assert.strictEqual(x.comparison()!.state, 'scanning');
    await until(() => labels(x)[0]?.startsWith('Scanning… ') === true);
    // The tree has no segments yet; these ask the server, which answers not_ready.
    await vscode.commands.executeCommand('rift.openSegment', 1);
    await vscode.commands.executeCommand('rift.nextDifference');
    await ready(x);
    assert.deepStrictEqual(msg.error, []);
    assert.strictEqual(labels(x).length, 3);
  });

  itMock('offers a rerun after a scan fails on the budget, and replaces the session', async () => {
    await vscode.workspace.getConfiguration('rift').update('mem', '512K', vscode.ConfigurationTarget.Global);
    await compare('a.sql', 'b.sql');
    await until(() => x.comparison()?.state === 'failed');
    const rows = labels(x);
    assert.match(rows[1], /^Scan failed: /);
    assert.strictEqual(rows[2], 'Rerun with --mem 2560K');
    assert.strictEqual(msg.error.length, 1);
    assert.deepStrictEqual(msg.buttons[0], ['Rerun with --mem 2560K']);
    await vscode.commands.executeCommand('rift.rerunWithMem');
    await ready(x);
    assert.strictEqual(x.comparison()!.mem, '2560K');
  });

  itMock('retries busy and shows no error', async () => {
    await compare('busy-a.sql', 'b.sql');
    await ready(x);
    assert.deepStrictEqual(msg.error, []);
  });

  itMock('shows an error once the busy retries run out, and keeps the old comparison', async () => {
    await compare('a.sql', 'b.sql');
    await ready(x);
    const before = x.comparison();
    await compare('stuck-a.sql', 'b.sql');
    assert.strictEqual(msg.error.length, 1);
    assert.match(msg.error[0], /no room/);
    assert.strictEqual(x.comparison(), before);
  });

  it('shows the server error for a missing file', async () => {
    await compare('missing.sql', 'b.sql');
    assert.match(msg.error[0], /No such file|not found|does not exist/i);
    assert.strictEqual(x.comparison(), undefined);
  });

  it('compares the two files selected in the Explorer', async () => {
    await vscode.commands.executeCommand('rift.compareSelected', uri('a.sql'), [uri('a.sql')]);
    assert.match(msg.info[0], /exactly two files/);
    await vscode.commands.executeCommand('rift.compareSelected', uri('a.sql'), [uri('a.sql'), uri('b.sql')]);
    await ready(x);
  });

  it('takes both files as one object, as a keybinding passes them', async () => {
    await vscode.commands.executeCommand('rift.compareFiles', { a: uri('a.sql').fsPath, b: uri('b.sql').fsPath });
    await ready(x);
  });

  it('closes the session with the comparison', async () => {
    await compare('a.sql', 'b.sql');
    await ready(x);
    await vscode.commands.executeCommand('rift.closeComparison');
    assert.strictEqual(x.comparison(), undefined);
    assert.deepStrictEqual(x.tree.getChildren(), []);
  });

  it('warns when the server crashes, restarts it and reopens the pair', async () => {
    await compare('a.sql', 'b.sql');
    await ready(x);
    // A process that dies right after starting is not restarted.
    await new Promise((r) => setTimeout(r, 2100));
    process.kill((await x.serverPid())!, 'SIGKILL');
    await until(() => msg.warning.length === 1);
    assert.match(msg.warning[0], /Restarting/);
    await until(() => x.comparison()?.state === 'ready' && x.comparison()!.movesTotal > 0);
    assert.strictEqual(x.comparison()!.session, 1);
  });
});
