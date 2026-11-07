+++
title = "Rift in VS Code: compare two huge files from the editor"
description = "The Rift extension compares two large files in VS Code: a tree of segments, two-pane hunk diffs, next and previous, and a group for moved blocks."
weight = 10
draft = true
[extra]
nav = "VS Code"
h1 = "Rift in VS Code"
lead = "The extension runs Rift for you and shows the result in the editor you already use."
summary = "Compare two large files in VS Code with the Rift extension."
+++

Run **Rift: Compare Files** from the command palette and choose two files. The extension shows the differences as a tree. Expand a segment to see its hunks, open a hunk in a two-pane diff, and step through hunks with next and previous. Moved blocks have a group of their own.

{{ media(ready=true, mp4="vscode-demo.mp4", gif="vscode-demo.gif", poster="vscode-demo-poster.jpg", width=1280, height=800, alt="The Rift extension comparing two large files in VS Code", caption="A recording on a generated 1 GiB pair.") }}

The extension talks to the same engine as the command line, so the [`--mem`](@/guide/memory.md) limit and the guarantees on this site apply.
