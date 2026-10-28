#!/bin/sh
set -eu

VERSION=0.2.0
REPO=${NIRUCLIP_REPO:-nirucon/niruclip}
PREFIX=${NIRUCLIP_PREFIX:-$HOME/.local}
BINDIR=$PREFIX/bin
MODE=install

usage() {
  cat <<'USAGE'
Usage: ./install.sh [--build]
  (no options)  install the released static binary
  --build       build from this checkout and install the result
USAGE
}

say() { printf '%s\n' "$*"; }
die() { say "niruclip install: $*" >&2; exit 1; }
sha256() { sha256sum "$1" | cut -d' ' -f1; }

install_one() {
  src=$1
  mkdir -p "$BINDIR"
  install -m 755 "$src" "$BINDIR/.niruclip-new"
  mv -f "$BINDIR/.niruclip-new" "$BINDIR/niruclip"
}

case "${1:-}" in
  '') ;;
  --build) MODE=build ;;
  -h|--help) usage; exit 0 ;;
  *) usage >&2; exit 2 ;;
esac

if [ "$MODE" = build ]; then
  command -v cargo >/dev/null || die 'cargo is required for --build'
  (cd "$(dirname "$0")" && cargo build --release --locked)
  install_one "$(dirname "$0")/target/release/niruclip"
else
  tag=${NIRUCLIP_VERSION:-$VERSION}
  base=${NIRUCLIP_BASE_URL:-https://github.com/$REPO/releases/download/v$tag}
  tmp=$(mktemp -d)
  trap 'rm -rf "$tmp"' EXIT HUP INT TERM
  for file in niruclip SHA256SUMS; do
    if command -v curl >/dev/null 2>&1; then
      curl -fsSL --retry 3 -o "$tmp/$file" "$base/$file" || die "cannot download $base/$file"
    else
      wget -qO "$tmp/$file" "$base/$file" || die "cannot download $base/$file"
    fi
  done
  expected=$(awk '$2 == "niruclip" {print $1}' "$tmp/SHA256SUMS")
  [ "${#expected}" = 64 ] || die 'missing checksum for niruclip'
  [ "$(sha256 "$tmp/niruclip")" = "$expected" ] || die 'niruclip checksum mismatch'
  [ "$("$tmp/niruclip" --version)" = "NIRUCLIP $tag" ] || die 'release reports the wrong version'
  install_one "$tmp/niruclip"
fi

say "NIRUCLIP $("$BINDIR/niruclip" --version | awk '{print $2}') installed in $BINDIR"
