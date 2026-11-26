#!/bin/sh
set -eu
BIN=${1:-./target/debug/niruclip}
expected=$(awk -F '"' '/^version = / {print $2; exit}' "$(dirname "$0")/../Cargo.toml")
[ "$("$BIN" --version)" = "NIRUCLIP $expected" ]
"$BIN" --status | grep -q '^state:'
"$BIN" --help | grep -q -- '--webcam-only'
"$BIN" --help | grep -q -- '--mic SOURCE'
"$BIN" --help | grep -q -- '--mic-channels'
"$BIN" --list-profiles | grep -q '^screen-camera'
"$BIN" --help | grep -q -- '--window-id ID'
"$BIN" --help | grep -q -- '--camera-preview'
# A window id that is not a number is refused before anything starts.
if "$BIN" --window-id nope --countdown 0 2>"${TMPDIR:-/tmp}/niruclip-smoke.$$"; then exit 1; fi
grep -q 'needs an X window id' "${TMPDIR:-/tmp}/niruclip-smoke.$$"
rm -f "${TMPDIR:-/tmp}/niruclip-smoke.$$"
echo 'NIRUCLIP smoke tests: PASS'
