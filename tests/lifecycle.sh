#!/bin/sh
set -eu

BIN=${1:-./target/debug/niruclip}
TMP=$(mktemp -d "${TMPDIR:-/tmp}/niruclip-test.XXXXXX")
trap 'rm -rf "$TMP"' EXIT HUP INT TERM
mkdir -p "$TMP/state" "$TMP/config" "$TMP/out"

cat > "$TMP/ffmpeg-ok" <<'EOF'
#!/bin/sh
last=; progress=; prev=; vaapi=
for arg in "$@"; do
  [ "$prev" = "-progress" ] && progress=$arg
  [ "$arg" = "h264_vaapi" ] && vaapi=1
  prev=$arg
  last=$arg
done
# The one-frame VAAPI probe writes to the null muxer ("-").
if [ "$last" = "-" ]; then
  [ -z "${FAKE_PROBE_COUNT:-}" ] || printf 'probe\n' >> "$FAKE_PROBE_COUNT"
  [ "${FAKE_VAAPI_PROBE:-fail}" = ok ] && exit 0
  exit 1
fi
if [ -n "${FAKE_LOG:-}" ]; then printf '%s\n' "$@" > "$FAKE_LOG"; fi
if [ -n "$vaapi" ] && [ "${FAKE_VAAPI_RUNTIME:-ok}" = fail ]; then
  echo 'Failed to initialise VAAPI connection' >&2
  : > "$last"
  exit 1
fi
: > "$last"
printf 'partial' >> "$last"
[ -z "$progress" ] || printf 'frame=1\nfps=30.0\ndrop_frames=0\ndup_frames=0\nspeed=1.0x\nout_time_us=33333\nprogress=continue\n' > "$progress"
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
last=; progress=; prev=
for arg in "$@"; do
  [ "$prev" = "-progress" ] && progress=$arg
  prev=$arg
  last=$arg
done
: > "$last"
printf 'partial' >> "$last"
[ -z "$progress" ] || printf 'frame=1\nfps=30.0\ndrop_frames=0\ndup_frames=0\nspeed=1.0x\nout_time_us=33333\nprogress=continue\n' > "$progress"
sleep 0.2
exit 1
EOF
cat > "$TMP/ffmpeg-silent" <<'EOF'
#!/bin/sh
last=
for arg in "$@"; do last=$arg; done
: > "$last"
trap 'exit 1' INT TERM
sleep 30 &
wait $!
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
chmod +x "$TMP"/ffmpeg-ok "$TMP"/ffmpeg-fail "$TMP"/ffmpeg-interrupt "$TMP"/ffmpeg-silent "$TMP"/ffprobe "$TMP"/xrandr "$TMP"/slop

common() {
  NIRUCLIP_STATE_DIR="$TMP/state" \
  NIRUCLIP_CONFIG="$TMP/config/config.toml" \
  NIRUCLIP_CAMERA=/dev/zero \
  NIRUCLIP_XRANDR="$TMP/xrandr" \
  NIRUCLIP_SLOP="$TMP/slop" \
  NIRUCLIP_FFPROBE="$TMP/ffprobe" \
  NIRUCLIP_NOTIFY=0 \
  NIRUCLIP_VAAPI_DEVICE="${FAKE_RENDER_NODE:-$TMP/no-render-node}" \
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

# A source that never delivers a frame is a clear failure, not a recording.
if NIRUCLIP_FFMPEG="$TMP/ffmpeg-silent" common "$BIN" --webcam-only --audio none --countdown 0 --output "$TMP/out/silent.mkv" >/dev/null 2>&1; then
  echo 'silent capture reported success' >&2
  exit 1
fi
common "$BIN" --status --json | grep -q '"state": "failed"'
common "$BIN" --status --json | grep -q 'capture produced no frames'

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
cam_line=$(grep -n '/dev/zero' "$TMP/args.log" | head -1 | cut -d: -f1)
screen_line=$(grep -n ':0+0,0' "$TMP/args.log" | head -1 | cut -d: -f1)
[ -n "$cam_line" ] && [ -n "$screen_line" ] && [ "$cam_line" -lt "$screen_line" ] || {
  echo 'camera input was not opened before the screen input' >&2
  exit 1
}
common "$BIN" --status --json | grep -q '"performance"'
common "$BIN" --status --json | grep -q '"fps"'

# Region uses the selected rectangle rather than a hardcoded display.
export FAKE_LOG="$TMP/region.log"; NIRUCLIP_FFMPEG="$TMP/ffmpeg-ok" common "$BIN" --region --audio none --countdown 0 --output "$TMP/out/region.mkv" >/dev/null 2>&1 &
pid=$!
wait_state recording
common "$BIN" --stop >/dev/null
wait "$pid"
grep -q '800x600' "$TMP/region.log"
grep -q ':0+10,20' "$TMP/region.log"

# C1: auto selects VAAPI when a render node exists and the probe succeeds,
# and the probe result is cached for the boot.
touch "$TMP/renderD128" "$TMP/renderD129" "$TMP/renderD130"
export FAKE_PROBE_COUNT="$TMP/probe.count"
export FAKE_LOG="$TMP/vaapi.log"
FAKE_RENDER_NODE="$TMP/renderD128" FAKE_VAAPI_PROBE=ok NIRUCLIP_FFMPEG="$TMP/ffmpeg-ok" common "$BIN" --full --audio none --countdown 0 --output "$TMP/out/vaapi.mkv" >/dev/null 2>&1 &
pid=$!
wait_state recording
common "$BIN" --status --json | grep -q '"encoder": "vaapi"'
common "$BIN" --status | grep -q '^encoder: vaapi'
common "$BIN" --stop >/dev/null
wait "$pid"
[ -s "$TMP/out/vaapi.mkv" ]
grep -q '^h264_vaapi$' "$TMP/vaapi.log"
grep -q '^-vaapi_device$' "$TMP/vaapi.log"
grep -q "^$TMP/renderD128\$" "$TMP/vaapi.log"
grep -q 'format=nv12,hwupload\[v\]' "$TMP/vaapi.log"
grep -q '^CQP$' "$TMP/vaapi.log"
if grep -q 'libx264' "$TMP/vaapi.log"; then echo 'VAAPI plan still used libx264' >&2; exit 1; fi
[ "$(wc -l < "$FAKE_PROBE_COUNT")" -eq 1 ]
FAKE_RENDER_NODE="$TMP/renderD128" FAKE_VAAPI_PROBE=ok NIRUCLIP_FFMPEG="$TMP/ffmpeg-ok" common "$BIN" --full --audio none --countdown 0 --output "$TMP/out/vaapi-cached.mkv" >/dev/null 2>&1 &
pid=$!
wait_state recording
common "$BIN" --stop >/dev/null
wait "$pid"
[ "$(wc -l < "$FAKE_PROBE_COUNT")" -eq 1 ] || { echo 'VAAPI probe was not cached' >&2; exit 1; }
grep -q '^h264_vaapi$' "$TMP/vaapi.log"

# C1: a failed probe records in software and says why.
FAKE_RENDER_NODE="$TMP/renderD129" FAKE_VAAPI_PROBE=fail NIRUCLIP_FFMPEG="$TMP/ffmpeg-ok" common "$BIN" --full --audio none --countdown 0 --output "$TMP/out/probe-failed.mkv" >/dev/null 2>&1 &
pid=$!
wait_state recording
common "$BIN" --status --json | grep -q '"encoder": "software"'
common "$BIN" --status --json | grep -q '"fallback": "VAAPI probe failed"'
common "$BIN" --stop >/dev/null
wait "$pid"
[ -s "$TMP/out/probe-failed.mkv" ]
grep -q '^libx264$' "$TMP/vaapi.log"
if grep -q 'vaapi' "$TMP/vaapi.log"; then echo 'software plan mentions VAAPI' >&2; exit 1; fi

# C1: no render node means plain software without a fallback note, and
# --encoder software never probes.
NIRUCLIP_FFMPEG="$TMP/ffmpeg-ok" common "$BIN" --full --audio none --countdown 0 --output "$TMP/out/no-node.mkv" >/dev/null 2>&1 &
pid=$!
wait_state recording
common "$BIN" --status --json | grep -q '"encoder": "software"'
if common "$BIN" --status --json | grep -q '"fallback"'; then echo 'no render node reported a fallback' >&2; exit 1; fi
common "$BIN" --stop >/dev/null
wait "$pid"
probes=$(wc -l < "$FAKE_PROBE_COUNT")
FAKE_RENDER_NODE="$TMP/renderD130" FAKE_VAAPI_PROBE=ok NIRUCLIP_FFMPEG="$TMP/ffmpeg-ok" common "$BIN" --full --audio none --encoder software --countdown 0 --output "$TMP/out/forced-sw.mkv" >/dev/null 2>&1 &
pid=$!
wait_state recording
common "$BIN" --stop >/dev/null
wait "$pid"
[ "$(wc -l < "$FAKE_PROBE_COUNT")" -eq "$probes" ]
grep -q '^libx264$' "$TMP/vaapi.log"

# C1: a VAAPI encoder that fails before the first frame is retried once in
# software; the recording succeeds and the fallback is recorded and cached.
FAKE_RENDER_NODE="$TMP/renderD130" FAKE_VAAPI_PROBE=ok FAKE_VAAPI_RUNTIME=fail NIRUCLIP_FFMPEG="$TMP/ffmpeg-ok" common "$BIN" --full --audio none --countdown 0 --output "$TMP/out/retry.mkv" >/dev/null 2>&1 &
pid=$!
wait_state recording
common "$BIN" --status --json | grep -q '"encoder": "software"'
common "$BIN" --status --json | grep -q '"fallback": "h264_vaapi on '
common "$BIN" --stop >/dev/null
wait "$pid"
[ -s "$TMP/out/retry.mkv" ]
[ ! -e "$TMP/out/retry.part.mkv" ]
grep -q '^libx264$' "$TMP/vaapi.log"
grep -q "\"$TMP/renderD130\":false" "$TMP/state/vaapi-probe.json"
unset FAKE_PROBE_COUNT FAKE_LOG

# Profiles are listable, saveable and apply the camera-only mode.
common "$BIN" --list-profiles | grep -q '^camera-mic'
common "$BIN" --save-profile test-profile --webcam-only --audio none --webcam-size small
common "$BIN" --list-profiles | grep -q '^test-profile'
common "$BIN" --profile test-profile --countdown 0 --status

echo 'NIRUCLIP lifecycle tests: PASS'
