# Session handoff - 2026-08-06

Continuation note for resuming work on a different machine/session -
git-tracked so it travels with `git pull`/`push` between the user's two
PCs. Overwrite wholesale at the end of a session rather than appending
history - git history is the append-only log, this file is just "where
things stand right now."

## Immediate blocker (why this session paused)

User is heading home to continue on the other PC. Mid-way through
verifying the new goal-detection primitive (see below) against real
match footage. Blocked on: could not confirm which moment "5:55, goal on
the left side" refers to.

Found the right video pair + calibration (via
`C:\Users\Rufan\AppData\Roaming\reco\config\gui.json`'s `recent_*`
entries - this is the actual source of truth for "what was the GUI last
looking at", not file mtimes):
- Left: `D:\VOETBAL_VIDEO\Berghem Sport J011-1\03 OJC -Bergem Sport
  04072026\L\DJI_20260704095935_0028_D_L01.MP4`
- Right: same folder, `R\DJI_20260704095935_0029_D_R01.MP4`
- Calibration: `D:\VOETBAL_VIDEO\Berghem Sport J011-1\03 OJC -Bergem
  Sport 04072026\DJI Action4 Final_1.json` - already has a `goal_geometry`
  (both `left` and `right` polygons present, drawn during this session's
  live GUI test) and a `field_roi`.

Extracted frames at t=355s (5:55) and a contact-sheet sweep from
t=280s to t=430s from both raw cameras (scratch files at
`D:\VOETBAL_VIDEO\RECO\scratch_goal_check\`, not git-tracked, safe to
delete) - no obvious goal-scoring moment visible in that window in
either camera. Likely wrong assumption about the time reference: either
"5:55" means match-clock time rather than raw-file time (the match spans
4 files, 0028 through 0031, each ~20 min - if recording started before
kickoff, match-time and file-time diverge), or it's simply a different
segment. Asked the user to clarify (file? match-time vs file-time?) -
question was interrupted by the user ending the session, so this is
still genuinely open. Resume by getting a precise time reference before
spending more time hunting frames.

Also worth knowing for next time: the two cameras are an L-shape rig at
the halfway line, each camera facing one end of the pitch - the left
camera's calibration space is NOT "the left half of the panorama," it's
"one full raw fisheye camera," so "goal on the left side" most likely
means "the goal visible in the left camera's frame," but that still
needs confirming against which end of the pitch the user means.

## This session's main thread: goal-scored detection (branch
`feat/goal-line-calibration`, pushed to `github` remote, not a PR yet -
explicitly holding off until the raw signal is verified against real
footage per the user's own "als het goed gelukt is" requirement)

Full context in project_goal_detection_idea.md (update it - it predates
this session's design pivot) and the new project memory for this branch
(save one referencing this file if not already present). Built, in
order, across 8 commits:

1. **First design attempt (superseded, still visible in git history -
   branch was not rebased, new commits correct forward instead):**
   `GoalGeometry` stored as panorama yaw/pitch-space polygons, with a
   from-scratch `screen_fraction_to_yaw_pitch`/`yaw_pitch_to_screen_
   fraction` inverse/forward projection pair in `reco-core` (real bug
   caught by its own round-trip test: `direction_to_yaw_pitch` needs a
   *normalized* direction vector, the unprojected camera ray wasn't
   normalized - fixed). A reco-gui editor was built on the *panorama*
   preview with a separate "GOAL LINE" card.

2. **User feedback, twice, corrected the design:**
   - First: don't put it on the panorama preview - reuse the exact same
     distorted lens-preview window the Field ROI editor already uses,
     with a single "Edit ROI / GOAL..." button and a small ROI/GOAL
     toggle pill top-left over that preview to pick which polygon is
     currently being drawn.
   - This meant `GoalGeometry` actually needed to store per-camera raw-
     distorted-frame-normalized `[0,1]` polygons - the *same* space as
     `FieldRoi` - not yaw/pitch. Removed the now-unused yaw/pitch
     projection functions and their tests from `reco-core` (no other
     caller; straightforward to rebuild later if e.g. reco-obs's
     interactive pan/zoom ever needs a panorama-click-to-yaw/pitch
     primitive).
   - The polygon-vs-line reasoning (a line only bounds width; a ball
     lobbed over the crossbar at the same horizontal position as a real
     goal would be indistinguishable without also bounding height) holds
     equally well in per-camera pixel space, so nothing was lost by the
     pivot.

3. **Final reco-gui editor** (`main.slint`/`main.rs`): one merged
   "DETECTION ZONES" card, single Edit/Done button, `zone-edit-mode` +
   `zone-edit-type` ("roi"/"goal") replacing the earlier separate `roi-
   edit-mode`/`goal-edit-mode` flags since both types now share identical
   lens-preview enter/exit bookkeeping. Goal overlay uses a distinct
   orange (`#ff8c1a`) vs the field ROI's green. User tested this live and
   confirmed it works well.

4. **`reco-autocam::GoalEntryDetector`** (new module
   `crates/reco-autocam/src/goal_events.rs`): a generic
   `ZoneEntryTracker` (outside->inside polygon transition, one event per
   entry not per frame-inside, reusing the same `point_in_polygon`
   primitive `RoiFilteredDetector` already relies on) plus a goal-
   specific wrapper with one tracker per camera side. 7 new tests, all
   passing. Deliberately scoped as a **raw, unconfirmed** signal -
   documented clearly that a 2D polygon-entry event doesn't prove a real
   goal (corner deliveries / shots over the bar can pass through the same
   on-screen region). The planned confirmation layer (ball returns to a
   kickoff/center-circle position) depends on kickoff detection, which
   doesn't exist yet - tracked as a separate, not-yet-started item, not
   stubbed out.

**Not done yet, in order**:
- Resolve the timestamp ambiguity above, then actually run the ball
  detector (`yolo26s.onnx`, path already in gui.json) over the right
  window and feed real detections through `GoalEntryDetector` to see if
  it fires at the right frame - this was the goal of the session's last
  stretch, interrupted before completion.
- `GoalEntryDetector` is not wired into any live pipeline yet (no CLI
  flag, no `--events` integration) - it's a tested, standalone primitive
  only.
- Kickoff/restart detection (needed before a raw entry can become a
  confirmed "goal scored" event).
- Once verified end-to-end, prepare as an upstream PR per the user's
  original instruction - probably rebuilt cleanly off `origin/main` in
  an isolated worktree, same pattern as PR #435/#464, since this branch
  has some now-superseded commits in its history from the design pivot.

## Other threads, unchanged since 2026-08-05 (condensed - full detail in
memory / earlier git history, not repeated here)

- **YOLO26 training pipeline**: auto-labeling pipeline built and
  validated (pilot: 300 images, 2491 boxes). Labeling-tool plan pivoted
  CVAT -> Label Studio on a Raspberry Pi 5 (CVAT doesn't fit the NAS's
  RAM or the Pi's arm64). Pi still physically unopened/not set up - see
  project_yolo26n_training_pipeline.md for the full checklist and
  status. `scripts/package_yolo_for_labelstudio.py` (committed) is ready
  and tested, blocked only on the Pi's real `LOCAL_FILES_DOCUMENT_ROOT`
  once it exists.
- **Veo Cam 3 competitive roadmap**: `docs/research-veo-cam3-comparison.md`,
  5 phases, goal detection above is Phase 1 item 2. Not filed as GitHub
  issues yet.
- **Upstream PRs** (#422-435, #464): all still awaiting owner
  review/merge on `reco-project/video-stitcher`. No new action needed
  this session.
- reco-gui app icon: still waiting on the user to provide a source image.

## Housekeeping this session

- Confirmed `git fsck --full` clean before pushing (only harmless
  dangling blobs, no corruption) - see feedback_git_object_corruption.md,
  this machine has a known history of loose-object corruption on large
  commits.
- User confirmed (again) plain ASCII only, no special characters - see
  feedback_no_em_dash.md.
- User explicitly said not to bother with release builds while
  iterating on this feature - debug build + smoke test (launch, load a
  real calibration, confirm no errors) is enough for now. Still rebuild
  and relaunch reco-gui.exe (debug) after every .slint/.rs change before
  claiming something is ready to test - a stale running process silently
  shows old behavior.

## Machine-specific reminders (still valid)

- FFMPEG_DIR and LLVM PATH needed per-shell for any build - see
  env_build_requirements.md. `FFMPEG_DIR/bin` also needs to be on PATH
  to run reco-gui.exe/reco-cli.exe (dynamically linked) or to use the
  standalone `ffmpeg.exe`/`ffprobe.exe` CLI tools for ad-hoc frame
  extraction (used this session to pull verification frames).
- Don't drive reco-gui's UI with synthetic mouse/keyboard input -
  reliably unreliable in this app, has corrupted real calibration data
  before.
- reco-obs won't build on this machine right now (missing
  `obs-frontend-api.lib`, headers-only OBS SDK setup - see
  env_build_requirements.md's OBS section) - expected, exclude it
  (`--exclude reco-obs`) from any workspace-wide build/test command
  rather than treating it as a new regression.
