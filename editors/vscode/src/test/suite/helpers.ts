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
