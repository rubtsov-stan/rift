# Rift

Rift compares text files of any size, from megabytes to hundreds of gigabytes, under a hard memory limit set on the command line. It builds a global skeleton of the match from content-sampled anchor lines, refines only the regions that need it, and always emits a correct edit script: applied to the first file, it yields the second. Regions it cannot refine within the budget are marked approximate instead of guessed.

Status: early development.
