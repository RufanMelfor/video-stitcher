# Session handoff — 2026-07-13

Continuation note for resuming work on a different machine/session (now
git-tracked so it travels with `git pull`/`push` between the user's two
PCs — was gitignored/local-only before this date, which meant it never
actually reached a second machine). Kept short and current; overwrite
wholesale at the end of a session rather than appending history — git
history is the append-only log, this file is just "where things stand
right now."

## Current state

Working tree clean, `main` up to date with `github/main`
(`ee785d3c`). Nothing uncommitted.

## What shipped this session (2026-07-13)

1. **`rig-calib` excluded from the workspace** (`3d4f3222`) — deprecated
   (superseded by reco-gui), no longer built/tested/linted by
   `--workspace` commands, but source kept on disk and still buildable
   standalone via `cargo build --manifest-path crates/rig-calib/Cargo.toml`.
2. **`top_tilt_x`/`top_tilt_z`** (`b67c38da`) — manual-only mirror of
   `ground_tilt_x/z` for the *top* of frame (distant structures/goal
   frames/skyline), since the existing correction is deliberately
   one-sided and never touched that region. New reco-gui sliders,
   `match.json` fields `topTiltX`/`topTiltZ`. No auto-fit path, by
   explicit user request.
3. **Adjustable band width** (same commit) — `groundTiltBandWidth`/
   `topTiltBandWidth`, new reco-gui sliders, control where each band's
   correction reaches full strength (ramp always starts at the fixed
   `0.08`). Defaults to `0.16` (the old hardcoded value), so existing
   calibrations render unchanged.
4. **`crates/reco-calibrate/FRICTION.md` point 22** (`ee785d3c`) — logs
   items 2-3 in that file's own long-running ground_tilt history.

Full technical detail for 2-3: `crates/reco-calibrate/FRICTION.md` point
22. For the much longer ground_tilt/seam-alignment saga that precedes all
of this (resolved 2026-07-07): FRICTION.md points 1-21.

## Environment (must re-set every new shell/session on any machine)

```bash
export FFMPEG_DIR="<path to the BtbN.FFmpeg.GPL.Shared.7.1 winget package>/ffmpeg-n7.1.4-7-gadcf20da26-win64-gpl-shared-7.1"
export PATH="/c/Program Files/LLVM/bin:$PATH"
```

Needed for anything touching `reco-io`/`reco-calibrate`/`reco-gui`/
`reco-cli` (ffmpeg-sys-next's build script needs the dev headers/libs +
Clang for bindgen). To *run* `reco-gui.exe` (not just build), the
FFmpeg `bin/` dir must also be on `PATH` at runtime (dynamic linking).

Building `reco-obs` additionally needs `OBS_INCLUDE_DIR` pointed at a
local copy of libobs's C headers (no winget package for these) - see
`crates/reco-obs/README.md` if that's ever needed on a fresh machine.

These paths are machine-specific (installed via winget on the original
machine) - adjust for wherever this is being resumed.

## Known pre-existing issues (not caused by recent work, not yet fixed)

- `reco-io`'s `matroska_reader_sees_partial_writes` test fails
  deterministically (`Decode(Ffmpeg("End of file"))`) - a reader opening
  a Matroska file while the writer is still running. Not investigated.
- `reco-core`'s `interop::cuda::tests::{test_cuda_available,
  test_shared_memory_allocation}` fail on machines without a working CUDA
  runtime (`cudaGetDevice` error code 3) - hardware/driver gap, not a
  code bug.
- `reco-obs` compiles all the way through Rust source but fails at the
  final *linking* step on a missing `obs-frontend-api.lib` - a separate
  OBS SDK linkage gap (headers are set up per above, the linkable `.lib`
  isn't). Not solved.

## Open threads (not started, no urgency)

- `imu_sync()` in `reco-calibrate/src/pipeline.rs` still gates
  `rig_tilt`/`rig_roll`/differential-orientation on the same
  `has_native_gyro` check as frame-sync reliability - decoupling them
  would likely make the manual `--enable-x-rx` flag unnecessary for
  quaternion-only cameras (DJI). See FRICTION.md / calibration history.
- Ground-plane homography from known pitch-marking dimensions (owner-
  approved as an optional/opt-in feature) - not started.
- Stale open PR #10 on the private fork ("Manual feature matching
  files") - superseded by work already on `main`; probably just needs
  closing.
