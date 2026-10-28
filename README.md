# NIRUCLIP

Small X11 screen, region and webcam recorder. Uses `ffmpeg` for capture and
`slop` for region selection; it is separate from NIRUSHOT. Session state lives at
`${XDG_STATE_HOME:-$HOME/.local/state}/niruclip/session.json`.

```sh
niruclip --full --audio mic --countdown 3
niruclip --region --audio both --webcam off
niruclip --webcam-only --audio mic
niruclip --stop
niruclip --status
```

Default output: `~/Videos/NIRUCLIP-YYYY-MM-DD_HH-MM-SS.mkv`. MKV is written as a
`.part.mkv` and renamed only after a clean ffmpeg exit; an interrupted recording
keeps the partial and marks the state failed for recovery.

Requires `ffmpeg`, X11, and `slop` for region/window capture. Audio uses the
PipeWire PulseAudio interface (`@DEFAULT_SOURCE@` / `@DEFAULT_MONITOR@`). No
network upload and no permanent daemon.
