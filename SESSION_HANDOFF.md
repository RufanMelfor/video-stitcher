# Session handoff — 2026-07-14

Continuation note for resuming work on a different machine/session -
git-tracked so it travels with `git pull`/`push` between the user's two
PCs. Overwrite wholesale at the end of a session rather than appending
history - git history is the append-only log, this file is just "where
things stand right now."

## Current state

`main` at `c209b822` (lookahead VRAM fix, see its own section below -
committed and pushed to `github`). Working tree otherwise clean.
Untracked `seam_debug.txt`/`seam_debug2.txt` in the repo root are
leftover local debug-log dumps from a previous session's diagnosis
work - harmless, safe to delete, not gitignored on purpose (no need to
bother).

**reco-gui.exe rebuilt and smoke-tested tonight** (release build,
launched, confirmed it reaches a healthy steady state - GPU init,
zero-copy preview pipeline, calibration/lens-database load, all clean
in the log with no errors) ahead of the user physically testing it
tomorrow. Full verification on `main`: `cargo fmt --check` clean,
`cargo clippy --workspace --all-targets --exclude reco-obs -D warnings`
clean, `cargo test --workspace --exclude reco-obs` all green except
two known pre-existing gaps (see "Known pre-existing issues" below,
both reconfirmed unrelated to tonight's changes - the CUDA ones are a
hardware/driver gap, and `matroska_reader_sees_partial_writes` fails
identically with or without tonight's commit, confirmed by testing
both states directly).

## Upstream contribution branches (prepared 2026-07-14/15, not yet pushed or PR'd)

The `reco-project/video-stitcher` owner asked (via a message relayed
through this session) for fork-only features back as **separate PRs,
one per feature**, built fresh off upstream's current `main`. The user
said to prepare *all* branches locally before any pushing/PR-opening;
that prep is now done for six independent branches, each verified with
its own `cargo fmt --check` / `cargo clippy -D warnings` / `cargo test`
pass **against upstream `origin/main`**, not this fork's `main`:

- `feat/windows-portability-fixes` (`879e379f`) - DirectML EP wiring,
  Slint/wgpu backend selection, reco-obs build.rs portability fixes.
  Clean cherry-picks from fork history.
- `fix/d3d11-stage-frame-unsafe` (`75e724c8`) - a **pre-existing**
  upstream clippy `-D warnings` failure on Windows (found while
  testing the branch above, not caused by it) - `D3d11StagingPool::
  stage_frame` needed `unsafe fn` + a `# Safety` doc comment, plus a
  few dead-code/cfg-scoping cleanups clippy flagged alongside it.
- `fix/concat-multisegment-seek` (`23170223`) - the concat-demuxer
  seek bug fix only; deliberately excludes the original commit's
  bundled multi-segment-persistence changes, which depend on a
  "reopen last files on startup" feature that doesn't exist on
  upstream `main` at all (itself a different, unpicked fork commit).
- `feat/inapp-roi-editor` (`00e4dfd7`) - cherry-pick + a real conflict-
  resolution pass against upstream's ROI-editor-adjacent changes
  (dead debug-log-window code and an unavailable `FlatButton`
  component both had to be dropped/adapted).
- `feat/seam-positioning` (`a0126951`) - **full reimplementation**, not
  a cherry-pick. Upstream's two intervening refactors (`#395` stitch
  unification, `#403` executor-spine/wgpu-free-engine) deleted
  `stitch_renderer.rs` and moved render-affecting rig state onto
  `Calibration.topology` instead of `ViewportConfig`. Rebuilt
  `seam_offset` + the debug line against the new `StitchCore` /
  `Executor` / `stitch::geometry::PlaneMap` architecture, with a CPU-
  side `BlendRule::Smoothstep` mirror and a new GPU-vs-CPU agreement
  test (`cpu_and_gpu_backends_agree_with_seam_offset`) that actually
  ran on real GPU hardware this session.
- `feat/color-matching-multiband` (`c433ded5`) - also a full
  reimplementation, built on the same seam-positioning-era
  architecture research. Per-camera exposure/white-balance matching at
  the seam band, sampling via a freshly-written `plane_uv_to_source_uv`
  (deliberately *not* reusing `lens::undistorted_to_distorted`, which
  uses a different/mismatched intrinsics convention - a bug class the
  original feature hit twice historically). **Caught a real regression
  via the existing agreement-oracle tests**: defaulting
  `color_match_enabled` to `true` (matching the original fork design)
  broke 7 CPU/GPU agreement tests because color matching has no CPU-
  side mirror; fixed by defaulting to `false` (opt-in), called out
  explicitly in the commit message as a deliberate departure from the
  original design. Multi-band spatial blend itself (blur/composite
  shaders) was **not** included - deferred as its own follow-up, not
  started.
  - **Note for next session**: this branch was briefly, accidentally
    committed on top of `feat/seam-positioning` (a bash pipe swallowed
    a `git checkout` failure's exit code, so a `||` fallback branch-
    creation never fired). Caught via `git branch --show-current`
    before anything was pushed; fixed by re-cherry-picking the color-
    matching commit onto a fresh branch off `origin/main` and manually
    resolving the resulting conflicts (each one was "does this
    reference `seam_offset`/`show_seam_line` from the sibling branch -
    drop it if so, since this branch must not depend on seam-
    positioning"). Both branches independently verified afterward
    (1 commit ahead of `origin/main` each, clean build/test/clippy).
    Mentioning this only so a future session doesn't need to
    re-derive it if something looks off - both branches are correct
    as of `c433ded5`/`a0126951`.

**Not yet done**: `feat/ground-top-tilt` (confirmed to not exist
upstream at all - the largest remaining feature), audio-sync/playback
UX, and live AKAZE detection preview were not started this session.
None of these branches have been pushed anywhere yet, and no PRs
have been opened - the user's instruction was prep-only until further
notice. Actually pushing/opening PRs additionally needs the real,
GitHub-recognized fork `RufanMelfor/video-stitcher` added as a local
remote (the day-to-day `github` remote, `RufanMelfor/reco-video-
stitcher-rig`, is **not** fork-linked to `reco-project/video-stitcher`
- confirmed via `gh repo view --json isFork,parent` - so GitHub will
refuse a PR from it directly). **Update 2026-07-15**: the user
confirmed `RufanMelfor/video-stitcher`'s `main` is now synced to
upstream's current tip (`ab553d35`, matching `origin/main` exactly),
so that remote is ready whenever the user says go for pushing/PRs.

**7th branch added 2026-07-15**: `feat/export-metadata-comment`
(`5ffb0a83`) - embeds a JSON snapshot of the settings actually used
(codec, quality, resolution, blend width, AI/autocam parameters) into
every export's container "comment" tag (`ffprobe -show_entries
format_tags`), so a batch of test exports with varying settings stays
self-describing without a sidecar file. New on `main` too (committed
`df7c3ee8` there first, since the user is actively using it for
testing), then ported to a fresh branch off `origin/main` - required
adapting the JSON builder since upstream's simpler `StitchArgs`/
`AutocamUiConfig` lack several fork-only fields (`blend_flip_direction`,
`multiband`, `seam_offset`, `lookahead_reduced_bit_depth`); the core
`EncoderConfig::metadata_comment`/`StitchJob::metadata_comment` builder
mechanism ported over unchanged since it doesn't depend on any of those.
Fully verified independently (fmt/clippy/test clean) same as the other
six.

**Appearance/restyle - not prepared as a branch, asked about
2026-07-15, deliberately left bundled for now**: the user asked whether
a PR exists for "how the program looks, all the new buttons etc." It
doesn't - the visual work lives in two fork-history commits, both
heavily bundled with unrelated functional features:
- `ba37aee5` "port rig-calib calibration UX + full transport/panel
  restyle" - new `FlatButton` component, transport-bar redesign,
  flat control language, FOV pill - bundled together with 8 unrelated
  functional ports (ground_tilt sliders, blend_flip_direction,
  variable playback speed, auto-calibrate tuning, sync-offset
  auto-detection + audio waveform panel, reopen-last-files-on-startup,
  playhead-jump fix).
- `d9d63850` "numeric-field polish, toolbar debug panel, transport
  layout tweaks" - NumEdit fields, "Expert Mode" button rename, panel
  reordering, transport layout - bundled with the Debug toolbar/log
  dialog (pure functionality, not appearance).
User's instruction: leave this un-split for now (don't prepare a 7th
branch), **but make sure it gets mentioned in the text whenever the
relevant PR(s) are written** - i.e. don't let the owner's PR review
silently miss that a restyle exists in fork history; it needs a
call-out (in whichever PR description ends up being the natural place,
or as its own note to the owner) that this feature exists but needs
untangling from unrelated functional commits before it can be its own
clean PR, same category of work as the concat-seek/persistence split.

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

## Export VRAM/lookahead limit - implemented, verified, and committed (2026-07-14/15)

Full write-up (root cause, competitive research against "Once Autocam",
full design reasoning including the Windows CUDA-import investigation
and the R16Unorm render-attachment discovery): `crates/reco-core/
FRICTION.md` "Lookahead pool VRAM cost scales with source bit depth" -
kept current, check there first for anything code-level. This section is
just the session-handoff summary.

**Shipped this session, committed as `c209b822`:**
- `session::vram_pool::LookaheadBitDepth` (`Native`/`Reduced8Bit`, opt-in,
  default `Native` = zero behavior change unless explicitly set via
  `StitchSession::set_lookahead_bit_depth`).
- `render::lookahead_downconvert::LookaheadDownconverter` +
  `shaders/lookahead_downconvert.wgsl` - a small GPU render pass that
  downconverts a P010/NV12 plane to 8-bit NV12, relying on wgpu's
  existing Unorm normalization (no manual bit math).
- `VramPool::copy_from_textures` (Linux/macOS) and the new
  `VramPool::copy_from_d3d11` (Windows) both branch the same way: same
  format in/out uses a bit-exact raw copy (`copy_texture_to_texture`,
  plane-aspect-selected on Windows via the new
  `D3d11StagingPool::plane_source`); `Reduced8Bit` runs the downconvert
  pass instead.
- **Windows (`interop::d3d11::D3d11StagingPool`) is now also
  implemented**, not deferred: the D3D11-imported pool shrank to a small
  fixed 4-slot bridge (was scaling with the full lookahead depth); the
  actual long-lived buffer is now `VramPool`, same as Linux/macOS, fed by
  `copy_from_d3d11` right after each frame is staged. Rendering
  (`frame_processing.rs`'s D3D11 buffered-path branch) now reads from
  `VramPool` bind groups, mirroring Linux's `render_gpu_resident` exactly.
  A real cross-API sync gap (wgpu DX12 read vs. the next D3D11
  `CopySubresourceRegion` write to the same shared-handle slot) was found
  and fixed with an explicit `device.poll(wait_indefinitely())`, matching
  the same pattern Linux/macOS already used for the identical hazard - see
  FRICTION.md for the full reasoning.
- **Verified end-to-end with a real export**, not just unit tests:
  `reco-cli stitch` against real 3840x2880 10-bit HEVC DJI footage with
  `--lookahead 1.5 --model <yolo onnx>` (real AI tracking) on this
  machine's NVIDIA RTX 3060 Ti. At `Native`, reproduced the user's
  original bug exactly. With `Reduced8Bit` enabled, the VRAM budget line
  dropped from "needs 4.71 GB" to "needs 2.36 GB" (exactly half, as
  designed) and the export completed cleanly: 150/150 frames encoded, no
  hangs, no driver faults, ball tracker acquired a real target, output
  file valid (correct dimensions/duration/codec) and a decoded frame was
  visually inspected - normal colors, no banding, no plane-swap
  corruption.
- Full verification: `cargo fmt --check`, `cargo clippy --all-targets
  -- -D warnings`, `cargo test --lib` (164 passed for `reco-core`, only
  the 2 pre-existing unrelated CUDA hardware-gap failures listed above;
  17 passed for `reco-io`) all green; `cargo check --workspace --lib
  --bins --exclude reco-obs` (reco-obs excluded for its own pre-existing
  OBS-SDK build gap) also green.

**Now user-facing, not just a code-level fix:**
- `StitchJob::lookahead_reduced_bit_depth(bool)` builder (`reco-io/src/
  stitch_job.rs`, mirrors the existing `.lookahead()` builder).
- `reco-cli stitch --lookahead-reduced-bit-depth` (`reco-cli/src/
  stitch.rs` + `main.rs`).
- `reco-gui` export panel: "Reduce lookahead memory (8-bit)" checkbox
  right under the Lookahead slider (`AutocamUiConfig::
  lookahead_reduced_bit_depth` in `export.rs`, Slint property
  `export-lookahead-reduced-bit-depth` in `ui/main.slint`).
- Re-verified with the real flag (not the earlier throwaway env var,
  which has been removed) on the same real footage - identical result to
  the original verification run: budget "needs 2.36 GB" instead of
  "4.71 GB", clean encode, no regressions.
- **If you tried this in the GUI before and got the old error message:
  that was a stale, pre-fix `reco-gui.exe` build** (binaries don't hot-
  reload) - rebuild with `cargo build -p reco-gui --release` and relaunch.

**Not yet done - the remaining gap before this is a fully shipped
feature, not a correctness concern:**
- Only tested on this session's Windows/NVIDIA machine. The Linux/macOS
  side of `VramPool` (which already existed structurally, just gained the
  `Reduced8Bit` branch) has not been live-tested this session - no
  Linux/macOS hardware available here. Code-reviewed and passes the full
  `cargo test`/`clippy` suite, but worth a real run on the other PC (or
  Linux/macOS CI) before fully trusting it.
- The VRAM risk slider in reco-gui's export panel (`lookahead-green-max`/
  `lookahead-red-min`, computed once when footage loads) does not yet
  recompute when the new checkbox is toggled - so the slider's red/green
  zone still reflects `Native` sizing even after checking the box. Not
  wrong (the actual export uses the checkbox correctly, verified above),
  just a stale visual hint - the fit-recompute block in `main.rs` (~line
  5337-5372) would need to also re-run on checkbox toggle, not only on
  file load.
- No GitHub issue filed upstream yet (user said "not yet" when first
  asked; now there's a concrete, verified fix to reference if that
  changes).

**Grayscale-for-AI-only (user asked, answered in-session, not
implemented)**: doesn't reduce the pool's memory by itself under the
current architecture, because the pool's *size* is driven by how long
the final render needs frames held (it re-renders the buffered frames,
it doesn't just peek at them for AI), not by what AI needs. Grayscale
would need the same kind of decoupling work as a downscale-for-AI
optimization (a separate, smaller AI-only buffer, decoupled from the
render-feeding one) to actually save memory - a plausible *complementary*
future optimization, not a substitute for this session's fix, and not
started.

**Next step whenever this is picked up again**: (1) optionally make the
VRAM risk slider re-fit live on checkbox toggle (polish, not
correctness), (2) if possible, a quick real-footage run on the other
(Linux/macOS, if applicable) machine to close the one platform this
session couldn't verify live, (3) decide whether to open the upstream
GitHub issue now that there's a working, user-facing fix to point to.

## Autonomous work note (2026-07-14 night → 2026-07-15)

The user went to sleep mid-session with instructions to keep working
and maximize progress toward a physically-testable GUI by morning, and
to put the machine to sleep when done or blocked. Everything from the
"Upstream contribution branches" section above through this point was
done under that instruction, without further check-ins. `reco-gui.exe`
was rebuilt and smoke-tested as the final step specifically because
that was the stated goal for tomorrow.
