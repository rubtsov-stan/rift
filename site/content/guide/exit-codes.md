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
| 2 | An error: an unreadable file, a budget too small, a duplicate key in key mode, an invalid option, or `-i` in a build without the terminal view. The message goes to standard error. |

The three cases on the generated pair, with the status printed after each run:

{{ term(name="exit-codes", title="exit status of three runs") }}

## In scripts

A difference is not a failure, so `set -e` stops a script on status 1. Test the status you care about:

```
rift a.sql b.sql > changes.patch
status=$?
[ "$status" -le 1 ] || exit "$status"
```

## Key mode can stop late

A run in [key mode](@/guide/key-mode.md) can stop with status 2 after it printed some changes, for example when a key occurs twice. The message on standard error says the output is incomplete. Check the status before you use the output.

## Git

`rift git-diff`, not in v0.2.0 and planned for 0.3, is the one exception: it exits 0 whether or not the files differ, because git treats any other status as a failure. See [git integration](@/guide/git.md).
