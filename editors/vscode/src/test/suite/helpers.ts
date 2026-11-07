import * as vscode from 'vscode';
import { Api } from '../../extension';
import { Node } from '../../tree';
import { mockBinary, until } from '../mock';

export const ID = 'rift-placeholder.rift-vscode';

export async function api(): Promise<Api> {
  const ext = vscode.extensions.getExtension<Api>(ID)!;
  const a = await ext.activate();
  await vscode.workspace.getConfiguration('rift').update('path', mockBinary(), vscode.ConfigurationTarget.Global);
  return a;
}

export const uri = (name: string) => vscode.Uri.file(`/x/${name}`);

export async function compare(a: string, b: string): Promise<void> {
  await vscode.commands.executeCommand('rift.compareFiles', uri(a), uri(b));
}

export async function ready(x: Api): Promise<void> {
  await until(() => x.comparison()?.state === 'ready' && x.comparison()!.movesTotal > 0);
}

export function labels(x: Api, n?: Node): string[] {
  return x.tree.getChildren(n).map((c) => {
    const i = x.tree.getTreeItem(c);
    return `${i.label as string}${i.description ? ` | ${i.description as string}` : ''}`;
  });
}

export async function closeAll(x: Api): Promise<void> {
  await vscode.commands.executeCommand('rift.closeComparison');
  await vscode.commands.executeCommand('workbench.action.closeAllEditors');
  void x;
}

export function diffTab(): { label: string; original: vscode.Uri; modified: vscode.Uri } | undefined {
  const t = vscode.window.tabGroups.activeTabGroup.activeTab;
  return t?.input instanceof vscode.TabInputTextDiff ? { label: t.label, original: t.input.original, modified: t.input.modified } : undefined;
}

export async function text(u: vscode.Uri): Promise<string[]> {
  return (await vscode.workspace.openTextDocument(u)).getText().split('\n');
}

// Replaces the window's message functions for a test and records what they were asked to show.
export function captureMessages(): { error: string[]; warning: string[]; info: string[]; buttons: string[][]; restore(): void } {
  const w = vscode.window as unknown as Record<string, unknown>;
  const names = ['showErrorMessage', 'showWarningMessage', 'showInformationMessage'];
  const saved = names.map((n) => w[n]);
  const out = { error: [] as string[], warning: [] as string[], info: [] as string[], buttons: [] as string[][], restore() {} };
  const slots = { showErrorMessage: out.error, showWarningMessage: out.warning, showInformationMessage: out.info } as Record<string, string[]>;
  for (const n of names) {
    w[n] = (msg: string, ...buttons: string[]) => {
      slots[n].push(msg);
      out.buttons.push(buttons);
      return Promise.resolve(undefined);
    };
  }
  out.restore = () => names.forEach((n, i) => (w[n] = saved[i]));
  return out;
}
