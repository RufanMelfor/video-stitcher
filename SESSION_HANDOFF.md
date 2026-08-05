# Session handoff — 2026-08-05

Continuation note for resuming work on a different machine/session -
git-tracked so it travels with `git pull`/`push` between the user's two
PCs. Overwrite wholesale at the end of a session rather than appending
history - git history is the append-only log, this file is just "where
things stand right now."

## Immediate blocker (why this session paused)

User is restarting the PC to enable virtualization in BIOS/UEFI, needed
for Docker Desktop (which is needed for CVAT, see below). Diagnosed via
`systeminfo`: `Hyper-V Requirements: Virtualization Enabled In Firmware:
No` (CPU supports it - `VM Monitor Mode Extensions: Yes` - just disabled
in firmware). **Next step on resume**: confirm Docker Desktop starts
cleanly after the BIOS change, then continue the CVAT setup below.

## Upstream PRs opened this session

- **PR #464** (`feat/reco-gui-card-redesign`) - groups Calibration/
  Stitching/Advanced/Lens sliders into labeled `CalGroupBox` cards. Built
  off `origin/main` (scoped down: Color Mapping and toolbar FOV/Reset/
  Expert-Mode changes excluded, no upstream base for either - depend on
  unmerged PR #427/#433). Opened, awaiting owner review. See
  [[project_gui_card_redesign]].
- **PR #427** (`feat/color-matching-multiband`) - was on hold pending the
  owner's answer on how to handle drift from current `main`. **User
  reports it's now "getest en is goed bevonden" (tested and found good)**
  - not reflected as a formal GitHub review/merge yet, approval came
    through some other channel. No longer blocking, no action needed.
- **PR #435** (`feat/default-calibration-preference`) - user tested,
  confirmed working, test-plan checkbox updated on GitHub. Awaiting owner
  review/merge like the rest of the batch.

## reco-gui visual work (this fork's own `main`, committed+pushed to
`github/main`, commits `e99c1380`/`ea2b6781`)

Card-grouped panels (Calibration/Stitching/Color Mapping/Advanced/Lens),
toolbar-level FOV badge (click-to-open popover, not hover - see
[[project_gui_card_redesign]] for *why* hover was abandoned, a real Slint
gotcha worth remembering) + Reset pill, Expert Mode aligned to the right
panel's actual boundary. All verified working by the user after several
iteration rounds (stretch-to-fill-parent gotcha, vertical-centering-in-
invisible-wrapper gotcha, hover-fights-with-child-controls gotcha - full
detail in that memory file, worth reading before touching this area
again).

**Not yet done**: app icon. User wants a custom round black/white camera-
aperture/football icon embedded in both `reco-gui.exe` and `reco-cli.exe`
(mechanism already scoped - mirror `crates/rig-calib/build.rs`'s
`winresource` + Slint `icon: @image-url(...)` pattern, reco-gui currently
has neither). Blocked on getting the actual source image file - a prior
attempt to locate the user's pasted screenshot via `%TEMP%` heuristics
found two unrelated (one apparently private) images by mistake; stopped
that approach and asked for an explicit file path instead. Still not
provided as of this session's end - ask again, don't resume guessing from
temp files.

**Also flagged, not started**: reco-gui's text renders with an uneven/
"wobbly" baseline (`renderer-femtovg-wgpu` lacks ClearType-style hinting,
worse at non-100% Windows display scaling). Fix would be switching to
Slint's Skia renderer - bigger change, own Cargo feature flag, needs
checking compatibility with the existing wgpu-28 shared-instance GPU path.
See [[project_skia_renderer_future_task]].

## YOLO26n continued-training effort (new this session, biggest thread)

User wants to keep training `yolo26n.onnx` (the ball/player model
reco-detect runs). Full detail in [[project_yolo26n_training_pipeline]] -
summary:

1. **Researched YOLO26**: it's an official Ultralytics release (Jan
   2026), `yolo26n.pt` downloads free/public - no need to chase down the
   external team that originally trained the `.onnx` the user has.
2. **Found and fixed a real doc bug**: `MappedDetection::class_id`'s doc
   comment in `crates/reco-core/src/detect/director.rs` claimed "0 =
   ball, 1 = person" - wrong for the actual deployed model, which emits
   standard COCO indices (0 = person, 32 = sports ball). Verified
   empirically (box aspect ratios, confidence) against a real
   `detections.jsonl` run. Comment rewritten to point at
   `class_names()`-based resolution instead of a fixed number. **Fix is
   committed** (see below).
3. **Built an auto-labeling pipeline** (the "use the current model to
   pre-label, human only fixes mistakes" approach the user asked for):
   - `reco.exe stitch <L> <R> -c <cal> -o <throwaway> --model
     yolo26n.onnx --events detections.jsonl --max-frames N --lookahead 0`
     dumps raw per-frame detections to JSONL. Detection-bound at ~2.2
     fps on this machine (RTX 3060 Ti, CPU/DirectML ORT path) - budget
     real wall-clock time and run in the background, a first attempt
     died to a 400s foreground tool timeout.
   - New script `scripts/export_yolo_labels.py` (**committed**, not
     fork-only) converts JSONL -> YOLO-format dataset: samples every Nth
     frame, extracts the matching raw camera frame via ffmpeg, filters
     to person(COCO 0)/ball(COCO 32) only (drops COCO noise classes -
     tennis racket, frisbee, kite, etc. - that a general model
     hallucinates on football footage), remaps to a clean local 2-class
     scheme (`0=person, 1=ball`), writes YOLO `.txt` labels. Confidence
     threshold deliberately low (0.20) since ball-class confidence
     averages only ~0.29 - a stricter default would gut ball recall.
   - Visually sanity-checked by drawing boxes back onto sample images
     (PIL - no `cv2`/opencv in this Python env). Person boxes align
     well; ball boxes are sometimes a few pixels off-center (expected,
     real thing for the human reviewer to fix).
4. **Pilot dataset produced**: `D:\VOETBAL_VIDEO\RECO\training\
   pilot_ojc_bgs\dataset\` - "03 OJC - Berghem Sport" match (04-07-2026),
   segment 1, first 4500 frames (~2.5 min), sampled every 30th frame ->
   150 frames Γ— 2 cameras = 300 images + YOLO labels + `classes.txt` +
   its own `README.md`. Left: 933 boxes/150 frames. Right: 1558
   boxes/150 frames.
5. **Read a forum thread the user linked** (forum.reco.cam,
   "training-new-yolo-models-and-integrating-them") and verified its
   technical claims against actual source - **critical for our own
   eventual re-export**: reco-autocam resolves ball/person class ids **by
   name** (`resolve_class_id()`/`resolve_or()` in `crates/reco-autocam/
   src/lib.rs`, case-insensitive exact match against `"person"`/`"ball"`/
   `"sports ball"`), falling back to hardcoded COCO ids 0/32 only if no
   name matches. **When we fine-tune and re-export, the model's class
   names must literally be `"person"`/`"ball"`** or the runtime lookup
   silently falls back to COCO ids that won't exist in a reduced-class
   model. Also confirmed: model must have end-to-end NMS baked in
   (`nms=True` at export), output shape `[1, N, 6]`.
6. **Labeling tool chosen**: CVAT, self-hosted via Docker - user
   explicitly picked local/self-hosted over Roboflow specifically because
   the footage includes a youth team ("Berghem Sport JO11-1" - onder-11,
   likely minors in frame). Flagged this privacy angle proactively before
   asking, user agreed it mattered. **This is why Docker Desktop is being
   set up now** - currently blocked on the BIOS virtualization setting
   (see top of this file).

**Not done yet**: CVAT itself isn't installed/running (blocked on
Docker). No labeling-tool review of the pilot batch. No `ultralytics`
Python environment set up. No actual fine-tuning run. Only one pilot
match/segment covered - user has 20+ full matches in `D:\VOETBAL_VIDEO\`
for eventual training-set diversity, but scaling up should wait until the
pilot batch's review workflow is validated end-to-end first.

## Uncommitted at end of session (needs a commit + push next time)

- `crates/reco-core/src/detect/director.rs` - the class_id doc fix
  (#2 above). Small, safe, verified via `cargo check -p reco-core`.
- `scripts/export_yolo_labels.py` (untracked, new file) - the JSONL ->
  YOLO converter (#3 above). Ran successfully twice on real data.

Both are real, verified, useful changes - just hadn't been committed yet
when the session paused for the Docker/BIOS issue. Commit these before
starting anything else next session.

## Machine-specific reminders (still valid)

- FFMPEG_DIR + LLVM PATH needed per-shell for any build - see
  [[env_build_requirements]].
- This machine can corrupt loose git objects on large commits - run
  `git fsck --full` before pushing (see [[feedback_git_object_corruption]]).
- Rebuild + relaunch reco-gui.exe fully after any `.slint`/`.rs` change
  before re-testing - a stale running process will silently show old
  behavior (bit this session more than once during the toolbar work).
- Don't drive reco-gui's UI with synthetic mouse/keyboard input - reliably
  unreliable in this app, has corrupted real calibration data before.
  Screenshot capture (no input) is fine and was used safely this session
  to verify layout without the user's help.
