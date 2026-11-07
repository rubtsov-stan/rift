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
    // `ready` is set before the first page of segments has arrived.
    await until(() => cmp.state === 'ready' && cmp.segments.length > 0);
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
  const M = 1 << 20;
  const err = (data: Record<string, unknown>, code = -32003) => ({ code, message: 'm', data });
  const adv = (bytes: number, failed = M) => err({ mem_bytes: failed, suggested_mem_bytes: bytes, suggested_mem: 'x' });
  const mem = (bytes: number, max = 1 << 30, reruns = 0) => rerunOffer(adv(bytes), max, reruns);

  it('adds 25% and rounds up to a power of two from 16M', () => {
    assert.deepStrictEqual(mem(5 * M), { kind: 'rerun', mem: '16M' });
    assert.deepStrictEqual(mem(1), { kind: 'rerun', mem: '16M' });
    assert.deepStrictEqual(mem(16 * M), { kind: 'rerun', mem: '32M' });
    assert.deepStrictEqual(mem(30 * M), { kind: 'rerun', mem: '64M' });
  });
  it('keeps 16M when the headroom lands exactly on it, and steps up just above', () => {
    assert.deepStrictEqual(mem(13421772), { kind: 'rerun', mem: '16M' });
    assert.deepStrictEqual(mem(13421773), { kind: 'rerun', mem: '32M' });
  });
  it('reaches 1G and names it in G', () => {
    assert.deepStrictEqual(mem(858993459, 4 * 2 ** 30), { kind: 'rerun', mem: '1G' });
    assert.deepStrictEqual(mem(858993460, 4 * 2 ** 30), { kind: 'rerun', mem: '2G' });
  });
  it('caps at the server limit', () => {
    assert.deepStrictEqual(mem(900 * M, 1 << 30), { kind: 'rerun', mem: '1G' });
    assert.deepStrictEqual(rerunOffer(adv(2 ** 31, 256 * M), 1 << 30, 0), { kind: 'rerun', mem: '1G' });
  });
  it('offers nothing when the capped value is not above the mem that failed', () => {
    assert.deepStrictEqual(rerunOffer(adv(2 ** 31, 1 << 30), 1 << 30, 0), { kind: 'limit', limit: 1 << 30 });
    assert.deepStrictEqual(rerunOffer(adv(900 * M, 64 * M), 32 * M, 0), { kind: 'limit', limit: 32 * M });
  });
  it('does not cap when the limit is unknown', () => {
    assert.deepStrictEqual(rerunOffer(adv(2 ** 31), undefined, 0), { kind: 'rerun', mem: '4G' });
  });
  it('offers nothing without a suggestion or for another error', () => {
    assert.strictEqual(rerunOffer(err({ mem_bytes: 1 }), 1 << 30, 0), undefined);
    assert.strictEqual(rerunOffer(err({ suggested_mem_bytes: 1 }, -32001), 1 << 30, 0), undefined);
    assert.strictEqual(rerunOffer(undefined, 1 << 30, 0), undefined);
  });
  it('offers twice, then points to the memory guide', () => {
    assert.strictEqual(mem(5 * M, 1 << 30, 0)?.kind, 'rerun');
    assert.strictEqual(mem(5 * M, 1 << 30, 1)?.kind, 'rerun');
    assert.deepStrictEqual(mem(5 * M, 1 << 30, 2), { kind: 'guide' });
    assert.deepStrictEqual(mem(5 * M, 1 << 30, 3), { kind: 'guide' });
  });
});
