import { spawn } from 'child_process';

// Runs `group <command> [args...]` in a process group of its own and kills the whole group when
// the command ends, however it ends: exit, failure, or a signal sent to this process. A test
// that leaves children behind (VS Code's extension host, the mock servers) cannot outlive the run.
// A SIGKILL of this process cannot be caught, and its group is then left running.
const [command, ...args] = process.argv.slice(2);
const child = spawn(command, args, { stdio: 'inherit', detached: process.platform !== 'win32' });

function sweep(): void {
  try {
    if (process.platform === 'win32') child.kill('SIGKILL');
    else process.kill(-child.pid!, 'SIGKILL');
  } catch {
    // The group is already gone.
  }
}

// The child left the foreground group, so a ^C does not reach it: sweep on the signal instead.
for (const [sig, n] of [['SIGINT', 2], ['SIGTERM', 15], ['SIGHUP', 1]] as const) {
  process.on(sig, () => {
    sweep();
    process.exit(128 + n);
  });
}
child.on('error', (e) => {
  console.error(e.message);
  sweep();
  process.exit(127);
});
child.on('exit', (code, signal) => {
  sweep();
  process.exit(code ?? (signal ? 129 : 1));
});
