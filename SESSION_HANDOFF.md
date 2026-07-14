# Session handoff — 2026-07-14

Continuation note for resuming work on a different machine/session -
git-tracked so it travels with `git pull`/`push` between the user's two
PCs. Overwrite wholesale at the end of a session rather than appending
history - git history is the append-only log, this file is just "where
things stand right now."

## Current state

Working tree clean, `main` up to date with `github/main` (`b19054b4`).
Nothing uncommitted. Untracked `seam_debug.txt`/`seam_debug2.txt` in the
repo root are leftover local debug-log dumps from this session's
diagnosis work - harmless, safe to delete, not gitignored on purpose
(no need to bother).

## What shipped this session (2026-07-14)

**`b19054b4`** - three independent reco-gui changes bundled in one commit:

1. **Seam debug line (Show seam line) is now grabbable properly.**
   Before: any drag anywhere in the preview while "Show seam line" was on
   hijacked pan/tilt entirely, and there was no visual affordance. Now:
   hovering the actual line shows a grab cursor, and only a press-and-
   drag that *starts* on the line moves it - everywhere else still pans/
   tilts normally.
   - New `reco_core::render::renderer::seam_line_screen_points()`
     projects the line's true on-screen position using the *live* camera
     yaw/pitch (queries `PoseControl::current_pose()` - not an
     approximation, the app already tracks this).
   - **Real bug found and fixed during verification** (worth remembering
     for any future screen-space math in this shader): the fragment
     shader remaps `uv` before testing it against `seam_offset` -
     `let uv = in.uv * 2.0 - vec2<f32>(0.5);` (extends [0,1] to
     [-0.5,1.5] so undistortion can sample beyond the plane's own edges).
     Missing that factor of 2 put the computed hit-test column ~140px off
     from the real line. Found by taking a real screenshot, measuring the
     line's pixel column programmatically (not eyeballing), and comparing
     against the computed value - confirmed via a permanent regression
     test (`seam_line_screen_points_matches_real_screenshot_measurement`)
     pinned to that real measurement.
   - Synthetic mouse automation (`SetCursorPos`/`mouse_event` via
     PowerShell) was tried to speed up verification and turned out
     unreliable *again* - it reported a stuck mouse position regardless of
     actual click target (see `feedback_synthetic_gui_automation_risk`
     memory, now with a third incident). Real user clicks + programmatic
     screenshot pixel-measurement was what actually got this fixed -
     don't trust synthetic click coordinates from this app again, but
     screenshot *analysis* (reading pixels back, not driving input) is
     fine and was reliable.
2. **"Reset" button for Auto Color Match tuning** - resets to
   `ViewportConfig::default()`, not whatever the loaded calibration has.
3. **"Reset AKAZE" button** in Auto-Calibrate's Advanced section - resets
   threshold/detect-y-min/detect-y-max/full-res-features to
   `AkazeConfig::default()`. Pure Slint (no Rust round-trip needed) since
   these were never persisted in `match.json` to begin with.
4. **Calibration section sliders now show 5 decimal places** (was 2-3):
   Intersect, Camera axis offset, x_ty, ground_tilt_x/z, top_tilt_x/z,
   ground_tilt_band_width, top_tilt_band_width.

Also this session (before the above): pulled 3 commits
(`f89036c0`..`71ad53a7`) authored from the user's *other* PC - Auto Color
Match tuning persistence + a ROI polygon bug fix - proof the git-based
cross-machine handoff (see below) works end to end.

## Cross-machine workflow (established 2026-07-13)

This file is the primary continuity mechanism between the user's two
PCs (different absolute repo paths, so this assistant's own per-machine
memory can't bridge them). Keep it updated each session; rely on it
(not assistant memory) for anything code/project-state related that the
*other* machine needs to know.

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
- Export can fail with "not enough VRAM for a 1.5s lookahead" on
  lower-VRAM cards with large source footage (e.g. 3840x2880 10-bit:
  ~66MB/stereo-frame, 71 slots for 1.5s = ~4.7GB, vs. ~3.2GB usable on a
  7.6GB card). Not a bug - `reco-core/src/session/vram_pool.rs`'s
  budget system is deliberately conservative and fails safely with a
  clear message (see closed upstream #360, already fixed here: preview
  VRAM no longer double-counted during export). User confirmed
  (2026-07-14) this is a real, known pain point they want fixed
  eventually, checked upstream issues - no exact match yet (#373 is
  preview-memory-during-playback, different; #379 is a different
  chained-export bug). No GitHub issue filed yet - user said "not yet"
  when asked. The real fix would be a lower-resolution *proxy* buffer
  for the lookahead pool (it only needs to support AI-trajectory
  prediction, not the final render, so full source resolution isn't
  actually required there) - an architecture change, not a quick fix.
