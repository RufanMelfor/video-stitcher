# Session handoff - 2026-08-05

Continuation note for resuming work on a different machine/session -
git-tracked so it travels with `git pull`/`push` between the user's two
PCs. Overwrite wholesale at the end of a session rather than appending
history - git history is the append-only log, this file is just "where
things stand right now."

## Immediate blocker (why this session paused)

User is heading to a different PC and will continue there. Currently
mid-way through setting up CVAT on their Synology NAS (see the YOLO26n
section below for why). Blocked on: pasting just CVAT's docker-compose.yml
into Synology Container Manager's "Create docker-compose.yml" project
option fails with `invalid mount config for type "bind": bind source path
does not exist: /volume1/docker/components/analytics/vector/vector.toml`.
Root cause: CVAT's compose file bind-mounts config files (vector.toml,
and likely more further in - Grafana provisioning, etc.) that live
elsewhere in the CVAT git repo, not just in docker-compose.yml itself.
Guided the user to instead get the whole CVAT repo onto the NAS first
(via SSH `git clone https://github.com/opencv/cvat.git` into
`/volume1/docker/cvat`, or download-zip-and-extract via File Station if
SSH isn't enabled), then point Container Manager's project at the
existing docker-compose.yml inside that checkout rather than pasting the
file fresh. Not yet confirmed working - check status on resume.

Local CVAT (running via Docker Desktop on this PC, not the NAS) already
works and has the pilot dataset loaded - see below. The NAS effort is
about making CVAT reachable from multiple workstations, it does not
block continuing to review the pilot batch locally in the meantime.

## Upstream PRs opened this session

- PR #464 (`feat/reco-gui-card-redesign`) - groups Calibration/Stitching/
  Advanced/Lens sliders into labeled `CalGroupBox` cards. Built off
  `origin/main` (scoped down: Color Mapping and toolbar FOV/Reset/
  Expert-Mode changes excluded, no upstream base for either - depend on
  unmerged PR #427/#433). Opened, awaiting owner review. See
  project_gui_card_redesign.md.
- PR #427 (`feat/color-matching-multiband`) - user reports it's now
  "getest en is goed bevonden" (tested and found good). Not reflected as
  a formal GitHub review/merge yet, approval came through some other
  channel. No longer blocking, no action needed.
- PR #435 (`feat/default-calibration-preference`) - user tested,
  confirmed working, test-plan checkbox updated on GitHub. Awaiting owner
  review/merge like the rest of the batch.

## reco-gui visual work (this fork's own `main`, committed and pushed to
`github/main`, commits `e99c1380`/`ea2b6781`)

Card-grouped panels (Calibration/Stitching/Color Mapping/Advanced/Lens),
toolbar-level FOV badge (click-to-open popover, not hover - see
project_gui_card_redesign.md for why hover was abandoned, a real Slint
gotcha worth remembering) plus Reset pill, Expert Mode aligned to the
right panel's actual boundary. All verified working by the user after
several iteration rounds.

Not yet done: app icon (still waiting on the user to provide the source
image file path, a prior attempt to locate it via %TEMP% heuristics found
unrelated private images by mistake, stopped that approach). Also
flagged, not started: reco-gui's text renders with an uneven baseline
(femtovg renderer limitation) - see project_skia_renderer_future_task.md.

## YOLO26n continued-training effort (biggest thread this session)

Full detail in project_yolo26n_training_pipeline.md. Summary of where
things stand:

1. Researched YOLO26: official Ultralytics release, `yolo26n.pt`
   downloads free/public.
2. Fixed a real doc bug in `crates/reco-core/src/detect/director.rs` -
   `MappedDetection::class_id`'s comment claimed "0 = ball, 1 = person,"
   wrong for the actual deployed model (standard COCO indices: 0 =
   person, 32 = sports ball). Committed (`9e70bc6c`).
3. Built and validated an auto-labeling pipeline:
   - `scripts/export_yolo_labels.py` (committed) - converts a
     `reco stitch --events detections.jsonl` run into YOLO-format
     pre-labels, filtered to person/ball, remapped to a clean 2-class
     scheme.
   - `scripts/package_yolo_for_cvat.py` (committed, `5c50b8e9`) -
     packages that into CVAT's classic "YOLO 1.1" import zip format
     (obj.data/obj.names/train.txt/obj_train_data), working around a
     real CVAT filename-collision gotcha (left/right camera frames share
     the same basename, needed a camera prefix before upload).
4. Pilot dataset produced: `D:\VOETBAL_VIDEO\RECO\training\
   pilot_ojc_bgs\dataset\` - 150 frames x 2 cameras = 300 images, 2491
   pre-label boxes total. Visually sanity-checked (PIL, no cv2 in this
   Python env) - person boxes align well, ball boxes sometimes a few
   pixels off-center (expected given the model's much lower confidence
   on that class).
5. Read a forum thread the user linked (forum.reco.cam,
   "training-new-yolo-models-and-integrating-them") and verified its
   claims against actual source. Important for our own eventual
   re-export: reco-autocam resolves ball/person class ids BY NAME
   (`resolve_class_id()`/`resolve_or()` in `crates/reco-autocam/
   src/lib.rs`, case-insensitive match against "person"/"ball"/"sports
   ball"), falling back to hardcoded COCO ids 0/32 only if no name
   matches. When we fine-tune and re-export, the model's class names
   must literally be "person"/"ball" or the lookup silently falls back
   to the wrong ids for a reduced-class model. Also: model needs
   end-to-end NMS baked in at export, output shape `[1, N, 6]`.
6. Labeling tool: CVAT, self-hosted via Docker - user explicitly chose
   this over Roboflow because the footage includes a youth team
   ("Berghem Sport JO11-1," likely minors in frame), flagged that
   privacy angle proactively before asking.
7. CVAT set up and working locally on this PC:
   - Cloned `https://github.com/opencv/cvat` into
     `D:\VOETBAL_VIDEO\RECO\training\cvat\cvat`, `docker compose up -d`
     from that directory (needed a retry once, first attempt hit a
     transient Docker Hub auth/network error, resumed fine).
   - Superuser created: username `admin`, password `RecoTrain2026!`
     (local-only account, user should change this eventually).
   - Task 3 ("OJC-BGS pilot - person+ball pre-labels") created via
     `cvat-cli` (`pip install cvat-cli`), 300 images + 2491 imported
     annotation shapes, confirmed via the SDK. Reachable at
     http://localhost:8080.
   - Real gotcha hit and solved: uploading `images/left/*.jpg` and
     `images/right/*.jpg` together fails - both cameras produce
     identically-named `frame_0000000.jpg` etc., and CVAT dedupes
     server-side by filename only, not full path, causing an
     IntegrityError. Fixed by using the already camera-prefixed copies
     from inside the packaged zip's `obj_train_data/` for the upload
     step instead of the original per-camera folders.
   - Also: `cvat-cli task create ... local <directory>` does not expand
     a directory into its contained files - pass an explicit glob
     (`*.jpg`) or file list, not a bare directory path, or it tries to
     read the directory itself as file bytes and fails with
     PermissionError.
8. Superseded (see below): was getting CVAT running on the user's
   Synology NAS too, blocked on a docker-compose bind-mount issue. Not
   pursued further - the whole labeling-infra plan changed instead.

## Labeling infra pivot (2026-08-05, later same day, on RUFAN_LAPTOP)

CVAT-on-NAS abandoned. User independently found the NAS (DS220+) isn't
suitable for CVAT. Checked: DS220+ is x86_64 (Intel Celeron J4025),
Container Manager officially supported - not an architecture problem,
but RAM caps at 6GB (2GB soldered + 4GB max expansion), below CVAT's
typical needs. User also has an unopened Raspberry Pi 5 8GB + NVMe -
checked CVAT's official Docker images (`cvat/server` on Docker Hub,
checked as of v2.72): linux/amd64 only, no arm64, so CVAT flatly cannot
run on the Pi regardless of RAM.

Decision: switch labeling tool from CVAT to **Label Studio** (lighter,
single container, `heartexlabs/label-studio:latest` confirmed
multi-arch incl. linux/arm64 on Docker Hub) hosted on the Raspberry Pi
5. NAS stays doing what it's good at (file/video storage), not running
the labeling tool - Label Studio's own docs recommend 8GB+ RAM anyway,
which the NAS can't reach even maxed out but the Pi meets natively.

Access: user already runs **ZeroTier** (not Tailscale, already had it)
between the work PC (daytime) and home laptop (evening) - same private-
overlay approach, no public exposure, no port-forwarding needed. Fits
the same privacy stance that ruled out Roboflow earlier (pilot footage
includes a youth team, "Berghem Sport JO11-1," likely minors).

Pi naming decided: hostname `reco-labelpi`, Linux user `rufan` (matches
existing device naming, avoids default `pi`/`admin`). Both set via
Raspberry Pi Imager's Advanced Options at flash time (OS: Raspberry Pi
OS Lite 64-bit). Full install checklist (OS/NVMe boot, ZeroTier, Docker,
Label Studio, firewall) written up and exported as a PDF the user saved
to their Desktop (`pi5_label_studio_setup.pdf`) - not yet acted on, Pi
hardware is physically unopened/uninstalled as of this note.

**Not done yet, in order**: physically set up the Pi (see PDF checklist)
-> convert `package_yolo_for_cvat.py` to a Label Studio import format
(CVAT-specific packaging no longer applies) -> human review of the pilot
batch -> `ultralytics` Python env on the desktop (training stays local,
not Google Colab - same privacy reasoning, and the desktop GPU is
already sufficient) -> actual fine-tuning run.

## Model size decision

Target checkpoint for fine-tuning: **`yolo26s.pt`** (small), not nano.
Reasoning: forum.reco.cam thread ("training-new-yolo-models-and-
integrating-them") found Small more accurate than Nano for small-ball
detection; user's own test running `yolo26x` (extra-large) through reco
worked but was noticeably slower. Small is the intended middle ground.
Plan: benchmark FPS on real target hardware (note: project also targets
NVIDIA Jetson per AGENTS.md, worth checking there specifically, not just
desktop) once the first fine-tuned checkpoint exists.

Note on model provenance: user had been testing with ONNX files from
https://huggingface.co/zwh20081/yolo26-onnx (community conversion,
labeled source "Ultralytics/YOLO26", AGPL-3.0, ONNX opset 12, all
n/s/m/l/x sizes incl. seg/pose/cls variants) - ONNX-only, no `.pt`
files, fine for runtime testing in reco but NOT usable as a fine-tuning
starting checkpoint. The actual training step needs the real `.pt` from
Ultralytics directly (`ultralytics` package auto-downloads it).

Not done yet: no human review of the pilot batch itself. No `ultralytics`
Python environment set up. No actual fine-tuning run. Only one pilot
match/segment covered - user has 20+ full matches in `D:\VOETBAL_VIDEO\`
for eventual training-set diversity, scale up only after the review
workflow is validated end-to-end.

## Future idea: automatic goal detection (not started, no code yet)

User wants, eventually, automatic detection of when a goal is scored.
Discussed and scoped a bit: this is NOT a YOLO class - it's a
cross-frame event, not a per-frame object. Current 2-class (person/ball)
training plan already covers what it needs (ball position per frame).
Real design:

- Ball trajectory (already tracked in `reco-autocam`) crossing a
  calibrated goal-line/goal-area -> candidate goal event.
- Validation heuristic from the user: after a real goal, the ball is
  always kicked off again from the center spot. A candidate goal event
  should be confirmed by checking the ball trajectory returns to/starts
  from the center circle shortly after - filters out false positives
  (post/bar bounces, saves, goal kicks) without needing a fancier model.
- New task identified: `reco-calibrate` needs to support defining
  lines/geometry around the goal (similar to existing camera/field-line
  calibration), since the physical camera rig is fixed per match (reco's
  autocam is a digital pan/zoom within the stitched frame, not a moving
  physical camera) - so goal position is a one-time per-setup
  calibration, not something to detect per-frame.
- Process note (user's explicit instruction): once this is actually
  built and working, it should go upstream as a proper PR, same as the
  other work this session (PR #464/#427/#435).

**Not yet filed as a real GitHub issue** - `gh` isn't installed/
authenticated on RUFAN_LAPTOP (only `origin` remote present here,
`https://github.com/RufanMelfor/reco-video-stitcher-rig`). File the
actual issue next time on the desktop machine where `gh` is already
authenticated as `RufanMelfor` (see git_object/gh-account notes above),
or the user can file it directly.

**Grew into a bigger roadmap same session**: user asked for a Veo Cam 3
feature/competitive research pass (explicitly research-only, no
implementation), then asked to turn the gaps into a phased task
breakdown - goal explicitly stated as reaching/exceeding Veo's
capability set as a free open-source community tool, not copying it.
Full writeup: `docs/research-veo-cam3-comparison.md` (feature-by-feature
comparison table + a 5-phase sized task breakdown: event tagging ->
match stats -> player identity -> polish/parity). The goal-detection
task above is Phase 1, item 2 of that breakdown - keep both in sync if
either changes. None of it filed as GitHub issues yet, same `gh`
blocker as above.

## Housekeeping this session

- Fixed a `gh` multi-account mixup that broke `git push github main`
  ("Repository not found") - `gh auth status` had switched active
  account away from `RufanMelfor` (the owner of the `github` and `fork`
  remotes) to a second logged-in account. Fixed via `gh auth switch
  --hostname github.com --user RufanMelfor`. See
  feedback_gh_multi_account_switch.md - can recur if the user logs into
  another GitHub account again (they were experimenting with multi-
  account setup in VS Code earlier the same session).
- User's display mangles non-ASCII characters (em dash, arrows,
  diaeresis) - flagged 4 times this session alone. Write everything in
  plain ASCII for this user, including in future SESSION_HANDOFF.md
  updates. See feedback_no_em_dash.md (memory file name unchanged,
  content broadened to cover all special characters, not just em dash).

## Machine-specific reminders (still valid)

- FFMPEG_DIR and LLVM PATH needed per-shell for any build - see
  env_build_requirements.md.
- This machine can corrupt loose git objects on large commits - run
  `git fsck --full` before pushing (see feedback_git_object_corruption.md).
- Rebuild and relaunch reco-gui.exe fully after any .slint/.rs change
  before re-testing - a stale running process will silently show old
  behavior.
- Don't drive reco-gui's UI with synthetic mouse/keyboard input -
  reliably unreliable in this app, has corrupted real calibration data
  before. Screenshot capture (no input) is fine and was used safely this
  session to verify layout without the user's help.
- Docker Desktop needed a BIOS change (SVM Mode / virtualization) plus
  two Windows features (Microsoft-Windows-Subsystem-Linux,
  VirtualMachinePlatform via elevated `dism.exe`) before it would start
  on this machine - now working.
