git -c diff.external='rift git-diff' diff --no-index a.sql b.sql > out.diff
head -n 12 out.diff | cut -c1-100
