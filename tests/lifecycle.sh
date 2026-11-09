#!/bin/sh
set -eu

BIN=${1:-./target/debug/niruclip}
TMP=$(mktemp -d "${TMPDIR:-/tmp}/niruclip-test.XXXXXX")
trap 'rm -rf "$TMP"' EXIT HUP INT TERM
mkdir -p "$TMP/state" "$TMP/config" "$TMP/out"

cat > "$TMP/ffmpeg-ok" <<'EOF'
#!/bin/sh
# The source listing behind --mic and --list-mics (ffmpeg -sources pulse).
if printf '%s\n' "$@" | grep -qx -- -sources; then
  [ -z "${FAKE_SOURCES_LOG:-}" ] || printf '%s\n' "$*" >> "$FAKE_SOURCES_LOG"
  [ "${FAKE_SOURCES:-ok}" = ok ] || exit 1
  printf '%s\n' 'Auto-detected sources for pulse:' \
    '* fake_output.analog-stereo.monitor [Monitor of Fake Speakers] (none)' \
    '  alsa_input.fake-builtin.analog-stereo [Built-in Audio Analog Stereo] (none)' \
    '  alsa_input.usb-Fake_Webcam-02.mono-fallback [Fake Webcam Mono] (none)'
  exit 0
fi
last=; progress=; prev=; vaapi=
for arg in "$@"; do
  [ "$prev" = "-progress" ] && progress=$arg
  [ "$arg" = "h264_vaapi" ] && vaapi=1
  prev=$arg
  # The live camera check is a second output after the recording.
  if [ "$arg" = "[probe]" ]; then probeout=1; last=$mainout; fi
  [ -n "${probeout:-}" ] || { mainout=$last; last=$arg; }
done
# Lossless remux of an unfinished file: "damaged" packets become "partial".
if printf '%s\n' "$@" | grep -qx copy && ! printf '%s\n' "$@" | grep -qx concat; then
  input=; prev=
  for arg in "$@"; do [ "$prev" = "-i" ] && input=$arg; prev=$arg; done
  [ ! -e "$last" ] || exit 1
  grep -q damaged "$input" || exit 1
  sed 's/damaged/partial/g' "$input" > "$last"
  exit 0
fi
# Lossless concat of pause/resume segments: copy every listed file.
if printf '%s\n' "$@" | grep -qx concat; then
  [ "${FAKE_CONCAT:-ok}" = ok ] || exit 1
  list=; prev=
  for arg in "$@"; do [ "$prev" = "-i" ] && list=$arg; prev=$arg; done
  [ ! -e "$last" ] || exit 1
  : > "$last"
  sed -n "s/^file '\(.*\)'$/\1/p" "$list" | while IFS= read -r part; do cat "$part" >> "$last"; done
  exit 0
fi
# A resume that cannot start a new segment.
if [ -n "${FAKE_FAIL_FLAG:-}" ] && [ -e "$FAKE_FAIL_FLAG" ]; then exit 1; fi
# The camera pre-check: 128x96 grey frames on stdout at 5 fps, a live
# (changing) picture unless FAKE_PRECHECK_FRAME names a PGM to repeat.
if printf '%s\n' "$@" | grep -qx rawvideo; then
  [ -z "${FAKE_PRECHECK_LOG:-}" ] || printf '%s\n' "$*" >> "$FAKE_PRECHECK_LOG"
  [ "${FAKE_PRECHECK:-live}" != none ] || exit 1
  i=0
  while [ "$i" -lt 150 ]; do
    if [ -n "${FAKE_PRECHECK_FRAME:-}" ]; then tail -c 12288 "$FAKE_PRECHECK_FRAME"; else head -c 12288 /dev/urandom; fi || exit 0
    sleep 0.2
    i=$((i + 1))
  done
  exit 0
fi
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
# New frames keep arriving (FAKE_STALL_FLAG freezes the counter).
if [ -n "$progress" ]; then
  (n=1; parent=$$; while kill -0 "$parent" 2>/dev/null; do sleep 0.5; [ -n "${FAKE_STALL_FLAG:-}" ] && [ -e "$FAKE_STALL_FLAG" ] && continue; n=$((n + 15)); printf 'frame=%s\nfps=30.0\ndrop_frames=0\ndup_frames=0\nspeed=1.0x\nout_time_us=33333\nprogress=continue\n' "$n" > "$progress.tmp" && mv "$progress.tmp" "$progress"; done) &
  ticker=$!
fi
# The live camera check: rewrite the probe frame three times a second, a
# changing picture unless FAKE_PROBE_FRAME names a PGM to repeat.
probe=; prev=
for arg in "$@"; do [ "$prev" = pgm ] && probe=$arg; prev=$arg; done
if [ -n "$probe" ]; then
  (parent=$$; while kill -0 "$parent" 2>/dev/null; do
    if [ -n "${FAKE_STALL_FLAG:-}" ] && [ -e "$FAKE_STALL_FLAG" ]; then sleep 0.3; continue; fi
    if [ -n "${FAKE_PROBE_FRAME:-}" ]; then cp "$FAKE_PROBE_FRAME" "$probe.tmp"
    elif [ -n "${FAKE_PLACEHOLDER_ONCE:-}" ] && [ -e "$FAKE_PLACEHOLDER_ONCE" ]; then cp "$FAKE_PLACEHOLDER_ONCE" "$probe.tmp"; else { printf 'P5\n128 96\n255\n'; head -c 12288 /dev/urandom; } > "$probe.tmp"; fi
    mv "$probe.tmp" "$probe"; sleep 0.3
  done) &
  writer=$!
fi
trap '[ -z "${writer:-}" ] || kill "$writer" 2>/dev/null; [ -z "${ticker:-}" ] || kill "$ticker" 2>/dev/null; [ -z "${FAKE_PLACEHOLDER_ONCE:-}" ] || rm -f "$FAKE_PLACEHOLDER_ONCE"; exit 0' INT TERM
# A camera stream that ends by itself (end of file on the only input).
if [ -n "${FAKE_EXIT_AFTER:-}" ]; then
  sleep "$FAKE_EXIT_AFTER"
  [ -z "${writer:-}" ] || kill "$writer" 2>/dev/null
  [ -z "${ticker:-}" ] || kill "$ticker" 2>/dev/null
  exit 0
fi
sleep 30 &
wait $!
EOF
cat > "$TMP/ffmpeg-fail" <<'EOF'
#!/bin/sh
# The camera pre-check gets no frames from this fake.
if printf '%s\n' "$@" | grep -qx rawvideo; then exit 1; fi
last=
for arg in "$@"; do
  if [ "$arg" = "[probe]" ]; then probeout=1; last=$mainout; fi
  [ -n "${probeout:-}" ] || { mainout=$last; last=$arg; }
done
: > "$last"
printf 'bad' >> "$last"
exit 1
EOF
cat > "$TMP/ffmpeg-interrupt" <<'EOF'
#!/bin/sh
# The camera pre-check gets no frames from this fake.
if printf '%s\n' "$@" | grep -qx rawvideo; then exit 1; fi
last=; progress=; prev=
for arg in "$@"; do
  [ "$prev" = "-progress" ] && progress=$arg
  prev=$arg
  # The live camera check is a second output after the recording.
  if [ "$arg" = "[probe]" ]; then probeout=1; last=$mainout; fi
  [ -n "${probeout:-}" ] || { mainout=$last; last=$arg; }
done
: > "$last"
printf 'partial' >> "$last"
[ -z "$progress" ] || printf 'frame=1\nfps=30.0\ndrop_frames=0\ndup_frames=0\nspeed=1.0x\nout_time_us=33333\nprogress=continue\n' > "$progress"
sleep 0.2
exit 1
EOF
cat > "$TMP/ffmpeg-silent" <<'EOF'
#!/bin/sh
# The camera pre-check gets no frames from this fake.
if printf '%s\n' "$@" | grep -qx rawvideo; then exit 1; fi
last=
for arg in "$@"; do
  if [ "$arg" = "[probe]" ]; then probeout=1; last=$mainout; fi
  [ -n "${probeout:-}" ] || { mainout=$last; last=$arg; }
done
: > "$last"
trap 'exit 1' INT TERM
sleep 30 &
wait $!
EOF
cat > "$TMP/ffprobe" <<'EOF'
#!/bin/sh
last=
for arg in "$@"; do
  if [ "$arg" = "[probe]" ]; then probeout=1; last=$mainout; fi
  [ -n "${probeout:-}" ] || { mainout=$last; last=$arg; }
done
# The first-packet probe of an unfinished file.
if printf '%s\n' "$@" | grep -qx 'packet=pts_time'; then
  grep -q -e damaged -e partial "$last" 2>/dev/null && echo 0.000000
  exit 0
fi
n=$(grep -o partial "$last" 2>/dev/null | wc -l)
[ "$n" -gt 0 ] || exit 1
# Every recorded segment contributes 1.5 s, so joins are checked for length.
printf '{"streams":[{"codec_name":"h264"}],"format":{"duration":"%s"}}\n' "$(awk -v n="$n" 'BEGIN{print n*1.5}')"
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
  NIRUCLIP_CAMERA="${FAKE_CAMERA:-/dev/zero}" \
  NIRUCLIP_PROBE_DIR="$TMP/state" \
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
# Camera overlay: 34 % of the output height at 16:9, filled and cropped,
# rounded with a border, 28 px from the top-left corner.
grep -q 'scale=654:368:force_original_aspect_ratio=increase:flags=bicubic,crop=654:368' "$TMP/args.log"
grep -q 'alphamerge' "$TMP/args.log"
grep -q 'overlay=28:28:format=auto' "$TMP/args.log"
# One capture clock: absolute camera timestamps, -copyts, a global zero shift.
grep -A1 -x -- '-ts' "$TMP/args.log" | grep -qx abs
grep -qx -- '-copyts' "$TMP/args.log"
grep -A1 -x -- '-avoid_negative_ts' "$TMP/args.log" | grep -qx make_zero
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

# U1: pause and resume record segments that are joined losslessly on stop.
NIRUCLIP_FFMPEG="$TMP/ffmpeg-ok" common "$BIN" --full --audio none --countdown 0 --output "$TMP/out/pause.mkv" >/dev/null 2>&1 &
pid=$!
wait_state recording pause.mkv
common "$BIN" --resume | grep -qx 'NIRUCLIP already recording'
common "$BIN" --pause | grep -qx 'NIRUCLIP paused'
common "$BIN" --status --json | grep -q '"state": "paused"'
common "$BIN" --status --json | grep -q '"elapsed_before": 1.5'
common "$BIN" --pause | grep -qx 'NIRUCLIP already paused'
[ -s "$TMP/out/pause.part.mkv" ]
common "$BIN" --resume | grep -qx 'NIRUCLIP resumed'
common "$BIN" --status --json | grep -q '"state": "recording"'
common "$BIN" --status --json | grep -q 'pause.seg2.part.mkv'
common "$BIN" --pause >/dev/null
common "$BIN" --resume >/dev/null
[ -s "$TMP/out/pause.seg3.part.mkv" ]
common "$BIN" --stop >/dev/null
wait "$pid"
common "$BIN" --status --json | grep -q '"state": "finished"'
[ "$(grep -o partial "$TMP/out/pause.mkv" | wc -l)" -eq 3 ] || { echo 'segments were not joined' >&2; exit 1; }
if ls "$TMP/out" | grep -q '^pause\..*part'; then echo 'segment files left behind' >&2; exit 1; fi
if common "$BIN" --status --json | grep -q '"segments"'; then echo 'segments left in a finished session' >&2; exit 1; fi

# U1: a stop while paused saves what was recorded.
NIRUCLIP_FFMPEG="$TMP/ffmpeg-ok" common "$BIN" --full --audio none --countdown 0 --output "$TMP/out/pause-stop.mkv" >/dev/null 2>&1 &
pid=$!
wait_state recording pause-stop
common "$BIN" --pause >/dev/null
common "$BIN" --stop >/dev/null
wait "$pid"
common "$BIN" --status --json | grep -q '"state": "finished"'
[ -s "$TMP/out/pause-stop.mkv" ]

# U1: SIGKILL with several segments, then --recover joins them.
NIRUCLIP_FFMPEG="$TMP/ffmpeg-ok" common "$BIN" --full --audio none --countdown 0 --output "$TMP/out/pause-kill.mkv" >/dev/null 2>&1 &
pid=$!
wait_state recording pause-kill
common "$BIN" --pause >/dev/null
common "$BIN" --resume >/dev/null
kill_recorder "$pid"
NIRUCLIP_FFMPEG="$TMP/ffmpeg-ok" common "$BIN" --recover >/dev/null
common "$BIN" --status --json | grep -q '"reason": "recovered from 2 segments"'
[ "$(grep -o partial "$TMP/out/pause-kill.mkv" | wc -l)" -eq 2 ]

# U1: if the join fails every segment is saved on its own; nothing is lost.
FAKE_CONCAT=fail NIRUCLIP_FFMPEG="$TMP/ffmpeg-ok" common "$BIN" --full --audio none --countdown 0 --output "$TMP/out/nojoin.mkv" >/dev/null 2>&1 &
pid=$!
wait_state recording nojoin
FAKE_CONCAT=fail common "$BIN" --pause >/dev/null
FAKE_CONCAT=fail common "$BIN" --resume >/dev/null
common "$BIN" --stop >/dev/null
wait "$pid"
common "$BIN" --status --json | grep -q 'separate parts'
[ -s "$TMP/out/nojoin.mkv" ] && [ -s "$TMP/out/nojoin-2.mkv" ]

# U1: a resume that cannot start ffmpeg keeps the recorded part.
export FAKE_FAIL_FLAG="$TMP/fail-next"
NIRUCLIP_FFMPEG="$TMP/ffmpeg-ok" common "$BIN" --full --audio none --countdown 0 --output "$TMP/out/badresume.mkv" >/dev/null 2>&1 &
pid=$!
wait_state recording badresume
common "$BIN" --pause >/dev/null
touch "$FAKE_FAIL_FLAG"
if common "$BIN" --resume >/dev/null 2>&1; then echo 'failed resume reported success' >&2; exit 1; fi
wait "$pid" || true
rm -f "$FAKE_FAIL_FLAG"
unset FAKE_FAIL_FLAG
common "$BIN" --status --json | grep -q '"state": "finished"'
common "$BIN" --status --json | grep -q 'could not resume'
[ -s "$TMP/out/badresume.mkv" ]
if ls "$TMP/out" | grep -q '^badresume\..*part'; then echo 'failed resume left a partial' >&2; exit 1; fi

# U2: free space is checked before a start and periodically while recording.
export NIRUCLIP_TEST_FREE_BYTES_FILE="$TMP/free"
export NIRUCLIP_TEST_DISK_INTERVAL_MS=200
printf 400000000 > "$NIRUCLIP_TEST_FREE_BYTES_FILE"; : > "$FAKE_NOTIFY_LOG"
if FAKE_NOTIFY=1 NIRUCLIP_NOTIFY_SEND="$TMP/notify-send" NIRUCLIP_FFMPEG="$TMP/ffmpeg-ok" common "$BIN" --full --audio none --countdown 0 --output "$TMP/out/nospace.mkv" >/dev/null 2>&1; then
  echo 'recording started with less than 500 MB free' >&2; exit 1
fi
[ ! -e "$TMP/out/nospace.mkv" ] && [ ! -e "$TMP/out/nospace.part.mkv" ]
grep -q '^Not recording: only 400 MB free' "$FAKE_NOTIFY_LOG"

printf 1500000000 > "$NIRUCLIP_TEST_FREE_BYTES_FILE"; : > "$FAKE_NOTIFY_LOG"
FAKE_NOTIFY=1 NIRUCLIP_NOTIFY_SEND="$TMP/notify-send" NIRUCLIP_FFMPEG="$TMP/ffmpeg-ok" common "$BIN" --full --audio none --countdown 0 --output "$TMP/out/lowspace.mkv" >/dev/null 2>&1 &
pid=$!
wait_state recording lowspace
grep -q '^Warning: low disk space: 1.5 GB free' "$FAKE_NOTIFY_LOG"
printf 300000000 > "$NIRUCLIP_TEST_FREE_BYTES_FILE"
wait "$pid"
sleep 0.3
common "$BIN" --status --json | grep -q '"state": "finished"'
common "$BIN" --status --json | grep -q '"reason": "stopped: only 300 MB free'
[ -s "$TMP/out/lowspace.mkv" ] && [ ! -e "$TMP/out/lowspace.part.mkv" ]
grep -q '^Recording stopped: disk almost full (300 MB free)' "$FAKE_NOTIFY_LOG"

printf 9000000000 > "$NIRUCLIP_TEST_FREE_BYTES_FILE"; : > "$FAKE_NOTIFY_LOG"
FAKE_NOTIFY=1 NIRUCLIP_NOTIFY_SEND="$TMP/notify-send" NIRUCLIP_FFMPEG="$TMP/ffmpeg-ok" common "$BIN" --full --audio none --countdown 0 --output "$TMP/out/shrinking.mkv" >/dev/null 2>&1 &
pid=$!
wait_state recording shrinking
printf 1900000000 > "$NIRUCLIP_TEST_FREE_BYTES_FILE"
sleep 1
common "$BIN" --status --json | grep -q '"state": "recording"'
common "$BIN" --stop >/dev/null
wait "$pid"
sleep 0.3
[ "$(grep -c '^Low disk space: 1.9 GB free' "$FAKE_NOTIFY_LOG")" -eq 1 ]
unset NIRUCLIP_TEST_FREE_BYTES_FILE NIRUCLIP_TEST_DISK_INTERVAL_MS

# U3: --status shows mode, elapsed time, size, encoder, fps, a drop warning
# and the output path; --status --json carries the same derived fields.
NIRUCLIP_FFMPEG="$TMP/ffmpeg-ok" common "$BIN" --full --webcam corner --audio mic --countdown 0 --output "$TMP/out/status.mkv" >/dev/null 2>&1 &
pid=$!
wait_state recording status.mkv
human=$(common "$BIN" --status)
for line in '^state: recording$' '^mode: full, camera bottom-right, audio mic, preset share$' '^elapsed: 0:00:00$' '^size: ' '^encoder: software$' '^fps: 30.0$' "^output: $TMP/out/status.mkv\$"; do
  printf '%s\n' "$human" | grep -q "$line" || { echo "status lacks $line" >&2; printf '%s\n' "$human" >&2; exit 1; }
done
if printf '%s\n' "$human" | grep -q '^warning:'; then echo 'healthy capture shows a drop warning' >&2; exit 1; fi
json=$(common "$BIN" --status --json)
for key in '"elapsed_seconds": 0.0' '"size_bytes": 7' '"drop_warning": false' '"encoder": "software"'; do
  printf '%s\n' "$json" | grep -qF "$key" || { echo "status --json lacks $key" >&2; exit 1; }
done
common "$BIN" --pause >/dev/null
common "$BIN" --status | grep -q '^elapsed: 0:00:01$'
common "$BIN" --status --json | grep -qF '"elapsed_seconds": 1.5'
common "$BIN" --stop >/dev/null
wait "$pid"

# An unfinished file (ffmpeg killed too: no duration, no cues) is offered for
# recovery and repaired losslessly by --recover.
NIRUCLIP_FFMPEG="$TMP/ffmpeg-ok" common "$BIN" --full --audio none --countdown 0 --output "$TMP/out/powerloss.mkv" >/dev/null 2>&1 &
pid=$!
wait_state recording powerloss
kill_recorder "$pid"
printf damaged > "$TMP/out/powerloss.part.mkv"
common "$BIN" --stop >/dev/null 2>&1 || true
common "$BIN" --status --json | grep -q '"recoverable": true'
NIRUCLIP_FFMPEG="$TMP/ffmpeg-ok" common "$BIN" --recover >/dev/null 2>&1
common "$BIN" --status --json | grep -q '"state": "finished"'
grep -q partial "$TMP/out/powerloss.mkv"
[ ! -e "$TMP/out/powerloss.part.mkv" ] && [ ! -e "$TMP/out/powerloss.part.mkv.repair.mkv" ]

# Camera mode selection: MJPEG 1080p for camera-only, 720p for an overlay,
# instead of whatever the driver last used (YUYV 640x480 on most webcams).
export NIRUCLIP_CAMERA_MODES='YUYV 640x480@30,MJPG 640x480@30,MJPG 1280x720@30,MJPG 1920x1080@30,MJPG 2880x1800@30'
export FAKE_LOG="$TMP/mode.log"
NIRUCLIP_FFMPEG="$TMP/ffmpeg-ok" common "$BIN" --webcam-only --audio none --countdown 0 --output "$TMP/out/mode1.mkv" >/dev/null 2>&1 &
pid=$!
wait_state recording mode1
grep -A1 -x -- '-input_format' "$TMP/mode.log" | grep -qx mjpeg
grep -A1 -x -- '-video_size' "$TMP/mode.log" | grep -qx 1920x1080
common "$BIN" --status --json | grep -q '"mode": "MJPG 1920x1080@30"'
common "$BIN" --status --json | grep -q '"height": 1080'
common "$BIN" --stop >/dev/null
wait "$pid"
NIRUCLIP_FFMPEG="$TMP/ffmpeg-ok" common "$BIN" --full --webcam corner --audio none --countdown 0 --camera-shape circle --camera-border none --mirror --output "$TMP/out/mode2.mkv" >/dev/null 2>&1 &
pid=$!
wait_state recording mode2
grep -A1 -x -- '-video_size' "$TMP/mode.log" | grep -x 1280x720 >/dev/null
grep -q 'hflip,scale=280:280:force_original_aspect_ratio=increase' "$TMP/mode.log"
common "$BIN" --stop >/dev/null
wait "$pid"
unset NIRUCLIP_CAMERA_MODES

# A camera that another program is streaming from: camera-only fails with a
# clear message; a screen recording continues without the camera and says so.
if NIRUCLIP_TEST_CAMERA_BUSY=1 NIRUCLIP_FFMPEG="$TMP/ffmpeg-ok" common "$BIN" --webcam-only --audio none --countdown 0 --output "$TMP/out/busy.mkv" 2>"$TMP/busy.err"; then
  echo 'busy camera did not fail' >&2; exit 1
fi
grep -q 'in use' "$TMP/busy.err"
export FAKE_LOG="$TMP/busy.log"
NIRUCLIP_TEST_CAMERA_BUSY=1 NIRUCLIP_FFMPEG="$TMP/ffmpeg-ok" common "$BIN" --full --webcam corner --audio none --countdown 0 --output "$TMP/out/busy2.mkv" >/dev/null 2>&1 &
pid=$!
wait_state recording busy2
common "$BIN" --status --json | grep -q '"status": "busy"'
common "$BIN" --status | grep -q 'recording without the camera'
if grep -q v4l2 "$TMP/busy.log"; then echo 'busy camera was still opened' >&2; exit 1; fi
common "$BIN" --stop >/dev/null
wait "$pid"
[ -s "$TMP/out/busy2.mkv" ]
if NIRUCLIP_TEST_CAMERA_BUSY=1 common "$BIN" --check-camera >/dev/null 2>&1; then
  echo '--check-camera accepted a busy camera' >&2; exit 1
fi

# Live picture check: the firmware privacy image (a still symbol on black)
# is reported in the state and --status; a live picture is "ok".
mkpgm() { # file fill bright
  { printf 'P5\n128 96\n255\n'; head -c 11500 /dev/zero | tr '\0' "$2"; head -c 300 /dev/zero | tr '\0' "$3"; head -c 488 /dev/zero | tr '\0' "$2"; } > "$1"
}
mkpgm "$TMP/placeholder.pgm" '\002' '\372'
NIRUCLIP_TEST_PRIVACY=0 FAKE_PROBE_FRAME="$TMP/placeholder.pgm" NIRUCLIP_FFMPEG="$TMP/ffmpeg-ok" common "$BIN" --webcam-only --audio none --countdown 0 --output "$TMP/out/privacy.mkv" >/dev/null 2>&1 &
pid=$!
wait_state recording privacy
i=0
until common "$BIN" --status --json | grep -q '"status": "privacy"'; do
  i=$((i + 1)); [ "$i" -lt 60 ] || { echo 'privacy image was not detected' >&2; exit 1; }
  sleep 0.1
done
common "$BIN" --status | grep -q 'privacy image'
common "$BIN" --stop >/dev/null
wait "$pid"
# The UVC privacy control is reported too.
NIRUCLIP_TEST_PRIVACY=1 NIRUCLIP_FFMPEG="$TMP/ffmpeg-ok" common "$BIN" --webcam-only --audio none --countdown 0 --output "$TMP/out/privacy2.mkv" >/dev/null 2>&1 &
pid=$!
wait_state recording privacy2
i=0
until common "$BIN" --status --json | grep -q 'privacy switch is on'; do
  i=$((i + 1)); [ "$i" -lt 30 ] || { echo 'privacy control was not reported' >&2; exit 1; }
  sleep 0.1
done
common "$BIN" --stop >/dev/null
wait "$pid"

# 0.5.0 camera recovery. NIRUCLIP_TEST_CAMERA_FORMAT lets a plain file stand
# in for the camera node, so "unplugging" is removing it (no root needed).
wait_json() {
  i=0
  until common "$BIN" --status --json | grep -q -- "$1"; do
    i=$((i + 1))
    [ "$i" -lt 150 ] || { common "$BIN" --status --json >&2; echo "timed out waiting for $1" >&2; exit 1; }
    sleep 0.1
  done
}
CAM="$TMP/cam0"
export FAKE_LOG="$TMP/args.log"
recover_run() { # output args...
  out=$1; shift
  FAKE_CAMERA="$CAM" NIRUCLIP_TEST_CAMERA_FORMAT=nut FAKE_NOTIFY=1 NIRUCLIP_NOTIFY_SEND="$TMP/notify-send" \
    NIRUCLIP_FFMPEG="$TMP/ffmpeg-ok" common "$BIN" "$@" --audio none --countdown 0 --output "$TMP/out/$out.mkv" >/dev/null 2>&1
}
parts() { grep -o partial "$1" | wc -l | tr -d ' '; }

# Overlay: the camera is unplugged, the screen recording continues without
# it in a new segment, the camera comes back and is attached again; the
# three segments are joined into one file.
: > "$CAM"; : > "$FAKE_NOTIFY_LOG"
recover_run lost --full --webcam corner &
pid=$!
wait_state recording lost.mkv
rm "$CAM"
wait_json 'was disconnected'
wait_json 'lost.seg2.part.mkv'
wait_state recording lost.seg2
if grep -q use_wallclock "$TMP/args.log"; then echo 'screen-only segment still opened the camera' >&2; exit 1; fi
: > "$CAM"
wait_json 'is back; recording with the camera again'
wait_state recording lost.seg3
grep -q use_wallclock "$TMP/args.log" || { echo 'returned camera was not attached again' >&2; exit 1; }
common "$BIN" --stop >/dev/null
wait "$pid"
common "$BIN" --status --json | grep -q '"state": "finished"'
[ "$(parts "$TMP/out/lost.mkv")" -eq 3 ] || { echo 'reconnect segments were not joined' >&2; exit 1; }
if ls "$TMP/out" | grep -q '^lost\..*part'; then echo 'reconnect segments left behind' >&2; exit 1; fi
sleep 0.3
grep -q 'was disconnected; recording continues without the camera' "$FAKE_NOTIFY_LOG"
grep -qx 'Camera is back; the recording continues with it' "$FAKE_NOTIFY_LOG"

# Overlay: a camera that does not come back within camera_reconnect_seconds
# is given up on; the screen recording goes on.
printf 'camera_reconnect_seconds = 1.5\n' > "$TMP/config/config.toml"
: > "$CAM"; : > "$FAKE_NOTIFY_LOG"
recover_run gone --full --webcam corner &
pid=$!
wait_state recording gone.mkv
rm "$CAM"
wait_json 'did not come back within 2 s; recording continues without the camera'
: > "$CAM"
sleep 1.5
common "$BIN" --status --json | grep -q '"state": "recording"'
if common "$BIN" --status --json | grep -q 'gone.seg3'; then echo 'camera re-attached after the reconnect time' >&2; exit 1; fi
common "$BIN" --stop >/dev/null
wait "$pid"
[ "$(parts "$TMP/out/gone.mkv")" -eq 2 ]
sleep 0.3
grep -q 'did not come back; the recording continues without it' "$FAKE_NOTIFY_LOG"

# Camera-only: the recorder waits for the camera and continues when it
# returns ...
rm -f "$TMP/config/config.toml"
: > "$CAM"
recover_run only-back --webcam-only &
pid=$!
wait_state recording only-back.mkv
rm "$CAM"
wait_json 'waiting up to 10 s for it to come back'
common "$BIN" --status --json | grep -q '"state": "recording"'
: > "$CAM"
wait_state recording only-back.seg2
common "$BIN" --stop >/dev/null
wait "$pid"
[ "$(parts "$TMP/out/only-back.mkv")" -eq 2 ]
# ... and saves a valid file by itself when it does not.
printf 'camera_reconnect_seconds = 1.5\n' > "$TMP/config/config.toml"
: > "$CAM"; : > "$FAKE_NOTIFY_LOG"
recover_run only-gone --webcam-only &
pid=$!
wait_state recording only-gone.mkv
rm "$CAM"
wait "$pid"
common "$BIN" --status --json | grep -q '"state": "finished"'
common "$BIN" --status --json | grep -q 'it did not come back within 2 s'
[ -s "$TMP/out/only-gone.mkv" ] && [ ! -e "$TMP/out/only-gone.part.mkv" ]
sleep 0.3
grep -qx 'The camera did not come back; saving the recording…' "$FAKE_NOTIFY_LOG"
grep -q '^Saved .*only-gone.mkv' "$FAKE_NOTIFY_LOG"

# A camera that stops delivering frames (probe and progress freeze) counts as
# lost after camera_stall_seconds; it is picked up again once it delivers.
export FAKE_STALL_FLAG="$TMP/stall"
: > "$CAM"
recover_run stall --full --webcam corner &
pid=$!
wait_state recording stall.mkv
touch "$FAKE_STALL_FLAG"
wait_json 'delivered no frames for 2.0 s'
rm -f "$FAKE_STALL_FLAG"
wait_json 'is back; recording with the camera again'
common "$BIN" --stop >/dev/null
wait "$pid"
[ "$(parts "$TMP/out/stall.mkv")" -ge 2 ]
unset FAKE_STALL_FLAG

# A camera-only stream that keeps ending is retried, but not forever.
: > "$CAM"
FAKE_EXIT_AFTER=0.5 recover_run flaky --webcam-only &
pid=$!
wait "$pid"
common "$BIN" --status --json | grep -q 'ended its stream; the camera kept failing'
common "$BIN" --status --json | grep -q 'failed 4 times within a minute; not trying again'
[ -s "$TMP/out/flaky.mkv" ] && [ "$(parts "$TMP/out/flaky.mkv")" -eq 4 ]

# camera_reconnect_seconds = 0 turns loss recovery off (0.4.0 behaviour).
printf 'camera_reconnect_seconds = 0\n' > "$TMP/config/config.toml"
: > "$CAM"
recover_run off --full --webcam corner &
pid=$!
wait_state recording off.mkv
rm "$CAM"
sleep 2.5
if common "$BIN" --status --json | grep -q 'disconnected'; then echo 'recovery ran although it is off' >&2; exit 1; fi
common "$BIN" --stop >/dev/null
wait "$pid"
[ "$(parts "$TMP/out/off.mkv")" -eq 1 ]
rm -f "$TMP/config/config.toml"
unset FAKE_LOG

# The pre-check runs with every countdown, also 0 (it delays the start a
# little instead of being skipped).
export FAKE_PRECHECK_LOG="$TMP/precheck.log"
: > "$FAKE_PRECHECK_LOG"
NIRUCLIP_FFMPEG="$TMP/ffmpeg-ok" common "$BIN" --webcam-only --audio none --countdown 0 --output "$TMP/out/pre0.mkv" >/dev/null 2>&1 &
pid=$!
wait_state recording pre0.mkv
[ "$(wc -l < "$FAKE_PRECHECK_LOG")" -eq 1 ] || { echo 'no camera pre-check with countdown 0' >&2; exit 1; }
common "$BIN" --status --json | grep -q '"status": "ok"'
common "$BIN" --stop >/dev/null
wait "$pid"

# A placeholder in the pre-check: the camera is reopened in another mode
# each time (same size in another format, then another size), then the
# warning is given as before. USB autosuspend is switched off meanwhile
# when the sysfs file is writable, and restored afterwards.
mkdir -p "$TMP/sys/class/video4linux/zero" "$TMP/sys/devices/usb1/1-1/1-1:1.0" "$TMP/sys/devices/usb1/1-1/power"
: > "$TMP/sys/devices/usb1/1-1/idVendor"
printf 'auto\n' > "$TMP/sys/devices/usb1/1-1/power/control"
ln -s "../../../devices/usb1/1-1/1-1:1.0" "$TMP/sys/class/video4linux/zero/device"
: > "$FAKE_PRECHECK_LOG"
NIRUCLIP_SYSFS="$TMP/sys" NIRUCLIP_CAMERA_MODES='YUYV 1280x720@30,MJPG 1280x720@30,MJPG 640x360@30' \
  FAKE_PRECHECK_FRAME="$TMP/placeholder.pgm" NIRUCLIP_TEST_PRIVACY=0 NIRUCLIP_FFMPEG="$TMP/ffmpeg-ok" \
  common "$BIN" --full --webcam corner --audio none --countdown 1 --output "$TMP/out/reopen.mkv" >/dev/null 2>&1 &
pid=$!
wait_state recording reopen.mkv
[ "$(wc -l < "$FAKE_PRECHECK_LOG")" -eq 3 ] || { cat "$FAKE_PRECHECK_LOG" >&2; echo 'expected 3 pre-checks' >&2; exit 1; }
sed -n 1p "$FAKE_PRECHECK_LOG" | grep -q 'yuyv422 -video_size 1280x720'
sed -n 2p "$FAKE_PRECHECK_LOG" | grep -q 'mjpeg -video_size 1280x720'
sed -n 3p "$FAKE_PRECHECK_LOG" | grep -q 'mjpeg -video_size 640x360'
json=$(common "$BIN" --status --json)
printf '%s\n' "$json" | grep -q 'reopening /dev/zero in MJPG 1280x720@30 (attempt 1 of 2)'
printf '%s\n' "$json" | grep -q 'reopening /dev/zero in MJPG 640x360@30 (attempt 2 of 2)'
printf '%s\n' "$json" | grep -q 'USB autosuspend off for the rest of this recording'
printf '%s\n' "$json" | grep -q '"status": "privacy"'
[ "$(cat "$TMP/sys/devices/usb1/1-1/power/control")" = on ]
common "$BIN" --stop >/dev/null
wait "$pid"
[ "$(cat "$TMP/sys/devices/usb1/1-1/power/control")" = auto ] || { echo 'autosuspend was not restored' >&2; exit 1; }
# A root-only sysfs file is left alone and the log says why.
printf 'auto\n' > "$TMP/sys/devices/usb1/1-1/power/control"
chmod 444 "$TMP/sys/devices/usb1/1-1/power/control"
if [ "$(id -u)" != 0 ]; then
  NIRUCLIP_SYSFS="$TMP/sys" FAKE_PRECHECK_FRAME="$TMP/placeholder.pgm" NIRUCLIP_TEST_PRIVACY=0 NIRUCLIP_FFMPEG="$TMP/ffmpeg-ok" \
    common "$BIN" --webcam-only --audio none --countdown 0 --output "$TMP/out/reopen-ro.mkv" >/dev/null 2>&1 &
  pid=$!
  wait_state recording reopen-ro.mkv
  common "$BIN" --status --json | grep -q 'USB autosuspend left as is (changing it needs root'
  common "$BIN" --stop >/dev/null
  wait "$pid"
  [ "$(cat "$TMP/sys/devices/usb1/1-1/power/control")" = auto ]
fi
chmod 644 "$TMP/sys/devices/usb1/1-1/power/control"
unset FAKE_PRECHECK_LOG

# A placeholder during the recording that a reopen cures: no warning.
cp "$TMP/placeholder.pgm" "$TMP/placeholder-once.pgm"; : > "$FAKE_NOTIFY_LOG"
FAKE_PLACEHOLDER_ONCE="$TMP/placeholder-once.pgm" NIRUCLIP_TEST_PRIVACY=0 FAKE_NOTIFY=1 NIRUCLIP_NOTIFY_SEND="$TMP/notify-send" \
  NIRUCLIP_FFMPEG="$TMP/ffmpeg-ok" common "$BIN" --full --webcam corner --audio none --countdown 0 --output "$TMP/out/cured.mkv" >/dev/null 2>&1 &
pid=$!
wait_state recording cured.mkv
wait_json 'privacy placeholder picture; reopening /dev/zero'
wait_state recording cured.seg2
wait_json '"status": "ok"'
common "$BIN" --stop >/dev/null
wait "$pid"
sleep 0.3
if grep -q 'privacy image' "$FAKE_NOTIFY_LOG"; then echo 'a cured placeholder was still reported' >&2; exit 1; fi
[ "$(parts "$TMP/out/cured.mkv")" -eq 2 ]

# Microphone choice. --list-mics hides monitors and notes a monitor default.
NIRUCLIP_FFMPEG="$TMP/ffmpeg-ok" common "$BIN" --list-mics > "$TMP/mics.txt"
grep -q '^  alsa_input.usb-Fake_Webcam-02.mono-fallback  *Fake Webcam Mono$' "$TMP/mics.txt"
grep -q '^  alsa_input.fake-builtin.analog-stereo  *Built-in Audio Analog Stereo$' "$TMP/mics.txt"
if grep -v '^note:' "$TMP/mics.txt" | grep -q monitor; then echo 'a monitor was listed as a microphone' >&2; exit 1; fi
grep -q '^note: the system default source is fake_output.analog-stereo.monitor' "$TMP/mics.txt"
NIRUCLIP_FFMPEG="$TMP/ffmpeg-ok" common "$BIN" --list-mics --json | grep -q '"description": "Fake Webcam Mono"'
if FAKE_SOURCES=fail NIRUCLIP_FFMPEG="$TMP/ffmpeg-ok" common "$BIN" --list-mics >/dev/null 2>&1; then echo 'an unlistable server was not reported' >&2; exit 1; fi
# --mic by description substring opens that source, and session.json says so.
export FAKE_LOG="$TMP/mic.log"; NIRUCLIP_FFMPEG="$TMP/ffmpeg-ok" common "$BIN" --full --audio mic --mic WEBCAM --countdown 0 --output "$TMP/out/mic.mkv" >/dev/null 2>&1 &
pid=$!
wait_state recording mic.mkv
common "$BIN" --status --json | grep -q '"source": "alsa_input.usb-Fake_Webcam-02.mono-fallback"'
common "$BIN" --status | grep -qx 'mic: Fake Webcam Mono (alsa_input.usb-Fake_Webcam-02.mono-fallback)'
common "$BIN" --stop >/dev/null
wait "$pid"
grep -A2 -x -- pulse "$TMP/mic.log" | grep -qx 'alsa_input.usb-Fake_Webcam-02.mono-fallback'
if grep -q '@DEFAULT_SOURCE@' "$TMP/mic.log"; then echo '--mic still opened the default source' >&2; exit 1; fi
# A missing microphone falls back to the system default with a notification.
: > "$FAKE_NOTIFY_LOG"
export FAKE_LOG="$TMP/mic-missing.log"; FAKE_NOTIFY=1 NIRUCLIP_NOTIFY_SEND="$TMP/notify-send" NIRUCLIP_FFMPEG="$TMP/ffmpeg-ok" common "$BIN" --full --audio both --mic scarlett --countdown 0 --output "$TMP/out/mic-missing.mkv" >/dev/null 2>&1 &
pid=$!
wait_state recording mic-missing.mkv
common "$BIN" --status --json | grep -q '"fallback": "microphone \\"scarlett\\" not found"'
common "$BIN" --status | grep -q '^warning: microphone "scarlett" not found; using the system default'
common "$BIN" --stop >/dev/null
wait "$pid"
grep -q '@DEFAULT_SOURCE@' "$TMP/mic-missing.log"
grep -q '@DEFAULT_MONITOR@' "$TMP/mic-missing.log"
grep -q 'not found; recording from the system default' "$FAKE_NOTIFY_LOG"
# Desktop-only and silent recordings never list sources.
export FAKE_SOURCES_LOG="$TMP/sources.log"; : > "$FAKE_SOURCES_LOG"
export FAKE_LOG="$TMP/mic-desktop.log"; NIRUCLIP_FFMPEG="$TMP/ffmpeg-ok" common "$BIN" --full --audio desktop --mic webcam --countdown 0 --output "$TMP/out/mic-desktop.mkv" >/dev/null 2>&1 &
pid=$!
wait_state recording mic-desktop.mkv
if common "$BIN" --status --json | grep -q '"mic"'; then echo 'desktop-only recording has a mic' >&2; exit 1; fi
common "$BIN" --stop >/dev/null
wait "$pid"
[ ! -s "$FAKE_SOURCES_LOG" ]
unset FAKE_SOURCES_LOG
# A saved profile remembers the microphone.
common "$BIN" --save-profile mic-profile --full --audio mic --mic webcam
grep -q 'mic = "webcam"' "$TMP/config/config.toml"

# Profiles are listable, saveable and apply the camera-only mode.
common "$BIN" --list-profiles | grep -q '^camera-mic'
common "$BIN" --save-profile test-profile --webcam-only --audio none --webcam-size small
common "$BIN" --list-profiles | grep -q '^test-profile'
common "$BIN" --profile test-profile --countdown 0 --status
# Camera device, shape, mirroring and border are remembered in a profile.
common "$BIN" --save-profile cam-profile --full --webcam corner --camera /dev/zero --camera-shape circle --mirror --camera-border 8aa87e
grep -q 'camera = "/dev/zero"' "$TMP/config/config.toml"
grep -q 'camera_shape = "circle"' "$TMP/config/config.toml"
grep -q 'camera_mirror = true' "$TMP/config/config.toml"
grep -q 'camera_border = "8aa87e"' "$TMP/config/config.toml"
if common "$BIN" --full --camera-shape hexagon --status >/dev/null 2>&1; then echo 'bad shape accepted' >&2; exit 1; fi

echo 'NIRUCLIP lifecycle tests: PASS'
