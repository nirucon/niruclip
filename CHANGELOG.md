# Changelog

All notable changes to NIRUCLIP. Earlier releases are described in their
GitHub release notes and commit messages.

## 0.14.0 (unreleased)

### Added
- **Window content capture**: `--window-id ID` records one X window's own
  picture through the Composite extension (no compositor needed) with
  MIT-SHM and Damage, also while it is covered by other windows or placed
  outside every screen. Frames reach ffmpeg as raw video through a FIFO with
  wall-clock timestamps, on one clock with the camera and the microphone; the
  camera overlay, pause and resume, camera recovery and disk checks work as
  with a screen. A resized window is fitted into its starting size with black
  bars, and a window that closes ends the recording and saves it. NIRUPRES
  3.1 Talk mode uses it to record only the slides.
- `--webcam-size tiny`: a camera overlay of 16 % of the picture height.
- `--camera-preview`: a small live colour picture of the camera as it is
  placed in the recording, for a presenter view (state field
  `camera_preview`).
- State fields (optional): `window`, `capture_started` (the first recorded
  frame, the zero of the timeline) and `camera_preview`. `--status` shows
  the recorded window.

### Changed
- **A camera overlay costs much less CPU.** The screen is converted to
  yuv420p before the overlay and the overlay is blended in yuv420p; with
  `format=auto` the whole picture was blended in RGB. A 1080p30 screen with
  a camera went from about 4 cores to about 2 with software encoding
  (window capture: about 1.6).

## 0.13.1 (2026-11-25)

The 0.13.0 tag was not released (a test needed ffmpeg on the build machine).
Everything below ships in 0.13.1.

## 0.13.0 (2026-11-25, not released)

### Added
- **The camera plays live**: Space shows the camera over the film where and
  when each clip has it, without rendering. It plays from a copy of the
  camera track made once in the background.
- **Title and camera lanes**: each clip's title and camera are bars of
  their own above the picture that move with their clip. Drag their ends
  and middle to time them, click for their settings, double-click to place
  them, Delete removes the title or turns the camera off. A double click on
  the title lane writes a title.
- **Lane switches**: hide the titles or the camera, mute the sound or the
  music, in the preview and the export.
- **Transport bar**: start, previous and next cut or marker, play, end, the
  time with frames, loop (R), snapping (N) and full screen (F, F11).
- **Several clips at once**: Ctrl+click and Shift+click choose clips, Ctrl+A
  all of them; panel changes, mute, duplicate and delete apply to all.
  Copy, cut and paste (Ctrl+C, X, V) at the playhead.
- **Markers** (B): named flags on the ruler that follow their clip, jumped
  to with the arrows and the transport buttons; chapters in MP4, MKV and
  WebM exports and a `.chapters.txt` list beside the film.
- **J and L cuts**: drag where the sound changes over off the cut, or set
  Sound cut in the panel: the next clip's sound comes early, or the
  previous one's runs on.
- **Volume line**: drag the line across a clip's sound to set its level,
  shown in percent and decibels.

### Fixed
- A film starting with a title card, a picture or a video without sound
  played silently in the live preview: the first piece now plays from a
  copy with silence under it.

## 0.12.0 (2026-11-24)

### Added
- **The camera on its own track**: `--camera-track separate` (config and
  profile key `camera_track`) records the screen clean and the camera as a
  second video track with its placement; `both` keeps the overlay too.
  Recording from the editor uses its own track by default (a new choice in
  the record dialog). `--export mp4|gif` puts the camera back in its corner.
- **Picture in picture in the editor**: the camera track (or any picture or
  film of the project) over a clip, moved, sized, shaped (rounded, circle,
  rectangle), with a border, a soft shadow, fade or slide in, for part of
  the clip, or off.
- **The placing view (E)**: one picture of the clip as it will be exported;
  drag the camera and the title into place, the wheel sizes them, single
  keys change shape, border, shadow, look, colour and effect.
- **Titles**: three new looks (lower third, neon, marker) with an accent
  colour, effects (slide up, slide in, pop, type itself), a free position,
  and templates for a lower third, neon heading, marker, pop heading and
  typewriter.

## 0.11.0 (2026-11-23)

### Added
- **Titles in any installed font**, in four styles and eight colours, with
  a box, an outline, a shadow or plain, in nine places, with their own
  start, length and fade, over several lines (`|`). Templates for a
  heading, a name tag and a subtitle bar. A font without the letters of
  the text falls back to the usual one.
- **Title cards** (Ctrl+T, + Card): text on a colour, as a clip.
- **Sound for speech**: per clip Voice, Voice in a noisy room or Even out,
  heard live in the preview; recordings from the editor with a microphone
  get Voice from the start.
- **Loudness**: the film is measured and levelled in two passes to −16 LUFS
  (new films), −14 or −23, with a peak limiter.
- **Music under speech**: the music is lowered a little or a lot while the
  film speaks.
- **Microphone and desktop on their own tracks**: a recording with both
  keeps a mix (what players play) plus the two sources alone, so the editor
  treats the voice without the desktop sound and gives the desktop its own
  level.
- **Cut pauses** (Ctrl+K): silences longer than a chosen length, at an
  automatic or chosen level, shown on the timeline and cut in one go.
- **Captions** (Ctrl+L): made from the speech by NIRUDICTATE, on this
  computer, in Swedish or English; edited in a list; they follow every
  edit. Burnt in, or as a subtitle track and an `.srt` at export.
- **Zoom** into a part of the picture, gliding in and out, and **hide** a
  part (blur, pixelate, black box) for a while; both drawn on a still of
  the clip.
- **Logo** over the whole film, in a corner, with size and strength.
- **Size limit** at export (8, 10, 25, 50, 100 MB): two passes, a smaller
  picture when needed. `--limit MB` and `--captions burn|track` for
  `niruclip edit --export`.
- The panel shows only the settings that apply to the clip.

### Changed
- Projects are schema 2. Older projects open with their titles in the same
  place and their sound as before (no levelling).
- A sound-only export no longer decodes the picture.
- ffmpeg and nirudictate helpers end with the editor, also when it is
  killed.

## 0.10.0 (2026-11-23)

### Added
- **Record from the editor.** Ctrl+R or the Record button: full screen
  (and which screen), a window, an area or the camera only; sound none,
  microphone (and which), desktop or both; the camera in a corner with its
  size; a countdown; and whether the film goes at the end or at the
  playhead. The editor hides while recording and comes back with the new
  film on the timeline when the recording is stopped from the bar or with
  `niruclip --stop`. For a window or an area the countdown comes before the
  click, so there is time to switch workspace. The choices are kept in
  `edit.toml`.

## 0.9.0 (2026-11-23)

### Added
- **`niruclip edit`, a video editor.** Import videos, sound and pictures in
  the usual formats, cut, split, trim and reorder them on a timeline, add
  music under the film, titles, speed, volume, fades, transitions (cross
  fade, dissolve, through black or white, wipes, slides, circle, pixelize)
  and picture effects (brightness, contrast, colour, blur, sharpen, rotate,
  mirror, crop). The preview is mpv in the window, on the graphics card;
  P renders a stretch with every effect. Export to MP4, a small MP4, WebM,
  MKV, GIF, MP3, WAV, FLAC or Opus, at the original size or 2160p to 480p,
  the whole film or a marked part, in the background. Projects are small
  `.niruclip` files; the sources are never changed. Undo and redo, autosave,
  keyboard first (F1 lists the keys). Open, import and export use the
  Videos folder unless changed under Settings. `niruclip edit --export`
  renders a project without a window. Needs `mpv`.
- The release carries `niruclip-edit.desktop` and `niruclip.svg`, so the
  editor shows in the launcher and under "Open with" for videos.

## 0.8.1 (2026-11-22)

### Changed
- **A plugged-in camera comes first.** Without a chosen camera, NIRUCLIP
  now takes a camera on a USB port, hub or dock before the laptop's own
  (the kernel marks built-in USB devices as fixed). A docked laptop with
  its lid closed filmed a black picture from its own camera; it now films
  with the external one, and with the dock unplugged the built-in camera is
  used again. `--check-camera` lists them in this order. A camera set with
  `camera =` or `--camera` is used as before.

## 0.8.0 (2026-11-19)

### Added

- Recording sounds: `record-start` plays just before the capture starts and
  `record-stop` once it has stopped, so neither ends up in the recording.
  They are NIRUX sound events (category `feedback`, on by default) played
  through NIRUNOTIFY; `--no-notify`, `NIRUCLIP_NOTIFY=0` or
  `NIRUCLIP_SOUND=0` keep them silent.

## 0.7.0 (2026-11-10)

Choose the inputs of a multichannel audio interface.

### Added

- `--mic-channels N|N,M|all` and a `mic_channels = "..."` key (top level and
  per profile, next to `mic`): record one input of a multichannel source as
  mono (`3`) or a pair as stereo (`1,2` or `1+2`), numbered from 1 like the
  interface (input 1 is AUX0). `all` or absent records the source as before.
- `--list-mics` shows each microphone's channel count and names (from
  `pw-dump`), plus how to pick inputs for sources with more than two channels.
  `--list-mics --json` adds `channels`, `channel_names` and `multichannel`.
- `session.json` `mic` has optional `channels`, `inputs`, `input_names` and
  `inputs_fallback`; `--status` shows the inputs in use.
- The mapping is done in the capture pipeline only (all channels opened with
  `stream.dont-remix` for this ffmpeg, then `pan`). No PipeWire node is
  created and the system default is not changed. Inputs the source does not
  have, an unreadable layout or a missing microphone give a notification and
  the usual recording, never a refused one.

### Changed

- The audio filters (`amix`/limiter for `--audio both`, the new `pan`) run as
  their own filter graph instead of inside the video graph, which lowered the
  dropped/duplicated frame counters with audio filtering in tests on othala.

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
