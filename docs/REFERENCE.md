# NIRUCLIP reference

Detailed behaviour of [NIRUCLIP](../README.md): capture geometry, encoding,
pause/resume, output and recovery, state file, audio, disk space and exit
codes.

## Capture geometry and camera

Camera size is a target share of the **final video width**: `small` 20 %,
`medium` 30 % (default), `large` 40 %. The camera is never upscaled beyond its
native width. An overlay is placed 16 pixels from the selected corner.

The camera (`/dev/video0`, or `NIRUCLIP_CAMERA`) is requested at 30 fps and its
`exposure_dynamic_framerate` control is set to 0 during capture, so a 30 fps
screen overlay does not show a 15 fps camera. When a camera overlay is used, the
camera input is opened before the screen input; the reverse order made FFmpeg
drop or duplicate most frames on the tested laptops even though either source
worked alone.

The previous `exposure_dynamic_framerate` value is stored in `session.json`
(`camera_restore`, with the device) before NIRUCLIP changes it, and is put back
when the recording ends. After a SIGKILL, a crash or a power loss, `--stop`,
`--recover` and the next recording start restore it from the stored session
once the recorder process is gone.

`--full` and an overlay use the primary connected RandR output at its real X11
origin; `--screen OUTPUT` chooses another connected output. `--region` asks for
a rectangle with `slop`. `--window` currently records a selected static
rectangle; it does not follow a window that moves or resizes. `--webcam-only`
does not need a usable X11 screen geometry.

The `share` (up to 1080p) and `small` (up to 720p) presets avoid upscaling a
smaller screen. `quality` keeps the screen dimensions, rounded down to even
values. Camera-only capture is capped at 1920 pixels wide (1280 with `small`)
and keeps the camera aspect.

## Encoding

`--encoder auto` (or `encoder = "auto"` in `config.toml`) uses VAAPI
(`h264_vaapi`) when a render node `/dev/dri/renderD*` exists **and** a one-frame
synthetic probe encode succeeds within 3 seconds. The probe result is cached per
boot, ffmpeg binary and render node in
`${XDG_STATE_HOME}/niruclip/vaapi-probe.json`. VAAPI uses constant QP mapped
from the preset (`share` 25, `small` 30, `quality` 22). `software` always uses
`libx264 -preset veryfast` with the preset CRF (23/28/20). `vaapi` skips the
probe but still needs a render node; `NIRUCLIP_VAAPI_DEVICE` selects a node
explicitly.

Software encoding is the guaranteed fallback: when the hardware encoder stops
before the first frame, NIRUCLIP retries once in software, marks that node as
failed for the rest of the boot, and records `encoder` and `fallback` in the
state and the start notification. `--status` shows the encoder in use.

## Pause and resume

`niruclip --pause` closes the current ffmpeg segment cleanly and the state
becomes `paused`; no ffmpeg runs and the recorder sleeps without polling.
`niruclip --resume` starts a new segment with the same capture settings and
encoder. On stop the segments are joined losslessly with the concat demuxer
(stream copy, no re-encode); the join is validated for length before the
segment files are removed. If a join fails, or segments ended up with different
encoders after a VAAPI fallback, every segment is saved as its own file instead
(`NAME.mkv`, `NAME-2.mkv`, ...), so nothing is lost. A resume that cannot start
keeps and saves what was already recorded. The segment list is stored in
`session.json` (`segments`), so `--recover` joins them after a crash. `--stop`
works while paused.

## Output and recovery

The default destination is `~/Videos/NIRUCLIP-YYYY-MM-DD_HH-MM-SS.mkv`.
`NIRUCLIP_OUTPUT` changes the directory and `--output` selects a file or an
existing/created directory. Existing output and partial files are never
overwritten; a numeric suffix is chosen instead. MKV is written as `.part.mkv`
and renamed only after it contains a usable video stream.

- A controlled stop finalizes a valid partial as a normal saved recording.
- An unexpected encoder failure with a valid partial is `interrupted`: the
  partial is kept and `niruclip --recover` validates and restores it.
- A failed startup with no usable media is `failed`, not recoverable.
- If ffmpeg itself was killed (a power loss), the partial has no duration or
  index; `--recover` remuxes it losslessly (stream copy) and replaces the
  unfinished partial only after the copy validates.
- A crash leaves stale state; the next status/stop check does not signal a
  reused PID.

A recording is only announced as started after FFmpeg has produced its first
frame, and a source that never delivers frames fails clearly instead of leaving
a false "recording" state.

## State and status

State is stored at `${XDG_STATE_HOME:-$HOME/.local/state}/niruclip/session.json`
(`NIRUCLIP_STATE_DIR` overrides the directory) and published atomically with
schema version 1. NIRUBAR and NIRU Launcher read the same file and show the
active or paused recording, an interrupted recovery, or a failure.

While recording, the state carries an optional `performance` object with
encoded frames, fps, dropped/duplicated frames and encoder speed.
`niruclip --status` shows the state, mode (with camera corner, audio and
preset), recorded time (excluding pauses), size so far, encoder and fallback,
fps and the performance counters, a drop warning, the output path, the number of
segments and the reason. It warns when the captured stream is losing more than
2 % frames or duplicating more than 5 %; NIRUBAR shows the same warning.

`--status --json` prints the stored session plus the derived
`elapsed_seconds`, `size_bytes` and `drop_warning` fields (all optional).

## Audio

`--audio both` mixes the microphone and desktop audio at unity gain
(`amix normalize=0`, so neither source is attenuated) and then applies a peak
limiter (`alimiter=limit=0.95:level=0`). The limiter only engages when the sum of
both sources would clip; it adds no automatic gain.

## Disk space

Before a recording starts, and every 30 seconds while recording (one `statvfs`
call in the recorder loop, no extra process), NIRUCLIP checks the free space in
the output directory. Below 2 GB it warns once with a notification. Below 500 MB
it does not start, or stops a running recording cleanly with a "disk almost
full" notification; the file is finalized and validated like a normal stop, and
the reason is kept in the state. `NIRUCLIP_TEST_FREE_BYTES_FILE` and
`NIRUCLIP_TEST_DISK_INTERVAL_MS` are test hooks for `tests/lifecycle.sh`.

## Exit codes

- `0`: recording saved, countdown cancelled by an explicit stop, or a
  profile/status operation succeeded.
- `1`: capture failed, a controlled stop left only an interrupted partial, or no
  active session was available to stop.
- Usage problems are reported on stderr with a non-zero status.

## Environment overrides

| Variable | Purpose |
|---|---|
| `NIRUCLIP_OUTPUT` | Output directory |
| `NIRUCLIP_CONFIG` | Configuration file instead of `~/.config/niruclip/config.toml` |
| `NIRUCLIP_STATE_DIR` | State directory |
| `NIRUCLIP_CAMERA` | Camera device (default `/dev/video0`) |
| `NIRUCLIP_VAAPI_DEVICE` | VAAPI render node |
| `NIRUCLIP_NOTIFY=0` | Disable notifications |
| `NIRUCLIP_FFMPEG`, `NIRUCLIP_FFPROBE`, `NIRUCLIP_SLOP`, `NIRUCLIP_XRANDR`, `NIRUCLIP_V4L2CTL`, `NIRUCLIP_NOTIFY_SEND` | Alternative helper programs (used by the tests) |
