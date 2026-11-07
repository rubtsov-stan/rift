import * as assert from 'assert';
import { Server } from '../../client';
import { Comparison } from '../../comparison';
import { RpcErrorObject } from '../../protocol';
import { mockBinary, until } from '../mock';

function setup(a = '/x/a.sql', mem = '256M') {
  const ref: { cmp?: Comparison } = {};
  const failures: RpcErrorObject[] = [];
  const server = new Server({
    command: mockBinary,
    args: [],
    onNotification: (n) => ref.cmp?.handle(n),
    onCrash: () => {},
    onRestarted: () => {},
  });
  const cmp = new Comparison(server, a, '/x/b.sql', mem, { change() {}, failed: (e) => failures.push(e) });
  ref.cmp = cmp;
  return { server, cmp, failures };
}

describe('Comparison', () => {
  it('goes from scanning to ready and loads the first page and the moves', async () => {
    const { server, cmp } = setup();
    await cmp.open();
    assert.ok(cmp.state === 'scanning' || cmp.state === 'ready');
    await until(() => cmp.state === 'ready' && cmp.movesTotal === 1);
    assert.deepStrictEqual(cmp.segments.map((s) => s.index), [1, 3, 5, 7, 9]);
    assert.strictEqual(cmp.summary?.differing_segments, 5);
    await cmp.close();
    server.dispose();
  });

  it('fails a scan that does not fit and keeps the suggestion', async () => {
    const { server, cmp, failures } = setup('/x/a.sql', '512K');
    await cmp.open();
    await until(() => cmp.state === 'failed');
    assert.strictEqual(failures[0].code, -32003);
    assert.ok((failures[0].data as { suggested_mem?: string }).suggested_mem);
    // Reopening the same pair replaces the failed session.
    await cmp.open('64M');
    await until(() => cmp.state === 'ready');
    server.dispose();
  });

  it('marks a segment approx when its refinement ends so', async () => {
    const { server, cmp } = setup();
    await cmp.open();
    await until(() => cmp.state === 'ready');
    const h = await cmp.hunk({ segment: 1 }, 3);
    assert.strictEqual(h?.pending, true);
    await until(() => cmp.segments[0].status === 'approx');
    server.dispose();
  });
});
