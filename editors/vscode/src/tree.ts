import * as vscode from 'vscode';
import { Comparison, rerunOffer } from './comparison';
import { fmtRange, num } from './format';
import { MoveBlock, Segment } from './protocol';

export type Node =
  | { kind: 'summary' }
  | { kind: 'error' }
  | { kind: 'rerun' }
  | { kind: 'diffs' }
  | { kind: 'segment'; seg: Segment }
  | { kind: 'moreSegments' }
  | { kind: 'moves' }
  | { kind: 'move'; block: MoveBlock; i: number }
  | { kind: 'moreMoves' };

const GROUPS: Node[] = [{ kind: 'diffs' }, { kind: 'moves' }];

const MB = (n: number) => `${(n / 1048576).toFixed(n < 10485760 ? 1 : 0)} MiB`;

export class RiftTree implements vscode.TreeDataProvider<Node> {
  private emitter = new vscode.EventEmitter<Node | undefined>();
  readonly onDidChangeTreeData = this.emitter.event;
  comparison?: Comparison;
  // The server's `limits.max_session_mem`, to judge a rerun offer.
  maxSessionMem?: () => number | undefined;
  private segmentNodes = new Map<number, Node>();

  refresh(): void {
    this.emitter.fire(undefined);
  }

  node(seg: Segment): Node {
    let n = this.segmentNodes.get(seg.index);
    if (n?.kind !== 'segment' || n.seg !== seg) {
      n = { kind: 'segment', seg };
      this.segmentNodes.set(seg.index, n);
    }
    return n;
  }

  getParent(n: Node): Node | undefined {
    return n.kind === 'segment' || n.kind === 'moreSegments' ? GROUPS[0] : n.kind === 'move' || n.kind === 'moreMoves' ? GROUPS[1] : undefined;
  }

  getChildren(n?: Node): Node[] {
    const c = this.comparison;
    if (!c) return [];
    if (!n) {
      const out: Node[] = [{ kind: 'summary' }];
      if (c.state === 'failed') {
        out.push({ kind: 'error' });
        if (rerunOffer(c.error, this.maxSessionMem?.(), c.reruns)) out.push({ kind: 'rerun' });
      } else if (c.state === 'ready') {
        if (!c.summary?.identical) out.push(GROUPS[0]);
        if (c.summary?.moves !== 'off' && (c.summary?.moves === 'skipped' || (c.summary?.moved_blocks ?? 0) > 0)) out.push(GROUPS[1]);
      }
      return out;
    }
    if (n.kind === 'diffs') {
      const rows = c.segments.map((s) => this.node(s));
      if (c.segments.length < c.segmentsTotal) rows.push({ kind: 'moreSegments' });
      return rows;
    }
    if (n.kind === 'moves') {
      const rows: Node[] = c.moves.map((block, i) => ({ kind: 'move', block, i }));
      if (c.moves.length < c.movesTotal) rows.push({ kind: 'moreMoves' });
      return rows;
    }
    return [];
  }

  getTreeItem(n: Node): vscode.TreeItem {
    const c = this.comparison!;
    switch (n.kind) {
      case 'summary':
        return this.summary(c);
      case 'error': {
        const item = new vscode.TreeItem(`Scan failed: ${c.error?.message ?? 'unknown error'}`);
        item.iconPath = new vscode.ThemeIcon('error');
        item.tooltip = c.error?.message;
        return item;
      }
      case 'rerun': {
        const offer = rerunOffer(c.error, this.maxSessionMem?.(), c.reruns)!;
        if (offer.kind === 'limit') {
          const item = new vscode.TreeItem("The server's limit is the ceiling for --mem");
          item.description = `limit ${MB(offer.limit)}`;
          item.iconPath = new vscode.ThemeIcon('info');
          return item;
        }
        if (offer.kind === 'guide') {
          const item = new vscode.TreeItem('Read the memory guide');
          item.iconPath = new vscode.ThemeIcon('link-external');
          item.command = { command: 'rift.openMemoryGuide', title: 'Open Memory Guide' };
          return item;
        }
        const item = new vscode.TreeItem(`Rerun with --mem ${offer.mem}`);
        item.iconPath = new vscode.ThemeIcon('debug-restart');
        item.command = { command: 'rift.rerunWithMem', title: 'Rerun' };
        return item;
      }
      case 'diffs': {
        const item = new vscode.TreeItem(`Differences (${num(c.segmentsTotal)})`, vscode.TreeItemCollapsibleState.Expanded);
        item.id = 'diffs';
        return item;
      }
      case 'segment': {
        const { seg } = n;
        const item = new vscode.TreeItem(`A ${fmtRange(seg.a)} ↔ B ${fmtRange(seg.b)}`);
        item.id = `seg-${seg.index}`;
        if (seg.status === 'approx') {
          item.description = 'approx';
          item.iconPath = new vscode.ThemeIcon('warning', new vscode.ThemeColor('list.warningForeground'));
          item.tooltip = 'Approximate: the exact edit script did not fit the budget, so this region is marked, not guessed.';
        } else {
          item.iconPath = new vscode.ThemeIcon('diff-modified');
          item.tooltip = `${num(seg.a.count)} lines in A, ${num(seg.b.count)} in B`;
        }
        item.command = { command: 'rift.openSegment', title: 'Open Difference', arguments: [seg.index] };
        return item;
      }
      case 'moreSegments':
      case 'moreMoves': {
        const segs = n.kind === 'moreSegments';
        const item = new vscode.TreeItem(`Load more… (${num(segs ? c.segments.length : c.moves.length)} of ${num(segs ? c.segmentsTotal : c.movesTotal)})`);
        item.iconPath = new vscode.ThemeIcon('ellipsis');
        item.command = { command: 'rift.loadMore', title: 'Load More', arguments: [segs ? 'segments' : 'moves'] };
        return item;
      }
      case 'moves': {
        const skipped = c.summary?.moves === 'skipped';
        const item = new vscode.TreeItem(skipped ? 'Moved blocks (not searched)' : `Moved blocks (${num(c.movesTotal)})`, skipped ? vscode.TreeItemCollapsibleState.None : vscode.TreeItemCollapsibleState.Collapsed);
        item.id = 'moves';
        if (skipped) item.tooltip = 'The budget did not allow the search, so the absence of moved blocks says nothing about the files.';
        else item.iconPath = new vscode.ThemeIcon('arrow-swap');
        return item;
      }
      case 'move': {
        const { block } = n;
        const item = new vscode.TreeItem(`A ${fmtRange(block.a)} → B ${fmtRange(block.b)}`);
        item.id = `move-${n.i}`;
        item.description = `${num(block.a.count)} lines`;
        item.iconPath = new vscode.ThemeIcon('arrow-right');
        item.tooltip = `Lines ${fmtRange(block.a)} of A are lines ${fmtRange(block.b)} of B.`;
        item.command = { command: 'rift.openMove', title: 'Open Moved Block', arguments: [block.a.start] };
        return item;
      }
    }
  }

  private summary(c: Comparison): vscode.TreeItem {
    const name = `${basename(c.a)} ↔ ${basename(c.b)}`;
    const s = c.summary;
    let item: vscode.TreeItem;
    if (c.state === 'opening') {
      item = new vscode.TreeItem('Starting…');
      item.iconPath = new vscode.ThemeIcon('loading~spin');
    } else if (c.state === 'scanning') {
      const p = c.progress;
      const pct = p && p.phase === 'scan' && p.total > 0 ? ` ${Math.floor((100 * p.done) / p.total)}%` : '';
      item = new vscode.TreeItem(`Scanning…${pct}`);
      item.iconPath = new vscode.ThemeIcon('loading~spin');
    } else if (c.state === 'failed') {
      item = new vscode.TreeItem('Failed');
      item.iconPath = new vscode.ThemeIcon('error');
    } else if (s?.identical) {
      item = new vscode.TreeItem('Files are identical');
      item.iconPath = new vscode.ThemeIcon('check');
    } else {
      item = new vscode.TreeItem(`A ${num(s?.a.lines ?? 0)} lines · B ${num(s?.b.lines ?? 0)} lines`);
      const parts = [
        `${num(s?.differs?.segments ?? 0)} differ`,
        `${num(s?.approx?.segments ?? 0)} approx`,
        `${num(s?.equal?.segments ?? 0)} equal`,
      ];
      if (s?.moves === 'found') parts.push(`${num(s.moved_blocks ?? 0)} moved`);
      else if (s?.moves === 'skipped') parts.push('moves not searched');
      const p = s?.progress;
      if (p?.phase === 'refine' && p.done < p.total) parts.push(`refining ${num(p.done)}/${num(p.total)}`);
      item.description = parts.join(' · ');
      item.iconPath = new vscode.ThemeIcon('git-compare');
    }
    item.id = 'summary';
    item.tooltip = new vscode.MarkdownString(
      [
        `**A** ${c.a}${s ? ` (${num(s.a.bytes)} bytes)` : ''}`,
        `**B** ${c.b}${s ? ` (${num(s.b.bytes)} bytes)` : ''}`,
        s ? `Budget ${MB(s.mem_bytes)}` : '',
      ]
        .filter(Boolean)
        .join('\n\n'),
    );
    if (!item.description) item.description = name;
    return item;
  }
}

export function basename(p: string): string {
  return p.split(/[\\/]/).pop() ?? p;
}
