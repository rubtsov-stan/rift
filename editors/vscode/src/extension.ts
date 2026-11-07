import * as path from 'path';
import * as vscode from 'vscode';
import { Server } from './client';
import { Comparison, rerunOffer } from './comparison';
import { num } from './format';
import { RpcErrorObject } from './protocol';
import { RiftTree, basename } from './tree';

// What tests reach into; not a public API.
export interface Api {
  tree: RiftTree;
  comparison(): Comparison | undefined;
  serverPid(): Promise<number | undefined>;
}

let shutdown: (() => Promise<void>) | undefined;

export function activate(ctx: vscode.ExtensionContext): Api {
  const cfg = () => vscode.workspace.getConfiguration('rift');
  const bundled = path.join(ctx.extensionPath, 'bin', process.platform === 'win32' ? 'rift.exe' : 'rift');

  const tree = new RiftTree();
  const view = vscode.window.createTreeView('rift.comparison', { treeDataProvider: tree });
  const status = vscode.window.createStatusBarItem(vscode.StatusBarAlignment.Left, 0);
  let comparison: Comparison | undefined;
  // A comparison whose `open` has not answered yet still gets notifications.
  let opening: Comparison | undefined;

  const server: Server = new Server({
    command: () => cfg().get<string>('path')?.trim() || bundled,
    args: ['serve'],
    onNotification: (n) => {
      opening?.handle(n);
      comparison?.handle(n);
    },
    onCrash: (reason, restarting) => {
      comparison?.lost(reason, restarting);
      // A binary that cannot start is reported by the command that needed it.
      if (reason.startsWith('cannot start')) return;
      void vscode.window.showWarningMessage(restarting ? `Rift: ${reason}. Restarting it.` : `Rift: ${reason}. It stopped right after starting, so it is not restarted.`);
    },
    onRestarted: () => {
      comparison?.open().catch(showError);
    },
  });
  tree.maxSessionMem = () => server.limits?.max_session_mem;

  function showError(e: unknown): void {
    const msg = e instanceof Error ? e.message : String(e);
    const hint = msg.startsWith('cannot start') ? ' Set rift.path to the binary.' : '';
    void vscode.window.showErrorMessage(`Rift: ${msg}${hint}`);
  }

  function render(): void {
    const c = comparison;
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

  function failed(error: RpcErrorObject): void {
    const offer = rerunOffer(error, server.limits?.max_session_mem);
    if (offer && !('limit' in offer)) {
      const label = `Rerun with --mem ${offer.mem}`;
      void vscode.window.showErrorMessage(`Rift: ${error.message}`, label).then((pick) => {
        if (pick === label) void vscode.commands.executeCommand('rift.rerunWithMem');
      });
    } else if (offer) {
      void vscode.window.showErrorMessage(`Rift: ${error.message}. The suggested --mem ${offer.mem} is over the server's limit of ${num(offer.limit)} bytes.`);
    } else {
      void vscode.window.showErrorMessage(`Rift: ${error.message}`);
    }
  }

  async function compare(a: string, b: string): Promise<void> {
    const next = new Comparison(server, a, b, cfg().get<string>('mem') ?? '256M', { change: render, failed });
    opening = next;
    try {
      await next.open();
    } catch (e) {
      showError(e);
      return;
    } finally {
      opening = undefined;
    }
    const prev = comparison;
    comparison = next;
    void prev?.close();
    render();
  }

  async function pick(label: string, start?: vscode.Uri): Promise<string | undefined> {
    const r = await vscode.window.showOpenDialog({ canSelectMany: false, canSelectFolders: false, openLabel: 'Select', title: label, defaultUri: start });
    return r?.[0].fsPath;
  }

  const reg = (id: string, f: (...args: never[]) => unknown) => ctx.subscriptions.push(vscode.commands.registerCommand(id, f));

  reg('rift.compareFiles', async (a?: vscode.Uri, b?: vscode.Uri) => {
    const pa = a?.fsPath ?? (await pick('Rift: file A', vscode.window.activeTextEditor?.document.uri));
    if (!pa) return;
    const pb = b?.fsPath ?? (await pick('Rift: file B'));
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
  reg('rift.closeComparison', async () => {
    const c = comparison;
    comparison = undefined;
    render();
    await c?.close();
  });
  reg('rift.loadMore', async (kind: 'segments' | 'moves') => {
    try {
      await (kind === 'segments' ? comparison?.loadSegments() : comparison?.loadMoves());
    } catch (e) {
      showError(e);
    }
    render();
  });
  reg('rift.rerunWithMem', async () => {
    const offer = rerunOffer(comparison?.error, server.limits?.max_session_mem);
    if (!comparison || !offer || 'limit' in offer) return;
    try {
      await comparison.open(offer.mem);
    } catch (e) {
      showError(e);
    }
    render();
  });

  ctx.subscriptions.push(view, status);
  shutdown = async () => {
    // A server that does not answer must not hold the window open.
    await Promise.race([comparison?.close(), new Promise((r) => setTimeout(r, 2000))]);
    server.dispose();
  };
  return { tree, comparison: () => comparison, serverPid: () => server.pid() };
}

export function deactivate(): Promise<void> | undefined {
  return shutdown?.();
}
