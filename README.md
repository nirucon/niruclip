# NIRUCLIP

**Part of [NIRUX - A minimal X11 desktop by Nicklas Rudolfsson](https://github.com/nirucon/nirux).**

**Early beta.** A small native X11 screen, region and camera recorder. NIRUCLIP
owns the recording process and a versioned state file; ffmpeg performs encoding
and `slop` selects a rectangle. It does not upload or run a background daemon.

```sh
niruclip --full --audio mic
niruclip --region --audio both
niruclip --full --webcam corner --corner bottom-right --webcam-size large
niruclip --webcam-only --audio mic
niruclip --profile screen-camera
niruclip --stop
niruclip --status --json
niruclip --recover
```

## Capture choices

| Option | Values |
|---|---|
| Mode | `--full`, `--region`, `--window`, `--webcam-only` |
| Audio | `--audio none`, `mic`, `desktop`, `both` |
| Camera | `--webcam off`, `corner`, `only` |
| Corner | `--corner top-left`, `top-right`, `bottom-left`, `bottom-right` |
| Camera size | `--webcam-size small` (20%), `medium` (30%), `large` (40%) |
| Preset | `--preset share` (up to 1080p), `small` (up to 720p), `quality` |
| Countdown | `--countdown 0..10` |

Camera size is a target share of the **final video width**. The camera is never
upscaled beyond its native width. The default is `medium`. An overlay is placed
16 pixels from the selected corner.

`--full` and an overlay use the primary connected RandR output at its real X11
origin. `--screen OUTPUT` chooses another connected output. `--region` asks for
a rectangle. `--window` currently records a selected static rectangle; it does
not follow a window that moves or resizes. `--webcam-only` does not require a
usable X11 screen geometry.

The `share` and `small` presets avoid upscaling a smaller screen. `quality`
keeps the screen dimensions, rounded down to even values. Camera-only capture
is capped at 1920 pixels wide (1280 with `small`) and keeps the camera aspect.

## Output and recovery

The default destination is `~/Videos/NIRUCLIP-YYYY-MM-DD_HH-MM-SS.mkv`.
`NIRUCLIP_OUTPUT` changes the directory and `--output` selects a file or an
existing/created directory. Existing output and partial files are never
overwritten; a numeric suffix is chosen instead. MKV is written as
`.part.mkv` and renamed only after it contains a usable video stream.

A controlled stop finalizes a valid partial as a normal saved recording. An
unexpected encoder failure with a valid partial is `interrupted`: the partial is
kept and `niruclip --recover` validates and restores it. A failed startup with
no usable media is `failed`, not recoverable. A crash leaves stale state; the
next status/stop check does not signal a reused PID.

State is stored at `${XDG_STATE_HOME:-$HOME/.local/state}/niruclip/session.json`
and published atomically with schema version 1. NIRUBAR reads the same file and
shows the active recording, an interrupted recovery, or a failure.

## Profiles and notifications

Profiles live in `${XDG_CONFIG_HOME:-$HOME/.config}/niruclip/config.toml`.
Built-ins are `screen-mic`, `screen-camera`, `camera-mic` and `screen-desktop`.
Use `--profile NAME`, list with `--list-profiles`, and save the current explicit
choices with `--save-profile NAME`. A configured `default_profile` applies when
no `--profile` is given; command-line options still override it.

Desktop notifications show the countdown, start and result when
`notify-send` is available. `--no-notify` or `NIRUCLIP_NOTIFY=0` disables them;
the installed launcher uses a visible countdown notification in its GUI flow.

## Dependencies and distribution

Requires `ffmpeg`, X11 and `slop` for screen/region capture, plus a camera and
PipeWire's PulseAudio interface for camera/microphone capture. The released
binary is static x86_64 (musl) for Debian, Arch and Void glibc. NIRUX installs
the matching packages and pins the release.

## Build and test

```sh
cargo test --locked
cargo clippy --locked --all-targets -- -D warnings
cargo build --release --locked
sh tests/lifecycle.sh target/release/niruclip
```

`tests/lifecycle.sh` uses fake ffmpeg/ffprobe and synthetic geometry. Run the
real camera, audio and display matrix on an authorized graphical host.

## Exit behavior

- `0`: recording saved, countdown cancelled by an explicit stop, or profile/status operation succeeded.
- `1`: capture failed, a controlled stop left only an interrupted partial, or no active session was available to stop.
- `2` is reserved by the wrapper for usage errors; NIRUCLIP reports usage problems on stderr with a non-zero status.

Recordings, partials and profiles are user data. NIRUCLIP never deletes a
personal recording as cleanup.

See [LICENSE](LICENSE).
