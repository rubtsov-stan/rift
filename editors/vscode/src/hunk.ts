import * as vscode from 'vscode';
import { Excerpt } from './format';

export const SCHEME = 'rift';
const KEEP = 64;

interface Doc extends Excerpt {
  side: 'a' | 'b';
}

// The excerpts behind the rift: documents: their text, and a hover for
// lines that are cut, lossy or have an unusual terminator.
export class HunkDocs implements vscode.TextDocumentContentProvider, vscode.HoverProvider {
  private docs = new Map<string, Doc>();
  private n = 0;

  // `name` ends the path so the editor picks the language from its extension.
  add(side: 'a' | 'b', name: string, e: Excerpt): vscode.Uri {
    const uri = vscode.Uri.from({ scheme: SCHEME, path: `/${++this.n}/${side}/${name}` });
    this.docs.set(uri.toString(), { ...e, side });
    // Open editors keep their text; only the lookup behind them is bounded.
    if (this.docs.size > KEEP) this.docs.delete(this.docs.keys().next().value!);
    return uri;
  }

  lookup(uri: vscode.Uri): Doc | undefined {
    return this.docs.get(uri.toString());
  }

  provideTextDocumentContent(uri: vscode.Uri): string {
    return this.lookup(uri)?.text ?? '';
  }

  provideHover(doc: vscode.TextDocument, pos: vscode.Position): vscode.Hover | undefined {
    const note = this.lookup(doc.uri)?.notes.get(pos.line);
    return note ? new vscode.Hover(note) : undefined;
  }
}
