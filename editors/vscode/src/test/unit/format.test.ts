import * as assert from 'assert';
import { excerpt, fmtRange, movedNote, title } from '../../format';

describe('format', () => {
  it('writes ranges', () => {
    assert.strictEqual(fmtRange({ start: 1204880, count: 61 }), '1,204,880–1,204,940');
    assert.strictEqual(fmtRange({ start: 7, count: 1 }), '7');
    assert.strictEqual(fmtRange({ start: 8, count: 0 }), 'before 8');
  });

  it('notes the move search, and reads a state it does not know as found', () => {
    assert.strictEqual(movedNote('found', 3), '3 moved');
    assert.strictEqual(movedNote('partial', 1200), '1,200 moved, list full');
    assert.strictEqual(movedNote('skipped', 0), 'moves not searched');
    assert.strictEqual(movedNote('off', 0), undefined);
    assert.strictEqual(movedNote('from-the-future', 2), '2 moved');
    assert.strictEqual(movedNote('from-the-future', 0), undefined);
  });

  it('titles a hunk with both original ranges', () => {
    const side = { start: 1, total: 0, lines: [] };
    const h = { status: 'differs' as const, range: { a: { start: 1204880, count: 61 }, b: { start: 1204877, count: 65 } }, a: side, b: side };
    assert.strictEqual(title('a.sql', 'b.sql', h), 'a.sql 1,204,880–1,204,940 ↔ b.sql 1,204,877–1,204,941');
  });

  it('marks cut lines, hidden lines, lossy lines and terminators', () => {
    const e = excerpt({
      start: 10,
      total: 5,
      lines: [
        { text: 'plain', bytes: 5, eol: 'lf' },
        { text: 'caf�', bytes: 4, eol: 'crlf', lossy: true },
        { text: 'aaaa', bytes: 1048576, eol: 'none', truncated: true },
      ],
    });
    assert.deepStrictEqual(e.text.split('\n'), ['plain', 'caf�', 'aaaa … [line cut, 1,048,576 bytes]', '… [2 more lines not shown]']);
    assert.ok(!e.notes.has(0));
    assert.match(e.notes.get(1)!, /Not valid UTF-8/);
    assert.match(e.notes.get(1)!, /CRLF/);
    assert.match(e.notes.get(2)!, /No line terminator/);
  });

  it('shows a carriage return inside a line, so it does not split the document line', () => {
    const e = excerpt({
      start: 1,
      total: 3,
      lines: [
        { text: 'x\r', bytes: 2, eol: 'crlf' },
        { text: 'x', bytes: 1, eol: 'crlf' },
        { text: 'a\rb', bytes: 3, eol: 'lf' },
      ],
    });
    assert.strictEqual(e.text.split(/\r\n|\r|\n/).length, 3);
    assert.deepStrictEqual(e.text.split('\n'), ['x\u240d', 'x', 'a\u240db']);
    assert.match(e.notes.get(0)!, /carriage return/);
    assert.ok(!/carriage return/.test(e.notes.get(1) ?? ''));
  });
});
