rift --key csv:id a.csv b.csv > keys.txt
for k in "~" "-" "+"; do grep -m 2 "^$k" keys.txt; done
cut -c1 keys.txt | sort | uniq -c
