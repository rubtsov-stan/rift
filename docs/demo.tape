# Record with: vhs docs/demo.tape  (run from the repository root, after
# cargo build --release -p rift-diff -p rift-gen)
Output docs/demo.gif

Set Shell "bash"
Set FontSize 14
Set Width 900
Set Height 500
Set Padding 10
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
