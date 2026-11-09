import * as assert from 'assert';
import * as vscode from 'vscode';
import { Api } from '../../extension';
import { HunkDocs } from '../../hunk';
import { until } from '../mock';
import { api, closeAll, compare, describeMock, diffTab, labels, ready, text } from './helpers';

describeMock('hunk diff', () => {
  let x: Api;
  before(async () => (x = await api()));
  beforeEach(async () => {
    await compare('a.sql', 'b.sql');
    await ready(x);
  });
  afterEach(() => closeAll(x));

  it('opens a segment as a diff of two rift: documents titled with the original ranges', async () => {
    await vscode.commands.executeCommand('rift.openSegment', 3);
    const t = diffTab()!;
    assert.strictEqual(t.label, 'a.sql 20–24 ↔ b.sql before 21');
    assert.strictEqual(t.original.scheme, 'rift');
    assert.strictEqual(t.modified.scheme, 'rift');
    const a = await text(t.original);
    assert.strictEqual(a.length, 11);
    assert.strictEqual((await text(t.modified)).length, 6);
  });

  it('follows segment_ready in the tree', async () => {
    const [, diffs] = x.tree.getChildren();
    await vscode.commands.executeCommand('rift.openSegment', 1);
    await until(() => labels(x, diffs)[0] === 'A 10–11 ↔ B 10–12 | approx');
  });

  it('marks cut lines, lossy lines and the lines it left out', async () => {
    await vscode.commands.executeCommand('rift.openSegment', 7);
    let t = diffTab()!;
    assert.strictEqual(t.label, 'a.sql 45 ↔ b.sql 41 (lossy lines, cut)');
    assert.match((await text(t.original))[3], /… \[line cut, [\d,]+ bytes\]$/);
    const hovers = await vscode.commands.executeCommand<vscode.Hover[]>('vscode.executeHoverProvider', t.modified, new vscode.Position(3, 0));
    assert.match((hovers[0].contents[0] as vscode.MarkdownString).value, /Not valid UTF-8/);

    await vscode.commands.executeCommand('rift.openSegment', 9);
    t = diffTab()!;
    const b = await text(t.modified);
    assert.strictEqual(b[b.length - 1], '… [1,211 more lines not shown]');
    assert.match(t.label, /\(cut\)$/);
  });

  it('opens the block holding a line of A and puts the cursor on it', async () => {
    await vscode.commands.executeCommand('rift.openMove', 22);
    const t = diffTab()!;
    assert.strictEqual(t.label, 'a.sql 20–24 ↔ b.sql before 21');
    const ed = vscode.window.visibleTextEditors.find((e) => e.document.uri.toString() === t.original.toString())!;
    // The excerpt starts at line 17 (3 lines of context), so line 22 is document line 5.
    assert.strictEqual(ed.selection.active.line, 5);
  });

  it('steps through the differences', async () => {
    const label = () => diffTab()?.label.replace(' (approx)', '');
    await vscode.commands.executeCommand('rift.nextDifference');
    await until(() => label() === 'a.sql 10–11 ↔ b.sql 10–12');
    await vscode.commands.executeCommand('rift.nextDifference');
    assert.strictEqual(label(), 'a.sql 20–24 ↔ b.sql before 21');
    await vscode.commands.executeCommand('rift.nextDifference');
    assert.strictEqual(label(), 'a.sql 35–38 ↔ b.sql 31–34');
    await vscode.commands.executeCommand('rift.previousDifference');
    assert.strictEqual(label(), 'a.sql 20–24 ↔ b.sql before 21');
    await vscode.commands.executeCommand('rift.previousDifference');
    await vscode.commands.executeCommand('rift.previousDifference');
    assert.strictEqual(label(), 'a.sql 10–11 ↔ b.sql 10–12');
  });

  it('does not reuse a document address after a reload, and says so for an unknown one', () => {
    const e = { text: 'x', notes: new Map<number, string>(), start: 1 };
    // Two activations stand for a window before and after a reload: the counter starts again.
    const before = new HunkDocs().add('a', 'a.sql', e);
    const after = new HunkDocs();
    assert.notStrictEqual(after.add('a', 'a.sql', e).toString(), before.toString());
    assert.match(after.provideTextDocumentContent(before), /no longer loaded/);
  });
});
