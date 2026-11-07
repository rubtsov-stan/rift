import { HunkResult, Line, Range, Side } from './protocol';

export const num = (n: number): string => n.toLocaleString('en-US');

// Lines 5-9 are "5–9", one line is "5", an empty range sits before its start.
export function fmtRange(r: Range): string {
  if (r.count === 0) return `before ${num(r.start)}`;
  if (r.count === 1) return num(r.start);
  return `${num(r.start)}–${num(r.start + r.count - 1)}`;
}

export function title(a: string, b: string, h: Pick<HunkResult, 'range' | 'a' | 'b'>): string {
  const flags = [h.a, h.b].some((s) => s.lines.some((l) => l.lossy)) ? ' (lossy lines)' : '';
  return `${a} ${fmtRange(h.range.a)} ↔ ${b} ${fmtRange(h.range.b)}${flags}`;
}

export interface Excerpt {
  text: string;
  // Hover text by 0-based line of the document.
  notes: Map<number, string>;
  // Document line of each protocol line is `index`; this is the A or B line number of index 0.
  start: number;
}

const EOL_NOTE = { lf: '', crlf: 'Line ends with CRLF.', none: 'No line terminator.' };

function lineNote(l: Line): string {
  const notes: string[] = [];
  if (l.lossy) notes.push('Not valid UTF-8: invalid bytes are shown as U+FFFD, so this text is not exact.');
  if (l.truncated) notes.push(`Cut: ${num(l.bytes)} bytes in the file, ${num(new TextEncoder().encode(l.text).length)} shown.`);
  if (EOL_NOTE[l.eol]) notes.push(EOL_NOTE[l.eol]);
  return notes.join('\n\n');
}

// One side as document text. A cut line and a cut side are marked in the
// text itself, so a diff never shows them as complete.
export function excerpt(side: Side): Excerpt {
  const notes = new Map<number, string>();
  const out = side.lines.map((l, i) => {
    const note = lineNote(l);
    if (note) notes.set(i, note);
    return l.truncated ? `${l.text} … [line cut, ${num(l.bytes)} bytes]` : l.text;
  });
  const hidden = side.total - side.lines.length;
  if (hidden > 0) out.push(`… [${num(hidden)} more lines not shown]`);
  return { text: out.join('\n'), notes, start: side.start };
}
