# Agent assignment: NIRUCLIP 0.3.0, a one-shot efficiency, robustness and repository update

## 0. Quick start

1. Read this file, `README.md`, `src/main.rs` and `tests/lifecycle.sh`. The audit baseline
   is `2959ed0` (0.2.3, pinned by NIRUX 0.12.199). Revalidate each item against current
   HEAD and skip items that are already fixed, noting that you did.
2. Implement **C1–C7, then U1–U3 in order**. Commit per item. Keep `cargo fmt --check`,
   `clippy -D warnings`, tests and `sh tests/lifecycle.sh target/release/niruclip` green.
3. Pass the gates in §4, release in §5 and report in §6.

This is an **execution** assignment. Do not stop after a plan or a branch. Make sensible
decisions, document them, and continue without routine questions.

## 1. Invariants

- NIRUCLIP owns the recording process and the versioned state file. Keep `session.json`
  schema 1 compatible: NIRUBAR and NIRULAUNCHER read it, so new fields must be optional.
- A recording is never lost: `.part.mkv` → validate → rename, recover after an
  interruption, and never overwrite an existing file.
- No upload and no daemon. The output stays MKV.
- **Software encoding stays available and is the guaranteed fallback.** Hardware encoding
  must never turn a working recording into a failed one.
- Keep `rust-version = 1.77` and the static musl release.

## 2. Findings (verified 2026-11-04 on niruxwork)

### C1: Every recording is software-encoded (battery, dropped frames)
`build_plan` always uses `libx264 -preset veryfast`. On niruxwork (AMD, `/dev/dri/renderD128`,
ffmpeg has `h264_vaapi`), a 6 s 1920x1200@30 x11grab encode used **89 % of a core with
libx264 and 58 % with h264_vaapi**. That is about a third less CPU, and more on busy
screens and at 2560x1440.
**Fix:**
- Add `--encoder auto|software|vaapi`, with config key `encoder`, default `auto`.
- `auto` uses VAAPI only when a render node exists **and** a 1-frame probe encode
  (`-f lavfi testsrc -frames:v 1 -c:v h264_vaapi -f null -`, 3 s deadline) succeeds. Cache
  the probe result per boot in the state dir.
- VAAPI path: `-vaapi_device <node>`, filters ending with `format=nv12,hwupload`,
  `-c:v h264_vaapi`, and quality mapped from the preset (`-qp` or `-global_quality`).
- If ffmpeg fails before the first frame on the hardware path, retry **once** in software
  automatically and record `encoder` and `fallback` in the state and the notification.
- Report the encoder in `--status`.

**Done when:** `tests/lifecycle.sh` covers auto→vaapi, a failed probe→software, and a
first-frame failure→software retry, using fake ffmpeg. Native recordings on niruxwork
(VAAPI) and on a host without VAAPI (or with it forced off) both produce valid files.
Record CPU before and after in the report.

### C2: There is no CI
Only `release.yml` exists. **Fix:** add `.github/workflows/ci.yml` with fmt, clippy,
test, a release build, `tests/lifecycle.sh` and `tests/smoke.sh`, plus an MSRV job on
1.77, the same as the other suite repos.

### C3: A stale branch
The remote has a `master` branch holding only "Initial NIRUCLIP capture orchestrator".
`main` is the default. **Fix:** tag it as `archive/master-initial`, push the tag, then
delete the remote `master` branch. Check that nothing in `nirucon/nirux` references it.

### C4: The camera setting is not restored after a crash
`CameraControl` sets `exposure_dynamic_framerate=0` and restores it only in `Drop`. After
SIGKILL, a crash or a power loss the camera stays fixed, which makes low-light video calls
worse later. **Fix:**
- Store the device and the previous value in `session.json` (an optional field) before
  changing it.
- Restore it in `--stop`, in `--recover`, and on the next start when the stored session
  is stale.
- Add a test with a fake `v4l2-ctl`.

### C5: Queues are too small for the inputs
`screen_input` uses `-thread_queue_size 8`. Under load with a camera overlay, ffmpeg
reports blocking queues, and the performance diagnostics show drops or duplicates.
**Fix:** use 512 for x11grab, v4l2 and pulse inputs. Verify with the existing performance
counters on niruxwork: screen + camera + mic for 60 s before and after, with the drop and
duplicate percentages in the report.

### C6: Mixed audio can clip
`--audio both` uses `amix ... normalize=0`. **Fix:** add a limiter (`alimiter=limit=0.95`)
after the mix. Keep the levels otherwise unchanged and document it.

### C7: Stopping is slow and opaque
Verify how long a controlled stop takes, from `--stop` to the final file. If it exceeds
3 s for a normal recording, investigate the cause (signal order, waiting on ffprobe
validation). Fix it while keeping validation. Notify "Saving…" immediately on stop.

## 3. UX

### U1: Pause and resume
Add `niruclip --pause` and `--resume`. Implement them as segments: stop the current
ffmpeg segment cleanly, start a new one on resume, and concatenate losslessly with the
concat demuxer on stop. The state shows `paused`. NIRUBAR treats an unknown state as
recording, so check nirubar's `recording.rs`. If it needs a change to show "paused", note
that in the report instead of changing nirubar.

### U2: Disk space guard
Before starting, and every 30 s while recording, check the free space in the output
directory. Warn at < 2 GB. Stop cleanly at < 500 MB with a clear notification. The partial
file stays valid.

### U3: Clearer `--status`
Human output: mode, elapsed time, size so far, encoder, fps, a drop warning and the output
path. `--status --json` gets the same fields (optional).

Out of scope: following a moving window, upload or sharing, GIF export, and editing.

## 4. Gates

Automated:
- fmt, clippy `-D warnings`, `cargo test --locked`, a release build,
  `tests/lifecycle.sh`, `tests/smoke.sh`, and the MSRV job.
- New lifecycle cases for C1 (three paths), C4, U1 pause/resume/concat with fake ffmpeg,
  and the U2 thresholds with a fake `statvfs` override (add an env hook for tests only).

Native, on **niruxwork** (the owner allows local testing):
- Record 30 s for each of: full screen + mic, region, screen + camera corner, camera only,
  and desktop + mic.
- Check playback with `ffprobe`.
- C1 VAAPI and software CPU.
- C5 drop counters.
- U1 pause/resume.
- A kill -9 during recording, then `--recover`, then check that the camera control was
  restored.
- **Delete every test recording afterwards**, and do not leave anything in `~/Videos`.
- Do not record while the owner is working in a way that captures private content beyond
  what is needed. Short, deleted test clips are fine.

## 5. Release

Bump to **0.3.0**. Tag `v0.3.0` through the normal workflow and verify the assets and
checksums. Then pin it in `nirucon/nirux` `manifest`:
- If `nirux/tools/suite-pin` exists, use it.
- Otherwise bump the manifest the usual way. Change only the niruclip line, and rebase if
  other pins land at the same time.

Install through `nirux update` and verify the running version.

## 6. Final report

For each item C1–C7 and U1–U3: the change, the tests and the evidence. Include CPU per
encoder, drop percentages before and after, and stop latency. Add the branch cleanup,
release URLs, installed versions and remaining limits. Mark anything unfinished as
**PARTIAL** and give the exact remaining steps.
