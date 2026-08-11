# Session handoff - 2026-08-11 (TGR_PC)

Continuation note for resuming work on a different machine/session -
git-tracked so it travels with `git pull`/`push` between the user's two
PCs. Overwrite wholesale at the end of a session rather than appending
history - git history is the append-only log, this file is just "where
things stand right now."

**No usernames/passwords/IP addresses in this file, ever** - see
feedback_no_credentials_in_tracked_files.md. Reference "see password
manager" / `zerotier-cli listnetworks` instead.

## Immediate state / what to do next

Full technical detail (every parameter tested, every number) is in the
git-tracked `YOLO26_Training.md` at the repo root - read its "Current
status / next step (end of day, 2026-08-11)" section at the very
bottom first, this is the condensed version.

**Two open items, in priority order:**

1. **Ask the user**: how should `reco-autocam`'s `ball_max_dist_from_cluster`
   (default 0.5 rad, controls whether the panner will chase a real ball
   that's far from the main player cluster - currently it won't, "Action"
   framing stays on the crowd) be handled? Options: raise the value via
   `--panner-config` (CLI-only, no GUI slider exists yet), add a GUI
   slider for it, tell the user to switch **Tracking mode -> ball** for
   matches where the ball matters more than the crowd, or leave as-is.
   Not answered yet as of session end.
2. **Waiting on the user**: LS project 8 ("Finetuned yolo26n (rough v1)")
   now has **28 new pending tasks** (228 total, 200 finished) - hard
   ball-miss frames from real footage, soccana pre-labeled, added this
   session. Once reviewed/corrected, re-export from LS and train yolo26n
   round 3 on the expanded (228-image) dataset.

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
