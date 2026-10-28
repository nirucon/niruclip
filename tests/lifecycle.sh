#!/bin/sh
set -eu

BIN=${1:-./target/debug/niruclip}
TMP=$(mktemp -d "${TMPDIR:-/tmp}/niruclip-test.XXXXXX")
trap 'rm -rf "$TMP"' EXIT HUP INT TERM
mkdir -p "$TMP/state" "$TMP/config" "$TMP/out"

cat > "$TMP/ffmpeg-ok" <<'EOF'
#!/bin/sh
last=
for arg in "$@"; do last=$arg; done
if [ -n "${FAKE_LOG:-}" ]; then printf '%s\n' "$@" > "$FAKE_LOG"; fi
: > "$last"
printf 'partial' >> "$last"
trap 'exit 0' INT TERM
sleep 30 &
wait $!
EOF
cat > "$TMP/ffmpeg-fail" <<'EOF'
#!/bin/sh
last=
for arg in "$@"; do last=$arg; done
: > "$last"
printf 'bad' >> "$last"
exit 1
EOF
cat > "$TMP/ffmpeg-interrupt" <<'EOF'
#!/bin/sh
last=
for arg in "$@"; do last=$arg; done
: > "$last"
printf 'partial' >> "$last"
sleep 0.2
exit 1
EOF
cat > "$TMP/ffprobe" <<'EOF'
#!/bin/sh
last=
for arg in "$@"; do last=$arg; done
grep -q partial "$last" 2>/dev/null || exit 1
printf '%s\n' '{"streams":[{"codec_name":"h264"}],"format":{"duration":"1.5"}}'
EOF
cat > "$TMP/xrandr" <<'EOF'
#!/bin/sh
cat <<'DISPLAY'
Screen 0: minimum 8 x 8, current 1920 x 1200, maximum 32767 x 32767
eDP-1 connected primary 1920x1200+0+0 (normal left inverted right x axis y axis) 344mm x 215mm
DISPLAY
EOF
cat > "$TMP/slop" <<'EOF'
#!/bin/sh
printf '%s\n' '10 20 800 600'
EOF
chmod +x "$TMP"/ffmpeg-ok "$TMP"/ffmpeg-fail "$TMP"/ffmpeg-interrupt "$TMP"/ffprobe "$TMP"/xrandr "$TMP"/slop

common() {
  NIRUCLIP_STATE_DIR="$TMP/state" \
  NIRUCLIP_CONFIG="$TMP/config/config.toml" \
  NIRUCLIP_CAMERA=/dev/zero \
  NIRUCLIP_XRANDR="$TMP/xrandr" \
  NIRUCLIP_SLOP="$TMP/slop" \
  NIRUCLIP_FFPROBE="$TMP/ffprobe" \
  NIRUCLIP_NOTIFY=0 \
  DISPLAY=:0 \
  "$@"
}

wait_state() {
  state=$1
  i=0
  while [ "$i" -lt 100 ]; do
    if common "$BIN" --status --json | grep -q "\"state\": \"$state\""; then return 0; fi
    sleep 0.1
    i=$((i + 1))
  done
  common "$BIN" --status --json >&2
  echo "timed out waiting for state $state" >&2
  return 1
}

# A successful controlled stop finalizes a valid file and clears recovery.
NIRUCLIP_FFMPEG="$TMP/ffmpeg-ok" common "$BIN" --webcam-only --audio none --countdown 0 --output "$TMP/out/one.mkv" >/dev/null 2>&1 &
pid=$!
wait_state recording
common "$BIN" --stop >/dev/null
wait "$pid"
common "$BIN" --status --json | grep -q '"state": "finished"'
[ -s "$TMP/out/one.mkv" ]
[ ! -e "$TMP/out/one.part.mkv" ]

# A second start is rejected while the first owns the session.
before=$(common "$BIN" --status --json | grep -o '"state": "[^"]*"' | head -1)
NIRUCLIP_FFMPEG="$TMP/ffmpeg-ok" common "$BIN" --webcam-only --audio none --countdown 0 --output "$TMP/out/two.mkv" >/dev/null 2>&1 &
pid=$!
wait_state recording
if NIRUCLIP_FFMPEG="$TMP/ffmpeg-ok" common "$BIN" --webcam-only --audio none --countdown 0 --output "$TMP/out/three.mkv" >/dev/null 2>&1; then
  echo 'parallel start was not rejected' >&2
  exit 1
fi
common "$BIN" --stop >/dev/null
wait "$pid"
[ -s "$TMP/out/two.mkv" ]

# An immediate encoder failure is failed and not advertised as recoverable.
if NIRUCLIP_FFMPEG="$TMP/ffmpeg-fail" common "$BIN" --webcam-only --audio none --countdown 0 --output "$TMP/out/bad.mkv" >/dev/null 2>&1; then
  echo 'failed encoder reported success' >&2
  exit 1
fi
common "$BIN" --status --json | grep -q '"state": "failed"'
common "$BIN" --status --json | grep -q '"recoverable": false'
if common "$BIN" --recover >/dev/null 2>&1; then
  echo 'invalid partial was recovered' >&2
  exit 1
fi

# An interrupted valid partial is recoverable and --recover preserves it.
if NIRUCLIP_FFMPEG="$TMP/ffmpeg-interrupt" common "$BIN" --webcam-only --audio none --countdown 0 --output "$TMP/out/interrupted.mkv" >/dev/null 2>&1; then
  echo 'interrupted recording reported success' >&2
  exit 1
fi
common "$BIN" --status --json | grep -q '"state": "interrupted"'
common "$BIN" --status --json | grep -q '"recoverable": true'
[ -s "$TMP/out/interrupted.part.mkv" ]
common "$BIN" --recover >/dev/null
common "$BIN" --status --json | grep -q '"state": "finished"'
[ -s "$TMP/out/interrupted.mkv" ]

# Spaces and Unicode are one argv path, and geometry/filter choices are explicit.
export FAKE_LOG="$TMP/args.log"; NIRUCLIP_FFMPEG="$TMP/ffmpeg-ok" common "$BIN" --full --webcam corner --corner top-left --audio both --webcam-size large --countdown 0 --output "$TMP/out/record möte.mkv" >/dev/null 2>&1 &
pid=$!
wait_state recording
common "$BIN" --stop >/dev/null
wait "$pid"
[ -s "$TMP/out/record möte.mkv" ]
grep -q '1920x1200' "$TMP/args.log"
grep -q ':0+0,0' "$TMP/args.log"
grep -q 'scale=1728:1080' "$TMP/args.log"
grep -q 'min(iw,690)' "$TMP/args.log"
grep -q '@DEFAULT_SOURCE@' "$TMP/args.log"
grep -q '@DEFAULT_MONITOR@' "$TMP/args.log"
grep -q 'amix=inputs=2' "$TMP/args.log"

# Region uses the selected rectangle rather than a hardcoded display.
export FAKE_LOG="$TMP/region.log"; NIRUCLIP_FFMPEG="$TMP/ffmpeg-ok" common "$BIN" --region --audio none --countdown 0 --output "$TMP/out/region.mkv" >/dev/null 2>&1 &
pid=$!
wait_state recording
common "$BIN" --stop >/dev/null
wait "$pid"
grep -q '800x600' "$TMP/region.log"
grep -q ':0+10,20' "$TMP/region.log"

# Profiles are listable, saveable and apply the camera-only mode.
common "$BIN" --list-profiles | grep -q '^camera-mic'
common "$BIN" --save-profile test-profile --webcam-only --audio none --webcam-size small
common "$BIN" --list-profiles | grep -q '^test-profile'
common "$BIN" --profile test-profile --countdown 0 --status

echo 'NIRUCLIP lifecycle tests: PASS'
