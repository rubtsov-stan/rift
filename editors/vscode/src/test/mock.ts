import * as fs from 'fs';
import * as os from 'os';
import * as path from 'path';

// An executable that runs test/mock-server.js, so the mock can stand in for
// the `rift` binary, which is called as `rift serve`. `env` is passed to the mock.
export function mockBinary(env: Record<string, string> = {}): string {
  const vars = Object.entries(env).map(([k, v]) => `${k}=${v} `).join('');
  const mock = path.join(__dirname, '..', '..', 'test', 'mock-server.js');
  const file = path.join(fs.mkdtempSync(path.join(os.tmpdir(), 'rift-mock-')), 'rift');
  fs.writeFileSync(file, `#!/bin/sh\n${vars}ELECTRON_RUN_AS_NODE=1 exec "${process.execPath}" "${mock}"\n`, { mode: 0o755 });
  return file;
}

export async function until(f: () => boolean, ms = 5000): Promise<void> {
  for (let t = 0; t < ms && !f(); t += 10) await new Promise((r) => setTimeout(r, 10));
  if (!f()) throw new Error('condition not reached');
}

// Set to the `rift` binary to run the suite against the real server instead of
// the mock. The two files of the pair come from RIFT_PAIR_A and RIFT_PAIR_B
// (src/test/run.ts generates them when they are not set).
export const REAL = process.env.RIFT_BIN;

// The requests the mock saw, when it runs with MOCK_LOG=`file`.
export function mockLog(file: string): { method: string; params: Record<string, unknown> }[] {
  return fs.existsSync(file) ? fs.readFileSync(file, 'utf8').trim().split('\n').filter(Boolean).map((l) => JSON.parse(l)) : [];
}
