import * as assert from 'assert';
import * as vscode from 'vscode';
import { Api } from '../../extension';
import { until } from '../mock';
import { api, captureMessages, clearRequests, closeAll, compare, requests, describeMock, diffTab, labels, ready, uri } from './helpers';

describeMock('compare', () => {
  let x: Api;
  before(async () => (x = await api()));
  afterEach(() => closeAll(x));

  it('keeps the session when the same pair is compared twice', async () => {
    await compare('a.sql', 'b.sql');
    await ready(x);
    await compare('a.sql', 'b.sql');
    await ready(x);
    await vscode.commands.executeCommand('rift.openSegment', 3);
    assert.ok(diffTab(), 'a hunk of the second comparison opens');
    assert.strictEqual(x.comparison()!.state, 'ready');
  });

  it('drops an older compare that finishes after a newer one', async () => {
    // The first pair waits out two busy answers; the second opens at once.
    const slow = compare('busy-race.sql', 'b.sql');
    await compare('a.sql', 'b.sql');
    await slow;
    await ready(x);
    assert.strictEqual(x.comparison()!.a, uri('a.sql').fsPath);
    await until(() => x.comparison()!.segments.length > 0);
    await vscode.commands.executeCommand('rift.openSegment', 3);
    assert.ok(diffTab());
  });

  it('keeps the session of the same pair when an older compare of it answers first', async () => {
    // The server hands the newer compare the older one's session before the older
    // one is known to be stale; closing it then would close the newer one's.
    const older = compare('lag-same.sql', 'b.sql');
    await new Promise((r) => setTimeout(r, 10));
    await Promise.all([older, compare('lag-same.sql', 'b.sql')]);
    await ready(x);
    await vscode.commands.executeCommand('rift.openSegment', 3);
    assert.ok(diffTab());
    assert.strictEqual(x.comparison()!.state, 'ready');
  });

  it('keeps the approx marks when the same pair is compared again', async () => {
    await compare('a.sql', 'b.sql');
    await ready(x);
    const [, diffs] = x.tree.getChildren();
    await vscode.commands.executeCommand('rift.openSegment', 1);
    await until(() => labels(x, diffs)[0].endsWith('approx'));
    // The server reuses the session and sends no segment_ready for it again.
    await compare('a.sql', 'b.sql');
    await ready(x);
    const [, again] = x.tree.getChildren();
    assert.ok(labels(x, again)[0].endsWith('approx'));
  });

  it('takes the status of an opened segment from the hunk', async () => {
    await compare('a.sql', 'b.sql');
    await ready(x);
    await vscode.commands.executeCommand('rift.openSegment', 1);
    const c = x.comparison()!;
    await until(() => c.segments[0].status === 'approx');
    c.segments[0].status = 'differs';
    await vscode.commands.executeCommand('rift.openSegment', 1);
    assert.strictEqual(c.segments[0].status, 'approx');
  });

  it('shows no failure dialog for a compare that was replaced meanwhile', async () => {
    const msg = captureMessages();
    try {
      // Its scan fails while the answer to `open` is still on the way.
      const older = compare('lag-failscan.sql', 'b.sql');
      await new Promise((r) => setTimeout(r, 60));
      await Promise.all([older, compare('a.sql', 'b.sql')]);
      await ready(x);
      assert.deepStrictEqual(msg.error, []);
    } finally {
      msg.restore();
    }
  });

  it('keeps the shown session when a replaced compare would evict it', async () => {
    // A pair is compared, a busy pair is compared on top (it retries), and the first pair
    // again: the busy one is replaced and must not open, which would evict the shown session.
    await compare('a.sql', 'b.sql');
    const busy = compare('busy-evictor.sql', 'b.sql');
    await new Promise((r) => setTimeout(r, 30));
    await Promise.all([busy, compare('a.sql', 'b.sql')]);
    await ready(x);
    // The retries would have run by now.
    await new Promise((r) => setTimeout(r, 800));
    await vscode.commands.executeCommand('rift.openSegment', 3);
    assert.ok(diffTab(), 'the hunk of the shown session opens');
    assert.strictEqual(x.comparison()!.state, 'ready');
  });

  const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));

  it('stops a replaced compare that is waiting between two busy retries', async () => {
    await compare('a.sql', 'b.sql');
    await ready(x);
    clearRequests();
    // Busy at 0 and 150 ms, it would open at 450 ms and evict the shown session.
    const busy = compare('busy-r1-evictor.sql', 'b.sql');
    await sleep(250);
    await Promise.all([busy, compare('a.sql', 'b.sql')]);
    await sleep(800);
    await vscode.commands.executeCommand('rift.openSegment', 3);
    assert.ok(diffTab(), 'the hunk of the shown session opens');
    assert.strictEqual(requests().filter((r) => r.method === 'open' && String(r.params.a).includes('r1-evictor')).length, 2);
  });

  it('lets the shown comparison work while a newer compare is still opening', async () => {
    const msg = captureMessages();
    try {
      await compare('a.sql', 'b.sql');
      await ready(x);
      const pending = compare('stuck-live.sql', 'b.sql');
      await sleep(20);
      await vscode.commands.executeCommand('rift.openSegment', 3);
      assert.ok(diffTab(), 'a click on the shown comparison opens its hunk');
      await pending;
    } finally {
      msg.restore();
    }
  });

  it('closes the late session of another pair while a compare of the shown pair waits for its room', async () => {
    const msg = captureMessages();
    try {
      await compare('a.sql', 'b.sql');
      await until(() => x.comparison()?.state === 'ready' && x.comparison()!.segments.length > 0);
      // The replaced compare's open answers late, evicts the shown session and holds the room (busy for
      // everyone else) until it is closed; the compare of the shown pair needs that room.
      const older = compare('lag-hog-evictor.sql', 'b.sql');
      await sleep(20);
      await Promise.all([older, compare('a.sql', 'b.sql')]);
      await until(() => x.comparison()?.state === 'ready' && x.comparison()!.segments.length > 0, 3000);
      await vscode.commands.executeCommand('rift.openSegment', 3);
      assert.ok(diffTab(), 'a hunk opens');
      assert.deepStrictEqual(msg.error, []);
    } finally {
      msg.restore();
    }
  });

  it('sends no close for the session a failed comparison shares with its replacement', async () => {
    const msg = captureMessages();
    try {
      await compare('sumfail.sql', 'b.sql');
      await until(() => x.comparison()?.state === 'failed');
      clearRequests();
      await compare('sumfail.sql', 'b.sql');
      await sleep(200);
      const shown = x.comparison()!.session;
      assert.ok(!requests().some((r) => r.method === 'close' && r.params.session === shown), 'the shown session is not closed');
    } finally {
      msg.restore();
    }
  });

  it('closes the session of a compare that finished late and is not shown', async () => {
    await compare('a.sql', 'b.sql');
    await ready(x);
    clearRequests();
    // Opens its own session after the answer's delay; replaced by then, nobody shows it.
    const older = compare('lag-s1.sql', 'b.sql');
    await sleep(20);
    await Promise.all([older, compare('a.sql', 'b.sql')]);
    await sleep(200);
    const shown = x.comparison()!.session;
    assert.ok(requests().some((r) => r.method === 'close' && r.params.session !== shown), 'its session is closed');
    assert.ok(!requests().some((r) => r.method === 'close' && r.params.session === shown));
  });

  it('shows no error for a replaced compare whose open was refused', async () => {
    const msg = captureMessages();
    try {
      // Busy on every try: it fails when replaced, and that is not the user's to see.
      const older = compare('stuck-gate.sql', 'b.sql');
      await sleep(20);
      await Promise.all([older, compare('a.sql', 'b.sql')]);
      assert.deepStrictEqual(msg.error, []);
    } finally {
      msg.restore();
    }
  });

  it('replaces a failed comparison when the same pair is compared again', async () => {
    const msg = captureMessages();
    try {
      await compare('sumfail.sql', 'b.sql');
      await until(() => x.comparison()?.state === 'failed');
      const failed = x.comparison();
      await compare('sumfail.sql', 'b.sql');
      assert.notStrictEqual(x.comparison(), failed);
    } finally {
      msg.restore();
    }
  });

  it('forgets the shown comparison when the server is stopped for a bad reply', async () => {
    const msg = captureMessages();
    try {
      await compare('garbage.sql', 'b.sql');
      await compare('a.sql', 'b.sql');
      await ready(x);
      // Stops the server; the new process numbers its sessions from 1 again, as a.sql's had.
      await compare('garbage.sql', 'b.sql');
      await compare('c.sql', 'b.sql');
      await ready(x);
      assert.strictEqual(x.comparison()!.a, uri('c.sql').fsPath);
      await vscode.commands.executeCommand('rift.openSegment', 3);
      assert.ok(diffTab());
      assert.ok(msg.warning.some((w) => /not JSON/.test(w)));
    } finally {
      msg.restore();
    }
  });

  it('reopens a compare whose server was stopped while it loaded', async () => {
    const msg = captureMessages();
    try {
      // The pair's second summary is not JSON, so the server is stopped (and not restarted)
      // while the second compare of the same pair loads.
      await compare('sumjunk.sql', 'b.sql');
      await ready(x);
      await compare('sumjunk.sql', 'b.sql');
      await until(() => x.comparison()?.state === 'ready' && x.comparison()!.segments.length > 0);
      await vscode.commands.executeCommand('rift.openSegment', 3);
      assert.ok(diffTab(), 'a click opens a hunk, not nothing');
    } finally {
      msg.restore();
    }
  });
});
