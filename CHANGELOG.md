# Changelog

All notable changes to NIRUCLIP. Earlier releases are described in their
GitHub release notes and commit messages.

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
