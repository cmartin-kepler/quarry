#!/bin/sh
# Sidecar stub: echo a fixture JSON, ignoring the tool's real trailing args
# (source path / page / bbox). Used to test the shell-out + adapt path.
cat "$1"
