+++
title = "git diff says \"Binary files differ\" for a large text file"
description = "Git treats a file over 512 MiB as binary and prints no diff. Use rift git-diff as an external diff for those files: setup, LFS and the cost."
weight = 4
[extra]
nav = "git: Binary files differ"
h1 = "git diff says \"Binary files differ\" for a large file"
lead = "The file is plain text. Git still refuses to show a diff once it passes 512 MiB."
summary = "Give git a diff for large text files with rift git-diff."
+++

`rift git-diff` ships in 0.3.0.

Git decides a file is binary when it is larger than 512 MiB. It prints one line and no diff. This is a 640 MiB pair of generated SQL dumps:

{{ term(name="git-binary", title="git diff on a large text file") }}

## The fix

Tell git to run Rift for the large files:

```
echo '*.sql diff=rift' >> .gitattributes
git config diff.rift.command 'rift git-diff --mem 1G'
```

The same command on the same pair, with Rift as the external diff:

{{ term(name="git-rift", title="git diff with rift git-diff") }}

Git's own headers stay, and the hunks come from Rift. Other files keep git's diff, because only `*.sql` is named in `.gitattributes`.

## Before you rely on it

- Git writes each blob that is not in the work tree to a temporary file before it runs the command. For a file not stored with LFS that is a full copy: 10 GB of disk for a 10 GB file.
- For an LFS file Rift reads the object from `.git/lfs/objects` in place, with no copy.
- `rift git-diff` exits 0 whether or not the files differ, because git treats any other status as a failure.

The full setup, including `git difftool` for the terminal view, is in the [git guide](@/guide/git.md).
