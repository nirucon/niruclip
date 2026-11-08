# NIRUCLIP reference

Detailed behaviour of [NIRUCLIP](../README.md): capture geometry, encoding,
pause/resume, output and recovery, state file, audio, disk space and exit
codes.

## Capture geometry and camera

### Which camera and which mode

Without `--camera` (or `camera` in the configuration, or `NIRUCLIP_CAMERA`)
NIRUCLIP queries every `/dev/video*` node with V4L2 ioctls and takes the first
video-capture node that is not an infrared sensor. Metadata nodes are not
capture nodes, and IR sensors (greyscale only, or named "IR") are skipped, so
a laptop with `/dev/video0`-`3` gets its RGB camera.

The capture mode is chosen from what the camera offers, never left to the
driver's last setting (which on most webcams is YUYV 640x480):

- full frame rate first (25 fps or more, or the best the camera has);
- then the smallest size that covers the target: 1280x720 for an overlay,
  1920x1080 for camera-only (1280x720 with `--preset small`), preferring the
  target's aspect ratio; a smaller camera uses its largest mode;
- then uncompressed YUYV when it can do that size at full rate, else MJPEG.

The chosen mode is shown by `--check-camera`, `--status` and in the
session (`camera.mode`).

### Timing and sync

The camera input uses `-ts abs`, and ffmpeg runs with `-copyts`: the camera,
the screen (x11grab) and pulse audio all keep their capture timestamps on the
wall clock, so they line up by when they were captured. The output is shifted
to start at zero as a whole (`-avoid_negative_ts make_zero`). Before 0.4.0
each input was shifted to zero on its own, and because the inputs are opened
one after another the camera overlay ended up about 0.3 s out of sync with
the microphone.

The camera is requested at 30 fps and its `exposure_dynamic_framerate`
control is set to 0 during capture, so a 30 fps screen overlay does not show
a 15 fps camera. When a camera overlay is used, the camera input is opened
before the screen input.

### Overlay

The overlay height is a share of the output height: `small` 20 %, `medium`
26 % (default), `large` 34 %, at the camera's aspect ratio (a circle is a
centre crop), never wider than 45 % of the video, with a margin of 2.5 % of
the height. Shapes are `rounded` (default), `circle` and `rect`; the border
is a thin ring (about 1 % of the overlay height) in the NIRUX theme accent
(`camera_border = "theme"`), a fixed colour, or `none`. The shape mask and
the ring are generated once per recording, so a shaped overlay costs no more
than a plain one. `--mirror` flips the camera horizontally.

### Camera health

- **Before recording:** a camera that another program is streaming from is
  detected without opening a stream (re-applying the current format is
  refused with EBUSY), and the programs holding it are named. Camera-only
  recording then fails clearly; a screen recording continues without the
  camera and says so.
- **During the countdown** (2 s or more): a separate short ffmpeg reads tiny
  greyscale frames (128x96, 5 per second, in memory only) and judges the
  frames after the first second.
- **While recording:** ffmpeg also writes a 128x96 greyscale copy of every
  camera frame to one tmpfs file (atomically replaced); NIRUCLIP looks at it
  twice a second, and also reads the UVC privacy control when the camera has
  one.

A picture is reported as a **privacy image** when it is still (mean change
between frames half a second apart below 0.2 grey levels) and looks like a
symbol on black (90 % of the pixels at most 24, some above 140). It is
**frozen** when every frame is bit-identical. A dark room is never reported:
a real sensor, however dark, has noise, so its frames are never identical
and their mean change stays far above 0.2. **No frames** means the camera
stopped delivering for 6 seconds (unplugged, or taken over). Problems and
recovery are notified once each, stored as `camera.status`/`camera.message`
in the session (`ok`, `checking`, `privacy`, `frozen`, `no-frames`, `busy`,
`missing`) and shown by `--status`, NIRUBAR (`● rec 00:12 · cam off`) and
NIRU Launcher.

`--check-camera` lists the cameras, the chosen modes and the privacy
control, checks for another user and looks at the picture for 2.5 s; it exits
1 when the camera is not usable. `--preview` opens a live window (ffplay)
showing the camera exactly as the overlay will look; it closes with q or Esc
and after two minutes.

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

## Export and share

`--export mp4` writes `NAME.mp4` next to the last saved recording (or
`--input FILE`): a lossless remux with `+faststart` when there is no trim,
so it is instant and plays in browsers and chat apps. `--from` and `--to`
(seconds) trim, which re-encodes so the cut is frame-accurate. `--export gif`
makes a 12 fps GIF up to 720 pixels wide with an optimised palette, at most
the first 30 seconds unless `--to` says otherwise. The MKV is never changed
and existing files are never overwritten. `--share` exports the MP4 and opens
NIRU Share (`nirulauncher --share`) with it.

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
| `NIRUCLIP_CAMERA` | Camera device (default: the first non-infrared capture camera) |
| `NIRUCLIP_VAAPI_DEVICE` | VAAPI render node |
| `NIRUCLIP_NOTIFY=0` | Disable notifications |
| `NIRUCLIP_FFMPEG`, `NIRUCLIP_FFPROBE`, `NIRUCLIP_FFPLAY`, `NIRUCLIP_SLOP`, `NIRUCLIP_XRANDR`, `NIRUCLIP_V4L2CTL`, `NIRUCLIP_NOTIFY_SEND`, `NIRUCLIP_LAUNCHER` | Alternative helper programs (used by the tests) |
| `NIRUCLIP_CAMERA_MODES`, `NIRUCLIP_TEST_CAMERA_BUSY`, `NIRUCLIP_TEST_PRIVACY`, `NIRUCLIP_PROBE_DIR`, `NIRUCLIP_DEV_DIR` | Test hooks for camera modes, a busy camera, the privacy control, the live-check directory and the device directory |
