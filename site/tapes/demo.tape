# Recorded by site/scripts/regenerate-demos.sh from the repository root.
# poster: 3.6
Output site/static/demo.gif
Output site/static/demo.mp4

Set Shell "bash"
Set FontSize 15
Set Width 960
Set Height 540
Set Padding 12
Set Framerate 12
Set TypingSpeed 40ms

Hide
Type 'export PS1="$ " && export PATH="$PWD/target/release:$PATH"'
Enter
Type "D=$(mktemp -d /tmp/d.XXXX) && rift-gen --seed 7 --size 64M --profile sql --out $D >/dev/null && cd $D && mv a a.sql && mv b b.sql"
Enter
Type "clear"
Enter
Show

Type "rift --summary a.sql b.sql"
Enter
Sleep 3s

Hide
Type "clear"
Enter
Show

Type "rift tui a.sql b.sql"
Enter
Sleep 1500ms
Type "n"
Sleep 1200ms
Type "n"
Sleep 1200ms
Type "n"
Sleep 1200ms
Type "p"
Sleep 1200ms
Type "q"
Sleep 500ms
