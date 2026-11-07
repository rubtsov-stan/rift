import { execFileSync } from 'child_process';
import * as fs from 'fs';
import * as os from 'os';
import * as path from 'path';
import { runTests } from '@vscode/test-electron';

// Downloads a stable VS Code on first use (into .vscode-test) and runs the
// suite in its extension host. With RIFT_BIN set the suite runs against that
// `rift` binary on a generated pair (RIFT_PAIR_SIZE, default 1G, seed 7)
// instead of the mock; RIFT_PAIR_A and RIFT_PAIR_B reuse an existing pair.
async function main(): Promise<void> {
  const root = path.resolve(__dirname, '..', '..');
  let tmp: string | undefined;
  if (process.env.RIFT_BIN) {
    process.env.RIFT_BIN = path.resolve(process.env.RIFT_BIN);
    if (!process.env.RIFT_PAIR_A || !process.env.RIFT_PAIR_B) {
      tmp = fs.mkdtempSync(path.join(os.tmpdir(), 'rift-pair-'));
      const gen = path.join(path.dirname(process.env.RIFT_BIN), 'rift-gen');
      execFileSync(gen, ['--seed', '7', '--size', process.env.RIFT_PAIR_SIZE ?? '1G', '--profile', 'sql', '--out', tmp], { stdio: 'inherit' });
      process.env.RIFT_PAIR_A = path.join(tmp, 'a');
      process.env.RIFT_PAIR_B = path.join(tmp, 'b');
    }
  }
  try {
    await runTests({
      extensionDevelopmentPath: root,
      extensionTestsPath: path.join(__dirname, 'suite', 'index'),
      launchArgs: ['--disable-extensions', '--disable-workspace-trust', '--skip-welcome', '--skip-release-notes'],
    });
  } finally {
    if (tmp) fs.rmSync(tmp, { recursive: true, force: true });
  }
}

main().catch((e) => {
  console.error(e);
  process.exit(1);
});
