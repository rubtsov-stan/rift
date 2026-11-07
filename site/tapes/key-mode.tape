# Recorded by site/scripts/regenerate-demos.sh from the repository root.
# poster: 5.2
Output site/static/key-mode.gif
Output site/static/key-mode.mp4

Set Shell "bash"
Set FontSize 15
Set Width 960
Set Height 400
Set Padding 12
Set Framerate 12
Set TypingSpeed 40ms

Hide
Type 'export PS1="$ " && export PATH="$PWD/target/release:$PATH"'
Enter
Type "D=$(mktemp -d /tmp/d.XXXX) && rift-gen --seed 7 --size 32M --profile shuffled --rows csv --out $D >/dev/null && cd $D && mv a a.csv && mv b b.csv"
Enter
Type "clear"
Enter
Show

Type "head -n 2 a.csv | cut -c1-100"
Enter
Sleep 1500ms
Type "rift --key csv:id a.csv b.csv > keys.txt; head -n 8 keys.txt; wc -l < keys.txt"
Enter
Sleep 3s
