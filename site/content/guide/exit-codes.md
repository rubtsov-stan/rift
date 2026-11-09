+++
title = "Rift exit codes: 0, 1 and 2, as diff uses them"
description = "Exit status 0 means identical files, 1 means they differ, 2 means an error. How to check it in scripts, and the exceptions in key mode and git."
weight = 7
[extra]
nav = "Exit codes"
h1 = "Exit codes"
lead = "Rift follows diff: 0 for identical files, 1 for different files, 2 for an error."
summary = "What each exit status means and how to use it in a script."
+++

| Status | Meaning |
|---|---|
| 0 | The files are identical. |
| 1 | The files differ. |
| 2 | An error: an unreadable file, an operand that is not a regular file (a pipe, FIFO, `<(command)` or directory), a budget too small, a duplicate key in key mode, an invalid option, or `-i` in a build without the terminal view. The message goes to standard error. |

The three cases on the generated pair, with the status printed after each run:

{{ term(name="exit-codes", title="exit status of three runs") }}

## In scripts

A difference is not a failure, so `set -e` stops a script on status 1. Test the status you care about:

```
rift a.sql b.sql > changes.patch
status=$?
[ "$status" -le 1 ] || exit "$status"
```

## When the reader quits early

If standard output closes before Rift finishes, as in `rift a b | head`, Rift stops without a message and exits 141. That is the status a shell reports for `diff` killed by SIGPIPE, so `${PIPESTATUS[0]}` in bash holds 141 for both. This applies to the unified diff, `--summary` and `--key` output.

## Key mode can stop late

A run in [key mode](@/guide/key-mode.md) can stop with status 2 after it printed some changes, for example when a key occurs twice. The message on standard error says the output is incomplete. Check the status before you use the output.

## Git

`rift git-diff`, new in 0.3.0, is the one exception: it exits 0 whether or not the files differ, because git treats any other status as a failure. It also exits 0, silently, when git's pager quits before the diff ends. See [git integration](@/guide/git.md).
