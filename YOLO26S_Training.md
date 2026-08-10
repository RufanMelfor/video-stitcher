# YOLO26s Training Log

Running log of the Label Studio + yolo26 fine-tuning thread. Update
this file (don't just rely on git history) whenever a new training
round, dataset change, or real finding happens - it's the single place
to check "where does the ball-detection model stand right now."

**SCOPE CHANGE (2026-08-10, later same day)**: user decided **yolo26n,
not yolo26s, is now the active target** ("voor nu gaan we alleen
yolo26n trainen en niet yolo26s"), still with referee as a real 3rd
class. The yolo26s round history below (`rough_v2` through `rough_v7`)
is paused, not abandoned - keep it as the reference for what worked
(imgsz, batch, epoch findings all transfer directly to yolo26n) but
don't resume yolo26s rounds without the user asking again. Plan: user
reviews/corrects two new Label Studio projects tonight, yolo26n gets
trained and tested inside the real `reco` app (not just mAP) the next
day - see "01 Vierluik + 02 RPC expansion" below for what's queued up.

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

| `rough_v7_3class_1280_b4_e150` | 1280 | 4 | 150 | 0.693 | 0.980 | 0.583 | 0.756 | 0.535 | 32 min |

All checkpoints under
`D:\VOETBAL_VIDEO\RECO\training\finetuned_yolo26n_roughv1_train\runs\<run name>\weights\best.pt`
(`rough_v7` under the `_3class` variant of that path - see its own row's
setup note below).

`rough_v7` is the odd one out in this table: **first 3-class run**
(person/ball/referee), trained fresh from `yolo26s.pt` rather than
continuing `rough_v1`, after finding `prepare_yolo_train_split_from_ls_export.py`
had been silently folding referee into person the whole time (see "Key
finding 3" below). Its own per-class breakdown: person mAP50 0.934,
ball mAP50 0.693, referee mAP50 0.641 (referee numbers are on only 3
val instances - noise, not a real signal yet). Not directly comparable
to `rough_v6`'s row above (different starting checkpoint, one more
class to learn from the same 170 images).

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

### Key finding 3: referee was being silently dropped, not just deprioritized

`prepare_yolo_train_split_from_ls_export.py`'s original class-map folded
LS's `referee` class into `person` (`DEFAULT_CLASS_MAP = {0: 1, 1: 0,
2: 0}`), on the assumption referee wasn't worth a dedicated class. That
assumption was wrong: the user confirmed the "Finetuned yolo26n
(rough v1)" LS project's correction pass (project 8) *did* label
referee as its own class - **141 instances, more than ball's 123**, out
of 200 images. Every round through `rough_v7` was trained without that
signal as a result. Fixed the script (now `{0: 1, 1: 0, 2: 2}`, referee
kept as class 2, `OUR_CLASS_NAMES = ["person", "ball", "referee"]`) -
`rough_v7` above is the first run trained with it.

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
- Project 17: was "01 Vierluik - ball-rich pre-labels (soccana)" -
  **deleted**, an early upload attempt at 150 frames/camera before the
  user capped it at 100; project 18 replaced it.
- Project 18: "01 Vierluik - ball-rich pre-labels (soccana)" (556 tasks)
- Project 19: "02 RPC - ball-rich pre-labels (soccana)" (200 tasks)

## soccana model + ball-rich frame selection (2026-08-10, expanding beyond the 03 OJC match)

Downloaded `soccana.pt` to this machine (user-approved third-party
download, see `feedback_external_code_execution` memory) from
`huggingface.co/Adit-jain/soccana/resolve/main/Model/weights/best.pt` ->
`D:\VOETBAL_VIDEO\RECO\training\models\soccana.pt`. **Verified its class
order empirically rather than trusting the earlier LS classes.txt**
(`ball, person, referee` - that was project 8's own labeling-config
order, unrelated to soccana's raw output): `model.names` is actually
`{0: 'Player', 1: 'Ball', 2: 'Referee'}`.

Added 3 more source matches beyond the original "03 OJC" one, all under
`D:\VOETBAL_VIDEO\Berghem Sport J011-1\`: `01 Vierluik Oefen 20062026`
(3 sub-matches - BG-Orion2, BG-Orion1, Treff-BG) and
`02 RPC -Berghem Sport`. Their calibration files were in the old
pre-2026-07-15 "match" format - converted by hand using the exact
mapping in `project_calibration_format_migration` memory, verified via
the same temp-`#[test]`-in-`calibration.rs` approach. 01's 3 sub-matches
share one physical rig/day and only one had its own raw calibration
file - reused that single converted `calibration_v2.json` for all
three (not yet contradicted, but an assumption worth confirming if
alignment ever looks off in one of the other two).

Built `scripts/select_ball_rich_frames.py` (committed) - samples
candidate frames from raw video at a fixed interval, runs a teacher
model, and keeps only the frames richest in ball detections (per the
user's request to bias the dataset toward ball-containing frames
instead of uniform time-sampling). Output already in
`export_yolo_labels.py`'s layout so the rest of the pipeline
(`package_yolo_for_labelstudio.py`) works unchanged.

**Two real gotchas hit building this**:
- `Path.replace()`/`os.replace()` refuses cross-drive moves on Windows
  (`WinError 17`) - the temp extraction dir is on `C:`, dataset output
  on `D:`. Fixed with `shutil.move` instead.
- Extraction speed: plain `ffmpeg -i` decodes these 4K HEVC files at
  only ~1.0x realtime. The generic `-hwaccel cuda` flag **silently
  falls back to software decode** on this machine (confirmed via the
  "Stream mapping" log line showing `hevc (native)`, not a cuvid
  decoder) - no speedup at all. The fix is the explicit decoder,
  `-c:v hevc_cuvid` before `-i`, which measured ~1.35-1.4x realtime.
  Modest, not dramatic - if a future session needs this faster, look at
  parallelizing multiple ffmpeg processes (CPU/NVDEC-session-bound, not
  yet tried) rather than expecting more from decoder flags alone.

Ran the full pipeline over all 8 raw videos (3 sub-matches + RPC, L+R
each) via a scratchpad driver script (not committed - the video-list +
per-job-loop pattern is simple to recreate, see this section for the
exact file list if needed). Initial run kept up to 150 frames/camera
(1033 images total) - **user pushed back**: "niet meer dan 100 per
video anders ben ik nog maanden bezig met reviewen". Since
`select_ball_rich_frames.py` already writes frames in ball-richness-
ranked order (`frame_000000` = richest), trimming to top-100 was just
deleting files past index 99 per camera - no re-detection needed.
Final: **01 Vierluik 556 images** (some cameras had fewer than 100
ball-positive candidates to begin with - orion2_left 77, orion1_left
79), **02 RPC 200 images** (100/side, both hit the cap). Pushed to LS
projects 18 and 19 (see the project list above) via a second throwaway
script, `push_yolo_labels_to_ls.py` (scratchpad, not committed) - same
one-file-at-a-time upload pattern as `push_rough_v2_to_ls.py`, but
posts already-computed YOLO labels as predictions instead of re-running
a model live.

## Current status / next step

**SCOPE CHANGE**: yolo26n (not yolo26s) is now the active target, per
the user - see the note at the top of this file. Plan: user
reviews/corrects LS projects 18 and 19 tonight (2026-08-10), yolo26n
gets trained and tested inside the real `reco` app the next day - which
means an ONNX export step this time (`nms=True` baked in, output shape
`[1, N, 6]`, class names literally `person`/`ball` for `reco-autocam`'s
`resolve_class_id()`), not just an mAP table. **Undecided**: how
`reco-autocam` should treat the new `referee` class at runtime (ignore
it, filter it out, something new) - raise this with the user before
wiring the export in.

Everything in "Key finding 1/2/3" above (imgsz, batch, epoch, NMS-free-
duplicates, referee-class findings) transfers directly to yolo26n -
don't re-discover any of it from scratch. The yolo26s round history
above is paused, not invalidated.

**Review guidance given to the user**: correctly-detected people/objects
that fall outside the field ROI (spectators, coaches, subs) should be
left alone during LS correction, not deleted - `reco-autocam`'s
`field_roi` filtering already excludes them at runtime, so training-data
*correctness* is what matters (real object, right box), not whether it
happens to stand on the pitch. Only fix genuinely wrong detections.

**Idea queued for a future round, not yet tried**: Roboflow's "Football
AI Tutorial" (Piotr Skalski, `youtube.com/watch?v=aBVGKoNZQUw`) found via
its transcript that **stretching frames to a square canvas beat
ultralytics' default letterbox-pad-to-square** for the presenter's
keypoint-detection model specifically (his own 10-version test) - not
explicitly confirmed for ball/player detection in that video, but the
same "don't waste pixels on padding" logic that made `imgsz` matter so
much here. Ultralytics letterboxes (preserve aspect ratio, pad with
gray) by default; replicating "stretch" would mean pre-resizing source
frames to a square target (ignoring aspect ratio) before training,
outside ultralytics' own resize step - not implemented, worth a cheap
A/B test on a future round given how fast these experiments run here.

Once ball detection is meaningfully better, resume the
`feat/goal-line-calibration` branch's real-footage verification (it was
explicitly paused for exactly this reason - see that branch's
`SESSION_HANDOFF.md` history and `project_goal_detection_idea` memory).
