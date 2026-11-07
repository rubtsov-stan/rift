# Rift for VS Code

Compare two text files of tens to hundreds of GB without opening them in an
editor. The Rift side panel lists the differing segments and the moved blocks;
selecting one opens VS Code's diff editor on just that excerpt.

Select two files in the Explorer and choose **Compare with Rift**, or run
**Rift: Compare Files…**.

## Settings

| Setting | Default | Meaning |
|---|---|---|
| `rift.path` | empty | `rift` binary to run. Empty uses the bundled one. |
| `rift.mem` | `256M` | Heap budget of one comparison, as for `rift --mem`. |
| `rift.contextLines` | `3` | Lines of context around a difference. |

## Commands

| Command | Key |
|---|---|
| Rift: Next Difference | `Ctrl+Alt+N` |
| Rift: Previous Difference | `Ctrl+Alt+P` |
| Rift: Go to Line in File A… | `Ctrl+Alt+G` |

Everything stays on your machine. The extension sends nothing anywhere.
