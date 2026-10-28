#!/bin/sh
set -eu
BIN=${1:-./target/debug/niruclip}
expected=$(awk -F '"' '/^version = / {print $2; exit}' "$(dirname "$0")/../Cargo.toml")
[ "$("$BIN" --version)" = "NIRUCLIP $expected" ]
"$BIN" --status | grep -q '^state:'
"$BIN" --help | grep -q -- '--webcam-only'
"$BIN" --list-profiles | grep -q '^screen-camera'
echo 'NIRUCLIP smoke tests: PASS'
