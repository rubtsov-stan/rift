import * as path from 'path';
import * as vscode from 'vscode';
import { Server } from './client';
import { Comparison, Compares, MEMORY_GUIDE, rerunOffer } from './comparison';
import { excerpt, num, title } from './format';
import { HunkDocs, SCHEME } from './hunk';
import { RpcErrorObject } from './protocol';
import { RiftTree, basename } from './tree';
import { HunkResult } from './protocol';

// What tests reach into; not a public API.
export interface Api {
  tree: RiftTree;
  docs: HunkDocs;
  comparison(): Comparison | undefined;
  serverPid(): Promise<number | undefined>;
}

let shutdown: (() => Promise<void>) | undefined;

export function activate(ctx: vscode.ExtensionContext): Api {
  const cfg = () => vscode.workspace.getConfiguration('rift');
  const bundled = path.join(ctx.extensionPath, 'bin', process.platform === 'win32' ? 'rift.exe' : 'rift');

  const tree = new RiftTree();
  const docs = new HunkDocs();
  ctx.subscriptions.push(
    vscode.workspace.registerTextDocumentContentProvider(SCHEME, docs),
    vscode.languages.registerHoverProvider({ scheme: SCHEME }, docs),
  );
  const view = vscode.window.createTreeView('rift.comparison', { treeDataProvider: tree });
  const status = vscode.window.createStatusBarItem(vscode.StatusBarAlignment.Left, 0);
  const server: Server = new Server({
    command: () => cfg().get<string>('path')?.trim() || bundled,
    args: ['serve'],
    onNotification: (n, epoch) => compares.handle(n, epoch),
    onCrash: (reason, restarting, deliberate) => {
      const reopen = compares.lost(reason, restarting, deliberate);
      // A binary that cannot start is reported by the command that needed it.
      if (reason.startsWith('cannot start')) return;
      const text = deliberate
        ? `${reason}. Compare the files again.`
        : reopen
          ? `${reason}. Restarting it.`
          : `${reason}. It stopped right after starting, so it is not restarted.`;
      void vscode.window.showWarningMessage(`Rift: ${text}`);
    },
  });
  const compares = new Compares(server, { change: render, failed, error: showError });
  tree.maxSessionMem = () => server.limits?.max_session_mem;

  function showError(e: unknown): void {
    const msg = e instanceof Error ? e.message : String(e);
    const hint = msg.startsWith('cannot start') ? ' Set rift.path to the binary.' : '';
    void vscode.window.showErrorMessage(`Rift: ${msg}${hint}`);
  }

  function render(): void {
    const c = compares.shown;
    tree.comparison = c;
    tree.refresh();
    void vscode.commands.executeCommand('setContext', 'rift.active', c !== undefined);
    view.description = c ? `${basename(c.a)} ↔ ${basename(c.b)}` : undefined;
    const p = c?.progress;
    if (!c || c.state === 'failed' || !p || (p.phase === 'refine' && p.done >= p.total)) {
      status.hide();
      return;
    }
    status.text = p.phase === 'scan' ? `$(sync~spin) Rift scan ${p.total ? Math.floor((100 * p.done) / p.total) : 0}%` : `$(sync~spin) Rift refine ${num(p.done)}/${num(p.total)}`;
    status.tooltip = `${basename(c.a)} ↔ ${basename(c.b)}`;
    status.show();
  }

  function failed(error: RpcErrorObject, c: Comparison): void {
    const offer = rerunOffer(error, server.limits?.max_session_mem, c.reruns);
    const text = `Rift: ${error.message}`;
    if (offer?.kind === 'rerun') {
      const label = `Rerun with --mem ${offer.mem}`;
      void vscode.window.showErrorMessage(text, label).then((pick) => {
        if (pick === label) void vscode.commands.executeCommand('rift.rerunWithMem');
      });
    } else if (offer?.kind === 'guide') {
      const label = 'Memory guide';
      void vscode.window.showErrorMessage(text, label).then((pick) => {
        if (pick === label) void vscode.commands.executeCommand('rift.openMemoryGuide');
      });
    } else if (offer?.kind === 'limit') {
      void vscode.window.showErrorMessage(`${text}. The server's limit of ${num(offer.limit)} bytes per comparison is the ceiling for --mem.`);
    } else {
      void vscode.window.showErrorMessage(text);
    }
  }

  const compare = (a: string, b: string) => compares.compare(a, b, cfg().get<string>('mem') ?? '256M');

  // Shows one hunk in the diff editor. `lineA` also puts the cursor on that line of A.
  async function show(target: { segment: number } | { line_a: number }): Promise<HunkResult | undefined> {
    const c = compares.shown;
    if (!c) return undefined;
    let h: HunkResult | undefined;
    try {
      h = await c.hunk(target, cfg().get<number>('contextLines') ?? 3);
    } catch (e) {
      showError(e);
      return undefined;
    }
    if (!h || c !== compares.shown) return undefined;
    const left = docs.add('a', basename(c.a), excerpt(h.a));
    const right = docs.add('b', basename(c.b), excerpt(h.b));
    await vscode.commands.executeCommand('vscode.diff', left, right, title(basename(c.a), basename(c.b), h), { preview: true });
    c.setVisible(h.range.a);
    const seg = h.segment;
    // An equal segment or a moved block is not in the list: stand after the last difference before it.
    const pos = c.segments.findIndex((s) => s.index === seg);
    if (pos >= 0 && h.status === 'approx') c.segments[pos].status = 'approx';
    c.current = pos >= 0 ? pos : c.segments.reduce((last, s, i) => (s.index < seg ? i : last), -1);
    if ('line_a' in target) {
      const ed = vscode.window.visibleTextEditors.find((e) => e.document.uri.toString() === left.toString());
      const line = Math.max(0, Math.min(target.line_a - h.a.start, (ed?.document.lineCount ?? 1) - 1));
      if (ed) {
        ed.selection = new vscode.Selection(line, 0, line, 0);
        ed.revealRange(new vscode.Range(line, 0, line, 0), vscode.TextEditorRevealType.InCenter);
      }
    }
    return h;
  }

  async function step(dir: 1 | -1): Promise<void> {
    const c = compares.shown;
    if (!c || c.state !== 'ready') return;
    const pos = c.current < 0 ? 0 : c.current + dir;
    if (pos >= c.segments.length && c.segments.length < c.segmentsTotal) {
      try {
        await c.loadSegments();
      } catch (e) {
        showError(e);
        return;
      }
      render();
    }
    if (pos < 0 || pos >= c.segments.length) {
      void vscode.window.setStatusBarMessage(pos < 0 ? 'Rift: this is the first difference' : 'Rift: this is the last difference', 3000);
      return;
    }
    if (await show({ segment: c.segments[pos].index })) {
      void Promise.resolve(view.reveal(tree.node(c.segments[pos]), { select: true, focus: false })).catch(() => {});
    }
  }

  async function pick(label: string, start?: vscode.Uri): Promise<string | undefined> {
    const r = await vscode.window.showOpenDialog({ canSelectMany: false, canSelectFolders: false, openLabel: 'Select', title: label, defaultUri: start });
    return r?.[0].fsPath;
  }

  const reg = (id: string, f: (...args: never[]) => unknown) => ctx.subscriptions.push(vscode.commands.registerCommand(id, f));

  // Both files can be passed as paths or URIs, or as one `{ a, b }` object, which is
  // all a keybinding's `args` can carry.
  const fsPath = (f?: vscode.Uri | string) => (typeof f === 'string' ? f : f?.fsPath);
  reg('rift.compareFiles', async (first?: vscode.Uri | string | { a: string; b: string }, second?: vscode.Uri | string) => {
    const pair = first && typeof first === 'object' && 'a' in first && typeof first.a === 'string' ? first : undefined;
    const a = pair ? pair.a : (first as vscode.Uri | string | undefined);
    const b = pair ? pair.b : second;
    const pa = fsPath(a) ?? (await pick('Rift: file A', vscode.window.activeTextEditor?.document.uri));
    if (!pa) return;
    const pb = fsPath(b) ?? (await pick('Rift: file B'));
    if (pb) await compare(pa, pb);
  });
  reg('rift.compareSelected', async (clicked?: vscode.Uri, selected?: vscode.Uri[]) => {
    const files = selected ?? (clicked ? [clicked] : []);
    if (files.length !== 2) {
      void vscode.window.showInformationMessage('Rift: select exactly two files.');
      return;
    }
    await compare(files[0].fsPath, files[1].fsPath);
  });
  reg('rift.openSegment', (index: number) => show({ segment: index }));
  reg('rift.openMove', (lineA: number) => show({ line_a: lineA }));
  reg('rift.nextDifference', () => step(1));
  reg('rift.previousDifference', () => step(-1));
  reg('rift.goToLineInA', async () => {
    const c = compares.shown;
    if (!c || c.state !== 'ready') return;
    // From a rift: document of A the cursor's own line is the default.
    const ed = vscode.window.activeTextEditor;
    const doc = ed && docs.lookup(ed.document.uri);
    const dflt = doc?.side === 'a' && ed ? String(doc.start + ed.selection.active.line) : '';
    const lines = c.summary?.a.lines;
    const text = await vscode.window.showInputBox({
      prompt: `Line in file A${lines ? ` (1–${num(lines)})` : ''}`,
      value: dflt,
      validateInput: (v) => (/^\d+$/.test(v) && Number(v) >= 1 && (!lines || Number(v) <= lines) ? undefined : 'Enter a line number of file A'),
    });
    if (text) await show({ line_a: Number(text) });
  });
  reg('rift.closeComparison', () => compares.close());
  reg('rift.loadMore', async (kind: 'segments' | 'moves') => {
    try {
      await (kind === 'segments' ? compares.shown?.loadSegments() : compares.shown?.loadMoves());
    } catch (e) {
      showError(e);
    }
    render();
  });
  reg('rift.rerunWithMem', async () => {
    const c = compares.shown;
    const offer = rerunOffer(c?.error, server.limits?.max_session_mem, c?.reruns ?? 0);
    if (offer?.kind === 'rerun') await compares.rerun(offer.mem);
  });
  reg('rift.openMemoryGuide', () => vscode.env.openExternal(vscode.Uri.parse(MEMORY_GUIDE)));

  ctx.subscriptions.push(view, status);
  shutdown = async () => {
    // A server that does not answer must not hold the window open.
    await Promise.race([compares.close(), new Promise((r) => setTimeout(r, 2000))]);
    server.dispose();
  };
  return { tree, docs, comparison: () => compares.shown, serverPid: () => server.pid() };
}

export function deactivate(): Promise<void> | undefined {
  return shutdown?.();
}
