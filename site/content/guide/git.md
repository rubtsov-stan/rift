+++
title = "Git integration: diff large files that git calls binary"
description = "Use rift git-diff as git's external diff for large files only. Setup with .gitattributes, LFS pointers, the temp-copy cost and git difftool."
weight = 8
[extra]
nav = "Git"
h1 = "Git integration"
lead = "Git calls any file over 512 MiB binary and prints no diff. rift git-diff gives git a real diff for those files."
summary = "Set up rift git-diff as an external diff for large files, and know what it costs."
+++

On a 640 MiB pair of generated SQL dumps, git prints this:

{{ term(name="git-binary", title="git on a file over 512 MiB") }}

With Rift as the external diff, the same command prints git's headers and a unified diff:

{{ term(name="git-rift", title="git with rift git-diff") }}

## Set it up for large files only

Name the large files in `.gitattributes` and define the driver, so everything else keeps git's own diff:

```
echo '*.sql diff=rift' >> .gitattributes
echo '*.csv diff=rift' >> .gitattributes
git config diff.rift.command 'rift git-diff --mem 1G'
```

Git runs the command through the shell, so options go inside the quotes.

`rift git-diff` takes the seven arguments git passes: `path old-file old-hex old-mode new-file new-hex new-mode`, and for a rename or copy also the new path and git's rename lines. It prints git's headers (`diff --git`, mode lines, `index`, `--- a/path`, `+++ b/path`, and `/dev/null` for an added or deleted file) and the unified diff from Rift.

## Exit status

The command exits 0 whether or not the files differ, because git treats any other status as a failure and stops with status 128. The status of `git diff` is git's own: 0, or 1 with `--exit-code` when there are differences. Rift exits 2 on an error.

## What differs from git's output

- Hunk headers carry no function name.
- The hash of a work tree file is written as `0000000`, because git does not pass one.
- Hashes are always 7 characters.

## LFS pointers

If a side is a Git LFS pointer, Rift reads the object from `.git/lfs/objects` of the common git directory and reads it in place. A missing object stops the run and names its oid: fetch it with `git lfs fetch`. If the object's size differs from the pointer's, Rift stops. Rift does not check the hash and ignores `lfs.storage`.

## The cost without LFS

Git writes every blob that is not in the work tree to a temporary file before it runs the command. For a file that is not stored with LFS, that is a full copy: on a 10 GB file, 10 GB of disk and the time to write it. Comparing a work tree file with the index makes one copy. Comparing two commits makes two.

## The terminal view

`git difftool` opens the [terminal view](@/guide/tui.md). It passes the two files as `$LOCAL` and `$REMOTE` and does not resolve LFS pointers:

```
git config difftool.rift.cmd 'rift -i "$LOCAL" "$REMOTE"'
git config difftool.prompt false
git difftool -t rift
```
