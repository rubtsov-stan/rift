import * as assert from 'assert';
import { spawn } from 'child_process';
import * as fs from 'fs';
import * as os from 'os';
import * as path from 'path';
import { until } from '../mock';

const GROUP = path.join(__dirname, '..', 'group.js');
const alive = (pid: number) => {
  try {
    process.kill(pid, 0);
    return true;
  } catch {
    return false;
  }
};

// Runs `script` through the launcher; it starts two background sleepers and records their pids.
function launch(script: string, direct = false) {
  const pids = path.join(fs.mkdtempSync(path.join(os.tmpdir(), 'rift-group-')), 'pids');
  const sh = `sleep 300 & echo $! >> ${pids}; sleep 300 & echo $! >> ${pids}; ${script}`;
  const p = direct ? spawn('sh', ['-c', sh], { stdio: 'ignore' }) : spawn(process.execPath, [GROUP, 'sh', '-c', sh], { stdio: 'ignore' });
  const exited = new Promise<number | null>((r) => p.on('exit', (code) => r(code)));
  const children = () => (fs.existsSync(pids) ? fs.readFileSync(pids, 'utf8').split('\n').filter(Boolean).map(Number) : []);
  return { p, exited, children };
}

describe('test launcher', () => {
  let left: number[] = [];
  afterEach(() => left.forEach((pid) => alive(pid) && process.kill(pid, 'SIGKILL')));

  it('leaves no child after a command that fails', async () => {
    const t = launch('exit 3');
    assert.strictEqual(await t.exited, 3);
    left = t.children();
    assert.strictEqual(left.length, 2);
    await until(() => left.every((pid) => !alive(pid)));
  });

  it('leaves no child after a signal', async () => {
    const t = launch('sleep 300');
    await until(() => t.children().length === 2);
    left = t.children();
    t.p.kill('SIGTERM');
    assert.strictEqual(await t.exited, 143);
    await until(() => left.every((pid) => !alive(pid)));
  });

  it('is needed: without it the children outlive the command', async () => {
    const t = launch('exit 3', true);
    await t.exited;
    await until(() => t.children().length === 2);
    left = t.children();
    assert.ok(left.every(alive));
  });
});
