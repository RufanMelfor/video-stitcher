# Session handoff - 2026-08-07

Continuation note for resuming work on a different machine/session -
git-tracked so it travels with `git pull`/`push` between the user's two
PCs. Overwrite wholesale at the end of a session rather than appending
history - git history is the append-only log, this file is just "where
things stand right now."

**No usernames/passwords/IP addresses in this file, ever** - explicit
user instruction 2026-08-07 after a Pi IP address slipped in the day
before. Reference "see password manager" / `zerotier-cli listnetworks`
instead. This also applies to this assistant's own memory files, not
just git-tracked ones.

## YOLO26 labeling pipeline - DONE end-to-end (2026-08-07)

Full detail in project_yolo26n_training_pipeline.md. Summary: the
Raspberry Pi is set up, Label Studio is running, the 300-image pilot
batch (with person/ball predictions) is imported and verified working -
images load, task/prediction counts confirmed via direct DB query. Two
real Label Studio gotchas hit and fixed along the way, documented in the
memory file:
- `LOCAL_FILES_SERVING_ENABLED`/`LOCAL_FILES_DOCUMENT_ROOT` env vars
  alone are not enough - also needs a registered Local Files storage
  connection (UI: Project Settings -> Cloud Storage -> Add Source
  Storage -> Local Files; or `POST /api/storages/localfiles`), or every
  image request 404s even though the files are genuinely there.
- Drag-and-drop import via the UI silently did nothing (confirmed via
  direct SQLite inspection - zero rows, not even a failed-import
  record). Re-did it via the REST API instead
  (`POST /api/projects/<id>/import`), which worked and is verifiable.
- API auth: legacy static tokens are disabled by default on this
  version: a token from Account & Settings is a JWT *refresh* token,
  single-use/rotates on exchange - trade it for a short-lived access
  token via `POST /api/token/refresh/` first, and expect to need a fresh
  one from the user each new session (can't be reused across sessions).

**Next**: human review of the pilot batch (user-driven), then set up
`ultralytics` locally and actually fine-tune. Nothing else blocking.

## Goal-scored detection (branch `feat/goal-line-calibration`, pushed to
`github` remote, not a PR yet)

Full context in project_goal_detection_idea.md. Built across this
session and the previous one: `GoalGeometry` calibration field
(per-camera raw-pixel space, mirrors `FieldRoi`), a merged reco-gui
editor (single "Edit ROI / GOAL..." button, ROI/GOAL toggle over the
lens preview), and `reco-autocam::GoalEntryDetector` (raw, unconfirmed
polygon-entry signal - 7 tests, all passing). See git log on the branch
for the full design-pivot story (yaw/pitch space abandoned in favor of
per-camera pixel space, per user feedback).

**2026-08-07 update - real-footage verification attempted, inconclusive
in an informative way**: ran actual ball detection (`yolo26s.onnx`) over
t=345-366s of the file/camera pair with the known goal moment (t=356-
358s), fed the raw per-frame ball detections through the real
`GoalEntryDetector` logic. Two findings, neither a code bug:

1. **Zero ball detections in the t=356-358s window itself** - gap
   between the nearest detections at t=351.97s and t=360.22s. Matches
   the earlier visual check (a scramble of players right at the goal
   mouth) - the ball was very likely occluded from the detector's view
   at the critical moment. A real limitation of bounding-box detection
   during a goal-mouth scramble, not something to "fix" in the entry-
   detection logic itself.
2. **The `goal_geometry.left` polygon drawn during this session's live
   GUI test does not line up with the real goal at all** - polygon sits
   around camera-x 0.40-0.45, but every ball detection on that camera in
   this window sits around camera-x 0.71-0.79, a completely different
   part of the frame. Almost certainly a rough test scribble from trying
   out the editor, not a deliberately-traced goal boundary.

So: the entry-detection code itself checked out fine (no false fires,
correctly requires the ball inside the polygon) but this specific
real-footage attempt couldn't actually exercise it meaningfully. Left
open with the user, unresolved which of two paths to take next:
- Redraw `goal_geometry.left` accurately around the real goal (need a
  reference frame around t=356-364s to click against), then re-run this
  same verification script.
- Find a cleaner example goal (ball clearly visible crossing the line,
  not obscured by a scramble) to prove the detection logic end-to-end
  before trusting it on harder cases.

Verification script (throwaway, not committed):
`D:\VOETBAL_VIDEO\RECO\scratch_goal_check\verify_goal_entry.py` - reads
the calibration's `goal_geometry` + a `reco stitch --events` JSONL dump,
replicates `point_in_polygon`/the entry-transition check in Python.
Re-runnable against a new events dump once the polygon or example
changes. `events_356.jsonl` (raw detections) and `verify_out.mp4`
(throwaway stitched output, not needed) also sit in that scratch dir -
safe to delete, not git-tracked.

**Not done yet, in order**:
- Resolve the polygon-accuracy / clean-example question above.
- `GoalEntryDetector` is not wired into any live pipeline yet (no CLI
  flag, no `--events` integration) - still a tested, standalone
  primitive only.
- Kickoff/restart detection (needed before a raw entry can become a
  confirmed "goal scored" event) - not started.
- Once verified end-to-end, prepare as an upstream PR - probably rebuilt
  cleanly off `origin/main` in an isolated worktree (this branch has
  some now-superseded commits in its history from the yaw/pitch design
  pivot), same pattern as PR #435/#464.

## Other threads, unchanged (condensed - full detail in memory)

- **Veo Cam 3 competitive roadmap**: `docs/research-veo-cam3-comparison.md`,
  5 phases, goal detection above is Phase 1 item 2. Not filed as GitHub
  issues yet.
- **Upstream PRs** (#422-435, #464): all still awaiting owner
  review/merge on `reco-project/video-stitcher`.
- reco-gui app icon: still waiting on the user to provide a source image.

## Housekeeping this session

- Confirmed a real gap in cross-machine git identity: commits from
  RUFAN_LAPTOP show a different author name/email than commits from
  this machine - two different local `git config user.name`/
  `user.email` setups, unrelated to which `gh` account has push access
  (both work fine). Not a bug, don't "fix" it. See
  feedback_cross_machine_handoff.md.
- User confirmed (again) plain ASCII only, no special characters.
- Explicit new rule this session: no usernames/passwords/IP addresses in
  any tracked file or in this assistant's memory, ever - see
  feedback_no_credentials_in_tracked_files.md (broadened scope
  2026-08-07). A Pi IP address that slipped into this file the day
  before was redacted (commit `56b4399b`).
- User explicitly said not to bother with release builds while
  iterating on the goal-detection feature - debug build + smoke test is
  enough. `reco-cli` release build IS worth it for real detection-
  verification runs (debug + CPU ORT is far too slow for a real video
  window).

## Machine-specific reminders (still valid)

- FFMPEG_DIR and LLVM PATH needed per-shell for any build - see
  env_build_requirements.md. `FFMPEG_DIR/bin` also needs to be on PATH
  to run `reco-gui.exe`/`reco.exe` (dynamically linked) or to use the
  standalone `ffmpeg.exe` CLI for ad-hoc frame extraction.
- Don't drive reco-gui's UI with synthetic mouse/keyboard input -
  reliably unreliable in this app, has corrupted real calibration data
  before.
- reco-obs won't build on this machine right now (missing
  `obs-frontend-api.lib`) - expected, exclude it (`--exclude reco-obs`)
  from any workspace-wide build/test command rather than treating it as
  a new regression.
- `reco stitch` with `--model`/AI tracking defaults to a 1.5s lookahead
  buffer that can exceed available VRAM on shorter GPUs even at modest
  output resolutions - pass `--lookahead 0` for any run that doesn't
  need the smoothed AI-panned output itself (e.g. a detection-only
  `--events` dump), it's not needed for that and avoids the VRAM error
  entirely.
