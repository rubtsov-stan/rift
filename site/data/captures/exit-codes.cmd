rift a.sql a.sql > /dev/null; echo "identical: exit $?"
rift a.sql b.sql > /dev/null; echo "differ: exit $?"
rift a.sql missing.sql > /dev/null; echo "missing file: exit $?"
