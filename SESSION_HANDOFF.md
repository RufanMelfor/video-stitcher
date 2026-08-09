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

**UPDATE 2026-08-06, RUFAN_LAPTOP session - blocker resolved (mostly)**:
user corrected the file location -
`D:\VOETBAL VIDEO\RECO test vid\DJI_20260704095935_0028_D_L01.MP4`
(note: different path/spacing than above, same match, local copy on
this laptop with its own calibration file already containing
`goal_geometry`). No YOLO ONNX model exists on this laptop at all
(`ai_model_path: null` in this machine's `gui.json`, no `.onnx`
anywhere on the drive - checked) so the actual `GoalEntryDetector`-vs-
real-detections test still could not run here, that needs a machine
with the model.

But: used the calibration's own `goal_geometry.left` normalized
coordinates directly (rather than eyeballing crop coordinates off a
downscaled wide shot - that was tried first and landed on the wrong,
distant goal on what looks like an adjacent pitch) to zoom into the
correct goal via ffmpeg crop+scale. Found a strong visual candidate at
**t=356-358s** in this exact file (close to the user's "5:55" /
t=355s estimate, well within normal manual-timestamp error): a player
down on the ground right in/near the goal mouth, several other players
converging at the same moment - matches "hard to see" well (obscured by
the scramble, not a clean shot-into-net view).

So: "5:55" was raw file-time after all, off by only ~2-3s - the
match-clock-vs-file-time theory above was likely a red herring. Frame
crops were scratch files in this laptop session's temp dir, not saved
persistently - re-extract from the source at t=350-362s if needed
rather than hunting for them.

**Next step, now unblocked**: on a machine with the ball-detection ONNX
model available, run detection over roughly t=350-362s of this exact
file/camera pair and feed the results through `GoalEntryDetector` to
confirm it actually fires in that window.

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
  RAM or the Pi's arm64). Update 2026-08-06: Pi is now physically set up
  and Label Studio is running, reachable at `http://192.168.191.204:8080`
  - see project_yolo26n_training_pipeline.md for the full checklist and
  status. Update 2026-08-06 (later): Label Studio project created on the
  Pi via the Visual labeling-setup editor (Custom template), with a
  `RectangleLabels` config for "person"/"ball" - object/control tag
  names should be `image`/`label` (script defaults), confirm via the
  Code toggle before importing tasks.json if unsure. Data import was
  skipped at creation time. Next (from the other PC): point
  `scripts/package_yolo_for_labelstudio.py` (committed, tested) at the
  Pi's real `LOCAL_FILES_DOCUMENT_ROOT`, rsync the pilot dataset onto
  the Pi, generate `tasks.json` with `--run-converter`, then import it
  into this project and do the actual human review - that's the
  remaining bottleneck before fine-tuning can start.
  **Update 2026-08-09 (RUFAN_LAPTOP session)**: the Pi's ZeroTier IP
  (`192.168.191.204`) is currently broken from this laptop - ICMP
  replies but every TCP port (8080, 22) times out, root cause not
  diagnosed. Use the Pi's plain LAN IP instead, `http://192.168.1.73:8080`
  (same subnet as this laptop, works directly). No SSH key set up to the
  Pi either (password auth only) - rsync-onto-`LOCAL_FILES_DOCUMENT_ROOT`
  steps still need real Pi access; worked around it entirely this
  session by uploading images through Label Studio's own REST file-
  upload API instead (`POST /api/projects/<id>/import` with the file as
  multipart - creates one task per image directly in LS's own storage,
  no rsync/SSH needed). Gotcha: that endpoint's response does *not*
  include `task_ids` in this LS version (1.23.0) despite creating the
  tasks fine - fetch `GET /api/tasks?project=<id>` afterwards and match
  by filename (`/data/upload/<project>/<hash>-<original_filename>`) to
  get real task ids back, e.g. before calling `POST /api/predictions/`.

  Before committing further to yolo26x as the pre-label teacher model,
  ran a visual side-by-side: same 150 frames (evenly sampled across
  `D:\VOETBAL VIDEO\RECO test vid\DJI_20260704095935_0028_D_L01.MP4`,
  left camera, ~20min) pushed into 4 new LS projects, each pre-labeled by
  a different model via the API - project 9: `Adit-jain/soccana` (yolo11n,
  football-trained, player/ball/referee); project 10: `martinjolif/yolo-
  football-player-detection` (yolo11m, football-trained, 4-class incl.
  goalkeeper); project 11: `martinjolif/yolo-football-ball-detection`
  (yolo11n, ball-only specialist); project 12: stock `yolo26x.pt` (COCO-
  pretrained, not football-trained, filtered to person/ball same as this
  project's own convention). **User's verdict: soccana (project 9) looked
  best of all four, beating even stock yolo26x** - domain-specific
  football training outweighed yolo26x's newer/larger architecture here.
  Ball-only specialist (project 11) was the weakest, missing the ball in
  94/150 frames. Practical implication: prefer `Adit-jain/soccana` (or
  similar football-trained yolo11) as the pre-label/teacher model in
  `export_yolo_labels.py`'s workflow instead of yolo26x - now has
  empirical support on this project's own footage, not just forum
  opinion. Needs a class-name remap at export time (soccana's player+
  referee -> person), same as already done for stock COCO.

  Also connected a *live* Label Studio ML backend to project 9 (not just
  static batch predictions): a small Flask app using the `label-studio-
  ml` SDK (`LabelStudioMLBase` subclass wrapping the soccana model),
  running locally on this laptop on port 9091, registered via `POST
  /api/ml/` - shows "Connected" in the project's Model tab and returns
  live predictions on demand. Needed a Windows Defender Firewall inbound
  rule for TCP 9091 (`New-NetFirewallRule ... -LocalPort 9091`) - fails
  with "Toegang geweigerd"/access denied unless PowerShell is run
  elevated (Administrator). Only live while this laptop + that Python
  process stay running; falls back to the static predictions otherwise.
  All throwaway scripts/weights for this comparison live in the session
  scratchpad, not git-tracked - this note is the only record of them.
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
