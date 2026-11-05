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
cat > "$TMP/v4l2-ctl" <<'EOF'
#!/bin/sh
# Fake camera control: the value lives in $FAKE_V4L2_STATE, sets are logged.
[ -n "${FAKE_V4L2_STATE:-}" ] || exit 1
case "$3" in
  --get-ctrl)
    [ -f "$FAKE_V4L2_STATE" ] || exit 1
    printf '%s: %s\n' "$4" "$(cat "$FAKE_V4L2_STATE")" ;;
  --set-ctrl)
    printf '%s' "${4#*=}" > "$FAKE_V4L2_STATE"
    printf '%s\n' "$4" >> "$FAKE_V4L2_STATE.log" ;;
  *) exit 1 ;;
esac
EOF
chmod +x "$TMP/v4l2-ctl"
chmod +x "$TMP"/ffmpeg-ok "$TMP"/ffmpeg-fail "$TMP"/ffmpeg-interrupt "$TMP"/ffmpeg-silent "$TMP"/ffprobe "$TMP"/xrandr "$TMP"/slop

common() {
  NIRUCLIP_STATE_DIR="$TMP/state" \
  NIRUCLIP_CONFIG="$TMP/config/config.toml" \
  NIRUCLIP_CAMERA=/dev/zero \
  NIRUCLIP_XRANDR="$TMP/xrandr" \
  NIRUCLIP_SLOP="$TMP/slop" \
  NIRUCLIP_FFPROBE="$TMP/ffprobe" \
  NIRUCLIP_V4L2CTL="$TMP/v4l2-ctl" \
  NIRUCLIP_NOTIFY="${FAKE_NOTIFY:-0}" \
  NIRUCLIP_VAAPI_DEVICE="${FAKE_RENDER_NODE:-$TMP/no-render-node}" \
  DISPLAY=:0 \
  "$@"
}

wait_state() {
  state=$1
  # An optional second argument must also appear (for example the output
  # name), so a stale state from a killed recorder is not mistaken for it.
  also=${2:-\"state\"}
  i=0
  while [ "$i" -lt 100 ]; do
    json=$(common "$BIN" --status --json)
    if printf '%s\n' "$json" | grep -q "\"state\": \"$state\"" && printf '%s\n' "$json" | grep -qF "$also"; then return 0; fi
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
grep -q 'amix=inputs=2:normalize=0,alimiter=limit=0.95:level=0\[a\]' "$TMP/args.log"
# C5: camera, screen and both pulse inputs get a 512-packet queue.
[ "$(grep -A1 -x -- '-thread_queue_size' "$TMP/args.log" | grep -c -x 512)" -eq 4 ] || {
  echo 'not every input has -thread_queue_size 512' >&2
  exit 1
}
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

# C4: the camera control is persisted before it is changed and restored by a
# normal stop, by --stop and --recover after SIGKILL, and by the next start.
export FAKE_V4L2_STATE="$TMP/camera.ctrl"
ctrl() { cat "$FAKE_V4L2_STATE"; }
kill_recorder() {
  json=$(common "$BIN" --status --json)
  recorder=$(printf '%s\n' "$json" | sed -n 's/^  "pid": \([0-9]*\).*/\1/p')
  child=$(printf '%s\n' "$json" | sed -n 's/.*"child_pid": \([0-9]*\).*/\1/p')
  kill -9 "$recorder"
  wait "$1" 2>/dev/null || true
  [ -z "$child" ] || kill -9 -- "-$child" 2>/dev/null || true
}
printf 1 > "$FAKE_V4L2_STATE"
NIRUCLIP_FFMPEG="$TMP/ffmpeg-ok" common "$BIN" --webcam-only --audio none --countdown 0 --output "$TMP/out/cam-normal.mkv" >/dev/null 2>&1 &
pid=$!
wait_state recording
[ "$(ctrl)" = 0 ]
common "$BIN" --status --json | grep -q '"previous": "1"'
common "$BIN" --stop >/dev/null
wait "$pid"
[ "$(ctrl)" = 1 ]
if common "$BIN" --status --json | grep -q camera_restore; then echo 'camera_restore left in a finished session' >&2; exit 1; fi

NIRUCLIP_FFMPEG="$TMP/ffmpeg-ok" common "$BIN" --webcam-only --audio none --countdown 0 --output "$TMP/out/cam-kill.mkv" >/dev/null 2>&1 &
pid=$!
wait_state recording
[ "$(ctrl)" = 0 ]
kill_recorder "$pid"
[ "$(ctrl)" = 0 ]
common "$BIN" --status --json | grep -q '"camera_restore"'
common "$BIN" --recover >/dev/null
[ "$(ctrl)" = 1 ] || { echo '--recover did not restore the camera control' >&2; exit 1; }
[ -s "$TMP/out/cam-kill.mkv" ]
if common "$BIN" --status --json | grep -q camera_restore; then echo 'camera_restore survived --recover' >&2; exit 1; fi

NIRUCLIP_FFMPEG="$TMP/ffmpeg-ok" common "$BIN" --webcam-only --audio none --countdown 0 --output "$TMP/out/cam-stop.mkv" >/dev/null 2>&1 &
pid=$!
wait_state recording
kill_recorder "$pid"
common "$BIN" --stop >/dev/null 2>&1 || true
[ "$(ctrl)" = 1 ] || { echo '--stop did not restore the camera control' >&2; exit 1; }
common "$BIN" --recover >/dev/null

NIRUCLIP_FFMPEG="$TMP/ffmpeg-ok" common "$BIN" --webcam-only --audio none --countdown 0 --output "$TMP/out/cam-stale.mkv" >/dev/null 2>&1 &
pid=$!
wait_state recording
kill_recorder "$pid"
: > "$FAKE_V4L2_STATE.log"
NIRUCLIP_FFMPEG="$TMP/ffmpeg-ok" common "$BIN" --full --audio none --countdown 0 --output "$TMP/out/after-stale.mkv" >/dev/null 2>&1 &
pid=$!
wait_state recording after-stale
common "$BIN" --stop >/dev/null
wait "$pid"
[ "$(ctrl)" = 1 ]
[ "$(cat "$FAKE_V4L2_STATE.log")" = "exposure_dynamic_framerate=1" ] || { echo 'next start did not restore the stale camera control' >&2; exit 1; }

# A camera already at 0 (or without the control) is left alone.
printf 0 > "$FAKE_V4L2_STATE"; : > "$FAKE_V4L2_STATE.log"
NIRUCLIP_FFMPEG="$TMP/ffmpeg-ok" common "$BIN" --webcam-only --audio none --countdown 0 --output "$TMP/out/cam-zero.mkv" >/dev/null 2>&1 &
pid=$!
wait_state recording
common "$BIN" --stop >/dev/null
wait "$pid"
[ ! -s "$FAKE_V4L2_STATE.log" ]
unset FAKE_V4L2_STATE

# C7: a stop announces "Saving…" before the file is finalized.
cat > "$TMP/notify-send" <<'EOF'
#!/bin/sh
for last in "$@"; do :; done
printf '%s\n' "$last" >> "$FAKE_NOTIFY_LOG"
EOF
chmod +x "$TMP/notify-send"
export FAKE_NOTIFY_LOG="$TMP/notify.log"
: > "$FAKE_NOTIFY_LOG"
FAKE_NOTIFY=1 NIRUCLIP_NOTIFY_SEND="$TMP/notify-send" NIRUCLIP_FFMPEG="$TMP/ffmpeg-ok" common "$BIN" --full --audio none --countdown 0 --output "$TMP/out/saving.mkv" >/dev/null 2>&1 &
pid=$!
wait_state recording saving
common "$BIN" --stop >/dev/null
wait "$pid"
sleep 0.3
grep -qx 'Saving…' "$FAKE_NOTIFY_LOG"
saving_line=$(grep -nx 'Saving…' "$FAKE_NOTIFY_LOG" | cut -d: -f1)
saved_line=$(grep -n '^Saved ' "$FAKE_NOTIFY_LOG" | cut -d: -f1)
[ "$saving_line" -lt "$saved_line" ]

# Profiles are listable, saveable and apply the camera-only mode.
common "$BIN" --list-profiles | grep -q '^camera-mic'
common "$BIN" --save-profile test-profile --webcam-only --audio none --webcam-size small
common "$BIN" --list-profiles | grep -q '^test-profile'
common "$BIN" --profile test-profile --countdown 0 --status

echo 'NIRUCLIP lifecycle tests: PASS'
