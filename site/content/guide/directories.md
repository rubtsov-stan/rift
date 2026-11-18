+++
title = "Directory diff: rift old/ new/ compares two trees"
description = "Compare two directory trees under one --mem, as diff -r prints them: files only in one tree, renames found by content, a unified diff per changed file."
weight = 11
[extra]
nav = "Directories"
h1 = "Directory diff"
lead = "Given two directories, Rift compares the trees file by file under the same memory limit, and finds renamed files by their content."
summary = "Compare two trees: added, removed, renamed and changed files, as diff -r prints them."
+++

```
rift old/ new/
```

New in 0.4. Rift walks both trees in step, one directory at a time, and prints what `diff -r` prints: an `Only in` line for an entry in one tree, a unified diff under a `diff -r` header for each changed file, and `Binary files ... differ` when the first 8 KiB of either file hold a NUL byte. Two lines are Rift's own: `Renamed:` for a file that moved, and `Renames approximate:` when the rename table filled (see below).

Two small trees, `A` and `B`: a file edited, one renamed, one removed, one added, a directory in one tree only, an empty directory, a path that is a directory in `A` and a file in `B`, and a symbolic link whose target changed:

{{ term(name="dir-diff", kind="diff", title="two small trees") }}

Names are in byte order within each directory, and a directory's contents come right after it. A renamed file appears once, at its new path. A directory in one tree only is one `Only in` line, as in `diff -r`; its files are still candidates for renames. `patch` applies the diffs of changed files and skips the other lines, so it does not create or delete files.

## --summary

`--summary` prints one status per path, relative to the two roots, then the counts, the peak heap and the time:

{{ term(name="dir-summary", title="one status per path") }}

`A` added, `D` removed, `M` changed, `R` renamed, `T` the type changed (a file became a directory or a link). A directory in one tree only ends in `/`. The counts are of entries that are not directories; files inside a removed or added directory count too.

## Renames

A file only in `A` and a file only in `B` are a rename when their bytes are equal. Rift finds them in three walks of the trees: the first counts the one-sided files by size, the second hashes the one-sided files whose size occurs in the other tree, and the files of one size and hash are then compared byte for byte. A hash match alone is never reported. Empty files are never renames. Only identical content counts: a file renamed and edited shows as removed and added.

When several files have the same content, removed and added files pair in byte order of their paths: the first removed with the first added, the second with the second, and the rest stay `Only in`. The result depends on the paths alone, not on the order the file system lists them in.

The rename table holds the candidates in the memory the walk leaves free. When it fills, the first line of the output says so, with a `--mem` to try, and the files that did not fit are listed as `Only in`: the listing stays correct, only some renames are missing.

## Links, special files and errors

- A symbolic link is compared by its target and never followed, so a link out of the tree or a loop is safe. The two operands themselves are followed.
- A FIFO, socket or device is never opened. One on both sides is reported on standard error as not compared.
- An entry Rift cannot read, such as a directory without permission, is reported on standard error and skipped; the walk goes on.
- A path that is a file in one tree and a directory in the other prints `File A/x is a directory while file B/x is a regular file`.
- An empty directory in one tree only is an `Only in` line.
- Names are bytes: they sort as bytes, and a name with a space, a control character or a byte above 0x7f is quoted as GNU diff quotes it.

## Exit status

0 when the trees are the same, 1 when they differ, 2 when something could not be compared, as with `diff -r`. With status 2 the rest of the output is still complete. A directory and a file (`rift dir file`) are refused with status 2, and so are `--key` and `-i` with directories.

## Memory

The whole run stays under one `--mem`. Each changed file is diffed in turn with the same budget, as `rift A/x B/x` would, and its memory is freed before the walk goes on. The walk holds the listings of the directories on its current path, 8 bytes per entry plus the bytes of its name, on each side. The listings, the rename table and Rift's buffers may take a quarter of the budget, so each file diff keeps the rest.

A tree can hold millions of files; what is bounded is one directory. At the default `256M`, a directory that both trees hold fits about 2 million entries with 8-byte names, or about 1.2 million with 20-byte names. A directory that does not fit is refused with a limit to rerun with:

{{ term(name="dir-refused", title="20 000 entries per tree at --mem 1M") }}

A file inside a directory diff can come out a little different from `rift A/x B/x` at the same `--mem`, since the walk holds part of the budget while it runs: the map of a file gets less room, so more of it can be coarse. The output is still a correct diff, and the same trees and `--mem` always give the same bytes, whatever the thread count or the order of the listings.

## pg_dump -Fd

`pg_dump -Fd` compresses each table by default (`toc.dat` plus `NNNN.dat.gz`), so every table shows as `Binary files ... differ`. Dump with `-Z0` (`--compress=0`) to get plain `NNNN.dat` files that Rift diffs line by line. `toc.dat` is binary in either case: when it differs, for instance between dumps taken at different times or of different schemas, it shows as `Binary files A/toc.dat and B/toc.dat differ`; two dumps taken in the same second of an unchanged schema can have the same `toc.dat`. An `UPDATE` can move a row to the end of its table's data, so it shows as a removed line and an added line at the end, not as one changed line. Reading the compressed files is [#73](https://github.com/rubtsov-stan/rift/issues/73).
