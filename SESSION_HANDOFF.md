# Session handoff - 2026-09-02 (TGR_PC), later same day: AI pitch-limit feature shipped + ball-tracker stuck-on-last-position bug fixed

## REPO STATE AT END OF SESSION (2026-09-02, later)

- Branch `main`, working tree clean except the pre-existing untracked
  `scripts/match-logger/Match Logger.html.txt`. Two commits made and
  pushed to `github/main` today (later than the morning's entry below):
  `7a87f1f8` (AI pitch-limit feature), `9ee7b926` (ball tracker fix).
  `git fsck --full` clean before push.
- reco-gui release+debug rebuilt with both changes (both
  `--features tensorrt`) - check timestamps before telling the user to
  test; a rebuild was in flight when this entry was written.
- reco-cli release also rebuilt during this session's diagnostics (used
  for CLI test exports with `--events`).

## DONE THIS SESSION (later, 2026-09-02)

1. **AI pitch-limit feature - user-confirmed working.** See
   `project_ai_pitch_limit_feature` memory for full detail. Two
   draggable lines on the stitched preview mark a manual world-space
   pitch ceiling/floor the AI director's output may never cross -
   persists to `Calibration.autocam_pitch_limits`, never restricts
   manual panning. Went through two real bugs found via user testing
   before it worked: (a) first version put the editor in the lens
   preview, where the black wedge isn't even visible - moved to the
   main stitched preview; (b) the clamp only restricted the center
   pitch, so a wide dynamic-FOV shot's rendered edge still showed the
   wedge - fixed to margin by half the vertical FOV. Also fixed a
   "nothing to drag" bug: a fresh calibration with no value set drew
   nothing at all, so a default placeholder position was added. New
   reusable reco-core geometry API:
   `unproject_screen_to_world`/`project_world_to_screen` +
   `ScreenProjection` (round-trip tested).

2. **Real ball-tracking bug found and fixed** (unrelated investigation,
   triggered by the user reporting jittery AI tracking / losing the
   ball near the sideline on the same match). See
   `project_ball_tracker_stuck_on_last_position` memory for the full
   diagnostic methodology and root cause.
   `BallTracker`'s `max_jump_rad` gate could permanently strand
   tracking on a stale position, rejecting a genuinely correct,
   player-anchored, high-confidence ball detection forever (only
   recoverable via a full ~3s coast-timeout cycle). Fixed: the jump
   gate is now skipped whenever player-anchor filtering is active
   (every anchor-passing candidate already cleared an independent
   plausibility check). 15 `reco-autocam` tests pass including a new
   regression test.

3. **Separate, real finding from the same investigation, NOT a code
   bug**: a persistent ~18.5s "ball not tracked" gap in a real test
   export turned out to be a static white field-marker disc on the
   halfway line, misclassified as "ball" by the model for 554
   consecutive frames on the exact same pixel position - visually
   confirmed via `dump_detection_frames.rs` + an ffmpeg crop. The
   tracker correctly rejected it (far from any player); the genuine
   ball wasn't confidently detected either (real play was far away
   near a goal at the time). This is a training-data hard-negative
   candidate, logged in `project_yolo26n_training_pipeline`'s
   2026-09-02 entry - not extracted into a training batch yet.

4. **Tested and ruled out** (before finding the tracker bug): lowering
   `detection_interval` (10->4) measurably improved ball presence
   (74.9%->82.7% of frames) but did NOT show a clear improvement in
   frame-to-frame pose smoothness when lowering `cluster_alpha`
   (0.08->0.025-0.03) - the lookahead/centered-smoothing already
   absorbs most of that regardless. Don't re-reach for cluster_alpha
   tuning as the fix for "jumpy" AI tracking without new evidence.

## OPEN / WAITING ON THE USER

- User is watching the earlier coverage-clamp-morning test videos
  (`_tune_baseline`/`_moderate`/`_aggressive`/`_baseline_trackerfix`,
  in `TEST VIDEO/`) and will report back which framing/smoothness
  feels best.
- Whether to extract the stray field-marker frames into an actual
  training batch (`project_yolo26n_training_pipeline`) - not started,
  ask before doing so given the established "balance for difficulty,
  don't just add data" lesson from the last training regression.
- The morning's coverage-clamp black-wedge item (see the entry below)
  is separately still open - the AI pitch-limit feature is the shipped
  mitigation for it, not a fix to the coverage clamp itself.

## Previous entry - 2026-09-02 (TGR_PC), morning: video-not-centered fixed; coverage-clamp corner-leak fix attempted, broke panning, REVERTED

## REPO STATE AT END OF SESSION (2026-09-02)

- Branch `main`, working tree clean except the pre-existing untracked
  `scripts/match-logger/Match Logger.html.txt` (not written this
  session, left alone as always). Four commits made and pushed to
  `github/main` today: `8eb872f5` (centering fix, KEPT), `910886d5`
  (coverage-clamp fix, BROKEN), `c22200af` (docs, superseded by this
  entry), `7ce4aec4` (revert of `910886d5`). `git fsck --full` clean
  before every push.
- reco-gui release+debug **rebuilt after the revert** (both
  `--features tensorrt`) - the binaries on disk right now do NOT
  contain the reverted coverage-clamp change, only the centering fix.
- Picked up the 2026-08-30 coverage-clamp/corner-leak todo list (see
  `project_coverage_clamp_corner_leak` memory for full detail,
  including the root-cause writeup on why the reverted fix broke
  panning - read that before attempting #2 again).

## DONE THIS SESSION (2026-09-02)

1. **Video doesn't start centered - FIXED, kept.** `run_loop.rs`'s
   `centered_smooth` window was lopsided at frame 0 (empty past,
   `post_smooth_half` already-migrated future poses from the panner
   warm-up), biasing the first rendered frames away from center. Fixed
   by capping the future side to however many past samples exist so
   far (`ahead_n = past_poses.len().min(post_smooth_half)`); frame 0
   now renders unsmoothed/centered, growing symmetrically from there.
   Tail/EOF behavior unchanged (already-accepted asymmetry). Commit
   `8eb872f5`. **Not yet user-confirmed in a real export.**

2. **Black-wedge corner leak - fix attempted, REVERTED after user
   testing found a real regression.** `coverage.rs`'s `clamp_with_margins`
   validated yaw only at the viewport's center pitch, not its full
   height. Added `yaw_range_over_span` to intersect yaw coverage across
   the viewport's actual vertical extent - looked good against the
   `verify_coverage_corner_gap.rs` repro script alone (leaks 3.5% ->
   1.1%, worst-case overshoot 137deg -> 78deg), so it was built and
   shipped for testing. **User found it broke real GUI panning
   (left/right locked, only up/down worked) and froze export framing
   near center.** Root cause: `clamp_with_margins`'s axis-aligned-box
   viewport approximation already over-estimates the true rendered
   frustum footprint (an accepted pre-existing approximation); requiring
   the WHOLE box to stay inside coverage (not just its center) is
   correct against the box but far stricter than reality, collapsing
   the valid yaw range for large stretches of completely normal camera
   positions, not just genuine edge cases. Reverted (`7ce4aec4`), GUI
   rebuilt from the reverted state. **#2 is open again** - full
   root-cause writeup and "what not to try next" in the
   `project_coverage_clamp_corner_leak` memory.

Both changes were build/clippy/fmt-clean and passed `cargo test -p
reco-core` (244/246; 2 pre-existing CUDA-device failures unrelated, no
GPU context in that shell) before shipping - the corner-leak regression
only showed up under real interactive use, which the verify script
doesn't test for (it checks for leaks, not over-restriction). Lesson:
for `safe_clamp`-adjacent changes, real panning/export testing is
required before considering a fix done, unit tests and the diagnostic
script are not sufficient on their own.

## OPEN / WAITING ON THE USER

- Real-footage confirmation of fix #1 (centering) above.
- Fix #2 (black wedge) is open again - do not resume with a smaller
  version of the reverted box-intersection approach without addressing
  why it over-constrained normal poses; see the memory's "lesson for
  the next attempt".
- The wide-shot auto-gamma A/B test from the 2026-09-01 entry below -
  still not done, still first-priority once picked back up.
- Everything else from the 2026-09-01 entry's OPEN list still applies.

## Previous entry - 2026-09-01 (TGR_PC): color-match band sky-exclusion fix + dynamic auto-gamma; big backlog of prior-session work finally committed

## Status update (later same day, from the 2026-08-29 OPEN list)

Four of the 2026-08-29 entry's "OPEN / WAITING ON THE USER" items,
confirmed by the user - **no longer open, don't re-list these**:
- Forum post about the YOLO26s model - posted.
- Label Studio API token (pasted in chat 2026-08-29) - rotated.
- Lookahead 0.8s / wider FOV / export quality settings - user says
  resolved for now (self-service GUI tuning, not a code task).
- Label Studio project 24 review (60 tasks, 28 blind-spot frames) -
  **still in progress**, user is actively working through it - keep
  this one on the list, just not blocked/stalled.

## FIRST THING NEXT SESSION - user explicitly asked to be reminded

**Do a proper wide-shot on/off A/B of the new "Auto gamma (dynamic)"
checkbox** (Color Mapping card, MANUAL GAMMA section). This session's own
CLI tests (fixed pose, full panorama visible) showed a big win (17-18%
seam gap down to 5.0%/2.3%), but when the user exported two real matches
with AI tracking on to compare, the difference looked small. Pulled the
exact same frame from both real exports and found a real but small
difference (~4-4.6/255) - almost certainly because that AI-tracked moment
was a tight zoomed crop on players, not a wide shot showing the seam, so
it wasn't a fair test. Need either: a real export moment where the AI
camera is wide (dead ball/stoppage), or the GUI preview at a wide FOV with
"Show seam line" on, toggling the checkbox live. Full detail:
`project_color_match_sky_exclusion_autogamma` memory.

## REPO STATE AT END OF SESSION (2026-09-01)

- Branch `main`. **10 commits made and pushed to `github/main` today**,
  clearing out a large multi-session backlog of uncommitted work plus
  today's own new fixes:
  - `5c179e86` `7a98c209` `8037d991` `bb12ab6a` `19847ebc` `d9776140`
    `190da5de` `6782312b` - the 2026-08-28/29 backlog (Label Studio
    model_version fix, Match Logger clock, coverage-clamp corner-leak
    diagnostic, IMU rounds 3-6 docs, export-quality controls + per-export
    log, detection-confidence slider, buffered-path color-match fix, app
    icon) - see each commit message for detail, nothing new to add here.
  - `de59b4a7` **color-match band no longer averages in the sky** - see
    `project_color_match_sky_exclusion_autogamma` memory and
    `crates/reco-core/FRICTION.md` for the full root-cause writeup.
  - `3c35e0ec` **dynamic auto-gamma + "Remeasure now" actually snaps now**
    - same memory.
- `git fsck --full` clean before every push today.
- Working tree clean except one pre-existing untracked stray file
  (`scripts/match-logger/Match Logger.html.txt`, not written this
  session, left alone as always).
- Binaries: reco-gui release + debug **rebuilt 2026-09-01 ~13:33/13:38**,
  both `--features tensorrt`, carrying everything through the auto-gamma
  fix (the runaway-bug fix landed *after* an earlier rebuild today, so
  that earlier build was stale - rebuilt again after; the current
  binaries are the correct, final ones). Release `reco-cli` also rebuilt.

## DONE THIS SESSION (2026-09-01)

1. **Cleared the entire multi-session uncommitted backlog** (see commit
   list above) - this was most of the session's first half. Nothing
   further to say here; see each commit's own message.

2. **Color-match band sky-exclusion fix + dynamic auto-gamma** - the
   session's main investigation, starting from a user-reported "extreme"
   seam at raw-file frame 31850. Full writeup in the
   `project_color_match_sky_exclusion_autogamma` memory and
   `crates/reco-core/FRICTION.md` (two new entries). Short version:
   - Root cause: the seam-band measurement sampled the sky half of each
     camera's frame too, which on this tilted rig can move in the
     *opposite* direction from the ground (a passing storm cloud gave one
     camera a darker sky and brighter grass, the other the reverse),
     letting a real ground-level mismatch cancel against sky noise in the
     mean. Fixed: ground-half-only sampling.
   - This calibration's own `color_match_band_width` was also a stale
     0.874 (default 0.15); reset, then empirically retuned to 0.5 - wider
     than default, but safe now that sky is excluded, and needed because
     narrow bands near the seam edge fragment badly on this lens (KB4
     corner-FOV-coverage gap).
   - Found "Remeasure now" never actually delivered a fresh measurement
     (just a ~15% EMA nudge) - fixed to snap.
   - Built a new "Auto gamma (dynamic)" toggle so gamma correction
     tracks lighting changes through a match instead of staying at
     whatever a user tuned it to for one moment - found and fixed a real
     divergent-feedback bug in the first working version before shipping.
   - **Verified strongly via CLI wide-shot tests** (17-18% seam gap down
     to 5.0%/2.3%, both directions correct, no reversal). **NOT yet
     confirmed via a fair real-export test** - see "FIRST THING NEXT
     SESSION" above.

## OPEN / WAITING ON THE USER

- The wide-shot auto-gamma A/B, see top of this file.
- Everything from the 2026-08-30 coverage-clamp/corner-leak todo list is
  **still fully untouched**: the black-wedge corner leak (root-caused,
  fix not designed), video-not-centered (root-caused, fix not designed),
  and a new third item raised this session but not investigated at all -
  image stabilization against wind-shake (the whole physical camera
  moving in hard wind - a different class of problem, needs gyro- or
  feature-based stabilization, not an autocam/panner fix). See
  `project_coverage_clamp_corner_leak` memory.
- Background research this session, not part of the delivered fix:
  real per-frame ISO/shutter/white-balance can be pulled straight out of
  a DJI Osmo Action 4's own metadata track (no extra dependency needed).
  Found a real but partial correlation (r=0.71) with the actual pixel
  mismatch - breaks down at scene-content-driven moments (different sky
  in each lens), which is exactly why pixel measurement (now fixed)
  remains the right primary signal. Scripts are session-scratchpad only,
  not committed - not needed again unless this line of investigation
  gets revisited specifically.

## Previous entry - 2026-08-29 (TGR_PC): Roll/Skew calibration sliders; model published to Hugging Face; round-5 training frames selected and uploaded to Label Studio

## REPO STATE AT END OF SESSION (2026-08-29)

- Branch `main`. **Four commits made and pushed to `github/main` today**
  (note: `origin` is the upstream project and is not writable - push to
  the `github` remote, which is what `main` tracks):
  - `5cc64c29` reco-gui: split the per-camera Rotation slider into Roll and Skew
  - `5dd41eb2` docs: record the published Hugging Face model repo
  - `5c85c585` docs: detection runs per camera, not on a stitched panorama
  - `604de7ec` scripts: select training frames by model failure, not by volume
- `git fsck --full` clean before pushing (only dangling blobs).
- **Everything listed in the 2026-08-28 entry below is still
  UNCOMMITTED** and was deliberately left alone this session: the
  `pipeline.rs` / `vram_pool.rs` / `executor.rs` / `frame_processing.rs`
  changes, the reco-gui build files, `FRICTION.md`, the Match Logger
  HTML, the four IMU example programs and the two `assets/` dirs.
- Binaries: reco-gui release + debug rebuilt 2026-08-29 (13:28 / 13:33),
  both `--features tensorrt`, carrying the Roll/Skew sliders.

## DONE THIS SESSION (2026-08-29)

1. **Roll/Skew sliders (committed, user-confirmed working).** The single
   per-camera "Rotation" slider drove `x_rx` (right) / `z_rz` (left),
   which rotate the plane around its own horizontal axis - under the
   oblique L-shape view that reads as a skew, so it could not correct a
   camera physically rolled in the rig. Each camera now has `Roll`
   (`x_rz` right / `z_rx` left, the rotation around the plane normal)
   and `Skew` (the previous behaviour, unchanged). The "ROTATION FIT"
   group is renamed "SKEW FIT" because its checkboxes force-fit the skew
   angles. `Topology`'s doc comments had `z_rx`/`z_rz` labelled the
   wrong way round versus the matrices in `render/scene.rs`; corrected.
   - **Not done, deliberately:** the left plane's rotation pivot is
     applied along world X, which for that plane is its *normal* rather
     than its width axis, so `z_rz` also shifts vertically and `z_rx`
     pivots at the plane centre instead of the seam edge (the right
     camera pivots correctly on the seam). Fixing it would move every
     existing calibration with a non-zero `z_rx`, which auto-calibrate
     fills in by default. Ask the user before touching it.

2. **Model published to Hugging Face.** <https://huggingface.co/Dura-S/reco-yolo26s-football>,
   public, weights + model card only. The user's HF account is `Dura-S`.
   `hf.exe` now lives in the user's roaming Python Scripts dir, which was
   added to the user PATH this session (backup of the old PATH:
   `D:\CLAUDE\user_path_backup_2026-08-29_1448.txt`). Re-uploading is
   `hf upload Dura-S/reco-yolo26s-football . --repo-type model`; its
   "0.00B transferred" output is a wrong progress bar, verify with
   `HfApi().model_info(..., files_metadata=True)` instead.

3. **Corrected a wrong claim that had already been published.** The
   model card and `YOLO26_Training.md` both said detection runs on a
   stitched panorama. It does not: `session/detection_dispatch.rs`
   dispatches `CameraId::Left` and `CameraId::Right` separately, each on
   that camera's own raw 3840x2880 frame, before any stitching. Both
   texts fixed, HF re-uploaded and verified live. **Training data must be
   raw per-camera frames** - not stitched panoramas, and certainly not
   the exported virtual-camera video.

4. **Round-5 training frames: 60 selected and uploaded.** Two new
   committed scripts, `scripts/pick_training_frames.py` and
   `scripts/upload_to_labelstudio.py`, replace the old ad-hoc drivers.
   Label Studio project 24 ("Ai Learning - yolo26s") went from 100 to
   160 tasks, each with the current model's boxes as editable
   predictions. Half the batch is a `blind_spot` group - frames where
   the model finds **no ball at all** while 15-21 players are on the
   pitch - which no earlier round contained. Full rationale and the
   exact commands are in `YOLO26_Training.md`'s round-5 section. The 720
   extracted candidates and all their detections are cached under
   `D:\CLAUDE\frame_pick\`, so a different selection costs no GPU time.

5. **Export performance investigated, not solved.** Measured on two
   identical full-match exports (same range, same cuts, 110577 frames
   each): 0.8s lookahead gave 27.86 fps, 1.2s gave 27.39 fps. More
   lookahead does nothing, so detection is **not** what holds the loop
   back - the async detect thread was already on, TensorRT confirmed
   active, detection interval already 3. Nothing is saturated (GPU
   63-69%, CPU 12-14%, disk 22%), which is the signature of a serialised
   pipeline. Two hypotheses were tested and refuted (the D: HDD, and the
   NV12 readback path - the latter is already double-buffered). A faster
   GPU alone will not reach 50 fps: with GPU work at ~63% of wall time
   the ceiling is ~81 fps and a 2.5x card lands near 48. **Next step is a
   `--features profiling` run** (`--max-frames 300`, then read
   `reco-trace.json`); the user declined it twice this session, so ask
   first.

## OPEN / WAITING ON THE USER

- Review the 60 new tasks in Label Studio project 24. The 28 blind-spot
  frames have **no ball in their suggestions on purpose**. The teacher
  over-predicts `referee`, and at the 0.05 confidence floor some
  suggested balls are noise - removing a wrong ball matters as much as
  adding a missed one, because a small ball left unlabelled teaches the
  model that there is no ball there.
- Forum post announcing the model is written but **not posted**:
  `D:\CLAUDE\forum_post_yolo26s_football.md` (128 words). Plan agreed: a
  new topic in General on <https://forum.reco.cam> (the README's
  `forum.reco-project.org` 301-redirects there), tag `ball-tracking`,
  then short cross-links from topics 134, 114 and 104.
- The user's Label Studio API token was pasted into that session's chat;
  they were advised to rotate it.
- Lookahead should go back to 0.8s in the GUI - 1.2s costs 540 MB VRAM
  and buys nothing.

## Previous entry - 2026-08-28 (TGR_PC): auto color match fixed on the lookahead export path; IMU investigation rounds 3-6 (measured, then refuted); Match Logger clock; several GUI changes

## REPO STATE AT END OF SESSION (2026-08-28, before a PC restart)

- Branch `main`, **265 unpushed commits** (normal for this working repo).
- **Everything below is UNCOMMITTED in the working tree.** `git fsck
  --full` clean (only dangling blobs, which are harmless) - checked
  before the restart per the known loose-object corruption risk on this
  machine.
- Modified: `Cargo.lock`, `SESSION_HANDOFF.md`,
  `crates/reco-calibrate/FRICTION.md`,
  `crates/reco-core/src/render/pipeline.rs`,
  `crates/reco-core/src/session/frame_processing.rs`,
  `crates/reco-core/src/session/vram_pool.rs`,
  `crates/reco-core/src/stitch/executor.rs`,
  `crates/reco-core/src/interop/d3d11.rs`,
  `crates/reco-io/src/{smart_source.rs,zero_copy.rs}`,
  `crates/reco-gui/{Cargo.toml,build.rs,src/main.rs,ui/main.slint}`,
  `scripts/match-logger/Match Logger.html`.
- New/untracked: four `crates/reco-calibrate/examples/*.rs` IMU
  diagnostics, `crates/reco-gui/assets/` + `crates/reco-gui/ui/assets/`
  (app icon), `scripts/match-logger/Match Logger.html.txt` (a stray
  copy that was already there, not written this session).
- Binaries: **release rebuilt 2026-08-29 08:51** (`reco-gui.exe`) and
  **08:46** (`reco.exe`), both `--features tensorrt`, carrying items 7,
  8 and 9 below plus the "Advanced panner expanded" change. The **debug
  build (08-28 15:26) is stale** - it predates all of it. Rebuild it
  before testing anything in debug.

## IN THE TREE, NOT YET IN A BUILT BINARY

Nothing - the 08-29 08:51 release build covers the whole tree. (The 15:53
build did not; the "Advanced panner" section now starting expanded,
`ui/main.slint`'s `panner-advanced`, landed after it. User's request:
it holds the settings actually being tuned per match, so collapsing it
by default just costs a click every session.)

## DONE THIS SESSION (2026-08-28)

### 1. Auto color match was dead in exports again - root-caused and fixed

**User report:** preview showed correct color matching, the exported
file did not. Same symptom as the original hardware-decode bug that was
fixed 2026-08-27, but that fix was verified and is genuinely in place.

**Root cause:** the 08-27 fix put `gather_band_samples` in
`StitchPipeline::render_imported_views` (the *immediate* D3D11VA path).
Windows' `D3d11Resident` handling in
`session/frame_processing.rs` has **two** branches: immediate, and a
buffered one taken whenever lookahead/AI tracking is on
(`current_vram_slot` set, `run_loop.rs`). The buffered branch renders
via `render_with_bind_groups` from the VRAM pool - bind groups cannot
be sampled, so the gather never ran and the correction stayed identity.
The preview uses the immediate path, hence the asymmetry the user saw.
Confirmed against their export log (`Lookahead bit depth: Reduced8Bit`,
`AI tracking: active`).

**Fix:**
- `VramPool` now also keeps per-slot plane views (built once at pool
  construction, not per frame) + a `plane_views()` accessor.
- New `StitchPipeline::render_with_bind_groups_measured()` - gathers,
  then renders. The plain `render_with_bind_groups` is kept but its doc
  comment now states explicitly that it does not measure.
- Both buffered paths (Windows `D3d11Resident`, Linux
  `render_gpu_resident`) switched to the measuring variant. The Linux
  one had the identical bug and was fixed in the same pass, untested
  (no Linux here).

**Test:** `the_buffered_bind_group_path_measures_the_seam_band`
(`stitch/executor.rs`, `#[cfg(feature = "gpu")]`) asserts *both* halves:
the plain form stays identity, the measured form produces a real
correction. **Mutation-checked** - the gather was temporarily removed
and the test failed with `got [0.0, 0.0, 0.0]`, then restored, so it is
not vacuous.

**Known gap, deliberately not covered:** the test exercises the pipeline
methods, not `frame_processing`'s choice between them - that would need
a full session with a VRAM pool. The doc comments on both methods are
the guard against a third recurrence.

**Verified:** user confirmed color match now works in a debug export.
`fmt`/`clippy -D warnings` clean; reco-core 240/242 (the usual 2 CUDA
failures, no CUDA runtime here); reco-gui 78/78.

**Follow-up the user should try:** they reported ~20fps on that debug
export. Debug is 5-10x slower than release, so that number says little -
but their calibration also has `color_match_interval_frames = 1`
(default 15), so the measurement now runs every single frame where
before it never ran at all. Suggested: set Measure interval to 15 in
Color Mapping and re-compare on the release build. Not yet done.

### 2. GUI changes

- **App icon**: `icon.jpeg` converted to `ui/assets/icon.png` (Slint
  window icon) + `assets/icon.ico` (multi-size 16-256, embedded in the
  .exe via a new `winresource` build-dependency in `build.rs`).
  Verified by extracting the icon back out of both built exes.
- **Transport buttons**: tooltips removed from all six; the play button
  no longer uses the `emphasized` (blue, wider) style - `emphasized` was
  deleted from `TransportButton` entirely. The play glyph is now a drawn
  `Path` triangle instead of "\u{25B6}", because Windows font
  substitution rendered that character as a *color* emoji regardless of
  the Text color, which is what made it look blue.
- **FOV Tight slider max 40 -> 50** (user request).
- **FOV persistence**: already worked (app-level `autocam_defaults` in
  `gui.json`, and `do_save_calibration` writes current FOV into the
  calibration). The real gap was that changing FOV did **not** mark the
  calibration dirty, so the user got no hint that Save Calibration was
  needed - and on next load the calibration's older values won, looking
  like "settings are not remembered". Now flags dirty, but only when the
  snapshot actually differs from the calibration's stored values (an
  unconditional flag would mark every freshly-loaded calibration dirty,
  since `apply_autocam_defaults` fires the same callback).
- **Calibration progress popup**: modal showing step, detail, a progress
  bar (only `FeatureMatching` reports a real fraction), a scrolling log,
  and a working **Cancel** (the `interrupted` flag existed but was never
  wired to any UI). Cancelling no longer tears down the loaded session
  the way a real calibration failure does.

### 3. Calibration sampling: anchor frame + sample window

Replaced the "Skip end" slider with:
- **Anchor frame** (text field + "Use current" button) - an exact frame
  number to centre sampling on; empty falls back to the playhead.
- **Sample window (s)** - keeps all sampled frames within that many
  seconds *centred* on the anchor. 0 = off (sample from the anchor
  onward, the old behaviour).

**Bug found and fixed in this same feature:** the first version computed
the window against the *preview timeline's* duration, which spans all
chained segments, while `calibrate_videos` only ever reads the **first**
segment. That produced a `skip_end_secs` larger than the actual file,
which `select_frame_indices` saturates to an empty range - silently
collapsing a requested 6 frames to 1 (`extracting 1 frames` in the log,
which is how it was caught). Now probes the real single-file duration
via `reco_io::ffmpeg::calibration_io::probe_video`, and warns loudly if
the anchor is past that file's end.

### 4. New calibration auto-saves to the match folder, not `LEFT/`

`suggested_calibration_path()`: when the left video sits directly inside
a folder named `left` (case-insensitive), the auto-saved calibration now
goes one level up - the match folder that also holds `LEFT/` and
`RIGHT/` - instead of inside `LEFT/`. An already-known location (match
folder pick, or a previously loaded calibration) always wins over this
derivation. 4 unit tests.

### 5. Match Logger: clock now pauses

`scripts/match-logger/Match Logger.html`'s header clock was
`Date.now() - kickoff`, so it ran straight through a pause and stayed
permanently ahead afterward by the total paused time. New
`liveElapsedMs()` walks the event log with proper per-period pause
bookkeeping - mirrors `reco-gui`'s `scoreboard_import::state_at` so the
phone app and the burned-in scoreboard agree. "Volgende periode" needed
no special handling (`btnPeriodAction` already auto-closes an open
pause). Tested by replaying three constructed event sequences in Node
(freeze during pause / resume excludes pause / period advance) - all
correct. No build needed, it is a standalone HTML file.

### 6. Calibration target images (in `D:\CLAUDE\`, not in the repo)

- `akaze_calibration_checkerboard_5120x1400_300dpi.png` - plain
  checkerboard, 200px squares.
- `akaze_calibration_coded_dots_grid_5120x1400_300dpi.png` - current
  one: black dots on white (max grayscale contrast - AKAZE here is
  grayscale-only, so colour helps it not at all), asymmetric row offsets
  and cycling dot sizes to break the repeating-pattern ambiguity, a
  black grid for extra corner features, plus per-row colour rings purely
  as a human reference when comparing left/right frames.

### 7. Export died at a cut-window boundary: `GetData timed out (>1M polls)`

**User report (release build, 16:15 export of the full match, 4 keep
windows):** `Export failed - session: zero-copy: staging copy failed:
GetData timed out (>1M polls)`. Note the GUI does *not* log export
failures (only a toast, see `main.rs`'s `ExportOutcome::Failed`), so the
run has to be reconstructed from the surrounding log lines.

**What the log shows** (`target/release/reco-gui.log`):
- Window 1 (78.59-1004.63s) rendered fine: 27753 frames in ~843s,
  ~33 fps.
- 16:29:47 `Cut range: seeking to 1268.34s`, decoders respawned,
  lookahead pre-fill of 29 frames succeeded, 16:29:50 `starting render`.
- ~196 more frames written (6.5s of video), then 82 seconds of nothing,
  then the timeout. `Encoder finished: 27949 frames written` is the
  teardown, not a success.

**Not the cause:** the auto color match change from earlier this session.
The same binary did 27753 frames at 33 fps in window 1, the TEST4 export
completed, and the calibration is on `color_match_interval_frames: 15`.

**Also not the cause (a wrong turn, user caught it):** the PAUZE
transition. It sits in the session's dedicated GPU transition slot
(`set_overlay_transition`), so a fade only rewrites an opacity uniform;
and because it is not a layer, the scoreboard stays the *only* layer and
`LayeredOverlaySource`'s CPU composite never runs. That was fixed
2026-08-26 - do not re-blame it.

**Fixed (both uncommitted, in the tree):**
1. `interop/d3d11.rs` `stage_frame`: the wait was a *poll count*
   (1M spins), which cannot tell a fast copy from a stalled one - here
   1M contended `GetData` calls took ~82s. Now a wall-clock deadline
   (`STAGING_COPY_TIMEOUT`, 30s) with a short spin phase
   (`STAGING_COPY_SPIN_POLLS`, 10k) then 200us sleeps, so a slow frame
   stays a slow frame instead of killing an hour-long export, and the
   poll loop stops taking the device's multithread lock from the very
   threads it waits on. A copy slower than 250ms now logs a warning -
   that is the diagnostic that was missing this time.
2. `smart_source.rs` `seek_to_secs` spawned the new decode pair
   *before* dropping the old one, so two full 4K D3D11VA decode pools
   (two texture arrays each) overlapped on one device at exactly the
   moment the new decoders allocate - on an 8GB card that reported
   ~2.9GB free at export start. Teardown was also only *signalled*, never
   waited on. `spawn_d3d11_decode_pair` now returns a
   `D3d11DecodePipeline` (receiver + the three join handles) and the new
   `WindowsZeroCopyState::shutdown_decode` drops the receiver and joins,
   used by both `seek_to_secs` and `Drop`.

**Verified:** fmt + clippy clean on reco-core/reco-io; reco-core 240/242
(the usual 2 CUDA failures), reco-io 56/56. Release reco-gui rebuilt
17:20 with `--features tensorrt`. **Not yet re-tested against a real
export** - the failing run was the full match with 3 cut ranges, so that
is the test.

**Still unproven:** the VRAM-overlap story is the best-fitting mechanism,
not a confirmed one. Fix 1 makes the next occurrence survivable and the
new warning will show whether copies are merely slow or genuinely stuck.

### 8. Audio now follows the cut ranges across a chained (multi-file) source

**The gap:** every export of a segmented DJI recording with cut ranges
logged `Cut ranges requested with a chained (multi-file) audio source -
not supported yet, audio will NOT exclude the cut ranges`. Video jumped
the cuts, audio read straight through, so sound drifted further out of
sync after every cut. On the 65-minute export with 3 cuts that is ~13
minutes of drift by the end.

**Why it was hard:** `audio_cut_windows` and `audio_start_time` are on
the *concatenated* timeline (what the concat demuxer gives the video
side), while each segment file's own packet timestamps restart near
zero. The old code compared one against the other, which is only correct
inside the first segment.

**Fix** (`ffmpeg/encoder.rs`): a virtual timeline.
- `AudioSegment { path, start_secs }` + `probe_audio_segments` lay the
  chain out from the per-file container durations - the same durations
  `adapters` sums for the video side, so both clocks agree.
- `segment_index_for` resolves any virtual position to its segment.
- `AudioPassthrough` carries `segments` / `segment_index` /
  `segment_start_pts`; every packet's position is `raw +
  segment_start_pts`, so all three comparisons (start trim, window end,
  post-seek discard) happen in the same space.
- A cut jump now opens the segment the target lives in (`open_segment`)
  and seeks within it on that file's own clock.
- `first_segment` is gone, replaced by `start_segment_index` +
  `start_trim_done`: setup opens the segment the start time falls in
  rather than always the first, which also fixes a start time past
  segment 1 landing on the wrong content (the old code read and
  discarded all of file 1, then spliced file 2 in from its beginning).
- Only remaining fallback: a chain whose durations can't be probed.

**Verified end to end, not just compiled.** Release CLI, real footage,
`--start-time 990 --cut-range 1004.6:1268.3 --max-frames 900` over two
chained segments - the jump target (1268.8s virtual) is in segment 2,
the exact case that was broken:
- Log: `seeking to 45.718s in segment 1 (1268.801s on the chained
  timeline)`. 1268.801 - 1223.083 = 45.718, correct.
- Cross-correlated the output's audio against the two source spans
  (`D:\CLAUDE\audio_cut_test`): window 1 r=0.913, window 2 r=0.971,
  both aligned within 7ms of ideal. Against the *wrong* span (what the
  old passthrough produced) r=0.024 / 0.078, i.e. noise.
- Control run without `--cut-range`: r=0.973 at the same 6ms offset, so
  the plain path is unchanged.
- Unit tests for `segment_index_for` and `probe_audio_segments`.
- `matroska_reader_sees_partial_writes` fails, but it fails identically
  with these two files reverted - pre-existing, unrelated.

Both release binaries carry the fix: `reco.exe` (19:57) and
`reco-gui.exe` (20:08, `--features tensorrt`). **Not yet exercised
through the GUI** - the natural test is the same full-match export with
its 3 cut ranges, listening at each cut.

### 9. Goal highlights export (2026-08-29)

User asked whether goal highlights for YouTube were possible. They are,
and the video engine needed **no** new capability: a highlight reel is an
ordinary cut-range export whose cuts happen to be everything between the
clips. Built, verified on real footage, not yet tried through the GUI.

**What was already there** (worth knowing before touching this again):
- `scoreboard_import.rs` already parses `EventKind::Goal` with a
  `SyncAnchor` giving video seconds, and `derived_cut_ranges` was an
  exact template for deriving windows from events.
- The decoder *seeks* past excluded ranges, so a reel decodes only its
  clips - the 2026-08-28 test reel took 26s wall clock, against 65
  minutes for the full match.
- `cut_range::output_frame_to_source_secs` already maps output frames
  back to source time for the scoreboard replay, so the clock shows real
  match time inside a reel with no extra work.
- Audio splicing across cuts *and* across chained segment files works as
  of item 8 above. Without that this feature would have shipped broken.

**Not viable: automatic goal detection.** `reco-autocam/src/goal_events.rs`
exists but has no call site, and its own doc explains why it is only a
primitive - a ball inside the goal-mouth polygon is not proof of a goal
(corner deliveries and shots over the bar pass through the same region),
and confirming needs kickoff detection that does not exist. The Match
Logger route is the one that works.

**Built:**
- `cut_range::merge_windows` + `gaps_between` - overlapping goal windows
  fold into one clip, and the interior gaps become the cut ranges. The
  outer edges are start/end times, never cuts, so nothing is decoded to
  be thrown away.
- `scoreboard_import::derived_goal_windows` - one window per `goal`
  event, asymmetric margins (15s lead / 10s trail by default) because
  the operator taps the button *after* the ball is in.
- reco-gui: "Highlights only (goals)" checkbox + two margin fields,
  mutually exclusive with auto-cut. Sets export start/end + cut ranges,
  and renames the output to `..._highlights.mp4` so a two-minute reel
  cannot land on top of the multi-gigabyte full export.
- reco-cli: `--keep-range START:END`, repeatable, the inverse of
  `--cut-range`. Conflicts with `--cut-range`/`--start-time`/`--end-time`
  via clap.
- `ScoreboardSettings` persists the two margins but **deliberately not
  the toggle** - restoring "highlights on" would silently turn the next
  session's ordinary export into a two-minute reel.

**Verified on real footage** (release CLI, two clips spanning a segment
boundary, `--keep-range 990:1010 --keep-range 1268:1288
--pause-overlay`): clip 1 cross-correlates at r=0.913 exactly on time,
clip 2 at r=1.000 within 16ms (AAC packet granularity), the PAUZE gap is
exactly 0 RMS for exactly 4s, and audio and video both run 43.98s. The
identical scenario written the old way (`--start-time 990 --end-time 1288
--cut-range 1010:1268`) produces the same 1318 frames, so `--keep-range`
is a pure convenience over the existing mechanism.

**Two measurement traps, recorded so the next session does not fall in:**
extracting the audio to WAV without `aresample=async=1` silently drops
the PAUZE gap and makes everything after it look 4s early; and a
reference span cut with `-ss` must be compared against the *right*
expected lag, which changes with the cut boundary.

**Not done:** YouTube chapters remapped from match time to reel time
(Match Logger already exports `youtubeChaptersText`, but those timestamps
are match time - `output_frame_to_source_secs` inverted is what maps
them). And upload is manual: the 2026-07-16 quota research called the
shared 10,000 units/day the showstopper, which is true for distributing
the app but not for one club on its own Google Cloud project - 6 uploads
a day is plenty there.

## OPEN TASKS - do these next

### 0. IMU orientation: investigation PAUSED after 6 rounds - needs one more test recording

Full detail in `crates/reco-calibrate/FRICTION.md` point 26 (rounds
1-6). Short version of where 2026-08-28 left it:

- **Round 3 (measured):** user recorded a genuinely level, stationary
  rig (`D:\VOETBAL_VIDEO\TEST VIDEO\Waterpas Test met raster\`). The
  real `rig_tilt` reads **-129.93 deg (left) / 172.21 deg (right)**
  where both should be ~0. Stable to 2 decimals across three different
  `skip_secs`. Both cameras are wrong *independently*, which rules out
  per-camera mount asymmetry as the whole story.
- **Round 4:** solved a per-camera correction against that (conjugate
  for left, a 180-deg composition for right) - drove the level test to
  ~-3 / ~-8 deg.
- **Round 5:** user supplied the rig CAD - the cameras are **84 deg
  apart** (42 deg each from centre), which is modelled *nowhere* in the
  IMU math. A rig pitch of `theta` therefore reads as
  `theta*cos(42)` pitch **and** `theta*sin(42)` roll (opposite sign L/R)
  per camera.
- **Round 6:** cross-validated round 4 against real match footage using
  those splay-corrected expectations - **refuted**. Measured 61.31 /
  -162.56 deg pitch against an expected ~14.9. Round 4 had overfitted to
  a single pose.
- **Sign convention (user-corrected):** the rig points *downward* in
  deployment, so its real tilt is **-20 deg, not +20**.

**Guess-and-check is exhausted (~40 candidates over 6 rounds) - do not
continue enumerating.** The problem has at least four entangled
multiplicative unknowns (sandwich order, telemetry-parser's own
unconditional 180-deg DJI transform, left's physical 180-deg mount, the
42-deg splay) and one pose cannot constrain them.

**The actual next step:** record the rig stationary at its real
**~-20 deg** deployment pose, spirit-level-verified, as a companion to
the existing flat footage. Two known, different poses give enough
independent equations to solve the rotation chain outright and verify
against both at once. User agreed to record this "another time".

Four throwaway diagnostics are in the tree (untracked) and reusable:
`crates/reco-calibrate/examples/{measure_level_imu_baseline,
check_imu_mount_rotation, solve_imu_correction,
verify_splay_corrected_imu}.rs`.

### 0b. Deferred GUI request: default Rig Tilt

User asked to auto-fill reco-gui's manual "Rig Tilt" slider
(`ui/main.slint`, `rig-tilt`, always starts at 0.0, no persistence).
Two options were offered - a persisted "Default Rig Tilt" preference vs.
remembering the last-used value (mirroring the autocam app-level
persist) - and the user deferred with "doen we later wel". **Correct
default value is -20** (see the sign note above).

### 6. Test the IMU capability fix on real DJI footage (just shipped, unverified on real recordings)

`crates/reco-calibrate/FRICTION.md`'s point 25 has the full writeup.
Short version: `imu_sync()`'s all-or-nothing gate (skip everything
whenever the LEFT camera alone lacked native gyro) used to also kill
orientation seeding for DJI rigs, even though DJI's quaternions could
supply it - a dormant `rig_tilt_from_quaternions` fallback existed but
was unreachable, and didn't cover the two-camera differential seed
anyway. Fixed: `gravity_vector()` itself now falls back to quaternions
(fixing a real, separate bug in the process - the old fallback ignored
`skip_start_secs` entirely), and the gate checks both cameras'
capability, not just the left one's `has_native_gyro`.

New: a status line under the calibration controls in reco-gui
("IMU: DJI Osmo Action 4 / DJI Osmo Action 4 - no gyro (sync uses audio
instead), orientation from quaternions"), and the same as a console line
in reco-cli.

**What to actually check on real footage, next session:**
1. Run Re-calibrate on the DJI pair, confirm the new IMU status line
   shows the expected "no gyro / orientation from quaternions" text.
2. Check the log for `differential orientation: roll=... pitch=...
   tilt=...`, `rig tilt: ...`, and `rig roll: left=... right=... avg=...`
   lines - these previously never printed at all for DJI (the function
   returned before reaching them); confirm they appear now, and that the
   values are physically plausible for this rig (a few degrees, not
   90-degree nonsense or NaN) - only synthetic quaternions were used in
   this session's own tests, real footage has never exercised this path.
3. Time the Re-calibrate run. The full telemetry parse (~76s per camera
   per its own doc comment) now runs for DJI where it never did before -
   expect roughly 70-150s longer than a pre-fix run. Confirm that's
   acceptable, not a surprise.
4. Turn on the "IMU seeds" checkbox (off by default) and compare a
   calibration run with it on vs off - does the non-empty seed actually
   change convergence/outcome now that it's real instead of always empty?
5. Sanity-check the no-regression case: a camera pair with no usable
   telemetry at all still falls back to audio sync exactly as before,
   with the status line correctly saying so.

### 7. Calibration re-investigation + alternative-relationship discussion (2026-08-27, mostly not built)

User asked for a from-scratch, deliberately blind second opinion on why
calibration parameters feel coupled, specifically to compare against
`FRICTION.md`'s existing 22-point investigation log later - full
independent findings are FRICTION.md's new point 23 (cam_d/intersect
near-degeneracy, seam weighting starving the near field, a RANSAC/
optimizer model mismatch, lens intrinsics that are looked up from a
public database and never fitted to the actual physical unit, three
already-built-but-unreachable more-expressive models). Followed by a
discussion of the user's own checkerboard/grid idea and alternatives
that need no physical target (vertical vanishing points, IMU - shipped,
see task 6 above - a moving bright point/the ball reusing the existing
detector) - point 24. Nothing there is built except the IMU piece;
worth reading before deciding what (if anything) becomes a real GitHub
issue.


### 1. DONE - three match-folder/cut features, built and green

`cargo fmt`, `clippy --all-targets -D warnings` (workspace, reco-obs
included via `OBS_INCLUDE_DIR`), `cargo test`: reco-core 237 passing (the
usual 2 CUDA failures, no CUDA runtime on this box), reco-io 50/50,
reco-gui 74/74. Release builds of reco-gui and reco-cli made with
`--features tensorrt`. **Not yet exercised by the user in the app.**

Verified against the real match folder that the export scan picks the
right file: of the five JSONs in `03 OJC -Bergem Sport 04072026`, only the
two Match Logger exports parse, and the most recently modified of those
(`OJC_vs_Berghem_Sport_DEMO_1min.json`) is the one that will auto-load -
worth knowing, since it is the 1-minute test log, not the full demo.

- **Match Logger export auto-loads with the match folder.**
  `scoreboard_import::find_export_in_folder` identifies the export by
  *content* (every small `.json` is parsed; the one with a non-empty event
  log wins, most recently modified breaks ties) because a match folder also
  holds the calibration, lens profiles and `clicks.json`, and the export's
  filename comes from the phone. `on_pick_match_folder` drops any
  previously loaded export first (it belongs to a different match) and
  toasts what it loaded. The Load button and the folder pick now share
  `adopt_match_logger_export`.
- **PAUZE "Hold" renamed to "Black"** (it always *was* the fully-black
  duration; only the label hid that), with a tooltip explaining that fade
  is composited over frames that were going to be encoded anyway and only
  the black stretch lengthens the export. The checkbox and both durations
  are now persisted app-level in `GuiSettings` - previously they reset to
  3.0/4.0 every restart, which is what the user actually kept running into.
- **Four auto-cut margins** (`Pause cut N s before / N s after`,
  `Match trim N s pre-KO / N s post-FT`) replacing the hardcoded 2.0 in
  `refresh_derived_cut_ranges`. New `ScoreboardSettings` fields, serde
  default 2.0, so old calibrations behave exactly as before. Editing
  re-derives immediately.

### 2. DONE (untested on real footage) - manual per-camera gamma, applied *before* the auto color match

Implemented as designed below. `Topology::color_gamma_left`/`_right`
(serde default 1.0), carried to the shader in the previously-unused
`color_scale.w` as 1/gamma, applied by `apply_gamma` in `fisheye.wgsl`
ahead of the automatic scale+offset, and mirrored per sample point in
`decode_transfer_yuv` so the measurement sees the same curve.
`StitchPipeline::set_color_gamma` forces a re-measure. Two sliders in the
Color Mapping card under "MANUAL GAMMA (BEFORE AUTO MATCH)", saved with
the calibration; `reco stitch --color-gamma-left/--color-gamma-right`
override per side (one flag does not reset the other camera).

Tests: `manual_gamma_reaches_rendered_pixels_per_camera` is a real GPU
render+readback proving the exponent reaches pixels **and** that a gamma
on one camera leaves the other half untouched (the likeliest wiring bug,
which a whole-frame brightness check would miss);
`manual_gamma_lifts_the_measured_band_mean` and
`manual_gamma_is_per_camera` cover the measurement side;
`a_broken_gamma_value_falls_back_to_identity` covers 0/negative/NaN in a
hand-edited calibration.

The reasoning that produced this design, kept because it is what makes the
ordering non-obvious: the auto stage is a **purely
additive YUV offset on the band mean**, clamped to +/-0.06 luma
(`color_match.rs` `tick`). It can align average brightness but cannot
change the *shape* of a tone curve, which is exactly how two action cams
with different AE/HDR disagree. Gamma is the missing degree of freedom.
Second reason: a shader uniform works on every path, including the
zero-copy export where the auto stage does nothing at all (see the finding
below).

Design, which costs almost nothing:

- `fisheye.wgsl` already has a `color_scale: vec4` uniform whose `xyz` is
  hardcoded to 1.0 and whose `w` is pure padding (`renderer.rs:2301`). The
  gamma exponent rides in that `w`: no new uniform, no extra bandwidth, one
  `pow()` behind a uniform branch that is skipped at gamma 1.0. Same trick
  as the overlay `opacity` added earlier today.
- Shader order becomes: sample -> `pow(rgb, 1/gamma)` per side ->
  `rgb_to_yuv` -> `* scale + offset` (auto) -> back to RGB. Literally
  "manual, before auto".
- **The one real pitfall: the measurement must see the same gamma.**
  `decode_transfer_yuv` (`color_match.rs:415`) already performs the exact
  same chain as the shader (raw YUV -> RGB -> BT.709 YUV), so the same
  `pow` goes there - **per sample point, before averaging**, since the pow
  of a mean is not the mean of the pows. Skip this and the auto stage
  measures a frame that no longer exists and fights the slider: the seam
  visibly wanders while dragging.
- Gamma on RGB, not on Y alone (a pow on luma only leaves chroma absolute
  and shifts saturation). One exponent per side, not per channel - per
  channel is white balance, which the chroma offset already attempts.
- Persist next to the other color fields in `Topology`
  (`color_gamma_left`/`_right`, serde default 1.0), two sliders at the top
  of the Color Mapping card above the auto block, CLI flag for parity.
  Range 0.5 - 2.0, default 1.0, step 0.05.

### Test export on the 26-08 match, and what it showed

Ran on `04 Beuningse Boys Berghem Sport 26082026`, first segment of each
side, at the user's real export settings (2560x1440, h264, balanced).
Artifacts kept in that folder: `colortest_a..d_*.mp4`, `qualitytest_ai_2k.mp4`
and `colortest_frames/` (full frames plus seam crops).

**Video quality: good, and the softness under AI zoom is geometric, not an
encoder problem.** 60s AI-tracked export ran at 36fps avg, 25.5 Mbps,
h264 High, no artifacts, no dropped frames, bottleneck reported as
readback. The fixed-pose frame is crisp. The AI-tracked frames are
visibly softer, and the arithmetic says they have to be. **Corrected
2026-08-27:** `fov_degrees` is the *vertical* field of view (nalgebra's
`Perspective3` convention), which the first pass of this note got wrong
by treating it as horizontal. The right comparison is output pixels per
radian, `(out_height / 2) / tan(fov / 2)`, against the lens' own
`fx = 1457` px/rad. For a 2560x1440 export that gives **1.11x** at
`fov_wide` 48 deg, **1.62x** at `fov_default` 34, and **2.80x** at
`fov_tight` 20. So the wide end is essentially 1:1 and the tight end is
where the softness comes from. Nothing in the pipeline can put back
detail the sensor never resolved; if sharpness at full zoom matters more
than framing tightness, `fov_tight` is the knob, not the encoder. (The
same figures are now shown live under the FOV sliders - see task 4.)

Also checked, since it looks alarming on a still: the background slopes
by up to 11 degrees in AI-tracked frames while the fixed-pose frame is
level (+0.6 deg). Measured by fitting the sky/treeline boundary
(`D:\CLAUDE\horizon_tilt.py`). It is **not** a roll bug: near field lines
and the far treeline slope by *different* amounts in the same frame,
which is perspective from an off-axis look direction, not a rotated
camera - and the user's older `test10_long.mp4` shows the same pattern
(level at t=120s, -7 deg at t=300s). Worth knowing because it gets
stronger the wider `fov_wide` is.

**Auto color match: confirmed inactive in a normal export, on the user's
own footage.** Four 10s renders of the identical source, differing only
in decode path and flags:

| variant | decode | color match | L-R imbalance |
|---|---|---|---|
| A | zero-copy (default) | on (nominally) | -22.98 |
| B | `--no-zero-copy` | on | -28.74 |
| C | `--no-zero-copy` | `--no-color-match` | -23.11 |
| D | `--no-zero-copy` | off, gamma 1.0 | -31.47 |

A and C agree to **0.13 gray levels (sd 0.05)** across 30 sampled frames.
The default export renders *exactly* as if color match were switched off.
B differs from C by 5.6 levels, so the correction is real - just
unreachable on the path every export actually takes.

What the correction does when it does run, from a debug-level run: the
seam band measures `left_mean` 0.345 vs `right_mean` 0.326 - a genuine
but small 4.8-level luma gap - and it applies -0.0094 left / +0.0094
right. **Not clamped** (the limit is 0.0394), so raising "Max luma offset"
would change nothing. Chroma correction is zero because this calibration
sets `color_match_max_chroma_offset` to 0.

Column profile of B-minus-C (`D:\CLAUDE\seam_profile.py`) also pins the
seam on screen: flat -2.9 levels left of x=1120, flat +2.9 right of
x=1440, crossing the midpoint at **x=1271** of 2560. So the blend is
about 320px wide on screen at this pose.

**A trap the user is currently in.** Their calibration already carries
`color_gamma_left 0.92 / color_gamma_right 0.83`, tuned tonight in the
preview - where auto color match *is* active. The same profile measured
against gamma 1.0 shifts the right camera 8.9 levels down relative to the
left, i.e. it pushes the two cameras apart in exactly the axis the
automatic stage was simultaneously pulling together by 5.9. In the export
the automatic half is absent, so the result cannot match what the preview
showed. **Until the GPU measurement lands, tune gamma with "Auto color
match" switched OFF**, so the preview shows what the file will contain.

### 3. DONE 2026-08-27 - color-match measurement moved onto the GPU

`render::band_gather` + `shaders/color_gather.wgsl`. The compute pass
gathers raw texels at the CPU-computed band positions; every bit of colour
maths stays in `color_match` (`mean_of_normalized_samples`), shared with
the CPU sampler. Readback is asynchronous and never waits.
`ColorMatchState` was split first (`measurement_due` /
`measurement_issued` / `apply_measurement`) so both paths honour the same
interval, clamp and EMA.

**Verified on the user's own footage**, 10s at 2560x1440 from the 26-08
match: the zero-copy export now lands within **0.29 gray levels** of the
CPU-decode reference, against a 5.6-level correction - where before the
fix it was identical to color-match-off (0.13). Cost: none measurable,
with the measurement running on *every* frame (their
`color_match_interval_frames = 1`): 87.6 and 99.0 fps against 82.8 and
98.7 with color match off.

**The one real trap, caught by the real-footage check and not by the unit
tests:** the shader renders a 180-degree-rotated camera by sampling at
`1-uv`, while the CPU path reverses the buffer. A gather reading raw
texels has to mirror its positions to match. On this rig only the left
camera carries `rotation=-180`, so the first attempt produced a
correction twice its true size and jittery, rather than an obvious mirror
image. `band_gather::mirrored_180` plus
`a_rotated_camera_is_measured_where_it_is_drawn` cover it - that test
asserts the flip *changes* the measurement, which is precisely what an
ignored flip would not do.

Also worth acting on: the user's saved `color_gamma_left 0.92 /
color_gamma_right 0.83` were tuned in a preview where the automatic match
was active and an export where it was not. Now that both agree, those
values are worth re-checking from scratch.

### Original plan, kept for the reasoning

**Not for speed - for coverage.** The measurement is 8x16 = 128 sample
points once per 15 frames; at 30fps that is ~256 samples/second of
closed-form undistort plus three byte reads each. It cannot be a
bottleneck, and applying the correction was always on the GPU (two vec4
uniforms in a pass that exists anyway). The reason to move it is that it
is the only way to have Auto Color Match work at all under hardware
decode. Today the choice is color correction (CPU decode, much slower) or
speed (zero-copy, no correction).

Refined after reading the zero-copy path (2026-08-26 late, no build -
the machine was busy running autocam):

**The GPU pass must be a dumb texel gather, not a second implementation
of the colour maths.** Have the compute shader return the 128 raw
`(Y, U, V)` texel values per camera and let the CPU run the existing
`decode_transfer_yuv` - limited-range expansion, YUV to RGB, the manual
gamma, RGB back to YUV, then the mean - byte for byte as it does today.
Two reasons this is not a detail: the curve now lives in exactly one
place on the CPU side (the SYNC_WITH contract with `fisheye.wgsl` stays a
two-way contract instead of a three-way one), and the readback shrinks to
128 x 2 x 4 bytes, about 1KB, which is nothing to move once every 15
frames.

**Where it hooks in.** Both zero-copy entry points -
`render_imported_views` (D3D11VA, the Windows path that matters) and
`render_imported_textures` (Metal) - set the left/right bind groups and
then call `render_to_target_gpu`, so that one function is the single
insertion point. It does not currently keep the texture views around
though, only the bind groups it built from them, so either the views get
threaded through or the pipeline holds onto the last pair. The gather
needs its own bind group anyway (a compute layout, not the render
pipeline's sampler layout), so threading the views through is the
smaller change.

**Sequence per measurement:** encode the gather into the same command
encoder as the render (no extra submit), `copy_buffer_to_buffer` into a
MAP_READ staging buffer, `map_async`, and read it on a later frame. Never
poll for it in the same frame - that is the stall that would make the
export slower rather than faster.

**One refactor first, testable without a GPU:** `ColorMatchState::tick`
currently takes a closure that performs the measurement inline. Split
that into "here is a fresh (left_mean, right_mean), fold it in" so both
the CPU sampler and the GPU readback feed the same EMA, clamp and
logging. That is a pure CPU change with the existing tests as the guard,
and it is what lets the GPU path reuse every bit of behaviour the CPU
path already has.

Original shape, still accurate:

1. The 128 sample positions stay computed on the CPU exactly as now
   (`undistorted_to_distorted`, including the `seam_offset` and
   `blend_flip_direction` handling that has already been got wrong twice -
   see reco-core FRICTION.md). They only change when the calibration
   changes, so they go into a uniform buffer once.
2. A compute pass samples both NV12 textures at those positions and writes
   128 values.
3. **Async** readback (`map_async`), consumed a frame or two later.

Step 3 is the trap: a synchronous readback stalls the pipeline and would
make the export *slower*, the opposite of the intent. Async is fine because
the EMA already smooths across measurements that are 15 frames apart.

Endgame, only if it proves worth it: keep the derived offsets in a storage
buffer and have `fisheye.wgsl` read them from there, so nothing crosses the
bus at all. The Color Mapping status line would then need an occasional
cosmetic readback.

### 4. DONE 2026-08-27 - the AI zoom range is drawn on the preview

A `zoom-range` widget sits directly under the three Field-of-view sliders
in the export dialog: the current preview frame with the widest and
tightest shot drawn on it as labelled boxes that resize live while the
sliders move, plus a line reading how far past the lens' resolving power
each end pushes the render (1.1x at wide, 2.8x at tight on this rig at
2560x1440).

Both traps called out in the plan were real and are handled: the boxes
scale by `tan(fov/2) / tan(preview_fov/2)` rather than `fov/preview_fov`,
and the upscale figure uses the *vertical* FOV convention. `Math.tan(x *
0.5deg)` and `root.preview-frame.width` both work in Slint 1.15, so no
Rust-side geometry was needed beyond one new property,
`lens-px-per-rad`, pushed from `calibration.lenses[0].fx`.

Still open, if it turns out to matter in use: the boxes are drawn
relative to the *preview's* current FOV, so zooming the preview rescales
them. A "hold the preview at its widest while this is open" mode would
make them absolute, at the cost of taking over the user's view.

### Original request

User: "nu heb ik totaal geen idee wat de min en max zoom is in de AI
parameters". They sketched it with a reference image: the full field view
with two labelled rectangles on it, "Lowest zoom" and "Highest zoom".

The sliders are `export-fov-tight` / `export-fov-wide` /
`export-fov-default` (main.slint ~5618), in degrees, in the Export
dialog's AI Tracking section. A number in degrees means nothing without a
picture, which is the whole complaint.

Proposed shape - draw the boxes on the existing preview rather than
building a second renderer:

- A "Show zoom range" toggle next to the three FOV sliders. While on, the
  preview holds a fixed wide pose (so the boxes have something stable to
  sit in) and two labelled rectangles are overlaid: `fov_wide` (widest
  the director will ever pull out to) and `fov_tight` (tightest it will
  push in). Dragging a slider resizes its box live - that is the whole
  point.
- Overlay it the way the ROI editor already overlays polygons on the
  preview; no reco-core change is needed.

**The one piece of maths to get right.** The box is *not* a linear
fraction of the preview. For a rectilinear virtual camera the half-width
of an inner FOV `f` inside a preview of FOV `F` is
`tan(f/2) / tan(F/2)`. At the FOVs in play (20-48 deg inside a ~75-100
deg preview) the linear approximation `f/F` is wrong by tens of percent,
which would draw a confidently mislabelled box - worse than no box.
Height follows from the output aspect ratio, not from a second FOV.

Worth adding while there: label each box with its own **upscale factor**,
computed from the lens (`fx` px/rad) and the export width -
`upscale = (out_width / fov_deg) / (fx / 57.2958)`. At this rig that reads
3.0x at 34 deg and 5.0x at 20 deg, and it converts "the tight shot looks
soft" from a complaint into a number the user can steer by before
exporting.

### 5. DONE 2026-08-27 (user request) - help moved out of hover and onto a "?" badge

User: "bij het hoveren over een functie krijg je een tooltip met uitleg,
dit wil ik niet meer door het hoveren, maar door klein blokje met een
vraagteken achter de functie text".

`Tip` used to be a TouchArea wrapping the whole control, opening its
popup after a 5-second hover. Now it lays the wrapped control out in a
row followed by a 14px "?" badge, and opens on click. One component, so
all 56 usages changed at once, and the badge only renders when a tip was
actually passed - `LabeledSlider`'s default (no tip) lays out exactly as
before.

Watch for this if the panels start clipping again: the badge adds ~18px
to a Tip's minimum width, and the left panel is 260px wide and sizes
itself to its widest child (see the auto-cut margin fix from 2026-08-26).
The longest tipped label in that panel is the "Auto-cut kickoff lead-in +
pauses" checkbox.

### Background finding that motivates 2 and 3: auto color match is inactive in every export

Evidence from the user's own `target/release/reco-gui.log`, on every export
this session: `SmartFileSource: D3D11VA zero-copy decode enabled`. On that
path the frames never reach the CPU, and
`render_imported_views` -> `render_to_target_gpu` (`pipeline.rs:773`)
renders with `ColorCorrection::default()`, i.e. identity - documented in
`color_match.rs`'s module doc as a known limitation.

The GUI preview, by contrast, decodes to CPU `YuvPlanes` and goes through
`render_to_view` (`preview.rs:155`), where the matching *does* run. So
everything tuned in the Color Mapping panel is visible in the preview and
absent from the file. That asymmetry is the most likely explanation for
"Auto Color Match kreeg ik niet helemaal goed", and it means no color-match
tuning session is trustworthy until task 3 lands.

Also worth checking once it does: if the Color Mapping status line sits
pinned at Y+0.060 / +/-0.040, the real difference exceeds what the auto
stage is even allowed to correct, and the clamp needs raising rather than
the measurement being made more accurate.

---

## Previous entry - 2026-08-26: SCOREBOARD BLUR ROOT-CAUSED AND FIXED (user-confirmed sharp); PAUZE moved to its own GPU overlay slot

**Update: the GPU transition path was tested on a real export after this
was written - 31fps, no dip during the fade ramps. The "Open: fps dips to
~9 during fade ramps" section further down is resolved. The user also
confirmed the scoreboard itself on that same export ("werkt tot nu toe
helemaal top"), so the sharpness fix and the GPU transition hold together
on real footage.**


**User confirmed on a real 2K export (test9_long.mp4): scoreboard stays
sharp with PAUZE enabled, fade-to-black works.** Delivering with
Fade 1.0 / Hold 2.0 to keep the remaining fps dip short.

## The actual root cause (after several wrong turns - read this first)

`LayeredOverlaySource` sized its shared canvas **from the layers
themselves**. First a fixed 960x540, then (2026-08-24) "the largest
active layer's own pixel buffer". The second sounds right and is
equally wrong, just harder to see:

A producer that pre-scales its capture (the scoreboard, via
`design_size` + `contain_fit_render_scale`) sizes that buffer to its
**on-screen footprint** - at 2K with placement scale 0.3 that is
768x432. The canvas then became 768x432, and `composite_over` placed
the layer *within* that canvas at its placement scale **again**,
squeezing 768x432 into 230x130, after which the GPU stretched it back
up to 768x432 on screen. Crisp -> 230x130 -> crisp-sized-again.

Two consequences that misled every earlier investigation:
- The damage was there **from frame 1 of the whole export**, not only
  during a transition - merely *registering* a second layer was enough,
  even before it ever drew anything. That is exactly why "PAUZE on" was
  blurry end-to-end while "PAUZE off" was sharp (test4 vs test5).
- It got worse at higher output resolutions, which made it look like a
  resolution bug.

**Fix**: the canvas is now pinned to the session's real output
resolution, in both actual pixels and `design_size`
(`LayeredOverlaySource::new` takes `output_size`; `stitch_job` passes
`(out_w, out_h)`). `composite_over` then performs, per layer, the
*identical* contain-fit computation the GPU shader would have performed
had that layer been attached alone - so every layer lands byte-exactly
where it belongs and a pre-scaled capture is resampled by nothing at
all. No derived-from-layers sizing, and no fixed constant, anywhere in
the path any more.

Regression test:
`a_pre_scaled_layer_is_composited_pixel_for_pixel_alongside_another_layer`
- checks a marker pixel lands on exact coordinates *and* that its
neighbour is unblended, which fails under any resampling.

## Wrong turns this session, so they are not repeated

1. **Bilinear sampling in `composite_over`** (2026-08-25, kept - it is
   independently correct, but it was never the cause; it cannot recover
   detail a geometric downscale already discarded).
2. **Splitting PAUZE into a 1x1 fade layer + a separate caption layer**
   (`pause_overlay::build_layers`, kept - genuinely better, and the 1x1
   fade can never dominate canvas sizing again, but on its own it did
   not fix the blur either).
3. **Capping the caption at 960x540** - reintroduced the very constant
   we were trying to remove. User caught this directly ("ik dacht dat we
   gestopt waren met 960x540"). The cap still exists but now only
   bounds *render cost*, not placement; it cannot affect quality
   because the canvas no longer derives its size from it.
4. Sizing the caption with `contain_fit_render_scale` **uncapped** made
   it render at full output resolution every frame - a real, live
   export slowdown the user hit and cancelled.

## Performance work that was needed alongside the fix

An output-resolution canvas is far more expensive to composite, so:
- **Duplicate-frame detection** in `LayeredOverlaySource::try_frame`:
  `reco_scoreboard`'s runtime re-captures at 30fps while its content
  changes ~1/sec. Compositing those duplicates collapsed a real export
  from 22fps to 7fps. A memcmp against the cached frame skips them.
- **`unchanged_alpha`** in `pause_overlay`: the constant-alpha hold (the
  longest part of a transition) now emits one frame, not one per frame.
  Confirmed working in test9 - frames 750-850 (the hold) ran at 29-31fps.
- **Empty-canvas fast path** (`out = src` over a transparent
  destination) and a **full-canvas flat row fill** for the fade. Both
  have tests asserting byte-identical output to the general blend path,
  since both are optimisations making a mathematical claim.
- **Row-bounds clipping** via `opaque_row_bounds` so a mostly
  transparent full-frame layer (the caption) only scans its text band.

## Why a trivial fade costs more than stitching (the reasoning behind the next step)

User's question, and it is the right one: "waarom wordt hij trager
tijdens een pauze, dat is toch een super simpele actie, dat kan nooit
zwaarder zijn dan stitchen en AI-volging."

It isn't heavier - it runs in the wrong place:

- Stitching and AI run entirely **on the GPU**, in VRAM. Blacking out
  3.7M pixels there is microseconds and moves **zero bytes**: the frame
  never leaves the card.
- The PAUZE fade runs **on the CPU**, in `composite_over` - one core,
  pixel by pixel - and then the finished full-frame canvas has to be
  handed to the GPU through `RgbaOverlayCompositor::upload`'s
  `write_texture`. At 2560x1440 that is ~14.7MB built on the CPU and
  another ~14.7MB pushed across the bus, **every fade frame**.

So the operation really is trivially simple; it is ~100x more expensive
than the stitch only because of where it happens and because its result
must be shipped to the GPU as a whole image each frame.

The reason it grew this way: `RgbaOverlayCompositor` has exactly **one**
overlay slot (one texture, one placement), so when a scoreboard and
PAUZE both had to be active, they were merged on the CPU into a single
image. That single design decision is the origin of everything that
went wrong today - the blur came out of the very same construction.

## DONE: PAUZE moved onto its own GPU overlay slot

Implemented after the restore point (tag
`scoreboard-sharp-cpu-composite`, commit `7eaf88dc`). The plan below was
followed as written:

- `rgba_overlay.wgsl` + `OverlayParams`: new `opacity` multiplier on the
  sampled alpha, occupying what was already padding - a fade costs no
  extra uniform bandwidth. `RgbaOverlayCompositor::set_opacity` writes
  it and no-ops when unchanged.
- New `OverlayTransitionSource` trait in `render::overlay`: `card()`
  (one fixed image, read once) + `advance()` (this frame's opacity).
  Deliberately separate from `OverlayFrameSource`, whose whole premise
  is that the *pixels* change.
- `StitchPipeline` gained a second, independent compositor
  (`transition`), drawn **before** `overlay` so a scoreboard stays
  legible on top of the dip rather than dimming with the video.
  `set_transition_frame` / `set_transition_opacity` /
  `clear_transition`, and it resizes alongside the main overlay.
- `StitchSession::set_overlay_transition` uploads the card once on
  attach; `refresh_overlay_transition` advances it once per encoded
  frame and pushes only the opacity.
- `pause_overlay::build_transition` rasterises **one** black+caption
  card at the full output resolution (affordable exactly because it
  happens once) and rides the existing `Schedule` for its alpha ramp.
- `stitch_job` attaches it through the session's transition slot
  instead of pushing layers.

**The decisive consequence**: `layers` now holds only real content
overlays, so a lone scoreboard takes the single-source path and
`LayeredOverlaySource` is not constructed at all in a normal export.
There is no CPU compositing left in the common case, and a fade is a
32-byte uniform write.

Tests added: `opacity_uniform_fades_the_composited_overlay_on_the_gpu`
(a real GPU render+readback at opacity 1.0 / 0.5 / 0.0 - this is the
mechanism the whole change rests on, so it is checked end to end, not
just as a uniform that compiles), plus
`transition_card_is_rasterised_at_the_full_output_resolution` and
`transition_advances_alpha_without_touching_its_card` (the card must be
immutable, or the per-frame cost is back).

Verified: `cargo fmt --check` and `clippy -D warnings` clean for
reco-core/reco-io/reco-cli; `cargo test -p reco-core --lib` 232 passing
(same 2 pre-existing CUDA failures), reco-io 50/50.

`pause_overlay::build_layers` and `LayeredOverlaySource` are both kept
but no longer used in production: `build_layers` is documented as
superseded, and `LayeredOverlaySource` remains the right mechanism if
two *content* overlays are ever attached at once (its output-resolution
canvas fix stays load-bearing for that case). Remove `build_layers`
once the GPU path has been confirmed on real exports.

**NOT yet user-tested.** The fade must be re-checked visually (it is now
drawn by a different code path entirely) and the fps during a ramp
re-measured from a fresh `events.jsonl` - the expectation is a flat
~30fps with no dip at all.

## Original plan (kept for reference)

**Give the PAUZE transition its own GPU overlay slot.** Not a rebuild -
a targeted change:

- Add an `opacity: f32` to `OverlayParams` + `rgba_overlay.wgsl`
  (multiply the sampled alpha by it).
- Give the pipeline a second `RgbaOverlayCompositor` dedicated to the
  transition, drawn over the existing one.
- `pause_overlay` reverts to producing **one** black-with-caption card
  (the pre-split design was right for the GPU all along; it was only
  wrong because it went through the CPU canvas), uploaded **once**, with
  only the per-frame alpha changing - a few bytes instead of 14.7MB.
- `stitch_job` registers the transition through that path instead of as
  a layer.

The decisive knock-on effect: with PAUZE off the layer list, the
scoreboard is the **only** layer, and `stitch_job` already skips
`LayeredOverlaySource` entirely at `layers.len() == 1` - so there is
zero CPU compositing left in a normal export, and the fade becomes free.

Keep `LayeredOverlaySource` and its tests: it is still the correct
mechanism if two *content* overlays are ever attached at once, and its
canvas-sizing fix stays load-bearing for that case.

## Open: fps dips to ~9 during fade ramps (export speed only)

Measured from the user's own `test9_long.events.jsonl` (release build):
normal 28-33fps, **fade-in frames 650-750: 9-11fps**, hold: 29-31fps
(free, as designed), **fade-out frames 900-950: 8.4fps**, then back to
30fps. The cliff in the earlier debug-build run began at exactly frame
661 - the first fade frame - which is how this was pinned down.

**The output video is unaffected; this is export wall-time only.**

CPU optimisation is now at diminishing returns (the row fill bought
111->102ms/frame at 2K). This is structural: at 2560x1440 any
full-frame CPU pass costs ~10ms in memory bandwidth alone, the canvas
allocation costs ~10ms, and the per-frame budget at 30fps is 33ms of
which the rest of the pipeline already uses ~33ms.

**The real fix is GPU compositing** - give `RgbaOverlayCompositor`
N texture slots and draw the layers in one pass instead of blending
them on the CPU, or (smaller) move the PAUZE fade+caption onto their
own GPU overlay slot with an `opacity` uniform, which would leave the
scoreboard as the only CPU layer - and with one layer `stitch_job`
skips `LayeredOverlaySource` entirely, so the cost goes to zero.
Deliberately **not** attempted while the user was delivering.

Immediate mitigation in use: Fade 1.0 / Hold 2.0 in the export panel
(`export-pause-overlay-fade-secs` / `-hold-secs`), which the user wanted
for pacing anyway and shortens the expensive window ~3x.

`crates/reco-core/examples/composite_bench.rs` was rewritten to measure
the current shapes (transition-ramping vs steady, at 1080p/2K/4K) - use
it before/after any GPU-compositing work.

## "1st Section" after a pause - not a bug, missing data

User reported the scoreboard stays on "1st Section" after a pause.
Checked: `state_at`'s `current_period` advances **only** on
`EventKind::PeriodStart` - `pause_start`/`pause_end` deliberately only
freeze the clock within the same section (an injury stoppage is not a
new section). Correct as designed.

The cause is the demo data: every `OJC_vs_Berghem_Sport_DEMO*.json`
declares `"periods": 4` but contains a single `period_start` (period 1).
Those files are hand-written (including the 1min one written this
session), so they do not reflect what the real app emits. The real
`scripts/match-logger/Match Logger.html` is fine - `btnPeriodAction`
("Volgende periode (n / N)") emits `period_start` with an incremented
period and auto-closes an open pause. In a real match that button is
pressed at each section break; pressing only Pause is what produces
this.

`OJC_vs_Berghem_Sport_DEMO_1min.json` has since been given a real
`period_start` (period 2) right after its pause, plus a second goal, so
the section advance can actually be verified (~40s into that export).

## test10_long.mp4 measurement (release, after all fixes)

19901 frames, 203.8s wall, **97.7fps average**; only 30 frames (0.15%)
over 100ms and **no contiguous slow region at all** - a fade ramp would
show as a block of consecutive slow frames. Unexplained: this run
averaged 3x faster than test9 (29fps) on an identical `run_config`;
possibly a different source/resolution, not established. If it matters,
confirm the fade is actually visible in that file before treating the
run as proof the fade cost is gone.

## Also shipped this session: auto-cut and match-end without a scoreboard

- The "Load Match Logger export..." button no longer sits behind the
  scoreboard ON/OFF toggle, so "Auto-cut kickoff lead-in + pauses" can
  be used with no banner rendered. The backend was already fully
  decoupled; only the Slint gating was in the way.
- New `scoreboard_import::derived_end_secs` + `AppState::
  scoreboard_derived_end_secs`: a `match_end` event now sets the export
  **End(s)** automatically, mirroring how `period_start` already set
  Start(s) ("dit moet straks ook als signaal einde wedstrijd zijn").
  Same "only reset what we set ourselves" behaviour.
- Short test fixture written for fast iteration:
  `D:\VOETBAL_VIDEO\Berghem Sport J011-1\03 OJC -Bergem Sport
  04072026\OJC_vs_Berghem_Sport_DEMO_1min.json` (kickoff +5s, one 10s
  pause, match_end +60s).

## State

All of the above is **committed nowhere yet** - still uncommitted in the
working tree, along with the 7 unpushed commits from previous sessions.
Both debug and release `reco-gui.exe` are built with `--features
tensorrt` and include everything. `cargo fmt --check` /
`clippy -D warnings` clean for reco-core + reco-io; `cargo test -p
reco-core --lib` 227 passing (the same 2 pre-existing unrelated CUDA
failures), `reco-io` 50/50, `reco-gui` scoreboard_import 17/17.

---

# Session handoff - 2026-08-26 (TGR_PC), earlier: bilinear fix alone NOT sufficient (test6_long.mp4, 2K), deeper canvas-sizing root cause found; new "auto-cut without scoreboard" toggle-decoupling shipped in Slint (not yet built)

User's real test (`test6_long.mp4`, 2K, PAUZE+Cut) still showed a bad
banner even with yesterday's bilinear `composite_over` fix in place -
confirms the bilinear fix (still uncommitted, see below) was real but
incomplete.

## Deeper root cause (code-confirmed via the actual math, not yet fixed)

`pause_overlay::CANVAS_WIDTH/HEIGHT` is a **hardcoded 960x540
constant**, completely independent of `design_size` or the export's
own output resolution. `LayeredOverlaySource::try_frame` sizes the
shared canvas to the largest *actual pixel* buffer among active layers
(`overlay_layers.rs:73-79`) - when PAUZE is active, that's always
exactly 960x540, no matter what.

Worked through the actual scale factors: `composite_over` places a
source using its `design_size` (1920x1080 for the scoreboard) against
the canvas's own *actual* pixel dimensions (960x540 when PAUZE is
active) - i.e. every scoreboard frame gets forced through a **fixed
0.5x design-to-canvas pixel ratio**, regardless of export resolution.
But the scoreboard's own actual captured buffer (from the 2026-08-25
`design_size`/zoom work) is deliberately sized to match its *real*
on-screen footprint at the export's *own* output resolution - at 2K
(2560x1440) that's `output_size/design_size = 1.333x` bigger than at
1080p. So at 2K, the scoreboard's crisp, correctly-sized Chrome capture
gets *downscaled* into the 960x540-capped canvas (real information
loss, not just a filtering artifact), then the outer GPU compositor
*upscales* that already-degraded canvas back up to the same on-screen
footprint. Net effect: crisp capture -> lossy downsample -> lossy
upsample = double resample that visibly softens/blurs text, and gets
proportionally worse the higher the export resolution (2K/4K) since
the scoreboard's true capture size scales with output resolution but
PAUZE's fixed 960x540 canvas cap does not. This fully explains why
2026-08-25's bilinear fix (correct as far as it went) didn't fix
`test6_long.mp4` - bilinear filtering can't recover detail that a
geometrically-forced downsample already discarded.

**Not fixed yet.** Real fix needs the shared canvas's actual-pixel
sizing decoupled from PAUZE's fixed constant - e.g. size the canvas
relative to the largest layer's *design-to-output* ratio instead of a
hardcoded 960x540, or give PAUZE itself a resolution-aware canvas size.
Not attempted yet this session - needs more thought before touching
`pause_overlay.rs`/`overlay_layers.rs` again.

## New, shipped this session: decouple "Auto-cut kickoff lead-in + pauses" from the scoreboard ON/OFF toggle

User's separate ask: use the Match-Logger-derived auto-cut feature
without the scoreboard banner ever rendering. Traced the whole chain
(`on_load_scoreboard_events`/`on_set_scoreboard_sync_point`/
`on_toggled_scoreboard_derive_cut_ranges` in `main.rs`, and
`scoreboard_package` at export time) - **the backend was already fully
decoupled**: cut-range derivation only ever needs `scoreboard_import` +
`scoreboard_sync_anchor`, and `scoreboard_package` (the thing that
actually attaches a rendered overlay layer) is separately gated purely
on `scoreboard-enabled`. The only real gate was `ui/main.slint`'s
"Load Match Logger export..." button being hidden behind
`if root.scoreboard-enabled`, which also hid the sync-point button and
the auto-cut checkbox (both of which were already gated on
`scoreboard-match-summary != ""`, not `scoreboard-enabled`, once
reached). Fixed with a single-line condition removal on that one
button. As a side effect, this workflow also naturally avoids the
whole PAUZE+scoreboard canvas bug above, since no scoreboard layer
ever gets attached.

**Not yet built or tested** - user is mid-test on the existing debug
build (the bilinear fix, see below) as of this edit; don't rebuild
until that test finishes, then build fresh with both this change and
the still-uncommitted bilinear fix included.

## Real fix for the deeper canvas-sizing bug: split PAUZE into a fade layer + a caption layer

User pushed back on the whole shared-canvas/bilinear direction ("we
maken het te moeilijk... waarom hebben we een vaste 960x540
constante?") - correctly. Worked through it together and landed on
splitting `PauseOverlaySource` (previously one combined black+"PAUZE"
card, fixed 960x540 actual pixels) into two independent
`OverlayFrameSource`s in [pause_overlay.rs](crates/reco-core/src/render/pause_overlay.rs),
sharing one alpha ramp but rendered/sized completely separately:

- **`PauseFadeSource`** - the black dip-to-black, covering the whole
  frame. A flat color has no detail to lose at any resolution, so its
  actual pixel buffer is always **1x1** regardless of export
  resolution or placement - can never again force a co-active layer's
  shared canvas down to something too small.
- **`PauseCaptionSource`** - just the "PAUZE" text, transparent
  background. Its actual pixel resolution is now computed via
  `contain_fit_render_scale` (the exact same helper
  `reco_scoreboard::runtime` already uses for the scoreboard's own
  Chrome capture) against the export's real output resolution, instead
  of a fixed constant - so it scales itself the same way the scoreboard
  already does, and stops being an oversized (or undersized) mismatch
  next to it.

Two independent `Schedule` instances (one per source) advance in
perfect lockstep with no locking needed, because
`LayeredOverlaySource::try_frame` calls `try_frame` on every registered
layer exactly once per output frame, in the same order, every time -
two counters ticking off the same external cadence never drift. New
tests cover this lockstep property directly, plus the fade always
being 1x1 and the caption's resolution actually scaling with output
size (1080p vs 4K).

`stitch_job.rs` now pushes both layers (`pause_overlay::build_layers`,
takes the export's real `(out_w, out_h)`) instead of one combined
source - always still first/bottom in `layers`, unchanged ordering
relative to any scoreboard/other registered layer.

This is believed to be the actual, complete fix for `test6_long.mp4`'s
bug (not just a mitigation like yesterday's bilinear change, which
stays in place too and is independently correct for any other
layer-size mismatch) - the scoreboard's own correctly-sized capture
should now no longer get forced through any lossy resample at all in
the common case, since neither PAUZE layer can dominate the shared
canvas below what the scoreboard actually needs.

**Verification so far**: `cargo check`/`clippy -D warnings`/`test` all
clean for `reco-core` and `reco-io` (223/225 reco-core, the same 2
pre-existing unrelated CUDA failures; 50/50 + doctests reco-io).

## test6/test7 real-world results: fade/caption split alone did NOT fix it - two more real bugs found

Built debug+release with the fade/caption split above and asked the
user to retest. Two real problems surfaced, in order:

### Bug A: caption rendered at full OUTPUT resolution every frame - real export slowdown

`build_layers`'s first version used `contain_fit_render_scale` for the
caption's actual pixel resolution with **no upper cap** - since the
caption's `design_size` (960x540) covers the *whole frame* at the
default placement (unlike the scoreboard's small banner, which is what
that helper was designed for), this scaled the caption's actual buffer
up to the *full output resolution* (2560x1440 at 2K) - re-filling a
~14.75MB buffer, pixel-by-pixel, every single frame of the transition,
including the entire (possibly multi-second) constant-alpha hold. User
caught this live ("halverwegen gaat de export superlangzaam") on a real
2K `test6_long.mp4` export and had to cancel it.

**Fixed**: capped `render_scale.min(1.0)` - the caption now never
exceeds the pre-split, already-known-cheap 960x540 (only shrinks below
it for a genuinely small output). New tests
`caption_resolution_never_exceeds_the_design_canvas_size` /
`..._shrinks_below_the_cap_for_a_small_output`.

### Bug B: a layer's cache entry never expired once transparent - canvas stayed forced to the old size for the REST of the export

Re-derived the exact `composite_over`/`contain_fit_render_scale` math
by hand for a concrete 2K/p=0.3 case and found the *pre-cap* build
(Bug A's build, still running when the user hit the slowdown) should
actually have given a **mathematically perfect 1:1** scoreboard mapping
- yet the user reported bad quality on that build too (`test7_long.mp4`,
after Bug A's build had time to finish a real run). That contradiction
pointed at a second, independent bug: `LayeredOverlaySource.cache[i]`
is only ever *set* when a layer's `try_frame()` returns `Some` - a
`None` return only means "no update this call" (existing, correct
behavior for e.g. a static logo), but nothing ever clears a slot once
a layer's producer is *permanently* done. `pause_overlay`'s
`was_active` logic sends one final fully-transparent "clear" frame when
a transition ends, then returns `None` forever - that final frame's
*size* stayed in the max-size fold `LayeredOverlaySource::try_frame`
uses for the rest of the export, forcing every subsequent scoreboard
frame through an unnecessary resample long after the transition itself
had ended - not just during it, which is what every earlier
investigation this whole saga (including today's) assumed the scope
of the bug was. This was **not** a new regression from today's split -
the original single-`PauseOverlaySource` design had the exact same
"cache never clears" property, just masked by always being a fixed
960x540 regardless.

**Fixed** (`overlay_layers.rs`): a newly-received frame that's fully
transparent (`is_fully_transparent`, short-circuits on the first
non-zero alpha byte) is dropped from the cache immediately instead of
being stored - it draws nothing anyway (`composite_over`'s
`src_a <= 0.0` skip), so excluding it from the max-size fold is free
and correct. New regression test
`layered_source_stops_sizing_the_canvas_to_a_layer_once_it_goes_fully_transparent`
(a big layer that goes transparent + a small layer that keeps
updating - canvas must shrink back down on the very next frame, not
stay stuck).

**Verification**: `cargo clippy -D warnings` / `fmt --check` clean for
`reco-core` + `reco-io`. `cargo test -p reco-core --lib`: 225/227 (same
2 pre-existing CUDA failures). Both debug+release rebuilt with
`--features tensorrt` a third time this session, including Bug A's cap
fix + Bug B's cache fix together (the very first build to have both).

**Not yet confirmed by a real test** - `test8_long.mp4` was run against
the build with Bug A's fix only (before Bug B was found), still showed
"zeer slechte kwaliteit" (screenshot: clock/score digits legible, team
labels/period text mush) - consistent with Bug B still being present in
that specific build. **User is re-running `test8_long.mp4` now against
the build that has both fixes** - this is the actual next real signal,
don't assume Bug B was the final answer either without seeing it. User
also separately reported an export stopping with an unspecified error
code on one of the interrupted runs - not yet identified, ask for the
exact message/log next.

---

# Session handoff - 2026-08-25 (TGR_PC), continued: real root cause found (PAUZE+scoreboard layering), fix applied but NOT YET committed - SESSION PAUSED before user test

**SESSION PAUSED 2026-08-25 (evening), user stopping for the day -
pick up by asking the user to test the fresh build (see "Not done"
below) before anything else, then commit only after that's confirmed.**

Direct continuation of everything below (commits `72e1307e` +
`090860fc`, already pushed... no, NOT pushed yet either - see repo
state at the very bottom). After those were committed and verified
via 3 clean tests, the user ran two more real exports specifically
comparing **PAUZE overlay on cuts**: `test4_long.mp4` (Pauze+Cut ON -
scoreboard very bad, confirmed bad from frame 1 through to the end via
direct frame extraction) vs `test5_long.mp4` (Cut only, Pauze OFF -
good). This is the real, reproducible differentiator the whole
"sometimes bad" mystery from earlier today was actually about - every
prior "bad" 2K result this session most likely also had PAUZE active
without our realizing it was the variable that mattered (vs. the
stale-process theory in section 6 below, which was real but not the
whole story).

## Root cause (code-confirmed, not just suspected)

`crates/reco-core/src/render/overlay_layers.rs`'s `LayeredOverlaySource`
combines the scoreboard and PAUZE into one shared canvas (needed
because the compositor only has one overlay texture slot). The shared
canvas is sized to the *largest* active layer's actual pixel buffer.
PAUZE's canvas is a fixed 960x540 (`pause_overlay::CANVAS_WIDTH/
HEIGHT`). Before today's zoom-fix work, the scoreboard's own texture
was always captured at full 1920x1080 - reliably *larger* than PAUZE's
960x540, so it always "won" and drew into the shared canvas pixel-for-
pixel with no resample (the code's own comment describes this as the
intended fast path). Today's `apply_zoom`/`design_size` decoupling
work (this crate's own earlier fix, same session) intentionally makes
Chrome pre-scale the scoreboard's capture *down* to roughly match its
real on-screen footprint - at a typical banner placement (~0.3-0.4x),
that's now well *under* 960x540, so PAUZE's fixed canvas wins instead.
The scoreboard's already-sharp, already-correctly-sized texture then
has to get resampled a second time to fit into that larger shared
canvas - and `composite_over` (the function doing that resample) used
**nearest-neighbor** sampling, reintroducing real, visible text
degradation - exactly the class of bug the whole zoom-fix was meant to
eliminate, just relocated to a second resampling step this crate's
earlier work never touched.

In short: **today's own zoom-fix broke an assumption
`overlay_layers.rs` was quietly relying on**, and this is a genuinely
new interaction, not a pre-existing bug independent of today's work.

## Fix applied (uncommitted)

`composite_over` switched from nearest-neighbor to bilinear sampling
(`crates/reco-core/src/render/overlay_layers.rs`) - resamples both
color and alpha channels properly instead of picking one texel. New
regression test `composite_over_bilinear_interpolates_a_downscaled_
striped_source` (a fine vertical-stripe source, deliberately not a
single hard edge - a single edge's exact pixel alignment turned out to
dodge every scale value tried by pure geometric bad luck across
several attempts, see the git history/scrollback for that whole
detour if useful context later; a striped source with an edge on
literally every column can't dodge). Verified: `cargo fmt --all
--check` clean, `cargo clippy -p reco-core -p reco-scoreboard -p
reco-gui --features tensorrt --all-targets -D warnings` clean, `cargo
test -p reco-core --lib` 220/222 (the same 2 pre-existing unrelated
CUDA failures, +1 new test vs. the 219 baseline). Release
`reco-gui.exe` rebuilt and confirmed (16:55) - includes this fix.
**Debug build was started right as the session paused - confirm it
actually finished (check `target/debug/reco-gui.exe`'s mtime) before
the user's next test**, and confirm no stale `reco-gui.exe` process is
still running from an earlier test (see
[[feedback_rebuild_gui_before_user_test]] and section 6 below - a
stale process was a real, separate false alarm earlier today).

**Deliberately NOT committed yet** - this fix is well-reasoned and
tests pass, but has *not* been confirmed against a real PAUZE+
scoreboard export (the exact `test4_long.mp4` scenario that surfaced
the bug). Commit only after that confirmation, per how the rest of
today went (multiple "this is it!" moments upstream of this one turned
out incomplete on the very next real test) - don't repeat that
pattern by shipping this one similarly unverified.

## Not done / next steps (READ FIRST NEXT SESSION)

1. Finish/verify the release build, then also build **debug** (not
   done this round at all).
2. Ask the user to re-run something close to `test4_long.mp4` (Pauze
   overlay on cuts + a scoreboard) and check text quality directly via
   frame extraction (not just eyeballing) - same rigor as every other
   check today, since "looks fine to me" has been wrong before this
   session.
3. Only commit + update this file's own "committed" framing once that
   test comes back clean. If it's *still* not fully clean, the next
   suspect (not yet investigated) would be the **outer** compositor's
   own GPU sampler now resampling a 960x540-dominated canvas a second
   time on top of this - see `RgbaOverlayCompositor`'s own bilinear
   GPU sampler in `rgba_overlay.wgsl`, unaffected by anything changed
   today, but worth eyeballing the math once more with this specific
   failure mode in mind if the CPU-side bilinear fix alone isn't
   enough.
4. **Nothing from this session has been pushed to `github/main` yet**
   - commits `72e1307e`/`090860fc` (the zoom/font/throttle fix + its
     handoff doc) plus the 5 commits from the day before are all still
     local-only. Ask before pushing once everything here is confirmed
     and committed, per usual.
5. Sections 1-6 below (already committed) remain accurate history -
   nothing in them needs correcting, this is a genuinely separate,
   later finding.

---

# Session handoff - 2026-08-25 (TGR_PC): design_size fix's DSF text-hinting bug found+fixed (zoom instead), scoreboard font-size legibility fix, queue-congestion throttle fix - all committed+verified

Direct continuation of 2026-08-24 below - user tested that session's
uncommitted `design_size` diff (mip-chain crash fix) for the first time.

## 1. `device_scale_factor < 1` breaks Chrome's text hinting - switched to CSS `zoom`

User's first real test: no more GetData-timeout crash (good), but
scoreboard text was **worse** than before, not better. Root cause: the
design_size fix drove Chrome's physical capture resolution via
`device_scale_factor` (CDP `SetDeviceMetricsOverride`). Since the
banner is usually a small fraction of the frame, the resulting DSF is
almost always well under 1.0 - and Chrome's font rasterizer assumes
DSF >= 1, producing visibly broken glyph hinting below that, worse
than a naive full-res-then-GPU-downscale would have looked.

Fixed (`crates/reco-scoreboard/src/runtime.rs`): `device_scale_factor`
now always stays at `1.0`. Scaling instead goes through CSS `zoom`
(Blink's own page-zoom mechanism - the same one behind a real
browser's Ctrl+-/Ctrl++), applied via a new `apply_zoom()` after each
`set_viewport()` resizes the CDP viewport to the *physical* target
size. `zoom` re-lays-out text (recomputes font metrics) for the final
physical size instead of sub-sampling glyphs rasterized for a
different one - the actual mechanism behind why real browser zoom
stays crisp at any level, which the DSF approach never was.

## 2. Team-label/period/etc CSS fonts too small at typical banner scale

Second test: clock/score now genuinely crisp (zoom fix confirmed
working), but "HOME"/"AWAY" and "1st Section" were still mush.
Real-frame extraction + pixel measurement showed why: at a typical
banner placement (~0.4-0.5x render scale), `.team-label`'s 18px design
font resolves to under 10 physical pixels tall - illegible with any
renderer, not a rendering bug. `.clock` (54px) resolves to ~29px and
was fine. Bumped `scoreboards/football/style.css`'s smallest text
elements (`.team-label` 18->26px, `.competition` 15->22px,
`.added-time` 20->26px, `.period` 22->28px, `.card-badge` 16->20px) -
package-only change, no Rust rebuild needed to test standalone
(confirmed via a new `dump_scoreboard.rs` throwaway example that
renders the real package at an arbitrary scale to a PNG).

## 3. Stale bundled `scoreboards/` copy - a real gotcha, not a code bug

Third test still showed no improvement after the CSS fix. Root cause:
`reco-gui/build.rs` copies `scoreboards/` next to the compiled binary
at build time (`target/{profile}/scoreboards`) so a release exe
doesn't need the dev-only `CARGO_MANIFEST_DIR` fallback - editing the
repo's `scoreboards/football/style.css` does nothing to an
already-built exe until it's rebuilt. Rebuilding picked up the fix
immediately. **Worth remembering**: any scoreboard-package-only change
still needs a rebuild to reach a previously-built exe, unlike a truly
filesystem-discovered debug build.

## 4. "1080p perfect, 2K a ramp" - false alarm, not a real resolution bug

User's next test (placement scale 0.3, 2560x1440 output) looked badly
warped even after both fixes above - worse than the font-size mush,
actual glyph-shape distortion ("1" reading like "7"). Investigated via
a new opt-in `RECO_SCOREBOARD_DUMP_DIR` env var in
`runtime.rs::capture()` (dumps every raw Chrome PNG to disk, removed
again before committing) to separate "Chrome's own capture is wrong"
from "something downstream (GPU compositor/encoder) corrupts it" -
raw captures were crisp at every scale tested (0.3, 0.4, 0.533),
proving the capture stage was never the problem. Confirmed the
shader's contain-fit math (`rgba_overlay.wgsl`) applies one uniform
scale to both axes - no non-uniform stretch is even possible there.
Confirmed via the real `reco-gui.log` that the export's own pipeline
*was* initialized at the correct 2560x1440 output size before the
scoreboard's first frame. Then a fresh 2K test (same debug-dump build)
came back **crisp end-to-end, matching 1080p** - the original "ramp"
result is now believed to have been the stale-bundled-directory issue
above (#3) or a transient stuck-scale from the queue-congestion bug
below (#5), not a resolution-dependent rendering bug. No code changes
resulted from this investigation beyond the throwaway debug dump
(reverted).

## 5. Queue-congestion throttle fix

Surfaced by the investigations above:
`reco-gui.log` showed repeated `"HTML renderer command queue is full"`
warnings during live placement dragging - `apply_zoom()`'s extra JS
eval (fix #1) made each `SetRenderScale` command slower to drain,
and `on_changed_scoreboard_placement` called
`apply_scoreboard_render_scale()` on every unthrottled pointer-move
tick, overflowing the 32-slot command queue during a fast drag. Fixed
by throttling `apply_scoreboard_render_scale()` itself (150ms, same
pattern as the existing `push_scoreboard_replay`/
`scoreboard_replay_last_push` throttle) - covers all 5 call sites at
once. The always-live position/size feedback
(`bridge.set_overlay_placement`) stays unthrottled right next to every
call site, so dragging still feels instant; only the
Chrome-re-render-for-quality step is debounced.

## Verification

`cargo fmt --all --check` clean. `cargo clippy -p reco-core -p
reco-scoreboard -p reco-gui --features tensorrt --all-targets -D
warnings` clean. `cargo test -p reco-scoreboard --lib` 13/13 (incl.
the real headless-Chrome DOM test). `cargo test -p reco-gui --features
tensorrt` 65/65. Both debug+release `reco-gui.exe` rebuilt with
`--features tensorrt`. User confirmed both 1080p and 2K exports crisp
end-to-end (raw capture *and* final encoded video, checked via direct
frame extraction + pixel-level crop/zoom, not just eyeballing the
preview) before this was committed.

## 6. Post-commit scare: "2K export is bad" - resolved as a stale-process/test-mixup, not a real bug

Right after committing, the user hit what looked like a second real
2K-specific regression: exports came out badly blurred/warped again,
apparently at random - some 2K exports fine, others bad, with no
correlation to settings, no errors logged, and (re-confirmed via the
`RECO_SCOREBOARD_DUMP_DIR` debug dump, temporarily re-added and
removed again) Chrome's raw capture staying crisp in every single
case, including runs whose final video was bad. Spent a long stretch
chasing real-seeming leads that all checked out clean (command-queue
congestion, browser crash/restart, VRAM exhaustion, encoder rate-
control settings, bitrate) before the user realized their own test
sequence had gotten tangled (an old `reco-gui.exe` process not
restarted after a rebuild at least once, confirmed via `Get-Process`
+ log timestamp cross-referencing). A final clean 3-test sequence
(1.7s / 90s / 300s, all 2560x1440, no PAUZE/cut-range, one fresh
still-open process, distinct output filenames per test, checked start
*and* end of each) came back **crisp at every duration** - confirms
the fix from sections 1-5 is genuinely solid; there was no second bug.

**Lesson for next time**: when a "same settings, worse now" report
shows up, check `Get-Process reco-gui | Select StartTime` against the
last build's mtime *before* spending time on new theories - a stale
running process is cheap to rule out and explains this exact
symptom pattern.

## Verification (final, post-cleanup)

Re-ran after removing the temporary debug dump a second time: `cargo
fmt --all --check` clean, `cargo clippy` clean, `cargo test -p
reco-scoreboard --lib` 13/13, `cargo test -p reco-gui --features
tensorrt` 65/65. Both debug+release `reco-gui.exe` rebuilt one more
time from the exact committed state (`72e1307e`) after the debug-dump
round-trip, confirmed via file mtime. No further code changes pending.

## Not done / next steps

- 2 throwaway diagnostic examples kept in
  `crates/reco-scoreboard/examples/` (`zoom_bench.rs`,
  `dump_scoreboard.rs`) - same precedent as the existing `gpu_bench.rs`,
  user's call, not deleted.
- The design_size diff's own "Not done" items from 2026-08-24 below are
  now superseded by this entry where they overlap (the crash fix and
  quality fix are both confirmed working) - anything not mentioned
  above from that list is still open.
- 4K export still not tested (user mentioned only 1080p/2K were tried).
- No upstream PR for any of this yet.

---

# Session handoff - 2026-08-24 (TGR_PC): scoreboard GetData-timeout crash root-caused + design_size fix, SESSION PAUSED before user test

**SESSION PAUSED 2026-08-24, user had to stop - pick up by asking the
user to test the fresh release build (see "Not done" below) before
anything else.**

Started by recovering from an accidentally-closed session: found 5
unpushed commits from earlier that morning
(`47bf6bc6`..`5b1c9e86`, continuing the PAUZE/scoreboard work from
2026-08-23 below - overlay quality + compositor perf, scoreboard
placement-reset, GPU-contention crash+recovery, perf benchmarks,
export-name suffix fix) plus an uncommitted, never-tested trilinear/
mipmap overlay-upload diff in `overlay.rs` sitting in the working tree.

## 1. Rebuilt release, user hit 16fps then a real crash

Rebuilt `reco-gui.exe` release w/ `--features tensorrt` to include the
5 commits + uncommitted mip diff. User's first test used the **debug**
exe by mistake (16fps - a red herring, debug is never used for fps
comparisons). Second test on the real release build **crashed**:
`Export failed - session: zero-copy: staging copy failed: GetData
timed out (>1M polls)`, exactly at the first cut-range window
transition (358s -> 452s), ~1 frame into the new window. GPU telemetry
during the crash showed only moderate load (53%/19% util, well under
the VRAM budget) - not an overload symptom, pointed at a race/resource-
lifecycle bug tied to the transition instead.

**Isolated via a controlled A/B**: stashed the uncommitted mip diff
(`git stash` - still there, see below), rebuilt release without it,
same export completed cleanly end-to-end (21415 frames, matching
known-good runs from 2026-08-23). With the diff back in, it crashed at
the same spot. **Confirmed: the mip-chain diff is the trigger**, not
GPU overload from today's other (committed) changes.

## 2. Root cause: reference_size double-duty in the overlay pipeline

User noted the mip-chain's quality "wasn't great anyway" even before
the crash, and that the scoreboard banner's *background* never changes
- only text (clock/score) does - prompting a rethink instead of just
debugging the mip approach.

**Real strategy chosen**: let headless Chrome itself rasterize the
scoreboard DOM at (roughly) its final on-screen physical size, via the
CDP `device_scale_factor` in `SetDeviceMetricsOverride` (CSS viewport
stays the package's fixed 1920x1080 design size - only the *physical*
capture resolution shrinks). Chrome's own text rendering/anti-aliasing
does the final-size rasterization instead of a GPU sampler minifying a
full-resolution capture - fixes quality at the source, not just the
crash, and needs no mip chain/GPU texture tricks at all.

**Found a deeper architectural conflict before writing any of this**:
`RgbaOverlayCompositor.reference_size` (drives the shader's contain-fit
placement math) was, in the current code, always re-derived from the
uploaded frame's own actual pixel dimensions on every `upload()` call -
intentionally, so `LayeredOverlaySource`'s shared canvas (sized to the
largest active layer, `47bf6bc6`'s own fix from that morning) can
legitimately change size frame-to-frame. Making Chrome's actual pixel
buffer smaller for quality would, unmodified, feed that smaller size
back into the SAME contain-fit math as a *logical* resize, silently
double-applying the scale-down. Flagged this to the user (bigger/
riskier than originally scoped) before proceeding -
**user chose "volledig doorzetten" (fully proceed)**.

## 3. Fix: `OverlayFrame.design_size`, decoupled from actual pixel size

New `design_size: (u32, u32)` field on `OverlayFrame`, independent of
`width`/`height` (the actual `rgba` pixel buffer size) - placement math
everywhere now keys off `design_size`, texture allocation/sampling off
the real pixel dimensions. Changed, all in one uncommitted diff (not
committed yet - see "Not done"):

- `crates/reco-core/src/render/overlay.rs`: `OverlayFrame.design_size`
  (+ `validate()` check); `RgbaOverlayCompositor` split into
  `reference_size` (design, drives the shader uniform, tracks
  `frame.design_size`) vs. new `texture_size` (actual GPU texture
  pixels, tracks `frame.width/height`, drives texture recreation).
  New `contain_fit_render_scale(design_size, output_size, placement)`
  helper - the same contain-fit math the shader uses, exposed for a
  producer (the scoreboard) to compute its own target render scale.
  2 new regression tests (`placement_uses_design_size_not_actual_
  texture_size`, `contain_fit_render_scale_*`).
- `crates/reco-core/src/render/overlay_layers.rs`: `composite_over`
  places using `source.design_size` but *samples* `source.width/
  height` (previously conflated); `LayeredOverlaySource`'s canvas
  reports `design_size` = max of layers' design sizes (independent of
  its own actual pixel buffer = max of layers' actual sizes). 3 new
  regression tests.
- `crates/reco-core/src/render/pause_overlay.rs`,
  `crates/reco-core/examples/composite_bench.rs`: `design_size` set
  equal to `width`/`height` (unaffected, PAUZE never pre-scales).
- `crates/reco-scoreboard/src/runtime.rs`: new
  `ScoreboardRuntime::set_render_scale(f32)` (public API) ->
  `RuntimeCommand::SetRenderScale` -> `set_viewport` now takes a scale
  param, applies `device_scale_factor` via CDP, returns the resulting
  physical capture size; `capture()` takes an explicit `design_size`
  param (always the package's fixed manifest viewport) separate from
  the physical `expected_size` it validates the screenshot against.
  Scale survives a lost-connection restart the same way `state_json`
  already does (both threaded through `run_worker`/`run_session`).
- `crates/reco-gui/src/export.rs`: computes render scale from
  `contain_fit_render_scale` (package viewport, export's own output
  resolution, `scoreboard_placement`) once, right after starting the
  export's `ScoreboardRuntime`.
- `crates/reco-gui/src/main.rs`: new `AppState::scoreboard_design_size`
  field (captured at runtime-start, since the runtime itself doesn't
  expose its package after moving it into the worker thread) + new
  `apply_scoreboard_render_scale()` helper, called from all 5 places
  placement or preview viewport size can change (`try_init`,
  `init_with_calibration`, `apply_scoreboard_settings`, the drag
  handler `on_changed_scoreboard_placement`, and the adaptive preview
  resize block).

## Verification

`cargo fmt --all --check` clean. `cargo clippy -p reco-core -p
reco-scoreboard -p reco-gui --features tensorrt --all-targets -D
warnings` clean (0 warnings). `cargo test -p reco-core --lib` 219/221
(2 known pre-existing unrelated CUDA failures on this machine, not
regressions - see 2026-08-20 entry below). `cargo test -p
reco-scoreboard --lib` 13/13 (incl. the real headless-Chrome DOM
integration test). `cargo test -p reco-gui --features tensorrt` 65/65.
Release `reco-gui.exe` rebuilt with `--features tensorrt` at 15:10,
includes all 5 unpushed commits + this uncommitted design_size diff.
**Debug build is now stale** (last built 10:56, predates all of
today's fixes) - don't use it for anything until rebuilt.

## Not done / next steps

- **User has not yet run a real export against this build** - session
  paused right as this was about to happen. First thing next session:
  ask for a test (ideally with scoreboard AND a PAUZE moment together,
  the narrowest edge case - see the design doc comments in
  `overlay_layers.rs` for why that combination can't be pre-scaled
  perfectly, only correctly, so it's worth eyeballing text quality
  there specifically) - confirm both (a) no more GetData-timeout crash
  at a cut-range window boundary, (b) scoreboard text actually looks
  sharp now, not just "not crashing".
- The whole design_size diff (7 files, all committed together as one
  change) is **still uncommitted** in the working tree - commit only
  after the user confirms the test above.
- Old buggy mip-chain diff still sits in `git stash@{0}` ("mipmap
  trilinear overlay fix (isolation test)") - superseded by the
  design_size approach, safe to `git stash drop` once the new fix is
  confirmed and committed. Don't drop it before that in case the new
  approach needs to fall back to it. (`stash@{1}` is an unrelated old
  WIP stash from 2026-07-14, not touched.)
- Debug `reco-gui.exe` needs rebuilding before any further debug-build
  testing (see [[feedback_rebuild_gui_before_user_test]]).
- The 5 commits from this morning are still unpushed to `github/main`
  (`git log --oneline github/main..HEAD` shows all 5) - push once the
  design_size fix is also committed, as one batch.
- Untracked stray file `scripts/match-logger/Match Logger.html.txt`
  (48KB, 21 Aug, next to the real `Match Logger.html` from 22 Aug)
  still sits in the working tree, never investigated - low priority,
  ask the user or just delete if confirmed to be a stale duplicate.

---

# Session handoff - 2026-08-23 (TGR_PC): PAUZE-overlay follow-up bug batch, committed+pushed (afc054ae)

Direct continuation of the "PAUZE" dip-to-black cut-range overlay
(commit `fe90ffb8`, previous session). User ran real GUI exports
against that build and reported a string of real bugs one by one, each
diagnosed via logs/frame extraction/code review, not assumed correct.
Full narrative in [[project_video_cutout_research]] ("STATUS
2026-08-23, update 2"); short version here.

## What was fixed, all in one commit (`afc054ae`)

1. **Leading cut-range silently not applied** ("de wedstrijd begint
   gewoon bij 0min") - `StitchJob::run`'s `skip_frames` used
   `start_time` directly instead of `keep_windows[0]`'s real first-kept
   position. Verified via CLI log ("skipping 3536 frames (first kept
   window starts at 118.00s)" instead of the old wrong "skipping 0
   frames").
2. **Integer overflow panic** in `extend_for_pause_overlay` on an
   open-ended `--pause-overlay` export (no `--end-time`) -
   `saturating_add` fix + regression test.
3. **Scoreboard showing "SHARKS FC" placeholder at t=0"** - the
   headless scoreboard browser always takes a screenshot immediately
   on start, before any real state is pushed. A first fix attempt
   (gate frames behind a "ready" flag) was proven insufficient by the
   user's own screenshots (race condition). Real fix: seed the
   runtime's *initial* state (computed for output frame 0 via a new
   `cut_range::output_frame_to_source_secs` inverse-mapping fn) before
   the renderer starts at all.
4. **Scoreboard clock stalling for a long time after a pause resumes**
   - the replay's `video_seconds` used a naive linear formula that
   didn't account for cut ranges; fixed using the same
   `output_frame_to_source_secs` fn.
5. **AI Tracking / Async Detect checkboxes and scoreboard settings
   (auto-cut kickoff, size, font, placement, banner color, logo size)
   not remembered across app restart.** Added app-level persistence
   (`GuiSettings::autocam_enabled`/`async_detect_enabled`/
   `scoreboard_settings`). Found a SECOND bug in the same area via a
   user follow-up report ("de positie en size van de scoreboard banner
   wordt niet goed opgeslagen") - a calibration-load code path was
   unconditionally re-applying the loaded calibration's OWN (often
   stale) copy of placement/banner/font/logo-size/auto-cut right after
   the app-level restore ran, silently discarding it on every
   calibration load (including at startup via Default Calibration).
   Fixed: calibration still supplies match-specific fields (Match
   Logger path, sync anchor, team logos, package/enabled); app-level
   settings now always win for the "preference"-style fields when
   present. Also fixed a same-function bug where restoring the
   placement's scale mid-restore fired a premature persist of
   not-yet-applied fields (font/logo/banner/auto-cut still stale at
   that point) - now re-persisted once more at the end of the restore
   so nothing drifts.
6. **Period label always "1st Half" even with >2 periods** - user
   wants "Section" always (confirmed via AskUserQuestion, real match
   has 4 periods). `scoreboards/football/scoreboard.js`'s
   `periodLabel` now always returns "Nth Section" - a plain JS asset,
   no rebuild needed for that part alone.
7. **New**: draggable start/end handles added directly on the main
   video scrubber timeline (`main.slint`), in addition to the existing
   small Start/End sliders in the export panel - needed a Slint
   z-order fix (handle TouchAreas declared after the Slider so they
   actually receive drag events instead of the Slider swallowing
   them).

## Verification

`cargo fmt --all --check` / `cargo clippy -p reco-gui --features
tensorrt --all-targets -- -D warnings` / `cargo test -p reco-gui
--features tensorrt` (64/64) / `cargo test -p reco-io` (50/50 + 3/3
doctests) all clean before building. Both debug+release
`reco-gui.exe` rebuilt with `--features tensorrt`
([[feedback_rebuild_gui_before_user_test]] /
[[feedback_always_tensorrt_build]]). `git fsck --full` clean (only
harmless dangling objects) before pushing per
[[feedback_git_object_corruption]]. Committed (`afc054ae`) and pushed
to `github` (RufanMelfor/reco-video-stitcher-rig) main.

**Not yet hands-on re-verified by the user in this exact build** - in
particular the Sharks-FC seed fix and the calibration-vs-app-level
placement fix both still need a real export + an actual app restart to
confirm. Ask next session if not already confirmed.

---

# Session handoff - 2026-08-22 (TGR_PC): Match Folder picker + export-to-folder + sync-offset prompt, upstream PR #476, Skia PR #477, scoreboard merged into main

## 7. Todo review + Skia renderer PR #477 opened; cut-range PR explicitly held back

User asked "wat staat er nog op todo" - compiled current state across
memory + SESSION_HANDOFF (see chat for the full list: awaiting user
input on YOLO recall + app icon, several merged-but-unconfirmed
features, 16 open upstream PRs with zero merges in a month, known
non-blocking bugs, paused research threads).

User picked "Skia-renderer-switch and cut-range-feature: merged in
main, no upstream PR yet" to act on. Scoped both before touching code:
- **Skia renderer**: verified trivial - `origin/main` still uses
  `renderer-femtovg-wgpu`, the exact same 2-line change from the
  original 2026-08-12 work (Cargo.toml feature flag +
  `default-font-family: "Segoe UI"`) applies cleanly with zero
  adaptation, no Calibration/AppState coupling at all. Built fresh off
  `origin/main`, verified (fmt/clippy/test 11/11/doc all clean, same
  pre-existing #423-tracked failures elsewhere), opened as
  [PR #477](https://github.com/reco-project/video-stitcher/pull/477),
  confirmed `MERGEABLE`.
- **Cut-range**: scoped as much bigger - `async_detect.rs` (the Async
  Detect Thread feature this depends on for its VramPool/FrameBuffer
  flush-on-cut logic) doesn't exist on `origin/main` at all, so a real
  port would mean porting that feature first too. Flagged this to the
  user before starting anything. **User said explicitly: hold off,
  still testing this feature** - no PR opened, none planned until told
  otherwise. See [[project_video_cutout_research]].

## 5. Sync-offset detect+save prompt after Select Match Folder

User caught a real gap: a calibration copied from Default Calibration
carries a `sync_offset` tuned for whatever footage it came from, which
almost certainly doesn't match a new match's camera start-time gap.
Added a confirm popup right after Select Match Folder (only on the
copy-from-default path, not when reusing an existing per-match file)
offering to run the existing "Detect Sync Offset" job and save straight
to the new calibration file. `AppState::pending_sync_offset_autosave`
flag routes the save; `start_sync_offset_detection()` extracted so the
manual button and the new prompt share the same job. Built, verified
(fmt/clippy/test 57/57), merged to internal `main`, both debug+release
rebuilt.

## 6. Same feature ported to PR #476 - corrected an over-estimate

User then asked to update PR #476 with this. First attempt: found the
whole sync-offset-detection mechanism (`sync_offset.rs`,
`on_compute_sync_offset`) doesn't exist on `origin/main` at all, and
told the user this needed a large new port. **User pushed back** ("but
wasn't sync-offset already standard in reco?") - correctly. Checking
precisely: the `sync_offset` field, manual slider, and
`save_calibration()` are all core and already upstream; only the
~90-line auto-*detection* wrapper was missing, and every dependency it
calls (`CalibrationPipeline`, `CalibrationConfig`, `calibration_io`)
already exists unchanged on `origin/main`. So it was a small, clean
port after all - ported as a second commit on the PR #476 branch
(`a57c3a06`), verified clean, PR body updated, pushed. See
[[project_match_folder_picker]] for the fuller lesson on not
extrapolating a scope estimate without checking the actual dependency
chain.

**Pushed to `github` (RufanMelfor/reco-video-stitcher-rig) main.** Two
new features built and merged same day (see
[[project_match_folder_picker]] for full detail), then ported to a new
upstream PR, then the previously-held-back scoreboard branch merged in
too on user request.

## 1. reco-gui "Select Match Folder" picker (`951fe85d`)

New `crates/reco-gui/src/match_folder.rs` module (unit-tested) scans a
chosen match folder for case-insensitive `Left`/`Right` subfolders,
collects+sorts each camera's videos, and derives a per-match calibration
filename from the folder name. New "Select Match Folder..." button
fills left/right/calibration in one pick (replaces, doesn't append to,
the current selection). Calibration: reuses an existing per-match file,
else copies the configured Default Calibration into the match folder,
else leaves it unset. Extracted the duplicated InputPath-building logic
from the manual Left/Right pickers into `input_path_from_picks`, reused
by all three pickers.

## 2. Export output suggested inside the match folder (`9290f26e`)

When the current selection came from Select Match Folder, the export
dialog now suggests an output path inside that folder, named after it
(team names + date) - new `AppState::match_folder` field, cleared by any
manual left/right re-pick.

Both verified: `cargo fmt --check`/`clippy --features tensorrt
--all-targets -D warnings`/`test --features tensorrt` all clean (44/44
tests), debug+release builds both succeeded.

## 3. Upstream PR #476 - hand-ported, not cherry-picked

User asked to open a PR, which surfaced that PR #435 (Default
Calibration, a dependency of the match-folder feature) had drifted into
`CONFLICTING` against current `origin/main` - same architecture
regression [[project_v054_upstream_sync]] found in August: upstream's
current main is still on `MatchCalibration`/`cal_baseline_layout`/
`bridge.renderer()`, not the `Calibration`/`bridge.engine()` model our
main uses. Hand-ported all 3 commits (plus #435's own commit) onto a
fresh branch off `origin/main`, resolving real conflicts by checking
each cherry-pick's actual diff (`git diff <commit>~1 <commit>`) against
what the 3-way merge proposed - several conflict "resolutions" the
naive cherry-pick suggested were actually unrelated content bleeding in
from OTHER not-yet-ported features on our internal main (AKAZE preview,
segment-restart-persistence, in-app ROI editor tests) and had to be
dropped, not merged in. Also: upstream's `main.slint` has no
`FlatButton` (fork-only unmerged restyle) - swapped to plain `Button`.

Verified reco-gui itself 100% clippy-clean on the ported branch (used
temporary local `#[allow(...)]` in unrelated crates to see past 6
pre-existing, already-tracked-by-#423 failures in
reco-core/reco-detect/reco-io/reco-autocam, then reverted those before
committing - never fix another PR's scope as a side effect). 25/25
tests, fmt clean, both debug+release build clean.

PR #476 opened, confirmed `MERGEABLE` (no conflicts) against
`origin/main`. Old PR #435 closed with a comment pointing to #476.

**Owner status check while doing this**: `gh pr list --author
RufanMelfor --state all` shows the owner has merged ZERO of the 16
still-open PRs in over a month since the first batch (2026-07-16). Two
(#422, #424) closed unmerged back on 2026-07-21, likely superseded by
the [[project_v054_upstream_sync]] hand-port rather than rejected.

## 4. Scoreboard merged into main (user request)

User asked mid-session to include the scoreboard in the next build for
continued testing. `feat/scoreboard-overlay` (held back since
2026-08-21 pending more testing, see
[[project_match_logger_scoreboard]]) merged into `main` with `--no-ff`,
clean (no conflicts, verified via `git merge-tree` first). Combined
build (match-folder-picker + export-to-folder + scoreboard) verified:
`cargo fmt --all --check` clean, `cargo clippy -p reco-gui --features
tensorrt --all-targets -D warnings` clean, `cargo test -p reco-gui
--features tensorrt` 57/57 pass, both debug and release
(`reco-gui.exe`, `--features tensorrt`) built successfully. Pushed to
`github` main (`181c6100`).

## 5. PR #476 CI: Security Audit job failing, unrelated to this PR

User asked about a "Rust CI / Security Audit (pull_request) Failing
after 17s" GitHub notification on #476. Root cause (pulled the actual
job log via `gh run view --job <id> --log`): the job's `cargo install
cargo-audit` step fails because `cargo-audit`'s own transitive
dependency `kstring@2.0.4` now requires rustc 1.96.0, but this repo's
CI pins 1.92.0 (same version the separate, passing "MSRV (1.92.0)" job
uses). Confirmed via `gh run list --workflow "Rust CI" --branch main`
that this same job has been failing on upstream's own `main` branch
since 2026-07-21 - a repo-wide, pre-existing CI/tooling issue, nothing
to do with #476's content. All other checks on #476 pass (Check &
Lint, Test, Documentation, every platform Check, cargo-deny, CLA,
Secret Scanning).



**Pushed to `github` (RufanMelfor/reco-video-stitcher-rig), branch
`feat/scoreboard-overlay` (16 commits). Not merged into `main` yet -
waiting on the user's go-ahead after more testing.** Separately,
`scripts/match-logger/Match Logger.html` itself is already on `main`
(`aaccc840`, pushed directly - just the one file pulled in from the
otherwise-stale `feat/mlpipe-gui` branch).

Big session: built the Match Logger -> reco-gui scoreboard overlay
feature end to end, starting from a port of the paused PR #474 work
([[project_scoreboard_overlay_pr474]]), through several rounds of real
user testing on their own 03 OJC vs Berghem Sport footage and demo
data, each round surfacing a real bug.

## What shipped

1. **Ported PR #474** (`204b0c57`) cleanly onto current `main` rather
   than resuming the stale `scoreboard-on-main` branch (it had fallen
   ~209 commits behind) - `reco-scoreboard` crate (headless-Chrome
   package runtime), `scoreboards/football` package,
   `render/overlay.rs` compositor. `main.rs`/`export.rs`/`main.slint`
   needed a 3-way diff apply (not a plain checkout) since they'd
   diverged since the fork point - applied cleanly, no conflicts.
2. **Match Logger import + replay engine** (`ae4bf166`) -
   `crates/reco-gui/src/scoreboard_import.rs`: parses a Match Logger
   export, `state_at(events, sync_anchor, video_seconds)` replays
   score/cards/clock at any point in the video's timeline (pauses
   freeze the clock, added time, clock stays continuous across
   halves). Moved the Scoreboard controls from the right Lens panel
   into a new SCOREBOARD card under DETECTION ZONES (left sidebar);
   also collapsed SEAM/RIG ALIGNMENT by default while in there (user's
   original ask that started the session).
3. **Live preview + export actually driven by the replay** (`ea614b75`)
   - PR #474 could previously only freeze one static state at export
   start; now both live scrubbing and export encode call `state_at()`
   on a timer, so score/cards/clock genuinely change at the right
   moments instead of staying frozen.
4. **Drag-to-reposition + logo/font/size/color editor** (`7d8ac8f5`) -
   new generic `OverlayPlacement` in the compositor (position + scale,
   package-agnostic, not football-specific), "Edit Scoreboard..."
   in-place editor mirroring the existing ROI-editor pattern.
5. **"Auto-cut kickoff lead-in + pauses" toggle** (`71c05f54`) -
   derives cut ranges from the same event log (pre-roll before
   kickoff, one range per logged pause, 2s context kept at each
   boundary), reusing the existing cut-range timeline; only from
   events actually in the log, never inferred gaps.
6. **Match Logger: optional card-tracking toggle** (`1f8bf666`) - hides
   the whole Cards section for age groups that don't play with cards
   yet (prompted by the user's own JO11 use case).
7. **Calibration persistence** (`2e245713`, `cd5a77cd`) - new
   `ScoreboardSettings` in `reco_core::calibration`, saved/restored on
   Save/Load Calibration. Stores file *paths* (Match Logger export,
   both logos), not embedded content - calibration files are capped at
   1MB (`MAX_CALIBRATION_FILE_SIZE`).

## Real bugs found testing on the user's own footage/data

- **Release build had no scoreboard packages at all** (`72cd8831`) -
  the dev-only `CARGO_MANIFEST_DIR` discovery fallback is
  `debug_assertions`-gated on purpose (a release binary shouldn't leak
  the build machine's source path); nothing copied `scoreboards/` next
  to a locally-built release exe. Fixed by having `build.rs` bundle it
  next to both profiles, matching how a packaged install would ship it.
- **Drag-to-reposition had "een gigantische vertraging"**, and
  separately **the banner didn't update after Load/sync-point/style
  changes** (`bca0ff15`, `d3646ae8`) - `vsync_render_tick` only does the
  actual redraw while playing/seeking/`preview_dirty`; several
  scoreboard handlers updated state instantly but never set
  `preview_dirty`, so nothing visible happened until some unrelated
  redraw trigger came along. Same root cause, found and fixed twice as
  more handlers were added over the session (placement/style first,
  then Load/sync-point).
- **"duplicate scoreboard id" logged as `error!` and shown as a
  persistent GUI error** (`5e3eece2`) - direct side effect of the
  build.rs fix above (debug builds now find the package via two
  overlapping roots); added `DiscoveryIssueSeverity` (Info for an
  expected root overlap vs Warning for a real problem) so a
  successfully-loaded package never shows a false alarm.
- **Card badge stayed visible showing "0 0" even with zero card
  events** (`2cc6314a`) - real CSS bug:
  `.card-badge { display: inline-flex }` ties `[hidden]`'s UA-default
  `display:none` on specificity, and an author stylesheet wins that tie
  - `hidden` was a no-op, and since the badge only ever writes its text
  when shown, it just displayed the static HTML placeholder "0"
  forever. Fixed with a combined `.card-badge[hidden]` selector.
- **Logo overlapped the HOME/AWAY label** (`bf231e0a`) - was
  `position: absolute` floating in the top corner; moved inline next
  to the team name via a flex row (mirrored for the away side).
- **Period label assumed 2 halves** (`bf231e0a`) - hardcoded "1st/2nd
  Half" map read wrong for e.g. youth quarters; now reads
  `sport.periodCount` (2=Half, 4=Quarter, else=Period).
- **Misaligned scoreboard-edit drag handle** (`0652b097`) - the
  placeholder rectangle assumed the banner sits centered in its
  package canvas; football actually anchors to the bottom
  (`.broadcast-safe-area`). Removed the handle entirely per the user's
  own feedback ("ik hoef geen polygon te zien") - the drag math itself
  was already pixel-accurate, only the decorative overlay disagreed
  with reality.
- **Misleading "Got a timeout while listening for browser events"
  error** (`8fd22db7`) - root-caused as a harmless `headless_chrome`
  crate quirk (a 30s idle timer on browser-level tab-lifecycle events
  our single-tab package never generates, unrelated to the actual
  page connection used for updates/captures). Raised to 6h.

## Demo/test data

`D:\VOETBAL_VIDEO\Berghem Sport J011-1\03 OJC -Bergem Sport 04072026\
OJC_vs_Berghem_Sport_DEMO.json` - hand-built Match Logger export
matching the real duration of the first L/R camera segment pair
(~1223s, ffprobe-measured), used throughout for testing. Current
content: OJC vs Berghem Sport, 2 periods, cards disabled, 3 pauses
(45s/60s/45s), 2 goals, one added-time call. Re-verified against the
real `state_at()`/`derived_cut_ranges()` code (a throwaway test, run
then reverted - not committed) every time it was edited, which caught
two of my own timestamp-arithmetic mistakes before they shipped.

## Not done / next steps

- Branch not merged into `main` yet - needs the user's go-ahead once
  they're satisfied with testing.
- Sync anchor / import / style are session-only in the *live-manual*
  editor path (no Match Logger loaded) - only the Match-Logger-driven
  replay path persists, via the new calibration settings.
- No upstream PR planned for this feature yet.
- The user's "kan ik niet alsnog een andere kiezen" (can't pick a
  different Match Logger file after already loading one) report is
  suspected to be the same missing-`preview_dirty` bug (fixed in
  `d3646ae8`, landed before this was reported) but not yet explicitly
  re-confirmed by the user after that specific fix.

# Session handoff - 2026-08-20 (TGR_PC), continued: cut-range GUI hardening

**Pushed to `github` (RufanMelfor/reco-video-stitcher-rig) main,
`3aef5a20`.** Direct continuation of the cut-range feature (video+audio+
CLI shipped earlier the same day, see the "Video cutout research"
section further down / [[project_video_cutout_research]]) - user asked
"kan ik dit ook in de GUI testen?", chose the full drag-timeline UI over
a numeric-only stopgap, then found 4 real bugs testing it for real on
their own footage. All 4 root-caused and fixed, verified on a real
2-file DJI export (30fps sustained, no crash, memory returns to a
stable baseline after export).

## The 4 bugs, in the order found

1. **Scrubber invisible on cold startup, only appeared after "+ Add
   cut"** - first fix attempt (`638876c6`, a `track-width` local-property
   change in the cut-range bands) was real but logically couldn't have
   been the actual cause (cut-ranges is empty before any cut exists, so
   that code path never even runs pre-Add-Cut). Real fix (`3aef5a20`):
   one explicit `window().request_redraw()` kick 150ms after the event
   loop starts, matching an existing documented gap in this renderer
   ("BeforeRendering fires even if nothing marked the window dirty
   yet" - previously only covered during playback/seeking, never at
   idle cold-start). **Not yet hands-on verified by the user** in this
   final build - only passively screenshotted in an earlier, since-
   superseded build.

2. **"VRAM pool exhausted" / "GetData timed out" crashing multi-window
   (cut-range) exports** - two stacked bugs in `run_buffered`
   (`638876c6`): a pop-before-check drain-loop bug (pre-existing,
   predates cut-ranges, silently dropped one VRAM slot whenever
   `frame_limit` not EOF stopped the steady-state loop) plus a missing
   final drain of leftover buffered frames at window boundaries. Fixed
   and verified via a 6-window CLI stress test (pool fully restored
   every window).

3. **A SEPARATE, more serious hang found after (2) shipped**: multi-
   window cut-range exports with **Async Detect enabled** hung
   indefinitely (20+ min, no error) at a window boundary - traced to
   `AsyncDetectThread`/`pending_finishers` never being drained between
   `run_buffered` calls. Each window restarts its produce-index counter
   at 0; a leftover unresolved async-detect result from the previous
   window gets handed to the new window's first `resolve_pending_
   world_state` call instead (FIFO is the only correlation - the
   ordering check is a `debug_assert!`, a no-op in release), which
   permanently desyncs the FIFO stream until a later `recv()` blocks
   forever waiting for a result that can never arrive. This is what a
   real GUI export actually hit, reported as "GetData timed out (>1M
   polls)" - that D3D11 timeout is a downstream symptom (frames stopped
   flowing once the pipeline deadlocked), not the root cause. Fixed
   (`739251ca`): drain exactly `pending_finishers.len()` stale results
   at the end of every `run_buffered` call. **Verified twice**: (a) CLI
   repro (2-file chained input, 2 cut ranges, `--async-detect`) hung
   20+ min before the fix, completed cleanly (4196 frames/330s) after;
   (b) the user's own real GUI export on their actual 2-part DJI
   footage (`ScrubberTest_v5`) completed at a sustained ~30fps with
   `frame_index` perfectly continuous across both cut boundaries
   (checked directly in the `.events.jsonl` - zero gaps/duplicates).

4. **Cut-range NumEdit fields could only take one keystroke before
   needing a re-click** - `on_cut_range_update` (fired on every
   keystroke, not just commit) called `sync_cut_ranges`, which always
   replaces the whole `cut-ranges` `ModelRc`, tearing down and
   recreating every row's widgets including the one being typed into.
   Fixed (`3aef5a20`): patch just the edited row via `Model::
   set_row_data` instead of replacing the model. **Not yet hands-on
   verified by the user.**

## A 5th, unrelated finding from the same session: silent TensorRT->DirectML fallback

Every `cargo build` this session (until caught) omitted
`--features tensorrt` - not a default feature
(`crates/reco-gui/Cargo.toml`: `default = ["autocam", "ort"]`) - so
detection silently ran on DirectML instead, no error/warning anywhere.
This alone dropped real export speed from the user's normal ~30fps to
17-22fps, independent of any cut-range bug. **User made this an
explicit standing rule going forward**: every `reco-gui`/`reco-cli`
build on this machine gets `--features tensorrt`, no exceptions - see
[[feedback_always_tensorrt_build]]. Same underlying gotcha as
[[project_tensorrt_sdk_setup]] from 2026-08-13, just re-triggered by a
long unattended build streak. Also: **user asked to always be asked
before running any build** ([[feedback_ask_before_building]]) - a
`cargo run`/`build` fired without asking mid-investigation is what
surfaced this whole rule.

## Verification

`cargo test -p reco-core` (194 passed, 2 pre-existing unrelated CUDA
failures), `clippy -p reco-core -p reco-gui --features tensorrt --all-
targets -- -D warnings` (clean apart from the pre-existing unrelated
`reco-gui/src/settings.rs:398,406` `field_reassign_with_default`
warning), `fmt --check` clean. Both debug+release `reco-gui.exe`
rebuilt with `--features tensorrt` including all 4 fixes above -
**this is the user's current working build**; passed to the user for
their own hands-on retest (drag/type interactions - per
[[feedback_synthetic_gui_automation_risk]], not something to simulate).
User's own real export (2-part DJI chained input, 2 cut ranges, Async
Detect + TensorRT + 8-bit-reduced lookahead) completed cleanly at
~30fps; process memory monitored live throughout (PowerShell polling
every 15s) - held steady ~3.7GB/9.3GB (working set/private) during the
export with no growth, dropped to a stable ~3.2GB/6.6GB afterward and
stayed flat for 5+ minutes - no leak.

## Not done / next steps

- Bugs 1 and 4 above (scrubber kick, NumEdit patch) are code-complete
  and committed but **not yet hands-on retested by the user** in this
  final build - ask for confirmation next session if not already given.
- No `Calibration.cut_ranges` persistence yet (still session-only,
  resets on calibration reload - flagged as a likely follow-up since
  the feature's first CLI slice, never picked up).
- No live overlap-prevention between two dragged/typed cut-range bands
  in the GUI (export-time validation catches it with a clear error -
  the user hit this once today, `cut_ranges: cut ranges overlap`,
  understood it as working-as-intended, not a bug).
- Upstream PR for the whole cut-range feature (CLI + audio + GUI) still
  not opened - user explicitly wants one once "further along"
  ([[project_upstream_pr_workflow]]'s pattern), not decided this
  session to be far-enough-along yet.
- The GetData-timeout investigation initially (incorrectly) suspected a
  D3D11VA/concat-demuxer seek-into-second-chained-file bug before the
  real async-detect cause was found - ruled out via 2 separate clean
  CLI repros (single seek into file 2: fine; 2-window cut-range reseek
  into file 2, no async-detect: fine) before isolating async-detect as
  the actual variable. Worth remembering only as a methodology note (a
  plausible-sounding first theory was wrong; kept narrowing variables
  one at a time rather than trusting it) - not a real remaining
  concern, chained-file seeking itself checked out clean.

---

# Session handoff - 2026-08-20 (TGR_PC)

**SESSION PAUSED 2026-08-20, user said "dit werkt, maar moet nog veel
aan gebeuren. Dit gaan we later doen."** (works, but needs a lot more -
picking this up later). Reviewed community PR #474 against
`reco-project/video-stitcher` (external contributor `wendibus` / Björn
Zelter): an HTML/CSS/JS scoreboard-overlay system - manifest-driven
package discovery, a sandboxed Chrome/CDP renderer, a sport-neutral
wgpu RGBA compositor after Autocam/before encode. This is exactly what
[[project_overlay_features_scoreboard_logo]]'s 2026-08-14 research
concluded reco-core needed; someone else built it first.

**First correction from the user mid-session**: I initially built the
PR's branch as-is (based on the v0.5.4 release point) and told the user
to test that. User caught it - **wrong base, doesn't have async-detect
or anything merged since.** Fixed by creating a fresh branch
`scoreboard-on-main` (worktree `D:\CLAUDE\worktrees\scoreboard-on-main`)
and rebasing the PR's 7 scoreboard-specific commits directly onto
current `main` tip (not a merge - PR history predates the v0.5.4-sync
hand-port, so `main` and the PR branch share no clean merge-base;
`git rebase --onto main <fork-point>` replays just the scoreboard
commits, keeps original wendibus/Zelter authorship).

**Conflicts resolved by hand, not blind accept-theirs** (each verified
against current architecture, not guessed):
- `render/mod.rs`/`render/pipeline.rs`: new `overlay` module + composite
  wrapper combined with `color_match` and the current
  `render_to_target*` signatures (`color_correction`/
  `multiband_blend_enabled`/`show_seam_line` args added to `main` after
  the PR's fork point).
- `session/mod.rs`: the PR's struct-field diff dragged in stale
  `detection`/`ball_tracker`/`panner`/etc. fields as merge context -
  those already moved into `StitchCore` in the post-refactor
  architecture; kept only the genuinely new `overlay_source` field.
- `main.rs`: dropped the PR's dead `MatchCalibration`/`IntentTranslator`/
  old `detect::director::ViewportPosition` imports (pre-v0.5.4-sync
  naming, see [[project_v054_upstream_sync]]), kept current
  `Calibration`/`geometry::ViewportPosition`, added the new
  `OverlayFrame`/`OverlayFrameSource` imports.
- `main.slint`: two additive UI blocks (Audio Sync vs. Scoreboard
  section) - kept both.
- Cargo.toml/Cargo.lock: unioned workspace members, regenerated the
  lock via `cargo build`.
- One real post-rebase compile error: `PreviewBridge`'s field is
  `engine` (`StitchCore`), not `renderer` - PR predates that rename.
  Fixed and committed separately.

`cargo test -p reco-scoreboard` all green (13 tests, including a real
Chrome DOM/transparent-capture/live-editor-publish test) on the ported
branch. Noticed but did NOT fix: `cargo clippy --all-targets` fails on
`crates/reco-gui/src/settings.rs:398`/`406`
(`field_reassign_with_default`) - pre-existing on `main` itself (from
the ball-coast-secs commit), unrelated to this port.

**Second ask: user's real product is football/soccer, not basketball**
("mijn scoreboard moet niet basketball zijn maar voetbal"). The PR's
`scoreboards/basketball/` is explicitly documented as a reference
fixture only (`scoreboards/README.md`), and `scoreboards/AGENTS.md`
already has a full "add football" walkthrough as its worked example -
followed it directly. Added `scoreboards/football/` (manifest, HTML,
CSS, JS, `assets/`), sport-neutral contract only (no Rust/Slint
changes):
- match clock counts up and carries across halves (not basketball's
  per-quarter countdown); "Next half" advances the period and clears
  added time only, never resets the clock or cards.
- period label mapped from `game.period` (1st/2nd Half, Extra Time 1/2,
  Penalties) instead of "Q2 / 4".
- yellow/red card badges per team (hidden at zero) replace fouls; cards
  accumulate for the whole match, unlike basketball's per-quarter
  team-foul reset.
- added-time badge (`+N'`) shown when `sport.addedTime > 0`.

Added 2 new Rust tests mirroring the basketball coverage exactly
(`bundled_football_is_discovered` in `discovery.rs`,
`football_api_updates_dom_and_keeps_transparent_pixels` in
`runtime.rs` - real Chrome, real DOM assertions, real editor-publish
round trip). `cargo test -p reco-scoreboard` now 15/15 green. No Rust
rebuild needed to pick up the package itself (filesystem-discovered at
runtime) - just restarted the running debug `reco-gui.exe`.

**Both debug+release `reco-gui.exe` built and verified running**, in
the worktree's own `target/` (not the main checkout's usual build
output):

```
D:\CLAUDE\worktrees\scoreboard-on-main\target\debug\reco-gui.exe
D:\CLAUDE\worktrees\scoreboard-on-main\target\release\reco-gui.exe
```

User confirmed the football scoreboard renders and works, but said "a
lot still needs to happen" without specifying what yet - **paused here
on the user's call, nothing more decided this session.**

**Not done**: no commit/push to any remote (all work is local commits
on `scoreboard-on-main`, on top of `main`, in the dedicated worktree -
`main` itself untouched). No merge/PR decision made about PR #474
itself (still an open external PR upstream, untouched by any of this).
The original `D:\CLAUDE\worktrees\pr-474-scoreboard` worktree still
exists holding the PR's untouched original branch, purely as a
reference to compare against - **not** the one to keep testing from,
superseded by `scoreboard-on-main`. Next session: ask the user what
specifically needs to change before picking this back up (design pass?
more fields? GUI polish? something else) - no todo list exists yet
beyond "much more to do."

# Session handoff - 2026-08-17 (TGR_PC, continues 2026-08-16)

**SESSION CLOSED 2026-08-17, user said "ik ga afsluiten voor vandaag"
- pick up from here next time.** Short session: built a new "Ai
Learning" ball-rich frame batch for Label Studio (4 new source videos,
LS project 24, 100 tasks/100 predictions) using the documented
`yolo26s_v4_imgsz1920` checkpoint, not soccana and not the unvalidated
tiled checkpoint - full detail in the dated section right below. The
big GPU-optimization thread from 2026-08-16 (further below) is
unchanged/still paused - nothing new there today.

One-line state on that thread (unchanged from yesterday): single-worker
async-detect is the one shipped win (1.22-1.42x, has a reco-gui
checkbox, commit history on `feat/async-detect-thread`); dual-worker
and CUDA-graphs were tried and reverted; batch-L+R was measured (1.06x)
but not integrated - pick that up first if resuming the optimization
thread. Nothing merged to `main`.

## 2026-08-17: "Ai Learning" ball-rich frame batch exported to Label Studio, model-choice correction, re-hit a known LS API quirk

User asked for a Label Studio pre-label export from 4 new raw videos in
`D:\VOETBAL_VIDEO\Berghem Sport J011-1\Ai Learning` - 2 recordings
(`0001` = 2026-05-30, `0005` = 2026-06-03), each with L+R cameras
(3840x2880 HEVC, ~20.4 min each), max 25 frames/video, only frames
with >=1 ball. Asked the user how to scope the LS project - **1
combined project for all 4 videos** (chosen, same pattern as the
earlier "01 Vierluik"/"02 RPC" per-source projects) vs. 2 separate
projects vs. direct-into-project-8.

**Model correction mid-session**: first pipeline run used `soccana.pt`
(matching the old convention) but the user stopped it before upload and
said to use **"de laatste ONNX"** instead. Found 2 undocumented-as-
"latest" candidates on disk with no clear winner from the filename
alone - asked the user which:
- `round4/runs/yolo26s_v4_imgsz1920/weights/best.pt` (2026-08-15,
  **documented, real-app-tested**, "best-tested checkpoint of the whole
  project" per `YOLO26_Training.md` / [[project_yolo26n_training_pipeline]])
- `round4/runs/yolo26s_tiled1920_full/weights/best.onnx` (2026-08-16,
  newer by file timestamp, but **undocumented/unvalidated** - needs 2x
  tiled L/R inference that isn't wired into `select_ball_rich_frames.py`
  or any production `reco-detect` path yet)

**User picked `yolo26s_v4_imgsz1920`.** Used the `.pt` weights (not
`best.onnx`) for the actual local pre-labeling pass: this machine's
`onnxruntime` only has `CPUExecutionProvider` (no `onnxruntime-gpu`
installed), while `.pt` runs on GPU via torch/CUDA - identical trained
weights, just a much faster backend for ~1600 candidate frames.
`best.onnx` remains the file that would eventually ship to
`reco-detect` itself; wasn't needed for this pass.

**Pipeline** (`select_ball_rich_frames.py`'s `process_video()`,
interval=3s, top_k=25, conf=0.15, imgsz=1920, camera tags
`0001_left`/`0001_right`/`0005_left`/`0005_right`):

```
0001_left:  408/408 candidates had >=1 ball, kept 25
0001_right: 250/408 candidates had >=1 ball, kept 25
0005_left:  408/408 candidates had >=1 ball, kept 25
0005_right: 399/408 candidates had >=1 ball, kept 25
```

Flattened via `package_yolo_for_labelstudio.py`'s convention (100
images total), created **LS project 24**, title corrected afterward to
`"Ai Learning - yolo26s_v4_imgsz1920 pre-labels"` (briefly carried a
leftover "(soccana)" title from the original plan).

**Real bug hit, and it was avoidable**: the fresh upload driver script
assumed `POST /api/projects/<id>/import`'s response includes
`task_ids` - it doesn't on this LS instance (only `task_count`/
`file_upload_ids`). **This exact quirk was already documented** in
[[project_yolo26n_training_pipeline]] from the 2026-08-09 and
2026-08-12 batches ("this API's response omits `task_ids` on LS 1.23.0
- match tasks back by filename via `GET /api/tasks` instead") - missed
because the new script wasn't checked against that memory before being
written. All 100 images were still correctly uploaded as tasks
(`task_count:1` each, confirmed via `GET /api/tasks?project=24` ->
`total=100`, zero duplicates) - just missing predictions. Fixed with a
short follow-up script: fetched the real task list, matched each
task's hash-prefixed `data.image` filename back to the flat label files
(filenames unique in this batch, so suffix-matching is safe - no repeat
of the earlier duplicate-filename orphan bug), posted predictions
retroactively. No re-upload, no data loss. Final verified state: 100
tasks / 100 predictions / 0 annotations, 0 unmatched.

**Lesson, now hit 3 times (2026-08-09, 2026-08-12, 2026-08-17): always
check [[project_yolo26n_training_pipeline]] for known LS-instance API
quirks before writing a new upload/import driver script**, don't
re-derive the response shape from scratch each session.

Also explained the SAHI-style tiled-1920 training session to the user
in English on request (pure recap, no new facts - already fully logged
in the 2026-08-15/16 sections below and in `YOLO26_Training.md`);
reconfirmed the tiled checkpoint is still not wired into any production
path, which is why today's batch deliberately used the non-tiled,
documented checkpoint instead.

**Not yet done**: user's review/correction pass on LS project 24 (100
tasks, currently all raw predictions). Dataset artifacts kept on disk
at `D:\VOETBAL_VIDEO\RECO\training\ai_learning_dataset\` (per
[[feedback_keep_test_artifacts]]). Nothing committed/pushed this
session - only scratchpad Python driver scripts were touched, not part
of the repo.

**2026-08-16: async detect thread - built, tested, measured end-to-end,
real 1.42x export speedup, VRAM cost measured, reco-gui checkbox added.**
Picks up the design from the entry below ("export-speed fix #2") that
was deliberately left unstarted on 2026-08-15. User asked to build it
this time, wanted it kept revertible - everything lives on an isolated
worktree (`D:\VOETBAL_VIDEO\RECO\worktree-async-detect`) / branch
(`feat/async-detect-thread`, 5 commits: foundation, session wiring, CLI
flag, VRAM measurement docs, GUI checkbox), `main` never touched.

**What it does**: moves only the ORT `session.run()` inference call to
a dedicated worker thread (GPU texture readback/preprocess stays
synchronous - textures aren't safe to hand to another thread). New
`UnifiedDetector::detect_split()` trait method (default = today's sync
behavior, zero risk to any backend that doesn't override it),
`AsyncDetectThread` (new, modeled on `async_encode.rs`), and solved the
"ROI wrinkle" that stopped the original design - `RoiFilteredDetector`
composes its own filtering onto the deferred result's `finish` closure
instead of silently skipping it, explicitly regression-tested. Session
wiring: `BufferedFrame.world_state` is now ready-or-pending, resolved
lazily right before a frame is actually consumed
(`run_panner_once`), with a stale-reuse fallback for the panner's
lookahead peek - same pattern already used elsewhere for stale
detections. New `reco-cli --async-detect` EXPERIMENTAL flag builds a
second, separate detector instance for the worker thread.

**Real measured result** (300-frame test, 03 OJC clip, RTX 3060 Ti,
TensorRT, same model/settings otherwise):

```
baseline:        28.8s (10.4fps avg)
--async-detect:  20.3s (14.8fps avg)   1.42x faster
```

Functional correctness verified bit-for-bit identical - both runs
produce exactly 242/300 raw-ball-detection frames (80.7%), mean
confidence 0.574.

**VRAM cost, measured** (`nvidia-smi` polled every 0.5s, same test,
steady-state average not raw peak - baseline had a misleading transient
startup spike above its own steady state):

```
baseline steady-state:       5226 MiB
--async-detect steady-state: 5622 MiB   (+395 MiB, +7.6%)
```

Real but modest - not the "doubles the whole session" risk originally
flagged, because only the detector's own footprint (TensorRT
engine+workspace) duplicates, not the much larger shared lookahead
VramPool.

**reco-gui checkbox added**: Export dialog, "Async AI detection
(experimental, faster export)" right below "Reduce lookahead memory
(8-bit)". Mirrors `--async-detect` 1:1. Deliberately NOT persisted in
AutocamDefaults (calibration or app-level) - always resets to off on
restart, unlike every other AI Tracking slider, so the experimental
flag can't silently stay on via a saved calibration. `check`/`clippy
-D warnings`/`fmt` all clean, committed (`fdffb740`). Both debug and
release `reco-gui.exe` built **in the worktree's own `target/`** (not
the main checkout's usual build output):

```
D:\VOETBAL_VIDEO\RECO\worktree-async-detect\target\debug\reco-gui.exe
D:\VOETBAL_VIDEO\RECO\worktree-async-detect\target\release\reco-gui.exe
```

To test: load a calibration, AI Tracking on with a model + lookahead >
0, tick the checkbox in Export, run an export and compare against one
with it unticked.

**GUI checkbox validated by the user's own real exports, 2026-08-16
(nvidia-smi polled live during each run).** First two attempts were
apples-to-oranges (debug-vs-debug showed no gain; a release pair had
mismatched clip lengths, 42s vs 130s) - flagged both times rather than
reporting a misleading number, then re-measured with a matched release
pair, same clip, only the checkbox differing:

```
                    async ON    async OFF
active duration     ~134s       ~164s        1.22x faster
GPU util (avg)      73.2%       56.8%
VRAM                6907 MiB    6488 MiB     +419 MiB
power (avg)         169W        152W
```

Confirms the earlier CLI-only 1.42x/+395MiB measurements in a real
end-to-end GUI export, not just a synthetic benchmark - same direction
and order of magnitude, slightly lower speedup here (expected, real
GUI overhead vs a controlled CLI run).

**Same day, follow-on: user asked to push GPU utilization further.**
Profiled the shipped async-detect build for real (not guesswork) -
found `yolo_inference` at ~90% of wall-clock, already well-overlapped.
Two more attempts, each measured via a true A/B (same clip/command,
code toggled):

1. Fixed `wgpu_preprocess.rs`'s blocking readback wait to target our
   own GPU submission index instead of "whatever's most recent" -
   **measured no real effect** (28.33s vs 28.51s, noise). Kept anyway,
   strictly more correct. Also confirmed TensorRT+FP16 genuinely
   active (ruled out a silent-DirectML-fallback explanation).
2. Added dual-worker mode (`AsyncDetectThread::new_dual`, one thread
   per camera instead of one shared) so Left/Right inference can
   overlap - CLI A/B measured a modest ~5-6% further speedup (24.7s ->
   23.3s), not the near-halving hoped for: two concurrent TensorRT
   contexts on this consumer GPU (no MPS) don't truly run in
   parallel, they contend and each call gets slower (40.1ms -> 77.3ms
   avg), eating most of the theoretical gain. Wired as
   `--async-detect-dual` / a second GUI checkbox, handed to the user
   to test themselves in the real GUI.
   **User's own GUI test found no win at all**: single-worker 72.0%
   GPU/6784MiB/~138s vs dual-worker 66.4% GPU (lower!)/7339MiB
   (+555MiB confirmed)/~136s (same). User decided to revert it
   ("andere strategie bedenken") - reverted clean, commit `f945d4eb`.
   Lesson: should have checked in before building the full GUI wiring
   for a change whose own CLI measurement was already a weak ~5-6%
   signal - over-invested before the user weighed in.

Poll-wait fix (`0868ead4`) kept, dual-worker (`978f62ae`) reverted
(`f945d4eb`) - all on `feat/async-detect-thread`, still not merged.
Both debug+release reco-gui rebuilt again, back to just the
single-worker async-detect checkbox - test paths:

```
D:\VOETBAL_VIDEO\RECO\worktree-async-detect\target\debug\reco-gui.exe
D:\VOETBAL_VIDEO\RECO\worktree-async-detect\target\release\reco-gui.exe
```

**Next ideas raised** - user asked for a different strategy: (a)
`--detection-interval > 1` - turned out to already be in daily use,
same thing as the GUI's existing "Detect every N frames" slider
(default 3), nothing to build; (b) batch Left+Right into one TensorRT
call instead of two sequential ones (`best_batch2.onnx` already
exported at
`D:\VOETBAL_VIDEO\RECO\training\round4\runs\yolo26s_tiled1920_full\weights\`
from an earlier experiment) - **not tried yet**, the only untried idea
left; (c) TensorRT CUDA graphs - **tried, CRASHED**.

**(c) CUDA graphs: `.with_cuda_graph(true)` on the TensorRT EP builder
crashed the async detect thread mid-export** (`expected typeinfo_ptr
to not be null` in ort-rs's value/mod.rs, ~150-200 frames into a
300-frame run). Worse than just "no speedup" - the thread died and the
export **silently kept running to completion** with frozen/stale
detections for the rest of the clip instead of erroring loudly. A real
correctness risk. Reverted immediately (commit `04796401`), TensorRT
engine cache restored from a pre-attempt backup, no second timing run
attempted. Left a code comment on the likely real fix (IoBinding with
pinned buffers instead of a fresh `Vec<f32>` per call) for whoever
revisits this.

**(b) batch L+R into one TensorRT call, measured via a standalone
throwaway benchmark instead of building full integration first**
(`crates/reco-detect/examples/bench_lr_batch.rs`, commit `d1df4e0c`) -
pure inference-latency comparison, no video/pipeline involved. **1.06x
- real but modest** (51.1ms sequential vs 48.3ms batched per produce
index), no downside risk unlike the other two attempts, but requires a
specially-exported fixed-batch=2 ONNX (`best_batch2.onnx`) rather than
whatever a normal training run produces - real UX friction for a small
win. **Session paused here on the user's call** ("we stoppen even
hier") rather than building the `AsyncDetectThread`/CLI/GUI
integration for it.

**Summary of all four post-single-worker attempts this session**:
poll-wait fix (no effect, kept, harmless); dual-worker (no real win,
reverted after the user tested it themselves in the GUI); CUDA graphs
(CRASHED the detect thread, reverted); batch L+R (modest 1.06x,
measured but not integrated, paused). The shipped single-worker
async-detect fix (1.22-1.42x, confirmed both CLI and GUI) remains the
one solid win from today. If picking this back up: batching is the
only remaining real (if small) lever, blocked on deciding how to
surface the "needs a batch2-exported model" requirement to a
non-technical user.

**Not done**: not merged to `main`, only the Windows D3D11VA path gets
real async behavior (every other residency still runs synchronously,
unaffected). Whether to merge/promote out of "EXPERIMENTAL" is the
user's call, not decided this session. See
[[project_async_detect_thread_design]].

Continuation note for resuming work on a different machine/session -
git-tracked so it travels with `git pull`/`push` between the user's two
PCs. Overwrite wholesale at the end of a session rather than appending
history - git history is the append-only log, this file is just "where
things stand right now."

**No usernames/passwords/IP addresses in this file, ever** - see
feedback_no_credentials_in_tracked_files.md. Reference "see password
manager" / `zerotier-cli listnetworks` instead.

## Immediate state / what to do next

**2026-08-16: dual-tile inference cost, profiled for real (not
estimated) - research concluded by the user, nothing merged.** Direct
follow-on from the overnight tiled-training entry below. User asked for
a real `--features profiling` measurement before deciding whether to
build production tiled-inference support, explicitly wanted it
revertible - built entirely on an isolated branch,
`experiment/tiled-dual-inference-profiling` (2 commits, off `main`, not
merged - revert is `git checkout main` or delete the branch; `main` was
never touched).

**Two variants measured, same 300-frame profiling run each time (03 OJC
clip, RTX 3060 Ti, TensorRT), same `yolo26s_tiled1920_full` weights
throughout so only the call pattern differs**:

```
                        baseline (1x)   sequential 2x calls   batched 1x(batch=2)
detect/camera-frame     100.9ms         213.6ms (2.12x)       324.3ms (3.21x)
throughput              9.1fps          4.1fps                2.8fps
raw-ball-detection      80.7%           88.0%                 88.0% (identical)
```

**Sequential wins over batched** - counterintuitive, verified not a bug:
functionally identical detection rate confirms the batching logic itself
is correct (same weights, same math, just one combined ORT call instead
of two) - the regression is a real TensorRT/GPU characteristic on this
hardware (a fixed batch=2 engine apparently isn't as well-optimized as
batch=1 for this model/GPU combo), not a code defect. Needed a separate
`best_batch2.onnx` export (fixed batch=2 input shape - ORT rejects a
mismatched batch axis) kept apart from the normal `best.onnx`.

**Verification method worth remembering for next time**: confirmed which
code path actually ran via trace **span counts** (`yolo_inference`
occurrences), not log output or wall-clock alone - `--features
profiling` builds silently drop ALL `log::*` calls (`init_profiling()`
in `reco-cli/src/main.rs` never bridges the `log` facade, unlike the
non-profiling `init_tracing()` path) - a real gap, not something this
session broke, worth knowing before trusting log output in any future
profiling session.

**Bonus**: this doubled as the frame-accurate real-pipeline back-to-back
test that the overnight entry below flagged as missing - these
raw-ball-detection numbers (80.7%/88.0%) come from the real Rust decode+
detect pipeline, not the ffmpeg-approximate Python re-test that had the
frame-index-alignment caveat.

**Conclusion, user's call, research closed**: the tiling win is real (on
both the LS val split and now real frame-accurate footage), but costs
~2.1x detection time at best (unbatched; batching made it worse here) -
user decided not to pursue production wiring further this session. If
picked up again later: the sequential variant
(`TiledWgpuPreprocessingDetector`, `RECO_TILED_DETECT=1`) is the one
worth building on, not the batched one. See
[[project_yolo26n_training_pipeline]].

**Overnight, 2026-08-15/16: SAHI-style left/right tiled training -
built, trained, exported, real-footage tested end-to-end while the user
slept - a real, sizeable win, but the real-footage numbers need an
honest asterisk (see below).** User asked to try the research idea noted
in the 2026-08-14 entry (splitting each 3840x2880 frame into two
overlapping square crops instead of one letterboxed square), explicitly
wanted a small feasibility test first before committing to more, then
delegated the whole rest of the sequence (full train -> ONNX -> real-
footage test -> repo cleanup) before going to bed.

**Dataset built and verified before training anything**: each round4
image -> two overlapping 2880x2880 crops (left x=[0,2880], right
x=[960,3840], 1920px overlap band) resized to 1920x1920 (2880 itself is
too large to train at directly, per the user's own sizing call).
Zero letterbox waste vs the old approach's ~25% wasted canvas on a
straight 3840x2880->square fit - and critically, the *scale factor* is
much gentler: old approach scales the whole frame 3840->1920 (0.5x, an
~18px ball becomes ~9px); this approach only scales 2880->1920 (0.67x,
same ball stays ~12px). Labels transformed (crop-offset + scale, box kept
if its center falls in the tile, so overlap-band objects appear in both
tiles) from 258 source images -> 516 tiles (438 train/78 val). **Verified
correct before training** by drawing transformed boxes back onto a random
sample of tiles - all tight and correctly placed, not assumed.

**8-epoch feasibility smoke test, as the user asked for first**: fresh
`yolo26s.pt`, imgsz=1920 (matches tile size exactly), batch=2/workers=2.
Result far exceeded the non-tiled 1920 8-epoch smoke test from
2026-08-14/15 at the same epoch count - ball P/R/mAP50/mAP50-95
1.000/0.703/0.719/0.456 vs the old approach's 0.448/0.389/-/0.230.
Verdict: clearly worth a full run.

**Full training, patience=100 (same convention as every other round)**:
early-stopped at epoch 159 (best @ epoch **59**). Held up after full
convergence, not just an early-epoch artifact:

```
              old (round4 1920-final, non-tiled, post-labelfix)   new (tiled, best@59)
ball P              0.921                                          0.987
ball R               0.471                                          0.708
ball mAP50           0.500                                          0.742
ball mAP50-95         0.338                                          0.507
```

~50% relative improvement on recall and mAP50-95 on the LS held-out val
split (same caveat as always - 17-24 ball instances, real but small
sample). Checkpoint:
`round4/runs/yolo26s_tiled1920_full/weights/{best.pt,best.onnx}`.

**ONNX export**: `nms=True` forced off by ultralytics itself (YOLO26 is
end2end/NMS-free, expected, zero-effect flag per every prior round's own
finding) - verified metadata directly: `1x3x1920x1920` in, `1x300x6` out,
names `{0:person,1:ball,2:referee}`, matches every prior checkpoint's
convention.

**Back-to-back real-footage test - a genuinely important methodology
caveat found, not swept under the rug.** Extracted all 899 frames
(t=100-130s, both cameras) fresh via `ffmpeg -ss 100 -c:v hevc_cuvid`
sequential decode (the Rust `dump_detection_frames` tool wasn't
rebuilt this run, `target/` had just been cleaned - see below), then ran
both the old and new checkpoint's own `model.predict()` directly in
Python on every frame (both cameras, tiled model gets both crops merged),
identical methodology for both models, conf floor 0.10:

```
                          old (full-frame)   new (tiled)
overall raw-ball rate      765/899 (85.1%)    860/899 (95.7%)
frames 720-898 window       85/179 (47.5%)    140/179 (78.2%)
frames 690-719 window       30/30 (100%)       30/30 (100%)
mean confidence (hits)      0.64                0.55
```

New model detects more, in both the overall rate and the window
historically labeled "hard" - **but the absolute numbers here do NOT
match every previous round's documented numbers on this exact clip/
window** (round4's own real-app test reported 49.1% overall / 0-18/179
in frames 720-898, not 85%/47.5%). Investigated rather than just
reporting the flattering framing: spot-checked frame 720 directly - the
**old** model scores 0.92 confidence there in this test, nothing like the
near-zero the historical "hard zone" would predict. **Conclusion: this
re-extraction's frame indices don't line up with the historical
frame-720-898 window** (ffmpeg's `-ss 100` sequential decode start point
isn't guaranteed to land on the same real match instant as the Rust
pipeline's own D3D11 seek+decode path used in every prior round) - this
test is sampling different real seconds of the match, not the same
"known-hard" stretch. **What this test IS still valid evidence for**:
old vs new ran under byte-identical methodology (same frames, same conf
floor, same code path) - the new model detecting the ball meaningfully
more often across this fresh, arbitrary 30s window is a real, independent
signal in the same direction as the controlled LS-metrics result above,
just not a confirmation that the *specific* historical 720-898 gap is
now 78% solved. **A real frame-index-accurate re-test (via the actual
Rust decode pipeline, once dual-tile inference exists there - see below)
is the next real validation step, not done tonight.**

**Not done, deliberately, both genuinely bigger jobs**:
- **No dual-tile inference wiring in `reco-detect`/`reco-autocam`** - this
  checkpoint cannot be dropped into the live app as-is; it needs 2 forward
  passes per camera per frame (left+right tile) merged with overlap-band
  dedup, not the 1 pass every other checkpoint uses. Given last night's
  own finding that AI detection already consumes ~87% of frame time
  (see the async-detect-thread entry below), doubling per-camera
  inference cost is a real, serious trade-off to weigh deliberately with
  the user, not decide unsupervised overnight.
- **No frame-accurate re-validation via the real Rust pipeline** - the
  back-to-back test above is Python-side and has the frame-indexing
  caveat spelled out above.

**Repository cleanup** (`D:\VOETBAL_VIDEO\RECO\repository`): removed
stray untracked clutter - `reco-trace.json` (20MB stale profiling
trace), `yolo26n.pt`/`yolo26s.pt` (26MB, misplaced in the repo root, not
part of the Rust project), `scripts/__pycache__/`. Ran `cargo clean` to
reclaim `target/` disk space - **partially failed** (`target/debug`
couldn't be removed, Windows file-lock, "os error 32" - some process
still had a handle on it). Deliberately left untouched: `.cargo/`
(checked - just `target-dir = "target"`, harmless but pointless to
delete) and `.claude/` (Claude Code's own directory - never touch).
**Consequence for next session**: the next `reco-gui.exe`/`reco.exe`
build will be a full rebuild, not incremental - budget more time than
usual. 1 commit (the label-QA audit log) is committed locally but not
pushed, per the "push only when asked" default.

**Next step, user's call**: decide whether the tiling win is worth the
2x-inference-cost trade-off enough to build the dual-tile
`reco-detect`/`reco-autocam` wiring, and/or get a frame-accurate
real-pipeline re-test before fully trusting the back-to-back numbers
above. See [[project_yolo26n_training_pipeline]].

**2026-08-15: full independent ball-label QA audit on round4's 258-task
set - found + fixed 6 real mislabels, a genuine recurring pattern (not
isolated as the same-day root-cause note below originally concluded).**
Follow-on from the frame-704/`winC_014` root-cause work below: user
rightly pushed back on "isolated, not a pattern" right after that fix,
insisting more frames likely carried the same kind of miss. Re-
investigated properly with methods that avoid the earlier pass's real
blind spot (checking a model's own predictions against labels it was
*trained on* means it can silently "agree" with a bad label it
memorized - agreement there isn't independent evidence):
1. Cross-checked all 214 ball instances against `soccana.pt` (different
   model lineage, never trained on this data) at imgsz=1920 (default
   imgsz=640 first attempt was itself worthless - shrinks an 18px ball to
   ~3px, produced 65% nonsense "disagreement"). Found 1 new confirmed
   mislabel (`right_frame_0029400`, a white field-marking dot labeled as
   ball).
2. Full official `model.val()` run of `yolo26s_v4_imgsz1920` against a
   **freshly re-pulled** LS project 8 export (not the possibly-stale
   local `round4` copy) - confirmed 258/258 tasks matched with zero drift
   beyond the 1 known fix; ball val-metrics nudged up slightly purely
   from the corrected count (R 0.444->0.471, mAP50 0.474->0.500).
3. Full 258-set audit using the production checkpoint itself (caveat:
   trained-on-same-data agreement isn't independent evidence, but
   confident *disagreement* despite training pressure to conform is still
   meaningful) - found 5 more confident disagreements, then a full visual
   contact-sheet scan of all 213 ball-label crops (not just the
   automated flags) caught 2 of those the nearest-neighbor method missed
   entirely (no competing detection nearby to flag against - a real
   coverage gap in the automated approach).

**Root cause, confirmed with hard pixel-coordinate evidence, not
guessed**: a single painted field marking (a flat white oval, no ball
seam pattern) sits at a fixed on-screen position for the static camera -
4 of 6 confirmed mislabels (all `right_frame_*`, from the original
200-frame auto-labeled batch, never the later winA/winB/winC hard-frame
batches) cluster within 7px of the exact same pixel coordinate (~731,1517
in the 3840x2880 right-camera frame). The original AI auto-labeler
evidently had a recurring false positive there; at 258-task review volume
a technically-round, already-pre-labeled small box doesn't visually stand
out as wrong without deliberately zooming in - not a diligence gap in the
user's review, a genuinely subtle recurring miss.

**Fixed both copies for all 6** (`right_frame_0029400`,
`left_frame_0023400`, `right_frame_0006000`, `right_frame_0020700`,
`right_frame_0021000`, `right_frame_0029700`): local
`round4/labels/{train,val}/*.txt` (deleted the spurious ball line, kept
the real second ball's line where one existed), and the LS project 8
source annotations - user did the LS-side deletions themselves via the LS
UI after being given the exact task-id/region-id list (kept it in their
own review flow rather than an API-side edit bypassing it), verified
after the fact via a fresh API re-pull that all 6 offending boxes are
gone and every real ball box is untouched.

**Revises the root-cause note directly below**
([[project_yolo26n_training_pipeline]]'s "winC_014 confirmed as an
isolated miss, not a pattern" section) - that conclusion was too
optimistic, since it only ever checked a same-family model's own
confident disagreements, which is blind to exactly this failure mode.
**Corrected picture: not zero-pattern, but a small (6/214 = ~2.8%), fully
explained, fully fixed, single-cause pattern** - no evidence of any other
kind of systematic label error after this pass (person/referee not
separately audited, but score well above ball in the metrics, so lower
priority).

**Not yet done**: no retrain triggered by this - same reasoning as
`winC_014` below, doesn't retroactively change any existing checkpoint,
worth folding into whenever the next real training round happens (more
hard-frame data is still the bigger lever, per the round-4 conclusion).
See [[project_yolo26n_training_pipeline]].

**2026-08-15: export-speed fix #2 (overlap AI detection with render/
encode) - thoroughly researched and designed, deliberately NOT started,
zero code changed.** User asked to continue optimizing export speed
after fix #1 (D3D11 seek, below) landed. Same profiling trace that
found fix #1 also found this: AI detection (readback+preprocess+
inference) consumes ~87% of "active" per-frame time, running
synchronously and blocking the frame loop, while decode/stitch/encode
together take ~4-5ms/frame and are already well-overlapped (async
encoder, 0 backpressure stalls). Went through a full plan-mode research
pass (2 parallel Explore agents + extensive direct reading) before
touching any code - this session's own earlier v0.5.4-sync experience
made clear that skipping this step on architecturally risky work is
how things go sideways. **User decided (end of session) to pick this
up fresh next session rather than push through now** - documenting the
complete findings here so the next session doesn't have to re-derive
any of this.

**Confirmed via direct code reading (file:line references), not
guessed:**
- The lookahead buffer (`session/run_loop.rs`'s `run_buffered`,
  `session/frame_buffer.rs`) already decouples *when* detection runs
  from *when* a frame renders (pre-detects N frames ahead so the panner
  sees future WorldStates) - but NOT *which thread* pays for it.
  `produce_one` (`run_loop.rs:382-420`) calls
  `detect_and_track_only` (`detection_dispatch.rs:216`) and blocks on
  it before the frame enters the buffer - same thread that later
  renders+encodes. No threading exists anywhere in the current loop.
- A **directly reusable precedent already exists in this exact
  codebase**: `async_encode.rs`'s `AsyncEncodeThread` - dedicated
  thread + bounded `mpsc::sync_channel` + buffer-pool return channel +
  non-blocking submit with backpressure stats. This is the template to
  follow, not a new pattern to invent.
- **Everything needed to run detection async is already `Send`-safe,
  verified directly in source, zero changes needed there**:
  `ort::session::Session` is `unsafe impl Send + Sync` (ort 2.0.0-rc.12
  vendored source, citing ORT's own C API contract); `TrtGpuDetector`
  is `Send` and re-asserts its CUDA context per-call via
  `cuda_ensure_context()` rather than pinning to a creation thread;
  `GpuContext`/wgpu `Device`/`Queue` are plain `Clone` with no interior
  mutability (`WgpuPreprocessingDetector` already holds its own cloned
  device/queue - direct precedent); `UnifiedDetector: Send` and
  `Tracker: Send` are already trait bounds; tracker/panner state has no
  interior-mutability tricks.
- **The one real constraint**: GPU-resident detection *borrows* texture
  views straight out of a 2-slot-per-camera decode/staging ring
  (Windows `D3d11StagingPool` `n_slots=4`; Linux `gpu_shared_views`,
  same effective size) that's explicitly documented (and, on Linux,
  backpressure-enforced) as unsafe to reuse until detection has
  *finished reading* it - today guaranteed only because detection is
  synchronous. Moving the *whole* detect call (GPU readback+preprocess
  +inference) async would need new owned-copy or GPU-pool-slot
  machinery to avoid a real race - genuinely new, unverified-by-
  compilation wgpu texture code, higher risk.

**Scope decision, made explicit with the user mid-implementation**:
of the ~127ms/detection-call cost, roughly ~58ms is GPU readback+
preprocess (NV12 → CHW tensor, existing compute shader +
`device.poll(wait_indefinitely)` blocking readback,
`wgpu_preprocess.rs:337-349`) and ~69ms is the actual ORT/TensorRT
`session.run()` inference (`reco-detect/src/detectors/cpu.rs:322`,
`yolo_inference` span, 2 calls/detection = one per camera). **Chose the
safer partial fix: move only the inference call async, leave
preprocessing synchronous** (unchanged, already-tested code, zero new
GPU plumbing) - real ~54% reduction in blocking time instead of the
theoretical ~87% full-async ceiling, but no new wgpu texture-readback
code that can't be incrementally compile-tested. The GPU-pool-slot
approach for the fuller ~87% win (modeled on `VramPool::acquire`/
`release`) remains a valid future refinement, just bigger/riskier -
not pursued this pass.

**The clean split point, already present in the code, no new
abstraction needed to find it**: `WgpuPreprocessingDetector::detect()`
(`crates/reco-autocam/src/wgpu_detector.rs:50-81`) already separates
these two steps internally - `self.preprocessor.preprocess(...)`
(GPU work, stays sync) produces an owned `tensor: Vec<f32>`, then
`self.inner.detect(camera, &DetectorFrame::PreprocessedChw{data:
&tensor, ...})` (the actual ORT inference, `self.inner` is the raw
`CpuYoloDetector`) - this inner call is exactly what should move to
the async thread. `Vec<f32>` is trivially `Send`, no GPU concerns at
all on the async side.

**The wrinkle found right before stopping, not yet resolved**: the ROI
filter (`RoiFilteredDetector`, `crates/reco-autocam/src/roi_filter.rs:159-176`)
wraps the *outside* of `WgpuPreprocessingDetector` and applies
`filter_by_roi(...)` to the `Vec<Detection>` *after* `detect()`
returns. Naively extracting just `WgpuPreprocessingDetector.inner` to
the async thread would bypass the ROI filter entirely (wrong -
detections outside the field would stop being filtered). Needs a small
`UnifiedDetector` trait extension before implementation continues:
```rust
enum DetectSplit {
    Done(Vec<Detection>),      // default: existing synchronous behavior, zero risk to every other detector
    Pending(/* owned tensor + enough to finish + re-apply any wrapper's post-processing */),
}
trait UnifiedDetector: Send {
    fn detect(&mut self, camera, frame) -> Result<Vec<Detection>, DetectorError>; // unchanged
    fn detect_split(&mut self, camera, frame) -> Result<DetectSplit, DetectorError> {
        Ok(DetectSplit::Done(self.detect(camera, frame)?))  // default = today's behavior
    }
}
```
`WgpuPreprocessingDetector` overrides `detect_split` to return
`Pending` with the owned tensor; `RoiFilteredDetector::detect_split`
delegates to `self.inner.detect_split(...)` and, when it gets back a
`Pending`, needs to carry its own ROI-filter step through to whenever
the async thread's inference result comes back (e.g. the `Pending`
payload could carry a boxed closure/continuation, or `DetectSplit`
could be structured so the caller applies a stack of pending
post-processing steps in order - not yet designed, this is where to
pick up).

**Rest of the plan (should still hold, wasn't invalidated by the ROI
wrinkle)**: new `crates/reco-core/src/async_detect.rs` (`AsyncDetectThread`,
modeled on `async_encode.rs`) submitting `(produce_index, owned tensor
+ metadata)` jobs and receiving `(produce_index, Vec<Detection>)`
results in strict FIFO order (single worker thread preserves ordering
for free, which the tracker contract requires -
`detect/tracker.rs:139-151` - exactly-once-per-frame, in order).
`run_loop.rs`'s `produce_one` submits without blocking; `run_panner_once`
becomes the resolution point (blocks on the result channel only if the
detect thread hasn't caught up yet - in steady state, with N frames of
lookahead buffer between produce and this point, it usually won't
need to). `FrameBuffer`/`BufferedFrame`'s `world_state` field needs a
pending-or-ready wrapper; `future_world_states()` needs a defined
fallback for not-yet-resolved entries (reuse the previous resolved
WorldState, matching the existing stale-detection-reuse pattern).
Scope: `run_buffered` (export path) only - the immediate/live-preview
path (`process_frame_any`) is unaffected, out of scope.

**Full context, all file:line references, the two research agents'
complete raw findings**: this transcript's own history has the full
detail (this session, after the v0.5.4 sync section below) - re-derive
from there if this summary isn't enough, rather than re-running the
same research agents from scratch.

**2026-08-15: v0.5.4 upstream sync finally done - long-deferred, turned
out to be a fundamentally different task than expected, merged+pushed
to `main`.** User asked to execute the sync (91+ fork-only commits / 23
behind `origin/main`), with two explicit constraints: don't lose any
built features, and prefer upstream's base since future updates need
to keep applying. Backed up first: tag `pre-v054-sync-backup-2026-08-15`
+ a full offline bundle at `D:\VOETBAL_VIDEO\RECO\backups\repo-backup-pre-v054-sync-2026-08-15.bundle`
(507MB, verified). A trial `git merge origin/main` produced 28
conflicts - investigating the biggest one (`calibration.rs`) uncovered
the real story: **upstream's current `main` is not ahead of us on the
calibration/render architecture, it regressed.** A "v0.5.3" merge
upstream (`c2705c2c`) combined the post-refactor state (our
`Calibration`/`Topology`/`Framing`/`Executor` model, which our fork
already adopted from upstream via `6ff32c37`) with a 20-commit bugfix
chain branched off an older pre-refactor point, and the resolution
discarded the newer architecture entirely - confirmed directly, zero
occurrences of `color_match_enabled`/`multiband_blend_enabled`/
`seam_offset`/`ground_tilt_x`/`Executor` anywhere in `origin/main`'s
reco-core today. Taking upstream's side would have broken compilation
in 8+ places and deleted every seam/color-match/tilt feature this fork
has built - rejected as a real downgrade, not a catch-up.

**Second, nastier discovery**: git's 3-way merge was silently
corrupting "unconflicted" regions in files affected by this
divergence - found 6+ separate cases (wrong type names, dropped
`[features]` blocks, phantom function calls to things that don't
exist on our side) in files git never even flagged as conflicted. Not
safe to trust *any* file the merge touched without a full diff against
real `main`, marked-conflict or not. Abandoned that merge entirely;
rebuilt the sync as a clean, minimal, hand-verified patch set applied
directly onto an untouched copy of `main` instead (11 files, no merge
commit lineage risk).

**What actually landed**: the 23 real upstream-only commits (merge-base
`ab553d35` to `origin/main` tip) mostly turned out to already be in our
history under different hashes (DirectML EP enable, Slint wgpu-backend
default, reco-obs Windows build.rs fixes, VRAM budget calc, CUDA
context-before-free drop fixes, audio-across-chained-segments,
recent-files video-input wiring, libcamera error logging - all
independently ported at some earlier point). Six were genuinely new
and got manually ported: frame-rate-mismatch fail-loud (`reco-io`),
stream-first fps probe fixing camera-original-HEVC-without-VUI-timing
silently defaulting to 30fps (exactly this session's own DJI Action 4
footage), encoder output stream fps-stamping, audio-passthrough now
respecting `sync_offset` (previously audio and video could start
misaligned by the sync correction itself), GUI export status/seek math
using real probed fps, and reco-detect's ORT-dylib-probe-before-ort
fix (prevents a real self-deadlock when load-dynamic + missing
runtime). Two lower-priority fixes (telemetry bug-report truncation,
replay-recording fps) identified but deliberately not ported -
low-risk, deferred.

**Verified**: `cargo build/test/clippy(-D warnings)/fmt` clean across
all 7 affected crates (241 tests passing, same 2 pre-existing CUDA
failures as always). Real end-to-end stitch against a real calibration
file + real DJI footage confirms calibration loading, audio
passthrough, and D3D11VA decode all still work. Both debug+release
`reco-gui.exe` rebuilt from the new `main`.

**Not done, deliberately**: no attempt to revert
`Calibration`/`Topology`/`Executor` naming to match upstream's
regressed shape - would touch ~40 files/92+ references for zero
functional gain and a second painful calibration-file-format
migration. If upstream ever fixes their own regression, worth
re-evaluating the "keep upstream's base" question then, not before.
See [[project_v054_upstream_sync]].

**2026-08-15: found + fixed a real export-throughput bug (D3D11VA
start-time doesn't seek), merged+pushed to `main`.** User asked to
watch the GPU during an export test, suspecting it wasn't fully
utilized (~13fps observed). Live nvidia-smi/WMI monitoring during a
real export showed nothing actually saturated (GPU SM ~58% avg, CPU
only ~1.4/24 cores) - not a raw compute limit. A `--features profiling`
perfetto trace (300 frames, 2560x1440, same settings/model as the
user's real export) found two distinct causes:
1. **`--start-time`/the GUI export in-point doesn't seek - it decodes
   and discards every frame from the start of the source up to the
   target.** Isolated cleanly with two 5-frame runs differing only in
   start time: `start-time 0` = 5.2s, `start-time 100` = 33.2s. That
   ~28s is invisible in the app's own "Processed X frames" counter
   (which only starts once real output begins) - it just looks like
   "takes a while to start", worse the further into a long recording
   the export begins. **Fixed** (Windows D3D11VA path): decode threads
   now do a real keyframe seek (`VideoDecoder::seek_to_secs`, which the
   CPU decode path already used) instead of brute-force decoding
   through the pre-roll. Verified: same 5-frame/start-100 test dropped
   to 8.9s; first-frame output confirmed pixel-identical to the old
   path at the same timestamp (correct seek target, not just faster).
   Also fixed a small unrelated pre-existing bug found via
   `clippy --all-targets` (`cuda_nv12_frames` cfg'd for
   `any(linux,windows)` but only ever called from Linux - dead code on
   Windows). **Scope: Windows D3D11VA only** - Linux CUDA zero-copy and
   macOS Metal zero-copy have the identical brute-force pattern and
   would benefit from the same fix, not done here (can't verify on
   either platform from this machine). See
   [[project_d3d11_start_time_seek_fix]].
2. **Once past startup, AI detection dominates frame time, not
   decode/stitch/encode** - the render/encode pipeline itself is fast
   and already well-overlapped (~4-5ms/frame, encoder async, 0
   backpressure stalls), but TensorRT detection (readback + preprocess
   + inference) ran ~87% of the "active" per-frame time in the same
   trace, blocking the main frame loop rather than overlapping with
   render/encode on its own thread. This explains the earlier
   nvidia-smi/CPU findings (nothing saturated - the process bounces
   between waiting on TensorRT and doing GPU render work). **Not yet
   fixed** - real architecture change (detection needs its own
   overlapped thread), bigger and riskier than fix #1, not started
   this session.
   Confirmed TensorRT genuinely active throughout (found a fresh
   matching `.engine` cache file) - ruled out the known silent-DirectML-
   fallback bug as an explanation here.
   Both debug+release `reco-gui.exe` rebuilt with fix #1.

**2026-08-15: "Ball coast time" tunable built, merged+pushed to
`main`, both debug+release `reco-gui.exe` rebuilt.** User exported a
test video with the new imgsz=1920 checkpoint (see below), confirmed
it's a real improvement, but flagged a UX gap: when the ball crosses
the field ROI line the tracker loses it immediately (the ROI filter
drops the detection before the tracker ever sees it - looks identical
to "no detection this frame"), so the camera never follows a player
stepping over the line to retrieve it. Diagnosed that `BallTracker`
already had exactly the right mechanism for this - a `Coaster`
frame-countdown holding an already-tracked ball's last position for a
brief gap - just hardcoded at `DEFAULT_COAST_FRAMES = 20` (0.67s
@30fps) and not exposed. Exposed it as `ball_coast_secs` end-to-end:
`AutocamConfig` field/builder, both `BallTracker` construction sites,
`reco-cli --ball-coast-secs` flag + run_config JSON, `AutocamDefaults`
(calibration-persisted, serde default preserves old-file behavior),
new reco-gui "Ball coast time" slider (0.2-5.0s) wired through the
usual snapshot/apply/export sites, plus new parameter docs + a
troubleshooting bullet in both `docs/ai-panner-tuning.md` and the NL
translation (explicitly distinguished from the existing
ball-anchor-range/ball-reach/FOV-wide breakaway checklist - ROI
filtering and panner gating are different failure modes with different
fixes). Raising it only affects an already-tracked ball, so it doesn't
make the panner more likely to trigger on e.g. kids playing with a
ball just outside the pitch. `cargo test/fmt/clippy` clean across
reco-core/reco-autocam/reco-cli/reco-gui; `--ball-coast-secs 2.5`
confirmed threading through into the events.jsonl run_config record on
a real export. **User-confirmed on real footage same day**:
"ball_coast: 2,5s lijkt aardig te werken" (works nicely) - docs (EN+NL)
and the settings table updated to recommend `2.5s` as the starting
value. See [[project_ball_coast_time_slider]].

**Overnight, 2026-08-14/15: imgsz=1920 full training run delegated
end-to-end while the user slept - a real, if modest, win over round-4.**
User spoke with the forum engineer (source of the original yolo26s
call) who trains at 1280 and 1920; feasibility-tested (8ep, `batch=1`,
15% data - only ~3.76GB VRAM), then a full run (`batch=2`, 258 tasks)
stopped naturally at epoch 225 (best@125, `patience=100`), 2.55h. Real-
app test on the standard 100-130s clip: overall raw-ball 51.1% (best of
every checkpoint ever tested, vs round-4's 49.1%), the persistent
720-898 dead-zone (always exactly 0/179 before, across every prior
round) now 18/179. **Not a clean win**: an intermediate 8-epoch
checkpoint scored *higher* on two curated review windows than the
fully-converged final - unexplained, not chased. **Frame-704
localization still doesn't improve** - same ~25px y-offset across all
4 checkpoints tested this session despite very different training,
looks systematic, worth its own investigation later. Also delegated:
ONNX export + a fresh frames-3705-3715 re-dump (labels + the fixed ROI
overlay, see below) for physical review. Checkpoint:
`round4/runs/yolo26s_v4_imgsz1920/weights/{best.pt,best.onnx}`. Full
detail in `YOLO26_Training.md`'s "imgsz=1920 experiment" section and
[[project_yolo26n_training_pipeline]]. **Not yet promoted to
production default** - user hasn't reviewed the overnight results yet
as of this note.

**Immediate follow-up, root-caused and fixed**: user (rightly) refused
to keep training until the "frame-704 offset looks systematic across
all 4 checkpoints" observation above was explained. Investigated
properly rather than guessing - turned out to be **one mislabeled
training example** (`winC_014` = frame 704), not a pipeline/model bug:
every other detection in that frame matched its ground truth almost
perfectly (rules out a general coordinate bug), 3 other ball labels
checked elsewhere in the dataset were all correct (rules out a
systemic labeling problem), and a pixel-grid zoom showed neither the
model nor the LS "ground truth" annotation actually touched the real
ball - the human reviewer had technically touched the box
(`origin: "prediction-changed"`) without ever moving it, same
confidence score as the original AI pre-label. All 4 checkpoints
tested this session were trained on this same image with this same bad
label, which is why the offset looked consistent - never a fair
generalization test to begin with. **Fixed both copies** (local
training label file + the LS source annotation via a transient token,
not saved to any file). Doesn't retroactively fix the 4 already-trained
checkpoints or by itself justify an immediate retrain. **Open
question, not yet done**: how many other ball labels have the same
kind of miss - only 4 spot-checked (3 clean, 1 bad); a real systematic
QA pass would be the principled next step. Full detail in
`YOLO26_Training.md`'s dedicated root-cause section and
[[project_yolo26n_training_pipeline]].

**That systematic QA pass done, same session, right after**: ran the
best checkpoint against every ball-labeled image in both train/val
splits (214 instances), matched each label to its nearest prediction.
164 OK, 16 LOW_IOU, 34 MISSED (the last almost certainly just the
known recall gap, not a labeling issue). Visually checked 5 of the 16
LOW_IOU cases (smallest-distance + the single largest) - found zero
*new* label errors: 2 were label-correct frames with multiple balls
where the model picked a different real ball, 2 were label-correct
with an unrelated model false positive elsewhere, 1 was `winC_014`
itself (still flags, expected - the current checkpoint trained on the
old label). **`winC_014` looks genuinely isolated, not a pattern -
safe to keep training/using this dataset without a full manual
re-audit.**

**Same session, earlier: found + fixed a real production bug in the
field-ROI filter, both merged+pushed to `main`.** User spotted (from a
`dump_detection_frames` overlay) that the ROI outline didn't sit on the
true sideline. Root cause: the GUI's ROI editor correctly converts
*points* from its rectified preview to raw-distorted storage space, but
not the *edges* between them - a straight line on the rectified preview
becomes a curve in the fisheye-distorted raw frame, so straight-lining
between the stored vertices drifts from the true boundary. This wasn't
just a debug-drawing issue - `RoiFilteredDetector`'s real point-in-
polygon filter had the exact same straight-edge approximation. Fixed by
moving a new `reco_core::lens::densify_polygon()` (+
`FieldRoi::densified()`) into `reco-core` so both the debug tool and
the production filter share one implementation; wired at the 3 call
sites that build `AutocamConfig` from a loaded `Calibration`. New unit
tests (caught one real bug in my own first test's assumptions along the
way - fixed, not hidden). `cargo test` green across reco-core/
reco-autocam/reco-io/reco-gui. Both debug+release `reco-gui.exe`
rebuilt. See [[project_roi_inapp_editor]] for full detail.

**Also same session: reco-gui's auto-opened update-browser replaced
with a toolbar button, merged+pushed to `main`, and an upstream PR
opened.** User found it annoying that every startup with a newer
release auto-opened a browser tab (even on debug builds). Now the
check result just sets `update-available`/`update-tag`; a new toolbar
button (visible only when true) opens the release page on click
instead. Turned out an upstream issue already existed for this
(#466, filed by a different contributor, scoped to debug builds only) -
our fix resolves it more completely (all builds, not just debug).
Opened **PR #468** against `reco-project/video-stitcher` referencing
`Closes #466`, built fresh off current `origin/main` in a throwaway
worktree (adapted to upstream's plain `Button` component and toolbar
layout, which lacks the fork-only FOV badge/`FlatButton` restyle).
Not yet reviewed/merged upstream. See
[[project_update_toolbar_button]].

**YOLO26 round 4 done: yolo26s on the 258-task set (+winC hard
frames), plus a copy_paste/rect experiment - full detail in
`YOLO26_Training.md`'s "Round 4" section, condensed here.** Ball
recall is still low (0.985 precision / 0.444 recall, mAP50 0.465) - a
quick `copy_paste=0.3 rect=True` experiment (user-requested, before
collecting more data) did NOT move recall (0.435, within noise) - only
a small mAP uptick. **Conclusion: data quantity/diversity is the real
bottleneck, not training config** - the next real lever is more
labeled hard-frame data, not more hyperparameter tuning.
- Checkpoints: `round4/runs/{yolo26s_v4_3class_1280_b4_e300,yolo26s_v4_copypaste_rect}/weights/{best.pt,best.onnx}`,
  both ONNX-verified.
- **Real gotcha hit+fixed**: first training attempt crashed
  (`OSError: [WinError 1455]`, paging file too small) with the default
  `workers=8` - `reco-gui.exe` was open using ~3.9GB RAM at the time.
  Fixed with `workers=2`, retried clean, actually faster (41 min vs
  round3's 66). Didn't close `reco-gui.exe` unasked.
- **Self-correction, important**: an earlier claim in this same
  investigation ("the ball visually merges with a player during a
  dribble", from a specific frame-665 screenshot) was **wrong** -
  user pushed back ("dat weg smelten heb ik niet echt gezien"),
  re-tested with fresh evidence (not the old screenshot) and the round-4
  model, and the ball was actually isolated elsewhere in frame - I'd
  pointed at the wrong pixel location. Corrected in memory. **Lesson
  applied going forward**: when a user disputes a specific visual
  claim, regenerate the evidence fresh, don't defend from memory.
- **Round-4 now tested in the real `reco` app (2026-08-14), confirms
  the LS-validation-set conclusion on real footage.** Full 899-frame
  CLI render of `yolo26s_v4_3class_1280_b4_e300` against the same
  100-130s 03 OJC clip used for every prior ball_weight/FOV-Wide/Ball-
  reach A/B test, current full recommended settings applied
  (`--player-anchor-rad 0.4 --panner-preset action --panner-config`
  with `cluster_mode trimmed_mean, ball_weight 0.5, dead_zone_rad 0.07,
  ball_max_dist_from_cluster 1.0, fov_wide 68` `--fov-alpha 0.06
  --cluster-alpha 0.06 --lookahead 0.5 --lookahead-reduced-bit-depth`),
  built with `--features tensorrt`. Ran clean end-to-end, no crashes -
  the checkpoint itself is not the problem, recall is.
  - Overall raw-ball-detection rate: 441/899 frames (49.1%) - flat vs
    `yolo26s_v3`'s documented 48.7% on the same clip, no real gain.
  - **Frames 720-898** (`yolo26s_v3`'s documented zero-raw-detection
    gap across all 3 prior A/B renders): still **0/179 frames** with a
    raw ball detection at round-4 too. `world_state.ball` confirms:
    158/179 frames have no `ball` entry at all, 20 `Coasting`, 1
    `Lost` - genuinely untracked the whole stretch, unchanged from v3.
  - One brighter spot: frames 690-719 (the window dumped for LS
    review) hit 24/30 = 80% raw-detection at round-4 - but confidence
    is modest clip-wide (mean 0.46, range 0.10-0.92).
  - Confirms this session's earlier conclusion on the LS val set from a
    second, independent angle (real match footage, not just mAP): data
    quantity/diversity is the bottleneck, not training config. The
    720-898 gap is the clearest concrete target for the next hard-frame
    batch.
  - Test artifacts kept (not deleted, per
    [[feedback_keep_test_artifacts]]): `round4_test_100-130.mp4` +
    `.events.jsonl` in this session's scratchpad - not yet copied
    anywhere permanent.
  - **Still not done**: no rigorous unseen-frame recall test across
    multiple clips (this is one clip, one 30s window - a real full-clip
    test, not a spot-check, but still a single sample).
  - **Follow-up: frame-704 localization miss found + a stopped
    imgsz=1536 experiment (2026-08-14).** Physically re-viewed the
    690-719 dump with round-4's own predictions drawn (per
    [[feedback_no_credentials_in_tracked_files]], see password manager
    for the LS token used transiently). Frame 704's ball box (conf 0.55)
    landed ~20px off the LS-corrected ground truth - real but modest,
    less dramatic than an unreferenced crop first suggested. **Real
    process mistake, caught by the user**: re-pushed this same frame to
    LS as a "new" task without noticing it was already there as
    `winC_014` (task 1951, annotated 2026-08-13) - the winC batch *is*
    this tool's frame690-719 dump, just under an opaque renamed
    filename with no `frame_index` in it. Deleted the duplicate task
    immediately; **lesson for any future LS push: always keep
    `frameNNN_<Camera>` (or the source frame_index) in the filename**,
    never rename to an opaque batch name first.
    Tried `imgsz=1280 -> 1536` to see if more resolution tightens ball
    box precision (same data/hyperparams otherwise). Ran to epoch 187,
    manually stopped - best checkpoint stayed at epoch 28 the whole run
    (159 epochs with no improvement), and that checkpoint's ball metrics
    were *worse* than round-4's (mAP50-95 0.292 vs 0.321, recall 0.398
    vs 0.444) - only the diluted "all" number was better. Not a fair
    comparison (round-4 trained to its own patience-stop at epoch
    169/best@69, this run's best is from a much earlier relative point)
    - genuinely unresolved, not repeated further this session. Checkpoint
    kept at `.../yolo26s_v4_imgsz1536/weights/` in case worth resuming
    later, ideally overnight/unattended per the user's own suggestion.
    **Follow-up, same day: real-app tested anyway (ONNX-exported,
    `imgsz=1536`).** Mixed result vs round-4 on the same clip: overall
    raw-ball rate worse (37.0% vs 49.1%), but frames 720-898 - zero raw
    detections in *every* prior test ever run on this clip - got a
    nonzero hit rate for the first time (15/179). Frame 704 specifically:
    confidence rose 0.55->0.82 but the box landed at virtually the same
    (still ~20px off) location - the original localization complaint
    didn't improve, only confidence in the same slightly-wrong spot did.
    **Real gotcha hit+fixed**: first attempt returned 0 detections of
    any class, silently (no crash) - root cause was a stale TensorRT
    engine-cache collision (`%LOCALAPPDATA%\reco\trt-cache\` reused
    round-4's cached 1280-shaped engine for this 1536-shaped model,
    shape mismatch silently no-op'd inference). Fixed by clearing the
    cache dir and re-running. **Lesson for every future session: always
    clear `trt-cache` before testing a model with a different imgsz than
    whatever was last cached** - see [[project_tensorrt_sdk_setup]].
    **Next candidate lever, research-only, not built**: splitting each
    3840x2880 frame into overlapping left/right 2880x2880 square crops
    (SAHI-style tiling) as training-data augmentation - zero letterbox
    waste, more effective ball resolution, without imgsz's VRAM/
    BatchNorm risk. See [[project_yolo26n_training_pipeline]] for full
    detail.
- See [[project_yolo26n_training_pipeline]] and
  [[project_dump_detection_frames_tool]] for full detail.

**TensorRT now installed and working end-to-end on this PC, plus a
real crash bug found+fixed - merged into `main` (`a140731b`,
`ecf5ad9c`).** User asked why the Export dialog showed "AI: DirectML
(CPU path...)" and wanted TensorRT since they believed it was already
installed.
- Root causes found, in order: (1) reco-gui/reco-cli default features
  don't include `tensorrt` - needs `--features tensorrt` explicitly;
  (2) the zip the user had was the **TensorRT-OSS GitHub source repo**
  (parsers/plugins/samples, zero DLLs) - not the real NVIDIA SDK
  binary distribution, an easy mix-up; (3) cuDNN was also missing -
  NVIDIA's download page only offered arm64 for Windows at this
  version, real fix was the `nvidia-cudnn-cu13` PyPI wheel (has a
  win_amd64 build) via `pip install --target`.
- Installed permanently: TensorRT 10.16.1 +
  `nvidia-cudnn-cu13`/`nvidia-cublas` under `D:\SOFTWARE\`, added to
  the persistent User PATH via PowerShell
  `[Environment]::SetEnvironmentVariable(...,'User')` (not `setx` -
  the existing PATH is long enough that `setx` risked truncating it).
- **Second, more serious bug found+fixed**: first `--features
  tensorrt` reco-gui build crashed every export with "AI tracking
  failed: DML EP can only be used with CPU EPs" - reco-gui's Cargo.toml
  unconditionally forces `directml` on Windows regardless of other
  features, so this was the first binary ever combining TensorRT +
  DirectML in one ORT session (ORT hard-rejects that combination).
  Fixed in `reco-detect/src/ort_session.rs`: DirectML is now only
  queued when neither `tensorrt` nor `cuda` is compiled in. Verified
  by reproducing with `reco-cli --features tensorrt,directml` (crashed
  before, clean after) and confirming a full 899-frame export with
  real AI tracking completes end-to-end on TensorRT.
- **Confirmed this bug also exists in upstream `v0.5.4`** (identical
  code, identical forced-directml Cargo.toml) - opened
  [PR #467](https://github.com/reco-project/video-stitcher/pull/467)
  against `reco-project/video-stitcher`, cherry-picked cleanly onto
  `origin/main`, tested there too.
- **IMPORTANT for future sessions on this machine**: always add
  `--features tensorrt` when rebuilding reco-gui/reco-cli for this
  user, and make sure the three PATH entries are exported in the build
  shell (`FFMPEG_DIR`-style, see env_build_requirements.md) - a plain
  `cargo build` still works but silently regresses to DirectML with no
  warning. See [[project_tensorrt_sdk_setup]].

**New debug tool: `dump_detection_frames` - merged into `main`
(`a9b30bfe`).** User said the AI "still isn't great" after the
ball_weight fix; asked me to investigate where/why the model misses
the ball. Manual `ffmpeg -ss` frame extraction was too imprecise
(keyframe-seek, kept landing on the wrong frame). Built
`crates/reco-io/examples/dump_detection_frames.rs`: given the source
videos + `events.jsonl` + calibration + the original start-time/sync-
offset, decodes the *exact* detector-input frame sequentially (same
technique as `reco-calibrate/examples/dump_undistorted.rs`, not a
lossy seek) and draws detection boxes + the field ROI polygon on top.
`--clean` skips all overlay drawing for Label-Studio-ready frame
exports (full native res).
- Found: a high-confidence ball detection right before a tracking
  interruption was visually **merged with a player's body** during a
  close dribble; a separate low-confidence "last detection" before a
  179-frame gap turned out to be a **false positive on a player's
  shoe**, not the ball.
- **User's own catch, confirmed via the new ROI overlay**: a real ball
  sighting sat visibly outside the field ROI polygon on one frame -
  `RoiFilteredDetector` drops out-of-ROI detections *before*
  `detections_raw`, so this failure mode is invisible without drawing
  the ROI too. **User decided not to widen the ROI** - it's
  intentionally tight to keep out a kid playing with their own ball
  outside the actual pitch.
- **Next step (user's, not yet done by me)**: dumped 30 clean frames
  (690-719, right camera only) to session scratchpad
  (`ls_review_frames/frame690_Right.png`...`frame719_Right.png`, not
  yet copied anywhere permanent) for the user to review themselves in
  the "Finetuned yolo26n (rough v1)" Label Studio project - a new
  hard-frame training batch. See [[project_dump_detection_frames_tool]].

**Ball weight raised 0.35 -> 0.5, validated via real CLI A/B renders I
ran myself - merged into `main` (`9cf3fede`). Also found+fixed a real
VramPool crash bug, and found (not yet fixed) a deeper one.** User
reported a new test ("Ai Planner Test v9") still lost the ball around
frame 704 despite the fov_alpha/cluster_alpha fix below, then asked me
to run the A/B tests myself against the real 03 OJC 100-130s clip
instead of iterating manually in the GUI each time.
- Root-caused by computing `field.rs`'s actual `target_pitch =
  (cluster_pitch + pitch_bias)*(1-w) + ball_pitch*w` blend against the
  real v9 events: at `ball_weight 0.35`, when the ball breaks toward
  the near touchline (drops in *pitch* while players stay up-pitch),
  the blended aim target never gets close enough to the ball - not a
  gate problem (Ball anchor range/Ball reach/FOV Wide all already
  correct) or a smoothing problem (fov_alpha/cluster_alpha already
  raised), a blend-weight problem specific to vertical separation.
- **Verified via 3 real CLI renders** (`reco.exe stitch --start-time
  100 --end-time 130` on the actual `DJI_20260704095935_0028_D_L01` /
  `0029_D_R01` pair + `DJI Action4 Final_1.json` + `yolo26s_v3`,
  everything else held constant): `ball_weight 0.35` reproduces the
  exact symptom (ball outside half-FOV for 15 frames, 696-710);
  `0.50`/`0.60` both fully fix it. Cost: +33%/+32% mean/p95
  frame-to-frame camera movement at 0.5 vs 0.35 - real but far short of
  the wobble `1.0` was already known to cause. Docs updated (EN+NL):
  Ball weight recommendation 0.35 -> 0.5, corner-breakaway checklist
  now 5 steps (added Ball weight as the 5th).
- **Also found, separate, not settings-fixable**: frames 720-898 (last
  ~6s of the 100-130s window) have zero raw ball detections at all
  across all three renders - a genuine `yolo26s_v3` recall gap on this
  clip. Noted in the doc's "Model" paragraph.
- **Real bug found+fixed along the way**: the first CLI attempt (Native
  bit depth, matching the docs' then-current "off, only on a VRAM
  error" recommendation) crashed immediately with a wgpu validation
  panic (`RENDER_ATTACHMENT not allowed on R16Unorm`), misreported by
  the caller as "VRAM allocation failed". `VramPool::new`
  (`crates/reco-core/src/session/vram_pool.rs`) requested
  `RENDER_ATTACHMENT` on every pool texture unconditionally on a
  stated-but-false "harmless otherwise" assumption - P010's Y plane
  isn't renderable on this backend regardless. **Fixed**: only request
  it when the downconvert pass actually needs it.
- **Second, deeper bug found, NOT fixed** (documented in place in
  `copy_from_d3d11`'s doc comment, needs its own session): after that
  fix, texture *creation* succeeds but the actual D3D11 zero-copy plane
  copy still fails - `Source format (P010) and destination format
  (R16Unorm) are not copy-compatible`. **Practical upshot**:
  `LookaheadBitDepth::Native` is currently broken end-to-end for any
  10-bit source under zero-copy with lookahead on -
  `--lookahead-reduced-bit-depth` is a hard requirement right now, not
  an optional VRAM fallback. Corrected the CLI help text, GUI tooltip,
  and both docs (previously all three said some version of "leave off
  unless you hit a VRAM error", which would crash any 10-bit-source
  user who followed that advice).
- Build+test verified on merged `main`: `cargo test -p reco-core --lib`
  182/182 (excluding the 2 pre-existing unrelated CUDA-context
  failures), `cargo fmt`/`cargo clippy` clean. Both debug and release
  `reco-gui.exe`/`reco.exe` rebuilt from merged `main`. Pushed
  (`main` + `fix/vram-pool-native-10bit-render-attachment`).
- See [[project_ball_weight_vertical_break_fix]] for full detail.

**AI Tracking / panner settings now auto-persist app-wide, no Save
calibration needed - merged into `main` (`e96dfb5f`).** User asked, right
after the `fov_alpha`/`cluster_alpha` feature below shipped: "save all
AI planner parameters as soon as they change, I don't want to re-enter
them every time I restart the program." The existing
`Calibration::autocam_defaults` only survives a restart if you
explicitly click **Save calibration** - this closes that gap one layer
up.
- New `GuiSettings::autocam_defaults` (`crates/reco-gui/src/settings.rs`,
  `<config>/reco/gui.json`) - same `AutocamDefaults` struct, but
  app-level and independent of any calibration.
- `main.slint`: new `autocam-settings-changed` callback, fired by
  `changed export-xxx => {...}` on all 18 AI Tracking/panner properties
  (every field `AutocamDefaults` covers). Not wired for
  `export-model-path` (already has its own MRU-style persistence) or
  progress/status fields.
- `main.rs`: factored the previously-duplicated 18-field snapshot/restore
  code (was inline in both `do_save_calibration` and
  `try_init_and_update`) into shared `snapshot_autocam_defaults()` /
  `apply_autocam_defaults()` helpers, now used by three call sites:
  calibration save, calibration load, and the new save-on-change
  handler.
- **Restore priority, in order**: `GuiSettings`' last-used values apply
  at startup, before any video/calibration is loaded; a loaded
  calibration's own `autocam_defaults` then overrides them if present -
  calibration-level priority unchanged, just with a real fallback
  underneath instead of hardcoded `.slint` literals.
- Docs updated (EN+NL) to explain the auto-save/restore + priority order.
- Tests: 3 new in `settings.rs` (JSON roundtrip, absent-until-set,
  missing-field backward compat). `cargo test -p reco-gui --bin
  reco-gui settings::` - 12/12 pass. `cargo fmt --check` and `cargo
  clippy -p reco-gui` clean (modulo the two pre-existing, unrelated
  issues noted below).
- **Nothing outstanding** - build+test verified on merged `main`,
  pushed. Next slider drag in the Export dialog should already persist;
  next app restart should already restore it without touching a
  calibration file.

**`fov_alpha`/`cluster_alpha` (Zoom/Aim smoothing speed) now tunable,
merged into `main` (`40c532f3`).** Root-caused from a real trace ("Ai
Planner Test v7"): user reported "from frame 701 I no longer see the
ball." Even with Ball anchor range, Ball reach, and FOV Wide all raised
correctly, `FieldPannerConfig`'s own smoothing rates
(`fov_alpha`/`cluster_alpha`, both already fields, never wired to any
consumer) default to ~0.01/0.012 - a ~3s time constant at 30fps. On the
trace: FOV climbed 38.7 -> only 39.9deg (target was past 65deg) over the
~20 frames the ball stayed trackable; aim pitch barely moved while the
ball's pitch shifted 0.24 rad in the same window. The computed target
was correct - the smoothing just hadn't caught up before the ball left
frame.
- `reco-core`: `fov_alpha`/`cluster_alpha` added to
  `Calibration::AutocamDefaults` (and so also to the events.jsonl
  `run_config` header - see below). `#[serde(default = "...")]` falls
  back to `FieldPannerConfig`'s own defaults (0.01/0.012), not `0.0`
  ("never move"), for calibrations saved before this field existed.
- `reco-cli`: new `--fov-alpha`/`--cluster-alpha` flags, applied last
  (highest priority) over `--panner-preset`/`--panner-config`; starts
  from `FieldPannerConfig::default()` if neither preset nor config file
  was given but one of these flags was.
- `reco-gui`: two new sliders ("Zoom (FOV)", "Aim (cluster)") under a
  new "Smoothing speed" subsection in Advanced panner, wired through
  the same 4 sites as every other panner slider this session.
- Docs (EN+NL): new explainer section citing the v7 trace numbers, added
  as gate #4 on the corner-breakaway checklist (Ball anchor range ->
  Ball reach -> FOV Wide -> Zoom/Aim smoothing), removed from "not yet
  exposed in the GUI".
- Verified via a real CLI run (`--fov-alpha 0.06 --cluster-alpha 0.05`):
  events.jsonl's `run_config` line reflects both values correctly.
- **Also fixed in passing**: two pieces of pre-existing `cargo fmt`
  debt from earlier this session's Ball-anchor-range feature
  (`crates/reco-autocam/src/lib.rs`) - unrelated to this feature,
  committed separately (`40c532f3`'s parent).
- **Still pre-existing, not touched, both block a clean
  `cargo clippy --all-targets -D warnings` run**: (1)
  `cuda_nv12_frames` dead-code warning in
  `crates/reco-core/src/session/detection_dispatch.rs` (also already
  called out below under the Skia renderer entry - predates this
  session, from the upstream-merge commit `6ff32c37`); (2)
  `clippy::field_reassign_with_default` in two tests in
  `crates/reco-gui/src/settings.rs` (predates this session, commit
  `e99c1380`). Neither is new debt from this session's work - flagging
  so they don't get mistaken for a regression later, but not fixed
  here (out of scope for either feature that touched nearby code).
- **Nothing outstanding** on this feature itself.

**Both debug and release `reco-gui.exe` rebuilt from `main` with the
above two features** (per
[[feedback_rebuild_gui_before_user_test]] - the release build was the
part called out as "not yet done" in the previous handoff, now done).
`cargo test -p reco-core -p reco-cli -p reco-autocam -p reco-io -p
reco-gui` all green except the same 2 pre-existing CUDA-context test
failures noted below (untouched module, not a regression).
`reco-obs` doesn't build in this environment at all (missing
`libobs`/`OBS_INCLUDE_DIR`, pre-existing, unrelated) - excluded from
the workspace-wide build/test commands this session, built the
touched crates explicitly instead.

**Events JSONL is now self-describing, merged into `main`
(`8ba19adc`).** User asked: when AI logging is on, put all the AI/panner
parameters at the top of the events JSONL, in English, and mention
which YOLO model was used right at the top too. Done in two passes -
the very first line of any `--events` output is now
`{"kind":"run_config","model_path":"...","config":{...}}` with
`model_path` first (which checkpoint produced the trace is the first
thing worth knowing) and every field from `docs/ai-panner-tuning.md`'s
settings tables inside `config`, before any `frame_start` line. New
`PipelineEvent::RunConfig` variant in reco-core reuses the
`Calibration::AutocamDefaults` struct added earlier this session for
`config` (same schema, no duplication) but keeps `model_path` as a
sibling field, not part of that struct - a machine-local absolute path
doesn't belong in the calibration-persisted version. Wired through
`StitchJob::ai_run_config(model_path, config)` in reco-io, populated
from both reco-cli (resolved CLI args + panner preset/config overlay)
and reco-gui (`AutocamUiConfig` directly). Also asked to reformat the
doc's settings tables as a plain aligned block instead of markdown
tables - done in both EN/NL, matches the `run_config` field order.
Verified with a real CLI run
(`--player-anchor-rad 0.35`) - the JSONL's first line matched exactly.
Docs updated (EN+NL) to mention this. Nothing outstanding here.

**Round-3 yolo26n training done + a yolo26s comparison run alongside
it, both ONNX-exported, neither tested in the real app yet.** LS
project 8's 228 corrected tasks (200 original + 28 hard frames) used
for the first time as a training set. Full detail in the
`YOLO26_Training.md` "Round 3" section (2026-08-12) - condensed here:

- `yolo26n_v3_3class_1280_b4_e300` and `yolo26s_v3_3class_1280_b4_e300`,
  same data (194 train/34 val) and hyperparams (imgsz=1280, batch=4),
  fresh from stock weights. Both ONNX-exported and metadata-verified
  (`1x3x1280x1280` in, `1x300x6` out, `{0:person,1:ball,2:referee}`).
  Checkpoints:
  `D:\VOETBAL_VIDEO\RECO\training\round3\runs\{yolo26n_v3,yolo26s_v3}_3class_1280_b4_e300\weights\`.
- yolo26s clearly wins the direct comparison (all mAP50 0.759 vs 0.721,
  mAP50-95 0.573 vs 0.474) - consistent with the original forum-based
  preference for Small over Nano.
- **Real scare, resolved**: both new models' ball mAP50 (~0.51-0.53)
  looked like a big regression vs `v2`'s reported 0.694 - but re-running
  `v2` against the *same* round3 val set (instead of its own original
  val slice) also gives it only 0.524. **Not a regression** - the
  round3 val set (34 images/16 ball instances) is just a harder sample
  than the old one, for every model tested. `v3`/`yolo26s_v3` are at
  least as good as `v2`, slightly better on recall, on the more honest
  sample.
- **Not yet done**: real-app test for either new checkpoint (mAP alone
  isn't trusted in this project - matches every prior round's own
  practice) before picking one to actually ship over `v2`.
- Also fixed two real pipeline gaps while building the round3 dataset:
  the 28 hard-frame images from 2026-08-11 only existed in that
  session's scratchpad, not on disk here - re-downloaded from LS; and
  LS's YOLO export uses REST-uploaded images' *hash-prefixed* filename
  as the label stem (not the clean name) - stripping the hash silently
  breaks the match. Both now documented in `YOLO26_Training.md` for
  next time.

**6 hard-frame LS tasks: pushed, then reverted same session - LS
project 8 back to 228, local copies deleted too.** Found the exact
`t=123.7-125.1s` window of 03 OJC (right camera, user-confirmed ~124s
match time) where yolo26n produces zero raw detections at all - a
genuine model recall gap, not a pipeline issue (see the Ball anchor
range section below for the pipeline-side issue that was separately
ruled out/fixed). Extracted 6 frames there, soccana found the ball in
all 6 where yolo26n found nothing. **User then said they'd already
reviewed similar frames the day before (2026-08-11) - asked to delete
these 6 again as redundant.** Done: LS tasks removed via `DELETE
/api/tasks/<id>/`, project 8 back to 228; the matching local copies
also removed from
`D:\VOETBAL_VIDEO\RECO\training\finetuned_n_preds\ls_flat\images\`.
**No further action needed on these 6 specifically** - the underlying
`t=123.7-125.1s` recall gap is still real and documented (see
[[project_yolo26n_training_pipeline]]) if it turns out worth targeting
again later, just not via these exact frames.

Real, reusable lessons from getting the image format wrong twice before
the revert (kept in [[project_yolo26n_training_pipeline]] for next
time): (1) this LS instance requires `model_version: "undefined"`
(literal string) on every pushed prediction or it silently doesn't
render in the UI - don't use a descriptive/traceability value; (2)
`reco-detect`'s real inference always **letterboxes the whole,
uncropped frame** (confirmed in `crates/reco-detect/src/detectors/*.rs`
doc comments) - never crop or stretch training/review images, always
use full native resolution, matching this project's own existing 228
tasks.

**AI Tracking settings now persist in the calibration JSON, merged into
`main` (`9d8f778e`).** User asked to stop re-entering the same panner
settings after every build/session. New `Calibration::autocam_defaults`
(`crates/reco-core/src/calibration.rs`) holds the tunable subset of
`AutocamConfig`/`FieldPannerConfig` (tracking mode, detection interval,
**Ball anchor range**, lookahead, preset/framing, cluster mode/bandwidth,
dead-zone, ball weight, **Ball reach**, **FOV Wide/Tight/Default**) -
deliberately excludes the model path (already persisted separately via
`user_settings`) and the enabled toggle.
- `reco-gui`: `do_save_calibration` snapshots the current Export-dialog
  slider values into `cal.autocam_defaults` on every "Save calibration"
  (these aren't part of the live renderer like topology/lens sliders, so
  sync-on-save rather than sync-on-every-edit); `try_init_and_update`
  restores them onto the sliders right after a calibration loads
  (before the VRAM lookahead-safety clamp, so a restored value still
  gets clamped if it wouldn't fit).
- `#[serde(default)]` throughout, so calibrations saved before this
  change keep loading fine (verified via a dedicated test).
- **Not yet done**: user hasn't actually saved a calibration with the
  recommended settings dialed in yet - next time they open the Export
  dialog, set the values below, and hit **Save calibration**, that
  calibration file becomes self-contained and won't need re-entering
  them again.

**New GUI slider merged into `main` (`ce036017`) - "Ball anchor range",
the real fix for the corner-breakaway ball going "Lost".** Follow-on
from the FOV Wide finding below: user asked to test the FOV Wide fix,
checked ball detection around t=24s in `Ai Planner Test v3.mp4`, and the
ball tracker was coasting/going `Lost` right through a moment where the
raw YOLO26 model actually detected the ball at **0.97 confidence** -
confirmed via raw `detections_raw` events, ruling out a model/recall
problem.
- Root cause: `BallTracker` (`crates/reco-autocam/src/trackers/ball.rs`)
  has its own **player-anchor gate**, upstream of everything in
  `FieldPanner` - a raw ball detection is only accepted if it's within
  `player_anchor_max_rad` (hardcoded `DEFAULT_PLAYER_ANCHOR_RAD = 0.20`
  rad / ~11deg) of at least one tracked player. A genuine breakaway ball
  sits outside this on purpose (that's what makes it a breakaway), so it
  gets dropped before the panner - and before Ball reach or FOV Wide -
  ever sees it. `with_player_anchor_rad()` existed as a builder method
  but was dead code in production (only unit tests called it).
- **Built and merged**: `AutocamConfig::player_anchor_max_rad` (new
  field + builder in `reco-autocam/src/lib.rs`, wired into both
  `BallTracker` construction sites), a `reco-cli --player-anchor-rad`
  flag, and a new **"Ball anchor range"** GUI slider (0.1-0.8 rad,
  top-level AI Tracking controls, next to "Detect every N frames" - it's
  a tracker knob, not part of `FieldPannerConfig` presets). Docs updated
  with the full 3-gate order (Ball anchor range -> Ball reach -> FOV
  Wide). `cargo test -p reco-autocam -p reco-gui -p reco-cli` all green,
  build+launch smoke-tested clean.
- **Not yet done**: user hasn't re-tested with Ball anchor range raised
  (try 0.3-0.5+) to confirm the t=24s breakaway is now actually tracked
  end to end (check the events JSONL for `state: Tracking` instead of
  `Coasting`/`Lost` during that window) and that the resulting shot
  matches the Once AutoCam reference framing.

**Panner testing (earlier this session, continues 2026-08-11's
investigation) - FOV Wide is the missing piece, not new panner code.** User exported
`Ai Planner Test v1/v2.mp4` + `.events.jsonl` (in the `TEST VIDEO`
folder) with Ball reach already raised to 1.0 rad, and compared against
a competitor's output (`Once AutoCam 100-130sec.mp4`, same match, t=24s)
that keeps a breakaway ball-carrier and the main group in frame
together - something our export couldn't reproduce yet.
- Root-caused by reading `FieldPanner::target_fov`
  (`crates/reco-autocam/src/panners/field.rs:745-777`): the widen-for-
  the-ball logic **already exists** (`needed = (ball_offset_deg +
  ball_frame_margin_deg) * 2`, then `fov.max(needed)`) but is clamped to
  `fov_wide`, and the `action`/`broadcast` presets cap that at 48/58° -
  too low for a genuine breakaway to ever open the shot up enough.
  **Confirmed by a controlled CLI A/B render** (same clip/moment/every
  other setting held constant, only `fov_wide` changed): 48° clips one
  of the two actors, 70° holds both, matching the competitor's framing
  style. No panner code change needed - `fov_wide` is already
  GUI-tunable up to 90°.
- **Updated recommendation** (now in `docs/ai-panner-tuning.md`/`.nl.md`):
  action preset + cluster_mode trimmed_mean + dead_zone 0.05-0.08 +
  ball_weight 0.35 + **ball_max_dist_from_cluster (Ball reach) 1.0** +
  **fov_wide (FOV Wide) 65-70°** (was just Ball reach alone before this
  session - that wasn't sufficient by itself, this session found why).
- Wrote an English problem write-up for the user to discuss with an
  engineer, initially claiming this needed new panner logic - **that
  write-up was wrong and was corrected once the A/B test disproved it**;
  don't reuse the first version if it's referenced anywhere.
- Analysis gotcha for next time: the events JSONL's `timestamp_ms`
  field (`frame_start`/`world_state`/etc.) is wall-clock elapsed
  *processing* time, not video presentation timestamp - correctly
  documented as such in `crates/reco-core/src/detect/panner.rs`'s doc
  comments, just easy to misread as video-relative time by the field
  name alone. Use `frame_index / output_fps` for actual video-relative
  timing when correlating events to specific moments in the exported
  clip.
- **Not yet done**: user hasn't re-tested their own export with the new
  `fov_wide: 65-70` recommendation applied.

**Workflow change this session, apply going forward**: user wants every
feature branch merged into `main` and pushed as soon as it builds, not
held back on its own branch pending testing/confirmation - `main` is the
always-integrated local test build. See
[[feedback_merge_features_into_main_immediately]]. Upstream PRs (via the
fork) are cut from `main`'s history later, once a feature is actually
confirmed working - being merged into `main` and being "PR-ready" are
independent.

**Three things landed and are now all merged into `main`, pushed to
`github/main` (`45741a99`). None are upstream-PR'd yet.**

1. **`feat/goal-line-calibration`**: Goal editor/calibration/entry-
   detection now in `main`. **Known limitation carried over, unchanged**:
   real-footage verification (from that branch's own history) found the
   test goal polygon misplaced - not yet a confirmed-working feature,
   needs that fix before it's PR-ready upstream. See
   [[project_goal_detection_idea]].
2. **Skia renderer swap** (fixes the wobbly-text report,
   [[project_skia_renderer_future_task]]): `renderer-femtovg-wgpu` ->
   `renderer-skia` in `crates/reco-gui/Cargo.toml`, plus
   `default-font-family: "Segoe UI"` pinned on the root Window (fixes a
   slightly-larger-text side effect the user caught - Skia/DirectWrite
   and femtovg/fontdb were resolving the previously-unset generic
   sans-serif fallback to fonts with different em-box metrics). **User
   confirmed text looks good** after the font-family fix. **Still open**:
   `cargo clippy -D warnings` fails on a pre-existing, unrelated
   `cuda_nv12_frames` dead-code warning in `reco-core` (not caused by
   this change) - needs its own fix before this can pass CI for an
   upstream PR.
3. **Ball-reach GUI slider**: answers 2026-08-11's open question about
   `ball_max_dist_from_cluster` (user picked "add a GUI slider"). New
   "Ball reach" slider in the Export dialog's Advanced panner section.
   Build-verified only so far - **not yet tested against the actual
   corner-ball footage** from 2026-08-11's investigation (see that
   session's recommended test settings further down, and the new
   `D:\VOETBAL_VIDEO\Berghem Sport J011-1\03 OJC -Bergem Sport
   04072026\TEST VIDEO\` location the user moved test clips to).

**Build state**: debug `reco-gui.exe` rebuilt from `main` with all three
merged and smoke-tested clean (loads calibration, zero-copy preview
initializes, no panics). `cargo test -p reco-autocam -p reco-gui` all
green (102 tests, including the ROI-polygon tests the goal-editor merge
touches). `cargo test -p reco-core` has 2 pre-existing, unrelated
CUDA-context failures (`interop::cuda::tests::test_cuda_available`/
`test_shared_memory_allocation`, `cudaGetDevice` error code 3) -
untouched module, looks like GPU-context contention from another running
process rather than a real regression, not investigated further.
**Release build not yet done this session** - attempting it was blocked
by the harness's permission classifier; run `cargo build --release -p
reco-gui` manually (with the FFMPEG_DIR/LLVM PATH env vars, see
env_build_requirements.md) before relying on a release binary, per
[[feedback_rebuild_gui_before_user_test]].

- **What changed** (mirrors the existing `ball_weight` slider's plumbing
  exactly):
  - `crates/reco-gui/ui/main.slint`: new `export-ball-max-dist-from-cluster`
    property (default 0.5) + a "Ball reach" `LabeledSlider` (0.0-1.5 rad)
    in the Export dialog's "Advanced panner" section, directly under
    Dead-zone.
  - `crates/reco-gui/src/export.rs`: new field on `AutocamUiConfig`,
    included in the export-run JSON and mapped onto
    `reco_autocam::panners::FieldPannerConfig`.
  - `crates/reco-gui/src/main.rs`: wired both directions - preset
    application sets the slider, export-start reads it back out.
  - `docs/ai-panner-tuning.md` + `.nl.md`: new "Ball reach" explainer
    paragraph, added to the presets table (0.5 in every preset - no
    preset overrides it), removed from the "not yet exposed in the GUI"
    list, and a new "camera won't follow the ball into a corner"
    practical-tuning bullet pointing at this slider (or `Tracking mode ->
    ball` as the alternative).
- **Verified this session**: `cargo build -p reco-gui` clean (no new
  warnings), and the built exe was launched (not clicked through - no
  synthetic mouse input, per feedback_synthetic_gui_automation_risk.md)
  and confirmed to start cleanly: loaded the last-used calibration/inputs,
  GPU pipeline and zero-copy preview came up with no errors in the log.
  **Not yet visually confirmed in the Export dialog UI**, and not yet
  tested against the actual "camera won't follow the ball into the
  corner" symptom from 2026-08-11.
- **Next step**: user opens the Export dialog, confirms the "Ball reach"
  slider renders correctly under Advanced panner -> Dead-zone, raises it
  above 0.5 rad, and re-runs the same corner-ball clip from 2026-08-11's
  `--panner-config` A/B testing to confirm the camera now follows. Once
  confirmed, branch off `main` (e.g. `feat/ball-reach-gui-slider`), commit,
  and open the upstream PR (see project_upstream_pr_workflow.md for the
  fork/PR mechanics used for every other feature PR so far).

**Also unresolved from 2026-08-11** (unchanged, see that session's full
arc below for detail):

1. **Waiting on the user**: LS project 8 ("Finetuned yolo26n (rough v1)")
   had 28 new pending tasks (228 total, 200 finished) as of 2026-08-11 -
   hard ball-miss frames from real footage, soccana pre-labeled. Per the
   (uncommitted, unmerged) `feat/mlpipe-gui` branch's own handoff note,
   the user confirmed later that same evening (on RUFAN_LAPTOP) that all
   228 are now reviewed/corrected - but round-3 training itself is still
   blocked there (no CUDA/data-drive on that machine) and needs to run
   **here on TGR_PC**: re-run `prepare_yolo_train_split_from_ls_export.py`
   against project 8's fresh 228-image export, then train `yolo26n_v3`
   (same 1280/b4/e300 hyperparams as `v2`).
2. **New branch discovered this session**: `git pull` on `main` looked
   like a no-op ("already up to date"), but a full `git fetch --all` found
   the user had pushed real work to a new, unmerged branch overnight -
   `feat/mlpipe-gui` (1 commit, `d5071077`): a Streamlit GUI
   (`scripts/mlpipe/`) consolidating the yolo26n export/train/ONNX-export
   pipeline, 15 regression tests, verified against the real Pi-hosted LS
   instance. Local tracking branch `feat/mlpipe-gui` now created (tracks
   `github/feat/mlpipe-gui`). Per its own handoff note it still needs a
   real run-through on a CUDA+data-drive machine (i.e. here) and a manual
   browser click-through - not done yet this session, deferred in favor of
   the ball-reach slider work above.

## This session's full arc (continues yesterday's yolo26n pivot -
2026-08-10's session ended with review not started; today's picked back
up with project 8 confirmed fully reviewed, LS API used live to check
18/19's status)

1. **Confirmed via the LS API** (not by asking blind) that only project
   8 (200/200 finished) was ready to train on; projects 18 ("01
   Vierluik", 2/556) and 19 ("02 RPC", 19/200) are barely started -
   deferred those, trained on project 8 only for a clean comparison to
   the earlier yolo26s `rough_v7` round (identical data/split/hyperparams).

2. **Trained two yolo26n rounds** on project 8's 170/30 split (3-class,
   person/ball/referee), fresh from stock `yolo26n.pt`:
   - `yolo26n_v1_3class_1280_b4_e150` (150 epochs, fixed) - ball mAP50
     0.649, all mAP50 0.736.
   - `yolo26n_v2_3class_1280_b4_e300` (300-epoch budget, ultralytics'
     own early-stopping kicked in at epoch 181, best checkpoint from
     epoch 81) - ball mAP50 0.694 (better recall, worse precision than
     v1), all mAP50 0.710. **v2 is the checkpoint in active use** -
     chosen for the better ball recall, the actual project goal.
   - Both a bit behind yolo26s's `rough_v7` on identical data (expected,
     smaller model), ~25% faster to train.

3. **Exported v2 to ONNX** (`nms=True` requested; ultralytics forces it
   off for this end2end architecture but the output shape is already
   the needed `[1,300,6]` regardless - confirmed via direct `onnx.load`
   metadata inspection: input `1x3x1280x1280`, output `1x300x6`, names
   `{0:person,1:ball,2:referee}` in the exact dict-string format
   `reco-detect` parses).

4. **Tested in the real `reco` app end-to-end**, CPU then GPU:
   - CPU (default `ort` feature): worked, ~1.7 fps, confirmed correct
     class-id resolution and real ball-tracker acquire/lose behavior on
     a live 03 OJC clip.
   - **Found and fixed a real gap**: `reco-cli`'s `Cargo.toml` had no
     `directml` feature passthrough (had `cuda`/`tensorrt`/`coreml`, not
     `directml`) - added the missing line. Also explains why the CPU
     run's log claimed "DirectML execution provider enabled" when it
     had actually silently fallen back to CPU (`reco_detect::ort_session`
     logs "enabled" on any `Ok` result without checking the EP actually
     attached - not fixed, just understood).
   - GPU (DirectML) build: **~18 fps avg, ~10x the CPU speed**,
     comfortably real-time-capable. Hit and fixed a VRAM budget error
     along the way (`--lookahead-reduced-bit-depth`, the fix already
     shipped in an earlier session).

5. **Added 28 hard-frame training examples to LS project 8** - pulled
   raw frames from two confirmed ball-miss windows in real footage,
   soccana-pre-labeled (20/28 got a soccana ball box), pushed via the
   REST import+predictions API (recreated the push script fresh this
   session, scratchpad only). Hit and fixed one real bug: re-uploading
   the same filename twice in one run silently mis-attaches the
   prediction to the *first* matching task, leaving an orphaned
   zero-prediction duplicate - caught via a `total_predictions==0`
   sweep, deleted the orphan.

6. **User reported a real symptom** from their own export test: camera
   pans stuck for ~7s during real, continuous open play (not a
   stoppage - confirmed by pulling actual video frames) whenever the
   ball goes undetected for a few seconds. Root-caused through a long
   back-and-forth of `--panner-config` A/B tests against the identical
   clip:
   - Dead-zone alone: didn't fix it.
   - `cluster_mode: density -> trimmed_mean`: fixed the freeze (density
     was locking onto a static sub-group of players instead of
     following the whole formation).
   - User then reported the *opposite* problem (wobbly/jittery) after
     applying the fix - turned out their real GUI settings (shared via
     screenshot) differed from this session's CLI reproduction in ways
     that mattered: `ball_weight=1.0` (manually maxed, not the `action`
     preset's own 0.35), `detection_interval=3`, `lookahead=0.5s`.
     Reproduced the wobble exactly once matched. Swapped in a relabeled
     `soccana.onnx` (fixed a real gotcha: `model.names[i]=...` on the
     ultralytics Python wrapper doesn't persist to the exported ONNX,
     had to patch the ONNX metadata directly) with identical settings -
     jitter was the same or worse, **ruling out yolo26n's detection
     quality as the cause**. Confirmed `ball_weight=1.0` was the actual
     culprit - `0.35` roughly halved the jitter without bringing the
     freeze back.
   - User then asked if `field_roi` explains a separate "camera won't
     reach the corner" symptom. Tested directly (calibration copy with
     `field_roi` stripped) - ROI does filter real detections but didn't
     change the camera's pan/tilt range. Found the real mechanism
     instead by pulling the actual video frame at the exact timestamp:
     a genuine, isolated ball far from the main player cluster gets
     rejected by the panner's `ball_near_cluster` gate
     (`ball_max_dist_from_cluster`, default 0.5 rad, not GUI-exposed) -
     "Action" framing's designed behavior (stay with the crowd), not a
     bug. Left as an open question for the user (see above).

## Other threads, unchanged since 2026-08-07

- **Goal-scored detection** (`feat/goal-line-calibration` branch):
  still paused pending better ball-model quality - yolo26n_v2 is
  meaningfully better than the original blocker, worth revisiting once
  the two open items above are settled.
- **Veo Cam 3 competitive roadmap**: `docs/research-veo-cam3-comparison.md`.
- **Upstream PRs** (#422-435, #464): awaiting owner review/merge.
- reco-gui app icon: still waiting on a source image from the user.
- v0.5.4 upstream sync: still deliberately deferred to its own session
  (91 fork-only vs 23 upstream commits, 20 real conflicts incl. a
  structural one - see `project_v054_upstream_sync` memory).

## Machine-specific reminders (still valid)

- FFMPEG_DIR and LLVM PATH needed per-shell for any Rust build - see
  env_build_requirements.md.
- This machine (TGR_PC) has a CUDA-working `ultralytics`/`torch` (RTX
  3060 Ti) - the CUDA build needed the explicit
  `--index-url https://download.pytorch.org/whl/cu128` at install time,
  default `pip install torch` gives CPU-only.
- `python` isn't on PATH as a bare command in this session's shell -
  use the full path,
  `C:\Users\Rufan\AppData\Local\Programs\Python\Python314\python`.
- Don't drive reco-gui's UI with synthetic mouse/keyboard input.
- `git fsck --full` before pushing, per feedback_git_object_corruption.md.
- `soccana.pt` and the newly-relabeled `soccana.onnx` both live at
  `D:\VOETBAL_VIDEO\RECO\training\models\` on this machine only - not
  git-tracked (third-party-derived binaries), redownload/re-export from
  the Hugging Face URL + relabeling steps in `YOLO26_Training.md` if
  working from the other PC.
- GPU renders can hit a transient `GetData timed out (>1M polls)`
  D3D11VA staging error after many back-to-back runs in one session -
  not reproducible, just retry.
