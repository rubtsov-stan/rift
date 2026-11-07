echo '*.sql diff=rift' >> .gitattributes
git config diff.rift.command 'rift git-diff --mem 1G'
git diff dump.sql > out.diff
head -n 10 out.diff | cut -c1-90
