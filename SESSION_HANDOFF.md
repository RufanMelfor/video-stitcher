# Session handoff - 2026-08-10 (TGR_PC)

Continuation note for resuming work on a different machine/session -
git-tracked so it travels with `git pull`/`push` between the user's two
PCs. Overwrite wholesale at the end of a session rather than appending
history - git history is the append-only log, this file is just "where
things stand right now."

**No usernames/passwords/IP addresses in this file, ever** - see
feedback_no_credentials_in_tracked_files.md. Reference "see password
manager" / `zerotier-cli listnetworks` instead.

## Immediate state / what to do next

**Waiting on the user**: they're reviewing/correcting two new Label
Studio projects tonight (18: "01 Vierluik - ball-rich pre-labels
(soccana)", 556 tasks; 19: "02 RPC - ball-rich pre-labels (soccana)",
200 tasks). Once done, the plan is: train **yolo26n** (not yolo26s -
see scope change below) and **test it inside the real reco app**, not
just check mAP numbers - this needs an ONNX export this time (`nms=True`
baked in, output shape `[1, N, 6]`, class names literally
`person`/`ball` so `reco-autocam::resolve_class_id()` picks them up by
name). **Still undecided**: how `reco-autocam` should treat the new
`referee` class at runtime - raise this with the user before wiring the
export in, don't assume "ignore it" or "filter it" without asking.

**Also queued, not yet tried**: a YouTube tutorial (Roboflow's "Football
AI Tutorial" by Piotr Skalski, `youtube.com/watch?v=aBVGKoNZQUw`) found
via transcript that stretching frames to a square canvas beat
ultralytics' default letterbox-pad-to-square for *his* keypoint-detection
model (his own 10-version test) - not explicitly confirmed for
ball/player detection, but the same "don't waste pixels on padding"
logic that made `imgsz` matter so much for us. Worth testing on a
future yolo26n round (would mean pre-resizing/stretching source frames
before training instead of relying on ultralytics' default resize) -
not implemented, just flagged as a real idea.

## This session's full arc (started by mis-reading last weekend's work,
then a very productive YOLO fine-tuning push - full technical detail in
the git-tracked `YOLO26_Training.md` at the repo root, this section is
the condensed narrative)

1. **Corrected a wrong summary of "what happened over the weekend"**:
   initially missed that `main` (not the `feat/goal-line-calibration`
   branch) had the real weekend content, and that Friday's session had
   *already* resolved the goal-detection timestamp blocker (t=356-358s)
   and run a real verification (found the test `goal_geometry` polygon
   was misplaced + zero ball detections during the actual goal moment,
   due to occlusion). Also missed that goal-detection was explicitly
   paused because ball-model quality was identified as the root blocker
   - i.e. this whole session's YOLO thread *is* the fix for that, not a
   separate topic. See `project_goal_detection_idea` memory.

2. **yolo26s round series** (`rough_v2` through `rough_v7`, full table
   in `YOLO26_Training.md`): confirmed `imgsz` (not epochs, not
   gradient accumulation - already automatic in ultralytics) was the
   real lever for the ~18px ball (3x3px at imgsz=640, 6x6px at 1280);
   found and fixed a real bug where `prepare_yolo_train_split_from_ls_export.py`
   was silently folding the `referee` LS class into `person` (141
   already-corrected referee instances discarded across every round
   through `rough_v6`); confirmed YOLO26 is an end2end/NMS-free
   architecture, so near-duplicate boxes are a training-convergence
   symptom, not something any inference-time NMS flag fixes.

3. **Downloaded `soccana.pt`** (user-approved, `Adit-jain/soccana` on
   Hugging Face) to this machine and verified its real class order
   empirically (`{0: Player, 1: Ball, 2: Referee}` - don't trust the
   older LS project's classes.txt order, that was a different,
   unrelated ordering set at labeling-config time).

4. **Expanded training data to 3 more matches**: `01 Vierluik Oefen
   20062026` (3 sub-matches sharing one rig/day) and `02 RPC -Berghem
   Sport`, both under `D:\VOETBAL_VIDEO\Berghem Sport J011-1\`. Their
   calibration files were in the old pre-2026-07-15 "match" format -
   converted by hand using the exact mapping in
   `project_calibration_format_migration` memory, verified via the
   temp-`#[test]` approach (test removed after, no lasting repo change).

5. **Built `scripts/select_ball_rich_frames.py`** (committed) - samples
   candidate frames from raw video, runs soccana, keeps only the
   frames richest in ball detections (user's explicit ask, not uniform
   time-sampling). Real gotchas hit and fixed: `Path.replace()` can't
   move files across drives on Windows (`WinError 17`, use
   `shutil.move`); the generic `-hwaccel cuda` ffmpeg flag silently
   falls back to software decode on this machine, need the explicit
   `-c:v hevc_cuvid` decoder for a real ~1.35-1.4x realtime speedup.

6. **User capped the dataset at 100 frames/camera** ("niet meer dan 100
   per video anders ben ik nog maanden bezig met reviewen") - trimmed
   an already-ranked 150/camera selection down to 100 without
   re-running detection (frames are saved in ball-richness order).
   Final: 01 Vierluik 556 images, 02 RPC 200 images - pushed to LS
   projects 18/19 above via a second throwaway script
   (`push_yolo_labels_to_ls.py`, scratchpad, not committed - posts
   already-computed labels as predictions, no live model needed).

7. **User guidance on review scope**: real people/objects correctly
   detected outside the field ROI (spectators, coaches, subs) should
   be **left as-is during correction**, not deleted - `reco-autocam`'s
   `field_roi` filtering already handles field-boundary exclusion at
   runtime, so training-data correctness (is it really a person, is
   the box right) matters, not whether it happens to stand on the
   pitch. Only genuinely wrong detections need fixing.

8. **Scope change, same session**: user decided **yolo26n, not yolo26s,
   is now the active training target** (still with referee as a real
   3rd class). The yolo26s round history is paused, not abandoned - all
   its findings (imgsz/batch/epoch/NMS-free/referee-fix) transfer
   directly to yolo26n, don't re-discover them.

## Other threads, unchanged since 2026-08-07

- **Goal-scored detection** (`feat/goal-line-calibration` branch):
  still paused pending better ball-model quality - directly downstream
  of the thread above, resume once yolo26n is trained and tested.
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
- Don't drive reco-gui's UI with synthetic mouse/keyboard input.
- `git fsck --full` before pushing, per feedback_git_object_corruption.md.
- `soccana.pt` now lives at `D:\VOETBAL_VIDEO\RECO\training\models\soccana.pt`
  on this machine only - not git-tracked (third-party binary), redownload
  from the Hugging Face URL in `YOLO26_Training.md` if working from the
  other PC.
