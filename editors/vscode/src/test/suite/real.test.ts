import * as assert from 'assert';
import * as vscode from 'vscode';
import { Api } from '../../extension';
import { until } from '../mock';
import { api, captureMessages, closeAll, compare, describeReal, diffTab, labels, ready, text } from './helpers';

// Against the real server nothing is known about the pair's exact numbers, so
// these check shapes. The mock-only scenarios (busy retries, lossy and cut
// lines, the fixture's exact counts) live in tree, hunk and protocol tests.
describeReal('real server', () => {
  let x: Api;
  let msg: ReturnType<typeof captureMessages>;
  before(async () => (x = await api()));
  beforeEach(() => (msg = captureMessages()));
  afterEach(async () => {
    msg.restore();
    await vscode.workspace.getConfiguration('rift').update('mem', undefined, vscode.ConfigurationTarget.Global);
    await closeAll(x);
  });

  it('lists the summary, the differences and the moved blocks', async () => {
    await compare('a.sql', 'b.sql');
    await ready(x);
    const root = labels(x);
    assert.match(root[0], /^A [\d,]+ lines · B [\d,]+ lines/);
    assert.match(root[1], /^Differences \([\d,]+\)$/);
    assert.match(root[2], /^Moved blocks \([\d,]+\)$/);
    const [, diffs, moves] = x.tree.getChildren();
    // The first page of 100, then a row that loads the next.
    const segs = labels(x, diffs);
    assert.strictEqual(segs.length, 101);
    assert.match(segs[100], /^Load more… \(100 of [\d,]+\)$/);
    assert.deepStrictEqual(segs.slice(0, 100).filter((l) => !/^A .+ ↔ B .+/.test(l)), []);
    assert.deepStrictEqual(labels(x, moves).filter((l) => !/^A .+ → B .+/.test(l)), [`Load more… (100 of ${x.comparison()!.movesTotal.toLocaleString('en-US')})`]);
    assert.deepStrictEqual(msg.error, []);
  });

  it('opens a segment as a diff of two rift: documents with lines in both', async () => {
    await compare('a.sql', 'b.sql');
    await ready(x);
    await vscode.commands.executeCommand('rift.openSegment', x.comparison()!.segments[0].index);
    const t = diffTab()!;
    assert.match(t.label, /^a ?.* ↔ b ?/);
    assert.strictEqual(t.original.scheme, 'rift');
    assert.strictEqual(t.modified.scheme, 'rift');
    assert.ok((await text(t.original)).length > 1 || (await text(t.modified)).length > 1);
  });

  it('steps through the differences and back', async () => {
    await compare('a.sql', 'b.sql');
    await ready(x);
    const label = () => diffTab()?.label;
    await vscode.commands.executeCommand('rift.nextDifference');
    await until(() => label() !== undefined, 20000);
    const first = label();
    await vscode.commands.executeCommand('rift.nextDifference');
    const second = label();
    assert.notStrictEqual(second, first);
    await vscode.commands.executeCommand('rift.previousDifference');
    assert.strictEqual(label(), first);
  });

  it('opens the block holding a moved line of A and puts the cursor on it', async () => {
    await compare('a.sql', 'b.sql');
    await ready(x);
    const line = x.comparison()!.moves[0].a.start;
    await vscode.commands.executeCommand('rift.openMove', line);
    const t = diffTab()!;
    const ed = vscode.window.visibleTextEditors.find((e) => e.document.uri.toString() === t.original.toString())!;
    const doc = x.docs.lookup(t.original)!;
    assert.strictEqual(ed.selection.active.line, line - doc.start);
  });

  it('offers a rerun after a scan fails on the budget, and replaces the session', async () => {
    await vscode.workspace.getConfiguration('rift').update('mem', '512K', vscode.ConfigurationTarget.Global);
    await compare('a.sql', 'b.sql');
    await until(() => x.comparison()?.state === 'failed', 60000);
    assert.match(labels(x)[1], /^Scan failed: /);
    assert.match(labels(x)[2], /^Rerun with --mem /);
    assert.strictEqual(msg.error.length, 1);
    // The suggestion is the budget the failed step needed. On a 1 GiB pair the
    // next step can fail again, with a slightly larger suggestion, so this checks
    // that the rerun replaced the session, not that it fits.
    await vscode.commands.executeCommand('rift.rerunWithMem');
    await until(() => x.comparison()!.state !== 'opening' && x.comparison()!.state !== 'scanning', 60000);
    assert.notStrictEqual(x.comparison()!.mem, '512K');
    if (x.comparison()!.state === 'failed') assert.match(labels(x)[2], /^Rerun with --mem /);
  });
});
