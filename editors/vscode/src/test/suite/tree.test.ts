import * as assert from 'assert';
import { Api } from '../../extension';
import { api, closeAll, compare, labels, ready } from './helpers';

describe('tree', () => {
  let x: Api;
  before(async () => (x = await api()));
  afterEach(() => closeAll(x));

  it('lists the summary, the differences and the moved blocks', async () => {
    await compare('a.sql', 'b.sql');
    await ready(x);
    const root = labels(x);
    assert.strictEqual(root.length, 3);
    assert.match(root[0], /^A 60 lines · B 1,561 lines \| 4 differ · 1 approx · 6 equal · 1 moved/);
    assert.strictEqual(root[1], 'Differences (5)');
    assert.strictEqual(root[2], 'Moved blocks (1)');
    const [, diffs, moves] = x.tree.getChildren();
    const segs = labels(x, diffs);
    assert.strictEqual(segs.length, 5);
    assert.ok(segs[0].startsWith('A 10–11 ↔ B 10–12'));
    assert.strictEqual(segs[1], 'A 20–24 ↔ B before 21');
    assert.strictEqual(segs[2], 'A 35–38 ↔ B 31–34 | approx');
    assert.deepStrictEqual(labels(x, moves), ['A 20–24 → B 47–51 | 5 lines']);
  });
});
