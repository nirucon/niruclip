# NIRUCLIP

Screen, region and camera recorder for the NIRUX X11 desktop.

**Status: alpha** · Part of the [NIRUX](https://github.com/nirucon/nirux) suite · [MIT License](LICENSE)

## About

NIRUCLIP is a small native X11 recorder. It owns the recording process and a
versioned state file; ffmpeg does the encoding and `slop` selects a rectangle.
It does not upload anything and runs no background daemon: a recording is one
process that lives exactly as long as the recording.

In NIRUX you normally start it from the recording view of NIRU Launcher, and
NIRUBAR shows the running, paused, interrupted or failed recording.

## Features

- Full screen, region, window rectangle or camera-only recording.
- Microphone, desktop audio or both (mixed at unity gain with a peak limiter).
- Camera overlay in any corner at three sizes, or camera only.
- `share`, `small` and `quality` presets that never upscale a smaller screen.
- VAAPI hardware encoding when a probe succeeds, with automatic software
  (`libx264`) fallback.
- Pause and resume with a lossless join of the segments on stop.
- Crash-safe output: `.part.mkv` files, validation before rename, and
  `--recover` for interrupted recordings, including a lossless remux after a
  power loss.
- Saved profiles plus four built-ins, and a configurable default profile.
- Free-space checks: a warning below 2 GB, a clean stop below 500 MB.
- Frame drop/duplication monitoring in `--status` and NIRUBAR.
- Countdown, start and result notifications.

## Requirements

- X11 (Debian, Arch and Void glibc, x86_64). The released binary is a static
  x86_64 musl build.
- `ffmpeg` (with `ffprobe`), `slop` for region capture, `xrandr`.
- For camera and microphone: a V4L2 camera, `v4l2-ctl` (`v4l-utils`) and
  PipeWire's PulseAudio interface.
- Optional: `notify-send`, a VAAPI driver for hardware encoding.

NIRUX installs the matching packages.

## Installation

### With NIRUX

NIRUCLIP is a core component: `nirux install` and `nirux update` install and
update it as part of the pinned suite generation.

### Standalone

```sh
./install.sh            # download the released static binary, verify SHA256SUMS, install to ~/.local/bin
./install.sh --build    # build from this checkout with cargo and install it
```

`NIRUCLIP_PREFIX` changes the prefix (default `~/.local`); `NIRUCLIP_VERSION`
installs another release.

## Usage

```sh
niruclip --full --audio mic
niruclip --region --audio both
niruclip --full --webcam corner --corner bottom-right --webcam-size large
niruclip --webcam-only --audio mic
niruclip --profile screen-camera
niruclip --pause
niruclip --resume
niruclip --stop
niruclip --status
niruclip --status --json
niruclip --recover
niruclip --list-profiles
niruclip --full --audio desktop --save-profile demo
```

| Option | Values |
|---|---|
| Mode | `--full`, `--region`, `--window`, `--webcam-only` |
| Audio | `--audio none`, `mic`, `desktop`, `both` |
| Camera | `--webcam off`, `corner`, `only` |
| Corner | `--corner top-left`, `top-right`, `bottom-left`, `bottom-right` |
| Camera size | `--webcam-size small` (20 %), `medium` (30 %, default), `large` (40 %) of the final video width |
| Preset | `--preset share` (up to 1080p), `small` (up to 720p), `quality` |
| Encoder | `--encoder auto` (default), `software`, `vaapi` |
| Countdown | `--countdown 0..10` |
| Output | `--output PATH` (file or directory) |
| Screen | `--screen OUTPUT` (connected RandR output for full/overlay) |
| Profile | `--profile NAME` |
| Notifications | `--no-notify` |

`--full` records the primary RandR output. `--window` currently records a
selected static rectangle; it does not follow a window that moves or resizes.

Recordings go to `~/Videos/NIRUCLIP-YYYY-MM-DD_HH-MM-SS.mkv` unless
`NIRUCLIP_OUTPUT` or `--output` says otherwise. Existing files are never
overwritten.

The details of encoding, pause/resume, recovery, the state file, audio mixing,
disk-space checks and exit codes are in [docs/REFERENCE.md](docs/REFERENCE.md).

## Configuration

Profiles live in `${XDG_CONFIG_HOME:-$HOME/.config}/niruclip/config.toml`.
Built-in profiles:

| Profile | Capture |
|---|---|
| `screen-mic` | Full screen and microphone |
| `screen-camera` | Full screen with camera overlay and microphone |
| `camera-mic` | Camera only with microphone |
| `screen-desktop` | Full screen with desktop audio |

`--save-profile NAME` saves the current choices (letters, digits, dot, dash and
underscore). A `default_profile` applies when no `--profile` is given;
command-line options still override it.

```toml
schema = 1
default_profile = "screen-mic"
encoder = "auto"            # auto, software or vaapi

[profiles.demo]
description = "Custom capture profile"
mode = "full"               # full, region, window, webcam-only
audio = "desktop"           # none, mic, desktop, both
webcam = "off"              # off, corner, only
corner = "bottom-right"
webcam_size = "medium"      # small, medium, large
preset = "share"            # share, small, quality
countdown = 3
screen = "DisplayPort-0"    # optional
```

Notifications are shown when `notify-send` is available; `--no-notify` or
`NIRUCLIP_NOTIFY=0` disables them. Other environment overrides are listed in
the [reference](docs/REFERENCE.md#environment-overrides).

## Files and paths

| Path | Purpose |
|---|---|
| `~/.local/bin/niruclip` | The program |
| `~/.config/niruclip/config.toml` | Profiles, default profile, encoder |
| `~/.local/state/niruclip/session.json` | Recording state (schema 1), read by NIRUBAR and NIRU Launcher |
| `~/.local/state/niruclip/vaapi-probe.json` | Cached VAAPI probe result per boot |
| `~/Videos/NIRUCLIP-*.mkv` | Recordings (`*.part.mkv` while recording) |

Recordings, partials and profiles are user data. NIRUCLIP never deletes a
personal recording as cleanup.

## Troubleshooting

- **A recording ended unexpectedly:** run `niruclip --status`, then
  `niruclip --recover`. Segments of a paused recording are joined too.
- **Choppy camera overlay:** check `--status` for the drop warning. NIRUCLIP
  already requests 30 fps and disables dynamic exposure framerate during
  capture.
- **Hardware encoding not used:** `--status` shows the encoder and any
  fallback. Delete `vaapi-probe.json` after a driver change, or force
  `--encoder software`.
- **Recording refused to start:** less than 500 MB was free in the output
  directory.

## Uninstall

With NIRUX, NIRUCLIP is removed by `nirux uninstall`. A standalone copy is one
file: `rm ~/.local/bin/niruclip`. Configuration and state are in
`~/.config/niruclip` and `~/.local/state/niruclip`; recordings are never
removed.

## Building from source

```sh
cargo test --locked
cargo clippy --locked --all-targets -- -D warnings
cargo build --release --locked
sh tests/lifecycle.sh target/release/niruclip
sh tests/smoke.sh target/release/niruclip
```

The tests use fake ffmpeg/ffprobe and synthetic geometry. The real camera, audio
and display matrix is run on a graphical test machine.

## Status, disclaimer and how it's made

**Status: alpha.** Recording, pause/resume and recovery work on my machines,
but `--window` is still a static rectangle and the camera/audio matrix depends a
lot on hardware.

NIRUX and every niru* project are made by me, Nicklas Rudolfsson
([@nirucon](https://github.com/nirucon)), for me. They are tailored to my own
needs, preferences and workflows, on my own machines. If they fit yours too,
that's a nice bonus.

- **Free and open source.** Anyone is free to use, study, change and share
  it under the [MIT License](LICENSE).
- **No guarantees.** There is no warranty of any kind. Use it at your own
  risk, read what you install and keep backups of anything that matters.
- **How it's made.** Some of these projects I built from scratch by hand. In
  more recent releases I have used Claude, Anthropic's AI, to do much of the
  heavy lifting under my direction: I decide what to build and how it should
  behave, and I test it on my own machines. It saves me a lot of time, and
  the result is quite OK.
- **Alpha, beta, dev, and maybe forever.** Most things here are alpha, beta
  or dev, and may stay that way. Maybe one day there will be a real stable
  release, but given how fast development moves, and how I am as a person,
  maybe never :)

| Label | Meaning |
|---|---|
| dev | Under active construction; may not build, install or work yet. |
| alpha | Works on my machines, but incomplete or rough; expect breaking changes. |
| beta | In daily use on my machines and mostly complete; rough edges remain. |

## License

MIT License, see [LICENSE](LICENSE). Copyright (c) 2026 Nicklas Rudolfsson.
