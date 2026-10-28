#!/bin/sh
set -eu
src=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
install -d "$HOME/.local/bin" "$HOME/.local/share/niruclip"
install -m 755 "$src/niruclip" "$HOME/.local/bin/niruclip"
printf 'NIRUCLIP 0.1.9 installed in %s\n' "$HOME/.local/bin"
