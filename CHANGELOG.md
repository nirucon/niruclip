# Changelog

All notable changes to NIRUCLIP. Earlier releases are described in their
GitHub release notes and commit messages.

## 0.6.1 (2026-11-09)

The HP 5MP laptop camera no longer switches itself off after a second.

### Fixed

- NIRUCLIP set the camera's `exposure_dynamic_framerate` control to 0 for a
  steady 30 fps. The HP 5MP camera answers that by turning its UVC privacy
  control on and showing its crossed-out-camera image one or two seconds into
  the stream (reproduced with plain ffmpeg and v4l2-ctl), so every camera
  recording on that laptop lost the camera after about a second and reported
  "the camera privacy switch is on".
- The control is now left alone by default. `camera_steady_framerate = true`
  turns the old behaviour back on for cameras that halve their rate in low
  light; the previous value is still saved first and always restored.

## 0.6.0 (2026-11-09)

Choose the microphone per recording.

### Added

- `--mic SOURCE|default` and a `mic = "..."` key at the top of the config
  and in profiles, next to `audio`. The value is a PipeWire/Pulse source
  name, a case-insensitive part of its description (`webcam`, `UMC1820`) or
  `default`. Precedence: `--mic`, profile, top-level `mic`; absent means the
  system default source, exactly as before. Used by `--audio mic` and the mic
  half of `--audio both`; the system default is never changed.
- `--list-mics [--json]` lists the input sources (name, description, `*` for
  the system default), without sink monitors.
- The source is resolved at start. A missing microphone falls back to the
  system default with a notification instead of failing the recording.
- `session.json` has an optional `mic` object (requested, source, name,
  description, fallback); `--status` shows the microphone and any fallback.

### Changed

- A user profile named like a built-in one (`screen-camera`, ...) now changes
  only the keys it sets and keeps the others from the built-in profile;
  before, missing keys fell back to the plain defaults. Profiles saved with
  `--save-profile` set every key and behave as before.

## 0.5.1 (2026-11-09)

### Fixed

- A segment whose ffmpeg had to be killed (a camera that hangs its reader)
  lost everything since its start, because Matroska clusters were only
  written every few seconds. Recordings now use one-second clusters, so at
  most about a second is lost and the segment repair (or `--recover` after a
  power loss) keeps the rest. Measured on the HP 5MP Camera with a blocked
  reader: 3 s + 5 s segments gave 5.0 s with 0.5.0 and 8.2 s with 0.5.1.

## 0.5.0 (2026-11-09)

Camera recording that survives a camera going away.

### Added

- **Camera loss recovery.** A camera is lost when its device node
  disappears, ffmpeg reports a camera input error, no camera frame arrives
  for `camera_stall_seconds` (default 2 s) or, camera-only, its stream ends.
  The user is notified at once and the current segment is closed cleanly.
  - Screen with camera overlay: recording continues without the camera in a
    new segment right away; a camera that returns within
    `camera_reconnect_seconds` (default 10 s) is probed and attached again.
  - Camera only: the recorder waits up to `camera_reconnect_seconds`
    (`camera.status` "missing") and continues in a new segment, or saves
    what was recorded.
  - Segments are joined losslessly into one file; a segment ffmpeg could not
    finish is remuxed, an empty one removed. A camera lost more than three
    times in a minute is given up on.
- **Placeholder reopen.** When the pre-check or the live check sees the
  firmware placeholder (privacy image) or a frozen picture, the camera is
  closed and reopened in another mode, up to `camera_reopen_attempts`
  (default 2), before the 0.4.0 warning is shown. USB autosuspend is kept off
  while reopening when that is possible without root
  (`camera_reopen_autosuspend`, default true); NIRUCLIP never uses sudo.
- `camera_events` in `session.json` (and the last five in `--status`) log
  every recovery step.
- Config keys `camera_reconnect_seconds`, `camera_stall_seconds`,
  `camera_reopen_attempts` and `camera_reopen_autosuspend`. All optional;
  existing configs keep working unchanged.
- Camera capabilities are cached per device node and boot
  (`camera-cache.json`).

### Changed

- The camera pre-check now runs with every countdown, also 0 and 1 s, in
  parallel with it; a short countdown waits for the check (about 1-2 s, at
  most 4 s) instead of skipping it.
- A failed start leaves no empty `.part.mkv` behind.

### Tests

- Lifecycle tests for unplug/replug, no return, stall, a stream that keeps
  ending, recovery turned off, the pre-check at countdown 0, reopen modes and
  USB autosuspend (writable and root-only), and a placeholder cured by a
  reopen. Unit tests for the config keys, reopen mode order, log detection,
  sysfs lookup and the fixed camera-only size.
