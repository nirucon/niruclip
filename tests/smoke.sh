#!/bin/sh
set -eu
./niruclip --version | grep -q '^NIRUCLIP 0.1.0$'
./niruclip --status | grep -q 'state'
./niruclip --help | grep -q -- '--webcam-only'
