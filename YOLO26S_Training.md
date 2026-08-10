# YOLO26s Training Log

Running log of the Label Studio + yolo26s fine-tuning thread. Update
this file (don't just rely on git history) whenever a new training
round, dataset change, or real finding happens - it's the single place
to check "where does the ball-detection model stand right now."

No usernames/passwords/IP addresses in this file, ever (see
`SESSION_HANDOFF.md`'s standing rule) - reference "see password
manager" instead.

## Goal

Better ball detection for `reco-autocam` (currently the weakest class -
low confidence, easy to miss during scrambles/occlusion). This is also
the blocker for the `feat/goal-line-calibration` branch's real-footage
verification, which is paused until this improves (see
`project_goal_detection_idea` memory / that branch's own
`SESSION_HANDOFF.md` history).

## Pipeline overview

1. `reco stitch --events detections.jsonl` (or a static-image run) to
   get raw per-frame detections from real match footage.
2. `scripts/export_yolo_labels.py` - JSONL -> YOLO-format pre-labels
   (person/ball, 2-class).
3. `scripts/package_yolo_for_labelstudio.py` - flattens into Label
   Studio's expected layout, camera-prefixes filenames (left_/right_) to
   avoid basename collisions.
4. Upload to Label Studio (running on the Raspberry Pi) via the REST
   file-import API (`POST /api/projects/<id>/import`, one file per
   request - a >1-file-per-request batch only registers 1 task
   regardless of batch size, a real LS quirk on this instance).
5. Human review/correction in Label Studio.
6. `scripts/prepare_yolo_train_split_from_ls_export.py` (added
   2026-08-10, commit `92a7cdfc`) - pulls LS's built-in YOLO export
   (labels only, no image bytes - matched back against the flat image
   set from step 3/4) and turns it into an ultralytics train/val split.
   Remaps LS's soccana-derived 3-class scheme (ball/person/referee) to
   this project's 2-class one (person/ball, referee folds into person).
7. `yolo detect train` (ultralytics CLI) - the actual fine-tune.
8. Push the resulting checkpoint's own predictions back into a new LS
   project (throwaway script, scratchpad only, not committed - see
   "Prediction round-trip script" below) for visual review before
   trusting the mAP numbers alone.

## Pre-label teacher model comparison (2026-08-09)

Before trusting yolo26x as the pre-label/teacher model, compared 4
candidates on the same 150 frames (left camera, ~20min drone test
video), each in its own LS project:

| model | notes |
|---|---|
| `Adit-jain/soccana` (yolo11n) | football-trained, player/ball/referee - **winner** |
| `martinjolif/yolo-football-player-detection` (yolo11m) | football-trained, 4-class incl. goalkeeper |
| `martinjolif/yolo-football-ball-detection` (yolo11n) | ball-only specialist - weakest, missed ball in 94/150 frames |
| stock `yolo26x.pt` | COCO-pretrained, not football-trained |

**Verdict: soccana beat even stock yolo26x** - domain-specific football
training outweighed yolo26x's larger architecture. Class-name remap
needed at export/training time: soccana's player+referee -> person.

Also connected a live LS ML backend (label-studio-ml SDK) for soccana,
port 9091 - static predictions are enough for the training pipeline
itself, the live backend was more a labeling-assist convenience.

## Training rounds

Base checkpoint for every round below is `train_roi2_s`'s `rough_v1`
(the very first fine-tune, 2026-08-07, trained on 200 **uncorrected**
raw pre-label images - not in this log's scope, see
`project_yolo26n_training_pipeline` memory for that earlier history).

All rounds from `rough_v2` onward use LS project 8 ("Finetuned yolo26n
(rough v1)", 200 tasks, **fully corrected** - `num_tasks_with_annotations
= finished_task_number = 200`) as the data source, split 170 train / 30
val (deterministic last-15%, same stem-sort convention as
`prepare_yolo_train_split.py`).

| run | imgsz | batch | epochs | ball mAP50 | ball P | ball R | all mAP50 | mAP50-95 | wall time |
|---|---|---|---|---|---|---|---|---|---|
| `rough_v2` | 640 | 16 (default) | 30 | 0.528 | 0.847 | 0.500 | 0.726 | 0.386 | 3.4 min |
| `rough_v3_1280` | 1280 | 1 (AutoBatch fallback) | 30 | 0.683 | 0.981 | 0.583 | 0.817 | 0.508 | 16 min |
| `rough_v4_1280_b4` | 1280 | 4 (explicit) | 30 | 0.677 | 0.982 | 0.667 | 0.818 | 0.555 | 6.8 min |
| `rough_v5_1280_b4_e150` | 1280 | 4 | 150 | 0.717 | 0.988 | 0.667 | 0.831 | 0.573 | 32.6 min |
| `rough_v6_1280_b4_e300` | 1280 | 4 | 300 | 0.728 | **1.000** | 0.655 | 0.838 | 0.581 | 64 min |

All checkpoints under
`D:\VOETBAL_VIDEO\RECO\training\finetuned_yolo26n_roughv1_train\runs\<run name>\weights\best.pt`.

**Caveat that applies to every row above**: val set is only 30 images /
12 ball instances - a single miss swings ball mAP50 noticeably. Treat
small deltas (e.g. v5 -> v6's +0.011) as noise-adjacent; the v2 -> v4
jump (+0.15) is the one large, trustworthy signal.

### Key finding 1: imgsz was the real lever, not epochs or accumulation

Native frame resolution is 3840x2880; the ball is ~18x18px at that
size. At the `imgsz=640` every prior round (incl. the original
`rough_v1`) used, that shrinks to **~3x3px** - barely a signal for the
network. Person boxes (30-80px) survive that downscale fine; the ball
doesn't. Bumping to `imgsz=1280` (~6x6px) was the single biggest jump
in the whole table (`rough_v2` -> `rough_v4`: ball mAP50 0.528 -> 0.677,
precision 0.847 -> 0.982).

Gradient accumulation was a dead end to chase separately - ultralytics
already does it automatically (`accumulate = round(nbs / batch_size)`,
`nbs=64` default, see `ultralytics/engine/trainer.py`). At `batch=1` it
was already accumulating to an effective batch of 64. The real problem
with `batch=1` is BatchNorm statistics computed over a single sample,
which accumulation doesn't fix - confirmed by forcing an explicit
`batch=4` (AutoBatch's own OOM during its probing step was an artifact
of the probe, not a real ceiling): same-ish ball mAP50 as batch=1, but
better recall, better mAP50-95, and 2.3x faster (`rough_v3` -> `rough_v4`).

More epochs (`rough_v4` -> `rough_v6`, 30 -> 300) gave real but rapidly
diminishing returns (+0.04 then +0.011) and pushed ball precision to a
ceiling of 1.0 while recall stayed flat/slightly noisy. **Conclusion:
epoch-scaling on this data is now exhausted - the next real gain has to
come from more ball-labeled data (currently only 12 ball instances in
val, similarly few in train), not more training compute.**

### Key finding 2: YOLO26 is end2end (NMS-free) - duplicate boxes are a training-convergence symptom, not a bug

`model.model.end2end == True` for this checkpoint family. Ultralytics
skips classic IoU-based NMS post-processing entirely for end2end
models (confirmed: passing `nms=True`/`agnostic_nms=True`/`iou=...`
explicitly to `predict()` has no effect) - the model itself is
responsible for one-to-one box assignment via its training objective.
With only 12 ball instances to learn from, that suppression hasn't
fully converged for the ball class specifically: `rough_v4` had near-
duplicate ball boxes (IoU ~0.79, e.g. two boxes at confidence 0.66/0.61
on the exact same ball in `left_frame_0000000.jpg`, inner_id 1 in the
LS review projects) that no inference-time flag can clean up.

More epochs measurably helped here too: that specific duplicate
resolved into a single, higher-confidence box (0.83) by `rough_v5`
(150 epochs). Across the full 200-image set, images with >=2 ball boxes
went from ~12/33 ball-detections at `rough_v4` to 8/69 at `rough_v5` to
8/79 at `rough_v6` - the duplicate *rate* keeps dropping even as more
images get a ball detection at all, but 300 epochs didn't eliminate it
outright. If it matters before more data arrives, a practical
workaround is a manual IoU-based dedup pass on the prediction script's
output (keep highest-confidence box per cluster) - not implemented yet,
would go in the prediction round-trip script below if needed.

## Prediction round-trip script (LS visual review)

`push_rough_v2_to_ls.py` (scratchpad only, **not committed** - lives at
the session scratchpad path, recreate from this description if needed
on another machine): runs a given checkpoint over the same 200-image
set as LS project 8, creates a new LS project, uploads images one-by-
one (see the batch-import quirk above), matches LS's task-list response
shape (`{"tasks": [...], "total": N}`, not the more common
`{"results": [...], "next": ...}` - a version-specific gotcha, don't
assume the DRF-standard pagination shape for this LS instance's `/api/tasks`),
and posts each box as a percentage-coordinate prediction
(`POST /api/predictions/`).

Env vars: `LS_TOKEN` (required), `MODEL_PATH` (defaults to the latest
round trained), `LS_PROJECT_TITLE`, `LS_PROJECT_ID` + `SKIP_UPLOAD=1` to
reuse an already-uploaded project (e.g. after fixing a bug mid-run
without re-uploading 200 images).

LS review projects created so far (Pi project ids, for whoever's
checking next - titles are self-descriptive in the LS UI):
- Project 8: "Finetuned yolo26n (rough v1)" - the human-corrected
  ground-truth source for all rounds above (not a prediction round-trip,
  this is the real reviewed dataset).
- Project 14: "yolo26s rough_v2 predictions"
- Project 15: "yolo26s rough_v4_1280_b4 predictions"
- Project 16: "yolo26s rough_v6_1280_b4_e300 predictions" (latest, final
  visual check for the epoch-scaling experiment)

## Current status / next step

Epoch-scaling on the existing 170/30 corrected split is exhausted (see
Key finding 1). **Next real lever: more ball-labeled data** - either
correct more frames from the existing wider LS sets
(`finetuned_n_preds`/`finetuned_s_preds`, or the earlier wide-sample v2
projects), or export/label a fresh, larger, ball-focused batch. Not
started yet as of this note.

Once ball detection is meaningfully better, resume the
`feat/goal-line-calibration` branch's real-footage verification (it was
explicitly paused for exactly this reason - see that branch's
`SESSION_HANDOFF.md` history and `project_goal_detection_idea` memory).
