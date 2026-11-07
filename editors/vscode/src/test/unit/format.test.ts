import * as assert from 'assert';
import { excerpt, fmtRange, title } from '../../format';

describe('format', () => {
  it('writes ranges', () => {
    assert.strictEqual(fmtRange({ start: 1204880, count: 61 }), '1,204,880–1,204,940');
    assert.strictEqual(fmtRange({ start: 7, count: 1 }), '7');
    assert.strictEqual(fmtRange({ start: 8, count: 0 }), 'before 8');
  });

  it('titles a hunk with both original ranges', () => {
    const side = { start: 1, total: 0, lines: [] };
    const h = { range: { a: { start: 1204880, count: 61 }, b: { start: 1204877, count: 65 } }, a: side, b: side };
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
});
