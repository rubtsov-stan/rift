import * as path from 'path';
import { runTests } from '@vscode/test-electron';

// Downloads a stable VS Code on first use (into .vscode-test) and runs the
// suite in its extension host.
async function main(): Promise<void> {
  const root = path.resolve(__dirname, '..', '..');
  await runTests({
    extensionDevelopmentPath: root,
    extensionTestsPath: path.join(__dirname, 'suite', 'index'),
    launchArgs: ['--disable-extensions', '--disable-workspace-trust', '--skip-welcome', '--skip-release-notes'],
  });
}

main().catch((e) => {
  console.error(e);
  process.exit(1);
});
