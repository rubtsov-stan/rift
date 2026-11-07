rift a.sql b.sql > changes.patch
patch a.sql changes.patch
cmp a.sql b.sql && echo identical
