// Drives a real VS Code window through the demo and records it. Run through
// scripts/record-demo.sh, which builds the extension and checks for ffmpeg.
import { createRequire } from 'node:module';
import { execFileSync } from 'node:child_process';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const require = createRequire(import.meta.url);
const { _electron: electron } = require('playwright-core');
const { downloadAndUnzipVSCode } = require('@vscode/test-electron');

const here = path.dirname(fileURLToPath(import.meta.url));
const root = path.resolve(here, '..');
const repo = path.resolve(root, '..', '..');

const opts = { out: path.join(root, 'media'), rift: process.env.RIFT_BIN, size: '1G', mock: false };
const argv = process.argv.slice(2);
for (let i = 0; i < argv.length; i++) {
  if (argv[i] === '--mock') opts.mock = true;
  else if (argv[i] === '--out') opts.out = path.resolve(argv[++i]);
  else if (argv[i] === '--rift') opts.rift = path.resolve(argv[++i]);
  else if (argv[i] === '--size') opts.size = argv[++i];
  else throw new Error(`unknown argument ${argv[i]}`);
}

const tmp = fs.mkdtempSync(path.join(os.tmpdir(), 'rift-demo-'));
const sh = (cmd, args) => execFileSync(cmd, args, { stdio: ['ignore', 'inherit', 'inherit'] });

try {
  await main();
} finally {
  fs.rmSync(tmp, { recursive: true, force: true });
}

async function main() {
  const work = path.join(tmp, 'work');
  let names;
  let riftBin;
  if (opts.mock) {
    // The mock answers every pair with its fixture; the files only need to exist.
    fs.mkdirSync(work);
    for (const f of ['a.sql', 'b.sql']) fs.writeFileSync(path.join(work, f), 'select 1;\n');
    names = ['a.sql', 'b.sql'];
    riftBin = path.join(tmp, 'rift');
    const mock = path.join(root, 'test', 'mock-server.js');
    fs.writeFileSync(riftBin, `#!/bin/sh\nexec "${process.execPath}" "${mock}"\n`, { mode: 0o755 });
  } else {
    riftBin = opts.rift ?? path.join(repo, 'target', 'release', 'rift');
    // Seed 7, the same pair as bench/data.sh and bench/ci-mem.sh.
    sh(path.join(repo, 'target', 'release', 'rift-gen'), ['--seed', '7', '--size', opts.size, '--profile', 'sql', '--out', work]);
    names = ['a', 'b'];
  }

  const userData = path.join(tmp, 'user-data');
  fs.mkdirSync(path.join(userData, 'User'), { recursive: true });
  fs.writeFileSync(
    path.join(userData, 'User', 'settings.json'),
    JSON.stringify({
      'rift.path': riftBin,
      'workbench.startupEditor': 'none',
      'workbench.tips.enabled': false,
      'workbench.colorTheme': 'Default Dark Modern',
      'update.mode': 'none',
      'telemetry.telemetryLevel': 'off',
      'editor.minimap.enabled': false,
      'chat.disableAIFeatures': true,
      'workbench.secondarySideBar.defaultVisibility': 'hidden',
    }),
  );

  // The native context menu of macOS cannot be driven, so the demo selects the two
  // files in the Explorer and runs the command with both paths from a keybinding.
  fs.writeFileSync(
    path.join(userData, 'User', 'keybindings.json'),
    JSON.stringify([{ key: 'f9', command: 'rift.compareFiles', args: { a: path.join(work, names[0]), b: path.join(work, names[1]) } }]),
  );

  const executablePath = await downloadAndUnzipVSCode();
  const videoDir = path.join(tmp, 'video');
  const env = { ...process.env };
  delete env.ELECTRON_RUN_AS_NODE;
  const app = await electron.launch({
    executablePath,
    env,
    args: [
      work,
      `--extensionDevelopmentPath=${root}`,
      `--user-data-dir=${userData}`,
      `--extensions-dir=${path.join(tmp, 'ext')}`,
      '--disable-workspace-trust',
      '--skip-welcome',
      '--skip-release-notes',
      '--new-window',
    ],
    recordVideo: { dir: videoDir, size: { width: 1280, height: 800 } },
  });
  const launched = Date.now();
  const page = await app.firstWindow();
  await app.evaluate(({ BrowserWindow }) => {
    const w = BrowserWindow.getAllWindows()[0];
    w.setContentSize(1280, 800);
    w.center();
  });
  const pause = (ms) => page.waitForTimeout(ms);
  const mod = process.platform === 'darwin' ? 'Meta' : 'Control';
  const row = (name) => page.locator(`.monaco-list-row[aria-label="${name}"]`).first();

  await row(names[0]).waitFor({ timeout: 60000 });
  await pause(1500);
  // The first seconds of the recording are an empty window; the cut skips them.
  const trim = Math.max(0, (Date.now() - launched) / 1000 - 0.3);
  await row(names[0]).click();
  await row(names[1]).click({ modifiers: [mod] });
  await pause(800);
  await pause(800);
  await page.keyboard.press('F9');
  await pause(1500);

  await page.locator('.activitybar [aria-label^="Rift"]').first().click();
  // A real scan of a large pair takes a while; the tree shows its progress meanwhile.
  await page.locator('.monaco-list-row', { hasText: /^Differences/ }).waitFor({ timeout: opts.mock ? 30000 : 20 * 60 * 1000 }).catch(async (e) => {
    await page.screenshot({ path: path.join(opts.out, 'rift-demo-failed.png') });
    throw e;
  });
  await pause(2000);

  // A hunk is shown once the diff editor has lines; the pause lets the view settle.
  const shown = async () => {
    await page.locator('.monaco-diff-editor .view-line').first().waitFor({ timeout: 60000 });
    await pause(2500);
  };
  const segments = page.locator('.monaco-list-row', { hasText: '↔ B' });
  await segments.nth(0).click();
  await shown();
  await segments.nth(2).click();
  await shown();
  const posterAt = (Date.now() - launched) / 1000 - 1;
  await page.keyboard.press('Control+Alt+N');
  await shown();
  await page.keyboard.press('Control+Alt+P');
  await shown();

  // Fold the long list so the moved-blocks group is on screen.
  await page.locator('.monaco-list-row', { hasText: /^Differences/ }).first().click();
  await pause(800);
  const moves = page.locator('.monaco-list-row', { hasText: /^Moved blocks/ }).first();
  await moves.waitFor({ timeout: 10000 });
  await moves.click();
  await pause(1200);
  await page.locator('.monaco-list-row', { hasText: '→ B' }).first().click();
  await shown();

  await pause(1000);
  const video = page.video();
  await app.close();
  const webm = await video.path();

  fs.mkdirSync(opts.out, { recursive: true });
  // The capture has a few grey rows below the window; cut them and restore 1280x800.
  const fit = 'crop=1280:794:0:0,scale=1280:800';
  const mp4 = path.join(opts.out, 'rift-demo.mp4');
  sh('ffmpeg', ['-y', '-v', 'error', '-ss', String(trim), '-i', webm, '-c:v', 'libx264', '-pix_fmt', 'yuv420p', '-crf', '26', '-vf', fit, '-movflags', '+faststart', mp4]);

  // The GIF must stay under 3 MB: step down until it does.
  const gif = path.join(opts.out, 'rift-demo.gif');
  for (const [fps, width] of [[8, 800], [6, 720], [5, 640], [4, 560], [3, 480]]) {
    const vf = `${fit},fps=${fps},scale=${width}:-1:flags=lanczos,split[s0][s1];[s0]palettegen=max_colors=96[p];[s1][p]paletteuse=dither=bayer:bayer_scale=4`;
    sh('ffmpeg', ['-y', '-v', 'error', '-ss', String(trim), '-i', webm, '-vf', vf, '-loop', '0', gif]);
    if (fs.statSync(gif).size < 3 * 1024 * 1024) break;
  }

    const poster = path.join(opts.out, 'rift-demo-poster.png');
  sh('ffmpeg', ['-y', '-v', 'error', '-ss', String(posterAt), '-i', webm, '-vf', fit, '-frames:v', '1', poster]);

  // The site shows the poster as a JPEG to keep the landing page light.
  const posterJpg = path.join(opts.out, 'rift-demo-poster.jpg');
  sh('ffmpeg', ['-y', '-v', 'error', '-i', poster, '-q:v', '4', posterJpg]);

  for (const f of [mp4, gif, poster, posterJpg]) console.log(`${f} ${(fs.statSync(f).size / 1024).toFixed(0)} KiB`);
}
