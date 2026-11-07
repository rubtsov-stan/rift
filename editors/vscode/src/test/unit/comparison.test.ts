import * as assert from 'assert';
import { Server } from '../../client';
import { Comparison, rerunOffer } from '../../comparison';
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
    await cmp.hunk({ segment: 1 }, 3);
    await until(() => cmp.segments[0].status === 'approx');
    server.dispose();
  });

  it('closes its session on the server', async () => {
    const { server, cmp } = setup();
    await cmp.open();
    const session = cmp.session;
    await cmp.close();
    await assert.rejects(server.request('summary', { session }), (e: { code: number }) => e.code === -32006);
    server.dispose();
  });
});

describe('rerunOffer', () => {
  const err = (data: Record<string, unknown>, code = -32003) => ({ code, message: 'm', data });
  it('offers the suggested budget', () => {
    assert.deepStrictEqual(rerunOffer(err({ suggested_mem: '5120K', suggested_mem_bytes: 5242880 }), 1 << 30), { mem: '5120K' });
  });
  it('names the limit when the suggestion is over it', () => {
    assert.deepStrictEqual(rerunOffer(err({ suggested_mem: '2G', suggested_mem_bytes: 2 ** 31 }), 1 << 30), { mem: '2G', limit: 1 << 30 });
  });
  it('offers nothing without a suggestion or for another error', () => {
    assert.strictEqual(rerunOffer(err({ mem_bytes: 1 }), 1 << 30), undefined);
    assert.strictEqual(rerunOffer(err({ suggested_mem: '1M' }, -32001), 1 << 30), undefined);
  });
});
