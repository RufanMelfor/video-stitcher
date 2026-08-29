# YOLO26 Training Log

Running log of the Label Studio + yolo26 fine-tuning thread. Update
this file (don't just rely on git history) whenever a new training
round, dataset change, or real finding happens - it's the single place
to check "where does the ball-detection model stand right now."

**SCOPE CHANGE (2026-08-10, later same day)**: user decided **yolo26n,
not yolo26s, is now the active target** ("voor nu gaan we alleen
yolo26n trainen en niet yolo26s"), still with referee as a real 3rd
class. The yolo26s round history below (`yolo26s_rough_v2` through `rough_v7`)
is paused, not abandoned - keep it as the reference for what worked
(imgsz, batch, epoch findings all transfer directly to yolo26n) but
don't resume yolo26s rounds without the user asking again. Plan: user
reviews/corrects two new Label Studio projects tonight, yolo26n gets
trained and tested inside the real `reco` app (not just mAP) the next
day - see "01 Vierluik + 02 RPC expansion" below for what's queued up.

No usernames/passwords/IP addresses in this file, ever (see
`SESSION_HANDOFF.md`'s standing rule) - reference "see password
manager" instead.

## Publishing yolo26s_v4_imgsz1920 to Hugging Face (2026-08-28)

User asked what it takes to share the trained model on their Hugging
Face account. Prepared, not yet uploaded - staged in
`D:\VOETBAL_VIDEO\RECO\training\hf_upload\reco-yolo26s-football\`
(`best.pt`, `best.onnx`, `README.md`).

**License: AGPL-3.0, and this is not a judgement call.** The checkpoint
carries Ultralytics' own field verbatim -
`license: AGPL-3.0 (https://ultralytics.com/license)` - and the ONNX
repeats it. It is a fine-tune of stock `yolo26s.pt` (`args.yaml`:
`model: yolo26s.pt`, `pretrained: true`) trained with the AGPL-3.0
ultralytics package, so the weights go out under the same terms. reco is
AGPL-3.0 too, so nothing conflicts.

**Privacy: weights only, never the dataset.** The images are amateur
youth football (minors). Also worth knowing: a run folder's
`train_batch*.jpg` / `val_batch*_labels.jpg` are real training frames
with recognisable children - they are NOT publishable, only `weights/`
is.

**Local paths leak by default.** Ultralytics stores absolute paths in
three places in a `.pt` (`train_args.data`, `train_args.project`,
`model.args.save_dir` - the last one holds the full run directory) and
once more in an ONNX's `description` metadata ("... trained on
D:\VOETBAL_VIDEO\..."). On a default Windows install that also exposes
the account name. New tool for this:
`scripts/strip_ultralytics_local_paths.py` (handles both formats,
rewrites each path to its basename, and refuses to write if its own
recursive sweep still finds one). Verified: predictions from the cleaned
files are bit-identical - 48 detections compared across 5 val images at
`imgsz=1920`, max box difference 0.000000 px, max confidence difference
0.000000, max weight difference 0, and the ONNX outputs match to 0.0 on
a fixed random input.

**Label provenance to disclose:** pre-labels came from
`Adit-jain/soccana` (itself a YOLO11 fine-tune), which lists **no
license** on its HF page - checked 2026-08-28. Human-corrected
afterwards, but credit it in the model card, and it is one more reason
not to publish the dataset.

**Which checkpoint - corrected mid-session.** First staged
`round4/runs/yolo26s_v4_imgsz1920` on the strength of this file's own
"not shipping either new checkpoint" verdict. **That verdict is now out
of date.** The user has been running
`merged_v1_tiled_1920/runs/full_patience100/weights/best.onnx` across
multiple real matches since and reports it works well - confirmed in
`reco-gui.log`, where the 2026-08-28 full-match export loaded exactly
that file. Their field experience over full matches outweighs the single
30-second regression clip, so the merged tiled checkpoint is what gets
published.

**Corrected a wrong claim while doing so:** this file says the tiled
checkpoint "needs tiled L/R inference (doesn't exist in production
yet)". It does not. `reco-detect`/`reco-autocam` contain no tiling at
all - the model is fed the whole frame in a single pass, letterboxed to
1920x1920, and that is how the user has been running it. Tiling was a
*training-data* decision, never an inference requirement. Do not repeat
the old claim.

**And a second wrong word, found 2026-08-29 and worth fixing everywhere
it appears:** that "whole frame" is **one camera's own 3840x2880
frame, not a stitched panorama**. `session/detection_dispatch.rs`
dispatches detection twice per processed frame, once as
`CameraId::Left` and once as `CameraId::Right`, each on the raw decoded
camera image before any stitching - which is also why the ball-tracker
log lines carry `cam=Left` / `cam=Right`. Stitching happens for the
exported video only. The 3840x2880 source being 4:3 gives it away: a
stitched panorama would be far wider. Consequences that matter:

- **Training data must be raw per-camera frames.** Feeding stitched
  panoramas (or worse, the exported 2560x1440 virtual-camera video)
  would train for an input that never occurs at runtime - reprojected
  geometry, dewarped lens distortion, different aspect, different
  object scale. That last one is the failure mode this project has
  already measured once, in the merged-dataset ball-size regression.
- Detecting on the stitched panorama instead would halve the inference
  count (one pass instead of two), but letterboxing a wide panorama
  into a 1920 square gives the ball *fewer* pixels. That is an
  architecture trade-off, not a dataset choice, and nobody has measured
  it.

Per-class val of that checkpoint (120 held-out tiles, measured fresh
rather than taken from the training log):

```
class      P       R      mAP50   mAP50-95
all      0.883   0.817   0.865    0.648
person   0.956   0.929   0.965    0.730
ball     0.844   0.612   0.724    0.500
referee  0.849   0.910   0.905    0.713
```

Ball recall 0.612 here vs round4's 0.444 is **not** a like-for-like
comparison - this val set is tiles, where the ball covers relatively
more pixels than in a full letterboxed panorama. The model card says so
explicitly.

**Published 2026-08-29:**
<https://huggingface.co/Dura-S/reco-yolo26s-football> (public, commit
`2fabada`) - `best.pt`, `best.onnx` and the model card, nothing else.
Verified after upload: the byte counts reported by the Hub match the
local staged files exactly, and the repo is not private.

How it was done, for the next update: the HF VS Code extension
(`huggingface.huggingface-vscode-chat`) is a Copilot Chat provider and
cannot upload models. Use the CLI instead - `pip install --user
huggingface_hub` puts `hf.exe` in
`%APPDATA%\Python\Python314\Scripts` (not on PATH by default), then
`hf auth login` with a **Write** token, then from the staging dir:

```
hf upload Dura-S/reco-yolo26s-football . --repo-type model
```

Re-run the same command to push a new checkpoint; it commits only what
changed. Always run `strip_ultralytics_local_paths.py` first, and sweep the
result for the strings `VOETBAL_VIDEO`, `Users\Rufan` and a drive-letter
prefix before uploading - that sweep was clean for this release.

## Round 5 candidate batch: selecting for failures, not volume (2026-08-29)

Added 60 frames from two new matches (`04 Beuningse Boys Berghem Sport
26082026`, `05 BMC20 -Berghem Sport`) to Label Studio project 24
("Ai Learning - yolo26s"), taking it from 100 to 160 tasks. Two new
committed scripts do the whole thing:

- `scripts/pick_training_frames.py` - candidate extraction + selection
- `scripts/upload_to_labelstudio.py` - upload + pre-labels as predictions

Exact commands used:

```
python scripts/pick_training_frames.py     --match-dir "<...>/04 Beuningse Boys Berghem Sport 26082026"     --match-dir "<...>/05 BMC20 -Berghem Sport"     --model "<...>/merged_v1_tiled_1920/runs/full_patience100/weights/best.pt"     --out "<...>/training/round5_candidates" --per-camera 15

python scripts/upload_to_labelstudio.py     --dataset "<...>/training/round5_candidates" --project 24     --url http://<host>:8080 --token-file <...>/ls_token.txt     --model-version "merged_v1_tiled_1920/full_patience100"
```

**Selection rationale, which is the whole point of this round.** The
merged-round regression was caused by adding *easier* data, so this batch
deliberately targets failures instead:

- `uncertain_ball` (32 of 60): a plausibly sized ball the model is unsure
  about. 18 of them under 15 px tall, confidences down to 0.05.
- `blind_spot` (28 of 60): **no ball detected at all** while 15-21 players
  are on the pitch, so play is happening and a ball is almost certainly in
  frame. Their pre-labels contain no ball by design - the reviewer has to
  find it, or confirm there is none. These are the only frames that can
  teach the model about its own misses, and no previous round contained
  any.

Ranking is by player count, not by ball richness. `select_ball_rich_frames.py`
ranks by the teacher's ball score, which selects for large, clearly visible
balls by construction - the documented cause of the regression. Person
detection is reliable (P 0.956 / R 0.929), so it is the safer ranking signal.

**A ball-size sanity cap is required, not optional.** The first run of this
selection stratified over ball size without one, and 14 of 60 frames were
picked on the strength of a "ball" over 40 px tall - up to 170 px, at
confidence 0.64. A real ball is ~18 px at 3840x2880. Without the cap the
large-ball bin fills with false positives. Default is now 5-30 px.

**Cost.** Keyframe-seeking one frame per candidate takes minutes for a full
match; `select_ball_rich_frames.py`-style linear decoding of the same
footage is ~1.4x realtime, i.e. 3.5 hours for 4 videos of ~75 min. 720
candidates (180 per camera) and every detection are cached, so re-selecting
with different parameters costs nothing - do that rather than re-extracting.

**Label Studio quirks hit (both now handled in the script).** The known
"import response has no `task_ids`" one recurred, worked around by matching
tasks back on filename. New one: `GET /api/tasks` returns **404 past the
last page** instead of an empty list, which crashes a naive pagination loop.

**Reviewer notes for this batch.** The teacher over-predicts `referee` (one
frame had 22 person + 10 referee boxes), and the low 0.05 confidence floor
means some suggested balls are noise. Removing a wrong ball matters as much
as adding a missed one: a small ball left unlabelled actively teaches the
model that there is no ball there.

**Does more data help by itself? No.** Two counts to keep in view: the set
holds roughly 7900 person boxes against 580 ball boxes, and every added
frame contributes ~20 persons and at most 1 ball, so uniform sampling makes
the ball *relatively rarer* every round. Keep the validation set frozen
across rounds, and keep using the same real-footage clip, or improvements
cannot be measured against anything.

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

All rounds from `yolo26s_rough_v2` onward use LS project 8 ("Finetuned yolo26n
(rough v1)", 200 tasks, **fully corrected** - `num_tasks_with_annotations
= finished_task_number = 200`) as the data source, split 170 train / 30
val (deterministic last-15%, same stem-sort convention as
`prepare_yolo_train_split.py`).

| run | imgsz | batch | epochs | ball mAP50 | ball P | ball R | all mAP50 | mAP50-95 | wall time |
|---|---|---|---|---|---|---|---|---|---|
| `yolo26s_rough_v2` | 640 | 16 (default) | 30 | 0.528 | 0.847 | 0.500 | 0.726 | 0.386 | 3.4 min |
| `yolo26s_rough_v3_1280` | 1280 | 1 (AutoBatch fallback) | 30 | 0.683 | 0.981 | 0.583 | 0.817 | 0.508 | 16 min |
| `yolo26s_rough_v4_1280_b4` | 1280 | 4 (explicit) | 30 | 0.677 | 0.982 | 0.667 | 0.818 | 0.555 | 6.8 min |
| `yolo26s_rough_v5_1280_b4_e150` | 1280 | 4 | 150 | 0.717 | 0.988 | 0.667 | 0.831 | 0.573 | 32.6 min |
| `yolo26s_rough_v6_1280_b4_e300` | 1280 | 4 | 300 | 0.728 | **1.000** | 0.655 | 0.838 | 0.581 | 64 min |

| `yolo26s_rough_v7_3class_1280_b4_e150` | 1280 | 4 | 150 | 0.693 | 0.980 | 0.583 | 0.756 | 0.535 | 32 min |

## yolo26n rounds (active target, 2026-08-11 onward)

First yolo26n round, same data source and hyperparams as `rough_v7`
(LS project 8, 170/30 split, 3-class person/ball/referee) for a direct
model-size comparison. Trained fresh from stock `yolo26n.pt`, same
reasoning as `rough_v7`'s fresh start (different class-head shape than
the old 2-class `rough_v1`).

| run | imgsz | batch | epochs | ball mAP50 | ball P | ball R | all mAP50 | mAP50-95 | wall time |
|---|---|---|---|---|---|---|---|---|---|
| `yolo26n_v1_3class_1280_b4_e150` | 1280 | 4 | 150 | 0.649 | 0.999 | 0.500 | 0.736 | 0.495 | 25 min |
| `yolo26n_v2_3class_1280_b4_e300` | 1280 | 4 | 300 (early-stopped at 181, best @ 81) | 0.694 | 0.861 | 0.583 | 0.710 | 0.484 | 28 min |

Checkpoints:
`D:\VOETBAL_VIDEO\RECO\training\rough_3class\runs\<run name>\weights\best.pt`.

`yolo26n_v1_3class_1280_b4_e150` per-class: person mAP50 0.895 (P 0.876,
R 0.842), ball mAP50 0.649 (P 0.999, R 0.500), referee mAP50 0.665 (P
0.262, R 1.000, only 3 val instances - noise, not a real signal, same
caveat as `rough_v7`).

`yolo26n_v2_3class_1280_b4_e300` per-class: person mAP50 0.902 (P 0.927,
R 0.814), ball mAP50 0.694 (P 0.861, R 0.583), referee mAP50 0.535 (P
0.267, R 1.000, same 3-instance noise caveat).

**Vs. `rough_v7` (yolo26s, identical data/hyperparams)**: yolo26n is
uniformly a bit behind, as expected for the smaller architecture (2.5M
params) - `v1` ball mAP50 0.649 vs 0.693, all mAP50 0.736 vs 0.756,
person mAP50 0.895 vs 0.934. Training is ~25% faster (25 min vs 32 min)
for the size/speed tradeoff.

**`v1` vs `v2` (epoch-scaling on yolo26n specifically)**: ultralytics'
own `EarlyStopping(patience=100)` kicked in at epoch 181 (no improvement
for 100 epochs), with the actual best checkpoint from **epoch 81** - the
300-epoch target was never reached, unlike `rough_v6` which trained the
full 300 for yolo26s. Ball recall improved (0.500 -> 0.583) and ball
mAP50 improved (0.649 -> 0.694), but ball precision dropped noticeably
(0.999 -> 0.861) and all-class mAP50 dropped slightly (0.736 -> 0.710,
driven mostly by referee's 0.665 -> 0.535 - within the 3-instance noise
band, not a real signal). Net read: **yolo26n converges faster than
yolo26s on this same small dataset and plateaus earlier** - more epochs
past ~80-100 isn't buying more on this data size for the n-variant
either, echoing rough_v4->v6's "epoch-scaling exhausted, need more
ball-labeled data" conclusion, just reached sooner.

All checkpoints under
`D:\VOETBAL_VIDEO\RECO\training\rough_v2_v6\runs\<run name>\weights\best.pt`
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
in the whole table (`yolo26s_rough_v2` -> `yolo26s_rough_v4_1280_b4`: ball mAP50 0.528 -> 0.677,
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

**yolo26n first round done (2026-08-11)**: `yolo26n_v1_3class_1280_b4_e150`
trained on LS project 8's 200 fully-corrected images (170/30 split,
3-class) - see the table above. LS projects 18 ("01 Vierluik", 2/556
finished) and 19 ("02 RPC", 19/200 finished) are **not** review-complete
yet (checked live via the LS API 2026-08-11) - this round used project 8
only, same data as `rough_v7`, for a clean model-size comparison. Not
re-training on 18/19 until the user finishes reviewing those.

Next step: export `yolo26n_v1_3class_1280_b4_e150`'s `best.pt` to ONNX
(`nms=True` baked in, output shape `[1, N, 6]`, class names literally
`person`/`ball` for `reco-autocam`'s `resolve_class_id()`) and test
inside the real `reco` app - not just the mAP table above. **Resolved
2026-08-10**: referee needs zero `reco-autocam` changes to export safely
- `resolve_class_id()` only looks up `"person"`/`"ball"` by name, so a
`referee`-named class is already naturally inert. See
`SESSION_HANDOFF.md` for the exact code reference.

**(Superseded - see "Current status / next step" at the very end of
this file for where things actually stand as of end of day
2026-08-11.)**

## First real-app test (2026-08-11)

Exported `yolo26n_v2_3class_1280_b4_e300`'s `best.pt` (chosen over `v1`
for its better ball recall - see the vs. comparison above) with `yolo
export format=onnx imgsz=1280 nms=True`. Ultralytics overrode
`nms=True` -> `False` itself ("not available for end2end models") but
the output shape is `[1, 300, 6]` regardless - YOLO26's own end2end head
already produces exactly the layout `reco-detect::detectors::cpu`
expects, the export flag is a no-op for this model family. Verified via
`onnx.load` before running anything: input `(1,3,1280,1280)`, output
`(1,300,6)`, metadata `names = {0: 'person', 1: 'ball', 2: 'referee'}` -
the exact dict-string format `parse_names_dict_string()` parses.

Ran `reco stitch` (release build, default `autocam+ort` CPU-detection
features) on a 30s clip of the 03 OJC match (t=300-330s,
`DJI_20260704095935_0028/0029`, `--tracking field --no-zero-copy`).
Ran clean end to end, no errors/panics, 899 frames encoded successfully
to `yolo26n_v2_ONNX_test1.mp4` in the match folder. `resolve_class_id()`
picked up the model correctly at startup: `ball=1, person=0`; referee
loaded but inert as expected, no code changes needed.

Detection stats across the clip's `--events` JSONL (899 frames):

| class | frames with >=1 det | total dets | mean conf | max conf |
|---|---|---|---|---|
| person | ~100% | 17836 | 0.607 | 0.988 |
| ball | 98 (10.9%) | 198 | 0.477 | 0.983 |
| referee | 812 (**90.3%**) | 1161 | 0.591 | 0.987 |

Ball tracker (from the run log): acquired the ball 3 times over 30s
(conf 0.10/0.60/0.16 at acquisition), lost track twice after the 20-
coast-frame timeout - consistent with the 0.583 val recall, not
continuous but picks it up repeatedly rather than never.

**Real finding, not a pipeline bug**: referee fires in 90.3% of frames -
that's the low val precision (0.267, see the table above) showing up in
practice, not just a small-val-set statistical artifact. The model is
much more trigger-happy on "referee" than the 3-instance val set alone
suggested. Doesn't affect anything today (referee is inert at runtime,
confirmed above), but worth knowing before anyone builds a real
referee-aware feature on top of this checkpoint - would need either more
referee-labeled data or a stricter confidence threshold for that class
specifically.

CPU-only detection (default `ort` feature, no CUDA/TensorRT EP) ran the
full pipeline at only ~1.7 fps average (899 frames / 538.7s wall,
excluding the ~29s lookahead pre-fill) - expected for imgsz=1280 YOLO on
CPU, not a regression. A GPU-backed EP build (`--features cuda` or
`tensorrt`) would be needed before this is usable at real-time capture
speed; today's test was purely to validate correctness end-to-end,
timing was not the point.

## GPU-backed (DirectML) re-run, same clip (2026-08-11)

**Found a real gap while wiring this up**: `reco-cli`'s `Cargo.toml` had
`cuda`/`tensorrt`/`coreml` feature passthroughs to `reco-autocam` but no
`directml` one, even though `reco-detect`/`reco-autocam` already
implement it. That's also why the first CPU run's log line
("`ORT: DirectML execution provider enabled`") was misleading - `ort`
itself logged a WARN two lines earlier that DirectML couldn't register
because its Cargo feature wasn't compiled in, but `reco_detect::ort_session`
logs its own "enabled" INFO unconditionally on the `Ok` result rather
than checking whether the EP actually attached, so the session silently
fell back to CPU while claiming GPU. Fixed by adding the missing
`directml = ["autocam", "reco-autocam/directml"]` line (same pattern as
the other three EPs) - `crates/reco-cli/Cargo.toml`. The misleading-log
part (real EP vs. requested-but-silently-declined) wasn't touched, only
the missing feature wiring that was the actual gap - worth a closer look
if it causes confusion again.

Rebuilt with `--features directml`, re-ran the identical clip (dropped
`--no-zero-copy` since GPU detection no longer needs CPU-resident
frames). First attempt hit a real, expected wall: `not enough VRAM for a
1.5s lookahead` - zero-copy's lookahead pool needs ~4.7 GB for 71 slots
at the source's native 3840x2880 10-bit, only ~4.1 GB was usable on the
3060 Ti's 8 GB (shared with the DirectML detection session + OS
display). Fixed with the already-shipped `--lookahead-reduced-bit-depth`
flag (see `project_export_vram_lookahead` memory) - halves the pool's
VRAM cost for 10-bit sources.

Result: `Successfully registered DmlExecutionProvider` confirmed in the
log (the real thing this time, not the misleading CPU-fallback message).
Same 899-frame clip, same ball-tracker acquire/lose pattern (identical
detections - DirectML vs CPU gave the same results, as expected for the
same weights/inputs) but **17.9 fps avg / up to ~650 fps burst** vs the
CPU run's 1.7 fps - roughly a 10x speedup, comfortably real-time-capable
for live capture at this resolution. Output:
`yolo26n_v2_ONNX_test2_gpu.mp4` in the match folder. Bottleneck shifted
to GPU readback (0.7ms/frame) rather than detection - detection is no
longer the pipeline's limiting factor on this hardware.

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

## Targeted hard-frame batch added to project 8 (2026-08-11)

The user's own `test_v054_yolo26n_v2.mp4` export (03 OJC, t=100-130s)
surfaced a real symptom: the panner froze on the player cluster for ~7s
during continuous, moving open play (confirmed via extracted frames -
not a stoppage/corner-kick) because the ball went completely
undetected the whole stretch. Root-caused as two compounding factors:
`yolo26n_v2`'s ball recall (0.583 val, ~11% of frames in the earlier
GPU test) makes multi-second ball-less stretches common, and
`reco-autocam`'s `FieldPanner` default `dead_zone_rad=0.20` combined
with its slow `cluster_alpha=0.012` EMA makes cluster-only tracking
(no ball to blend toward) very sticky when nothing large enough occurs
to escape the dead zone - not a crash, a config/tuning interaction. See
this session's chat log for the full diagnosis (events-JSONL frame-by-
frame analysis + extracted video frames). `dead_zone_rad` is exposed in
`reco-gui`'s export settings (Dead-zone slider, `ui/main.slint`, range
0.0-0.5 rad, default 0.20) - worth the user trying a lower value (e.g.
0.05-0.08) before any code change; `cluster_alpha` is not GUI-exposed.

Pulled 28 raw per-camera frames (14 left + 14 right, `hevc_cuvid`
decode, no rotation filter - matches `select_ball_rich_frames.py`'s
convention) from the two confirmed ball-miss windows: t=111-121s
("winA", the user's freeze) and t=311-321s (a similar miss stretch in
this session's own earlier GPU test clip), same first-segment 03 OJC
source videos, 1 frame/1.5s. Ran `soccana.pt` as the pre-label teacher
(same model/conf=0.15/imgsz=1280 as `select_ball_rich_frames.py`) - 20/28
got a soccana ball box (the other 8 are hard even for the stronger
teacher, still useful as review candidates). Pushed all 28 into LS
project 8 ("Finetuned yolo26n (rough v1)") via the same one-file-per-
request import API + separate predictions POST pattern as
`push_rough_v2_to_ls.py`/`push_yolo_labels_to_ls.py` (recreated fresh
in-session, scratchpad only, not committed - hit and fixed one real bug
this time: matching a newly-imported task back to its file by filename
substring breaks if the same filename gets uploaded twice in one run,
silently attaches the prediction to the *first* match and leaves an
orphaned zero-prediction duplicate task - hit this for one frame,
caught it via a `total_predictions==0` sweep afterward, deleted the
orphan). Project 8 is now **228 tasks, 200 already finished, 28 new
pending** - ready for the user to review before the next yolo26n
training round.

## Panner freeze: dead-zone alone didn't fix it, cluster_mode did (2026-08-11)

Re-ran the exact same clip/model 3x via `reco stitch --panner-config`
(shallow JSON overlay onto `FieldPannerConfig`, accepts any struct field
- confirmed by reading `crates/reco-cli/src/stitch.rs`, not just the
GUI-curated subset) to isolate the real fix, using
`--lookahead-reduced-bit-depth` + GPU/DirectML build throughout:

1. `dead_zone_rad=0.06` alone (default 0.20) - **did not fix it**. Pose
   still froze bit-exact across the same ~440-660 frame window. This
   ruled out the dead-zone-alone theory from the earlier diagnosis.
2. `dead_zone_rad=0.06` + `cluster_alpha=0.05` (default 0.012, not
   GUI-exposed) - partial improvement (moved for ~100 frames) then
   froze again at a *different* fixed point for another ~80 frames.
   Ruled out "EMA too slow" as the sole cause too.
3. `dead_zone_rad=0.06` + `cluster_mode=trimmed_mean` (default
   `density`, **is** GUI-exposed as a dropdown) - the freeze
   essentially resolved: continuous drift through the whole previous
   freeze window, down to one short ~2s settle that reads as natural
   rather than stuck (vs. the original ~7s hard lock).

**Real conclusion, revising the earlier diagnosis**: the dominant cause
wasn't the dead-zone or the EMA rate, it was `ClusterMode::Density`
itself - picking the single densest neighbor-count peak (bandwidth 0.30
rad) tends to lock onto a compact, relatively static group (e.g. a
defensive line) rather than the group actually carrying the play,
especially once the ball is no longer detected to break the lock via
`ball_weight` blending. `trimmed_mean` (confidence-weighted mean of the
`keep_fraction`-closest 80% of *all* tracked players, not a density
peak) tracks the whole formation's drift instead and doesn't get stuck
the same way. Side effect worth knowing, not a bug: `trimmed_mean`
framed noticeably wider (FOV crept up toward ~55° vs. `density`'s
~30-40° here) since it isn't zeroing in on one tight sub-group.

**Recommendation for the user's export settings**: switch **Cluster
mode -> trimmed_mean** and lower **Dead-zone -> ~0.05-0.08** (both
already GUI sliders/dropdown, no code change needed). Test outputs for
comparison, same clip/model throughout:
`test_v054_yolo26n_v2_lowdz.mp4` (dead-zone only, still froze),
`test_v054_yolo26n_v2_lowdz_fastalpha.mp4` (dead-zone + alpha, partial),
`test_v054_yolo26n_v2_trimmed.mp4` (dead-zone + trimmed_mean, best) -
all in the 03 OJC match folder alongside the original
`test_v054_yolo26n_v2.mp4`.

**User's own export of this recommendation felt "wiebelig en
schommelig" (wobbly/jittery)** - not reproduced by this session's own
30s test-clip numbers (frame-to-frame `|dyaw|`: `dz=0.06 density`
mean 0.00115/p95 0.00423/flips 10, `dz=0.06 trimmed_mean` mean
0.00077/p95 0.00297/flips 3 - trimmed_mean was *smoother* by this
metric on this clip, not jitterier), so either the user's dead-zone
value, clip/timerange, or other settings (ball_weight?) differed from
this session's test, or real crowded-scramble footage exposes
whole-formation-mean noise this calm 30s window didn't. Tried the
obvious middle ground - `dead_zone_rad=0.12` + `trimmed_mean` - and it
confirmed a genuine, structural tension rather than a free lunch: jitter
dropped further (mean 0.00052, flips 2) but the longest freeze-run grew
to **327 frames (~11s)**, worse than `dz=0.06`'s 55-frame (~1.8s) one.
Single dead-zone knob trades freeze-resistance against wobble-
resistance directly - raising it to kill wobble measurably brings the
freeze back. Asked the user for their exact settings/clip before
tuning further rather than guessing blind; `velocity_alpha` (default
0.06, downstream smoothing of the actual camera motion, separate from
the dead-zone's "move at all" gate) flagged as the next lever to try if
dead-zone alone can't hit both goals at once - not tested yet.

**Resolved: not a yolo26n problem, it's `ball_weight`.** User shared
their actual export-settings screenshot - real values differed from
this session's earlier CLI tests in ways that mattered:
`--panner-preset action` (not `broadcast`), **`ball_weight=1.0`** (not
the `action` preset's own default 0.35 - manually maxed by the user),
`detection_interval=3` (not the default 1), `lookahead=0.5s` (not the
1.5s default). Re-ran with those exact settings and it reproduced the
wobble: `mean|dyaw|=0.00299` vs. this session's earlier `dz=0.06
trimmed_mean` test's `0.00077` - a real ~4x jump, freeze gone
(`longest_frozen_run` down to 21 frames).

To isolate model vs. config, relabeled `soccana.pt`'s class names
(`Player/Ball/Referee` -> `person/ball/referee` - direct ONNX metadata
patch via the `onnx` lib, not `model.names[i]=...` on the ultralytics
wrapper, which silently doesn't persist back to the exported graph;
`resolve_class_id` only matches `"person"` literally, `"player"` is not
an accepted alias) and exported with `nms=True` (works for this
non-end2end yolo11n architecture, unlike yolo26's forced `nms=False`).
Ran the identical settings with `soccana.onnx` swapped in for the model
- **jitter was the same or slightly worse** (`mean|dyaw|=0.00502`,
`longest_frozen_run=51`), despite soccana being the established
stronger ball-detection teacher. A better ball model didn't fix it,
which rules out "yolo26n's ball detection is too flaky" as the cause -
confirms this is a panner-config interaction, not a model-quality one.

Confirmed the specific lever: same run with **`ball_weight=0.35`**
(the `action` preset's own default, everything else unchanged) roughly
**halved** the jitter (`mean|dyaw|=0.00142`, p95 `0.00685` vs. `0.01384`
at `ball_weight=1.0`) while keeping the freeze suppressed
(`longest_frozen_run=52` frames, ~1.7s - still far short of the
original 222-327 frame freezes). **Recommendation for the user:** bring
Ball weight back down from 1.0 to somewhere around 0.35-0.5, keep
`trimmed_mean` + the lowered dead-zone (~0.05-0.08) for the freeze fix -
`ball_weight=1.0` was the one setting doing the most damage. Test
outputs: `test_v054_yolo26n_v2_repro.mp4` (ball_weight=1, the wobbly
repro), `test_v054_soccana_repro.mp4` (same settings, soccana model),
`test_v054_yolo26n_v2_bw035.mp4` (ball_weight=0.35, the fix).

## "Camera doesn't go to the corner" - not `field_roi`, it's the `ball_near_cluster` gate

With `ball_weight=0.35` fixing the wobble, the user asked whether
`field_roi` was clipping the camera away from a corner where the ball
kept disappearing. Tested directly: re-ran the identical clip/settings
against a calibration copy with `field_roi` stripped entirely (`if let
Some(roi) = cal.field_roi` in `reco-cli/src/stitch.rs` - omitting the
key skips `RoiFilteredDetector` wrapping altogether, confirmed by the
absence of the "Autocam: field ROI filtering enabled" log line).

Result: ROI *is* filtering real detections (without it: 28.4 avg
players/frame vs. 21.6 with it, 530/899 vs. 419/899 frames with a
tracked ball - both real, measurable effects, filtering is doing
something) but the camera's actual pan/tilt **range did not widen**
without it (yaw span 0.522 rad without ROI vs. 0.617 rad with it -
if anything slightly narrower without ROI, no evidence of a clipped
corner being freed up). Ruled out `field_roi` as the cause of this
specific symptom.

Found the real mechanism by pulling the raw frame at the exact
timestamp of a high-confidence (0.83), long-tracked (19 frames) ball
detection the camera never panned to (`yaw=-0.612, pitch=-0.300`,
frame 702 of the clip = ~t123.4s match time, right camera): it's a
**real ball**, sitting alone near the touchline/corner, well separated
from the main body of players still clustered further up-field (visual
check confirms this, not a false positive). Computed the actual gate
in `panners/field.rs::decide_with_lookahead` -
`ball_near_cluster = dist(ball, cluster_centroid) < ball_max_dist_from_cluster`
(default `0.5` rad) - with players clustered around pitch~0.135 and the
ball at pitch=-0.300, the distance comfortably exceeds 0.5 rad, so
`ball_near_cluster` is false and **`ball_weight` never engages at all**
for this detection, regardless of its value. This is "Action" framing
working as designed (stay on the main group, don't whip-pan to an
isolated/stray ball) rather than a bug - but it means no `ball_weight`
tuning alone fixes a ball that has strayed this far from the pack.

`ball_max_dist_from_cluster` is **not GUI-exposed** (checked
`ui/main.slint` and `reco-gui/src/{main,export}.rs` - no property/
binding for it anywhere), only reachable via `--panner-config` on the
CLI. Options if the user wants the camera to follow an isolated ball
like this: (a) raise `ball_max_dist_from_cluster` past 0.5 rad via
`--panner-config` (no GUI path yet - would need one added if this
should be user-tunable), (b) switch **Tracking mode -> ball** for
clips/matches where prioritizing the ball over the crowd is preferred
(forces ball-only following, no cluster gate), or (c) accept current
behavior as intended broadcast-style framing for this specific
scenario. Not yet decided with the user which of these to pursue.

## Current status / next step (end of day, 2026-08-11)

This is the up-to-date summary - the "Current status" note earlier in
this file (right after the first yolo26n round) is superseded, kept
only for its own paragraph's context.

**Where things stand:**

1. **`yolo26n_v2_3class_1280_b4_e300`** (`ball_max_dist_from_cluster`
   section above has its exact checkpoint path) is the current best
   yolo26n checkpoint - trained, ONNX-exported (`nms=True` requested,
   ultralytics auto-no-ops it for this end2end architecture but the
   output shape is already `[1,300,6]` regardless), and verified
   end-to-end in the real `reco` app on both CPU and GPU (DirectML)
   builds. `reco-cli`'s `Cargo.toml` now has the `directml` feature
   wired up (was missing, real gap - see the GPU-backed-rerun section
   above).
2. **28 hard-frame tasks added to LS project 8** ("Finetuned yolo26n
   (rough v1)", now 228 total / 200 finished / **28 pending**) -
   ball-miss frames from two confirmed real-play windows, soccana
   pre-labeled. **Waiting on the user to review these** before the next
   training round (round 3) - once done, re-run
   `prepare_yolo_train_split_from_ls_export.py` against project 8's
   fresh export (will now be 228 images, not 200) and retrain.
3. **Panner tuning for `reco-autocam`'s `FieldPanner`** (all findings
   from this session, GUI-actionable unless noted):
   - **Cluster mode -> `trimmed_mean`** (was `density`) - fixes the
     multi-second freeze during ball-less stretches; `density` locks
     onto a static sub-group instead of following the whole formation.
   - **Dead-zone -> ~0.05-0.08** (was 0.20 default / 0.12 for the
     `action` preset) - needed alongside `trimmed_mean`, tested and
     recommended.
   - **Ball weight -> ~0.35-0.5** (not `1.0`) - confirmed via a soccana
     side-by-side that pinning it to the max causes visible wobble
     regardless of which model supplies the ball detections; this was
     the dominant wobble cause, not model quality.
   - **`ball_max_dist_from_cluster`** (default `0.5` rad, **not
     GUI-exposed**) is why the camera won't swing to a real, isolated
     ball far from the main player cluster ("Action" framing's designed
     behavior, not a bug) - **open question for the user**: raise this
     via `--panner-config` (CLI-only today), add it as a GUI slider,
     switch to **Tracking mode -> ball** for matches where the ball
     matters more than the crowd, or leave as-is. Not decided yet -
     ask the user first thing next session if not already answered.
   - `field_roi` was tested and ruled out as the cause of the
     "camera won't reach the corner" symptom (A/B calibration test,
     see above) - it does filter some real detections but didn't
     change the camera's actual pan/tilt range.
4. Test videos for all of the above live in the 03 OJC match folder
   (`D:\VOETBAL_VIDEO\Berghem Sport J011-1\03 OJC -Bergem Sport
   04072026\`), prefixed `test_v054_*` and `yolo26n_v2_ONNX_*` -
   filenames map to settings in each section above; keep them for
   reference per the user's standing "don't delete test artifacts"
   preference.

**Concretely, tomorrow:**
- Get the user's answer on the `ball_max_dist_from_cluster` question
  above (raise it / add GUI slider / use ball tracking mode / leave it).
- Nudge on reviewing the 28 pending LS project-8 tasks if not done yet.
- Once both are settled, round-3 yolo26n training with the expanded,
  corrected project-8 data is the natural next step.

## Round 3: yolo26n vs yolo26s on the expanded 228-task dataset, plus a
val-split false-alarm (2026-08-12, TGR_PC)

`ball_max_dist_from_cluster` question resolved same session (now a GUI
slider, "Ball anchor range" + "Ball reach" + "FOV Wide" - see
`SESSION_HANDOFF.md` and `docs/ai-panner-tuning.md` for that thread, not
repeated here). LS project 8's 28 pending tasks confirmed fully
corrected (228/228). Round 3 training run:

1. **Data prep**: `prepare_yolo_train_split_from_ls_export.py` against a
   fresh LS project-8 YOLO export (`GET /api/projects/8/export?
   exportType=YOLO`) - **228 images now, not 200**. Real gotcha: the 28
   hard-frame images (added via the REST import API on 2026-08-11) were
   only ever local to that session's scratchpad, not present in this
   machine's `finetuned_n_preds/ls_flat/images/` flat directory the prep
   script matches against. Had to re-download all 28 from LS
   (`GET {LS_URL}{task.data.image}`) before the split script could find
   them. **Also a real filename-stem gotcha**: LS's YOLO export uses the
   *exact* uploaded filename as the label stem - for the 200 originally
   local-files-served images that's the clean name
   (`left_frame_0000000` etc.), but for the 28 REST-uploaded hard frames
   it's the *hash-prefixed* upload name (`5d95c9ee-right_winA_001` etc.)
   - stripping the hash prefix (as done on first attempt) breaks the
   match silently (0 labels found for those 28). Fixed by keeping the
   uploaded filename exactly as LS reports it. Result: **194 train / 34
   val** (deterministic split, same convention as before).
2. **`yolo26n_v3_3class_1280_b4_e300`**: fresh from stock `yolo26n.pt`,
   same hyperparams as `v2` (imgsz=1280, batch=4). Early-stopped at 185
   epochs (best @ 85), 36 min.
3. **`yolo26s_v3_3class_1280_b4_e300`**: same data/hyperparams, fresh
   from stock `yolo26s.pt`, run alongside `v3` for a direct n-vs-s
   comparison on identical data (the earlier yolo26s comparison,
   `rough_v7`, predates the referee-class fix and the 28 hard frames, so
   wasn't a fair comparison point anymore). Ran the full 300 epochs (no
   early stop this time), 66 min.
4. Both exported to ONNX immediately (`nms=True` requested, ultralytics
   force-disables it for this end2end architecture same as always,
   output shape already `[1,300,6]` regardless) - verified via
   `onnx.load` metadata: both `1x3x1280x1280` in, `1x300x6` out,
   `{0:person,1:ball,2:referee}` names, on both checkpoints.

**First look was alarming** - `v3`'s ball mAP50 (0.512) looked far
below `v2`'s reported 0.694, and `yolo26s_v3` showed the same drop
(0.526) despite yolo26s otherwise clearly outperforming yolo26n on this
data (all mAP50 0.759 vs 0.721, mAP50-95 0.573 vs 0.474 - consistent
with the original forum-based decision to prefer Small). Both new
models also showed ball precision jumping to a perfect 1.000 with
recall dropping to ~0.49, vs v2's 0.861 P / 0.583 R - looked like a real
regression, and one affecting both architectures identically, which
argued against it being architecture-specific.

**Root-caused, not real**: re-ran `yolo val` with `v2`'s checkpoint
against `v3`'s val set (round3's 34 images) instead of `v2`'s own
original 30-image val slice. **`v2` scores ball mAP50 0.524 on this
val set - essentially identical to `v3` (0.512) and `yolo26s_v3`
(0.526)**, and `v2`'s recall on this set (0.438) is actually *lower*
than either new model's (0.485/0.496). The apparent "regression" was
entirely a val-split artifact: the round3 val set (34 images, only 16
ball instances) is simply harder for ball detection than the old
200-set's val slice was, for every model tested including the one
previously reported as 0.694. (The round3 val set is drawn entirely
from the original 200-image pool, not the 28 hard frames - confirmed
via directory listing, 0 `win*`-named files in `images/val/`.) **No
regression - v3/yolo26s_v3 are at least as good as v2, likely slightly
better on recall**, on a genuinely harder/more honest val sample.

**Checkpoints**:
`D:\VOETBAL_VIDEO\RECO\training\round3\runs\{yolo26n_v3_3class_1280_b4_e300,yolo26s_v3_3class_1280_b4_e300}\weights\{best.pt,best.onnx}`.

**Not yet done**: neither new checkpoint tested in the real `reco` app
yet (mAP numbers only so far, matching this project's own standing
caution not to trust mAP alone) - that's the natural next step before
picking one to actually ship.

## Round 4: yolo26s on the 258-task set (+winC hard frames), plus a copy_paste/rect experiment (2026-08-13)

Follow-on from the "camera still isn't great" AI review: built a new
`dump_detection_frames` debug tool (`crates/reco-io/examples/`) to
visually inspect exactly what the detector saw at specific missed
frames, found a genuine 179-frame raw-detection gap window (frames
690-719 of a 30s test export), pushed 30 hard frames from that window
("winC" batch) to LS project 8, user reviewed/corrected all 30.

**Data prep**: fresh `GET /api/projects/8/export?exportType=YOLO`
(258 tasks), `prepare_yolo_train_split_from_ls_export.py` -> 219 train /
39 val (all 30 winC frames landed in train; val set drawn entirely from
the original 228-pool, same population round3's val came from).

**Real gotcha hit and fixed**: first training attempt (`workers=8`,
default) crashed with `OSError: [WinError 1455] Het wisselbestand is te
klein` (paging file too small) loading a CUDA DLL in a dataloader
worker - `reco-gui.exe` was open using ~3.9GB RAM at the time, leaving
only ~7.8GB free of 32GB total; 8 concurrent worker processes each
loading their own torch/CUDA DLLs exhausted it. Fixed with
`workers=2` - retried clean, actually *faster* (41 min vs round3's 66)
since this workload is GPU-bound at batch=4, not dataloader-bound.

**`yolo26s_v4_3class_1280_b4_e300`**: fresh from stock weights, same
hyperparams as round3. Early-stopped at 169 epochs (best @ 69).

```
              Precision  Recall  mAP50  mAP50-95
all               0.784   0.770  0.710     0.510
person            0.945   0.867  0.919     0.619
ball              0.985   0.444  0.465     0.321
referee           0.422   1.000  0.745     0.589
```

Checkpoint: `round4/runs/yolo26s_v4_3class_1280_b4_e300/weights/{best.pt,best.onnx}`
(ONNX verified: `1x3x1280x1280` in, `1x300x6` out, correct class names).

**Ball precision near-perfect, recall low** (0.985 / 0.444, on only 18
val instances) - when the model says ball it's almost always right, but
misses over half. Asked how to improve recall; answered prioritized
(more hard-frame data > augmentation tuning > runtime confidence
threshold - the last one rejected: the shoe false-positive found later
was already above the current 0.1 threshold, so lowering it would add
false positives, not find more real balls).

**Quick experiment, requested before collecting more data**:
`yolo26s_v4_copypaste_rect` - same data/hyperparams +
`copy_paste=0.3 rect=True`. 199 epochs (best @ 99), 42 min.

```
              Precision  Recall  mAP50  mAP50-95
ball              1.000   0.435  0.478     0.334
```

**Honest result: no meaningful recall improvement** (0.444 -> 0.435,
within noise for n=18) - a small mAP uptick (box-quality), not more
balls actually found. Confirms data quantity/diversity is the real
bottleneck at this dataset size (~18-141 ball instances depending on
split), not these particular augmentation knobs at these particular
values.

**Sanity check with an important caveat**: ran the new model on the 30
winC images and diffed against the LS-corrected ground truth - 30/30
matched (IoU 0.82-0.97). This is **not evidence of generalization** -
these exact images were in the training set, so it only confirms the
labels/pipeline were used correctly, not that the model improved on
genuinely unseen ball situations.

**Self-correction worth recording**: an earlier claim (this same
investigation, before round-4 training existed) that a specific ball
detection was "visually merged with a player's body" turned out to be
wrong on re-inspection with fresh evidence - the ball was actually
isolated elsewhere in the frame; the original screenshot pointed at the
wrong pixel location. Re-tested properly with the round-4 model on a
freshly-decoded (not reused) frame before concluding anything. General
practice going forward: when revisiting a specific visual claim,
regenerate the evidence, don't reuse an old screenshot from memory.

**Real-app test, 2026-08-14**: full 899-frame CLI render of
`yolo26s_v4_3class_1280_b4_e300` against the same 100-130s 03 OJC clip
used throughout the ball_weight/FOV-Wide/Ball-reach A/B work, current
full recommended panner settings, `--features tensorrt`. Ran clean,
no crashes. Overall raw-ball rate 49.1% (441/899), flat vs this round's
own 48.7% LS-val-set finding - confirms "no real gain" from a second,
independent angle. Frames 720-898 (the documented zero-recall gap from
all 3 ball_weight A/B renders) is still exactly 0/179 at round-4 too -
genuinely untracked the whole stretch, unchanged. One brighter spot:
frames 690-719 hit 24/30 = 80% raw-detection, though confidence stays
modest (mean 0.46). See [[project_yolo26n_training_pipeline]] for the
frame-704 localization-error follow-up (round-4 detects the ball there
but the box lands ~20px off the LS-corrected ground truth at low
confidence 0.55 - a real but modest miss, not the dramatic "on bare
grass" a lone unreferenced crop first suggested).

## imgsz=1536 experiment - stopped, inconclusive/negative on ball metrics (2026-08-14)

Prompted by the frame-704 localization-error finding above: does more
resolution reduce that kind of box-precision miss? Resumed from
`yolo26s.pt` fresh, same 258-task dataset/hyperparams as
`yolo26s_v4_3class_1280_b4_e300` except `imgsz=1280 -> 1536` (still
divisible by 32, batch=4 still fits - ~8.2-8.4GB of the 3060 Ti's 8GB
card, close to the ceiling but no OOM). Real gotcha before starting:
`reco-gui.exe` was open using ~6GB RAM (only ~4GB free, even tighter
than the round-4 `workers=8` paging-file crash) - user closed it first,
freed to ~15.9GB, ran with `workers=2` as usual.

Ran to epoch 187 (stopped manually - `patience=100` never triggered on
its own; the internal fitness Ultralytics tracks for patience isn't
just the `mAP50-95` column, so it kept training well past where a naive
epoch-28+100 estimate predicted it would stop). **Best checkpoint the
whole run ever produced was epoch 28** - no improvement in the
following 159 epochs:

```
              Precision  Recall  mAP50  mAP50-95
ball  1280 (round-4, e169)  0.985   0.444  0.465   0.321
ball  1536 (this run, e28)  0.782   0.398  0.403   0.292
all   1280 (round-4, e169)  0.784   0.770  0.710   0.510
all   1536 (this run, e28)  0.652   0.763  0.716   0.527
```

**Honest conclusion: worse on the metric that matters (ball), better
only on the aggregate "all" number** (which is diluted by person/
referee). This is not a fair apples-to-apples comparison though -
round-4 trained to its own patience-triggered stop at epoch 169 (best
@69); this run's best came from epoch 28, far earlier in relative
training progress, before we know whether it would have kept improving.
**Not repeated/extended further this session** - stopped by user
request after ~6 hours of wall-clock time (started 08:14, stopped
14:23) with no improvement since epoch 28, judged not worth the
GPU-time cost to let it fully self-terminate. Checkpoint kept at
`round4/runs/yolo26s_v4_imgsz1536/weights/{best.pt,last.pt}`
in case it's worth revisiting (e.g. resuming overnight next time,
per the user's own suggestion, rather than babysitting it turn by
turn during a live session).

**Real-app test of this checkpoint, 2026-08-14 (same clip/settings as
round-4's own real-app test above)**:

```
                          round-4 (1280,e169)  imgsz1536 (e28)
overall raw-ball rate     49.1% (441/899)      37.0% (333/899)
frames 720-898 (the
known zero-recall gap)    0/179                15/179
frames 690-719            24/30 (80%)          21/30 (70%)
mean ball confidence      0.46                 0.39
```

**Mixed, not a clean win or loss.** Overall recall is worse. But frames
720-898 - zero raw ball detections in *every* prior test on this clip
(`yolo26s_v3`, `yolo26s_v4_3class_1280_b4_e300`, all 3 `ball_weight`
A/B renders) - has a nonzero hit rate for the first time. Frame 704
specifically: confidence rose 0.55 -> 0.82, but the box landed at
virtually the *same* pixel location as round-4's (still ~20px off the
LS-corrected ground truth) - the original localization complaint this
experiment was chasing did not improve, only confidence in the same
slightly-wrong spot did.

**Real gotcha hit + fixed along the way**: first attempt at this test
returned 0 detections *of any class*, not just ball - looked like total
model failure. Root cause: a genuine **ONNX Runtime TensorRT-EP engine
cache bug**, not reco-detect's own code. `%LOCALAPPDATA%\reco\trt-cache\`
is a single shared cache keyed by a graph hash that apparently doesn't
fully distinguish two structurally-identical graphs (same 384 nodes,
same op sequence - this and the 1280 checkpoint share the exact same
architecture) differing only in their baked-in input shape (`1x3x1280x1280`
vs `1x3x1536x1536`) - confirmed via direct `onnx.load` inspection that
the two ONNX files genuinely declare different input shapes, yet
TensorRT tried to reuse the 1280 model's cached engine (built the day
before) for the 1536 model's inference call, failing at
`IExecutionContext::setInputShape()`. **Fix**: delete the stale
`.engine`/`.timing` files in that cache dir and re-run - TensorRT
rebuilds a fresh, correctly-shaped engine (cold build took ~5 min this
time vs the usual <1 min warm run). **Going forward: always clear
`trt-cache` before testing a model with a different `imgsz`/input shape
than whatever was last cached**, even on the same `reco.exe` build -
this is not specific to any one model pair, likely bites any two
same-architecture models with different declared input dims.

**Not yet done**: no rigorous genuinely-unseen-frame recall test yet
(would need a proper held-out clip, not single spot-checks). The
left/right 2880x2880-crop dataset-augmentation idea (SAHI-style tiling,
research-only, not built - see chat log 2026-08-14) is still the more
promising next lever than pushing `imgsz` further - it recovers the
~25% letterbox-padding waste any square input has on this 4:3 source
without the same VRAM/BatchNorm risk profile as a much larger `imgsz`,
and the 720-898 partial-fix signal above suggests *some* form of
better-resolved small-object training data does help that specific gap,
worth chasing further via a cleaner lever than raw `imgsz`.

## imgsz=1920 experiment - a real win over round-4, delegated overnight (2026-08-14/15)

Prompted by a real engineer's own recommendation (contacted via the
forum, the same one behind the original yolo26s-over-yolo26n call) -
trains at both 1280 and 1920 as two separate models, no combined
technique. Feasibility smoke-tested first (8 epochs, 15% data
fraction, `batch=1`): only ~3.76GB VRAM even with `reco-gui.exe` open.
**Real gotcha hit + fixed along the way**: `round4/data.yaml`'s (and
`round3`/`rough_v2_v6`/`rough_3class`'s) internal `path:` field still
pointed at the pre-rename folder name from the earlier training-folder
cleanup - missed at the time. Fixed all 4. **Lesson: after renaming a
training folder, check `data.yaml`'s own internal `path:` field, not
just the folder name.**

An 8-epoch run on the *full* 258-task dataset (still `batch=1`) already
beat round-4's own epoch-8 on mAP50-95 (0.443 vs 0.380) - different,
better early trajectory than the inconclusive imgsz=1536 experiment
ever showed, and its real-app test (see below) hit 100% raw-ball on the
690-719 window and 51/179 (28.5%) on the previously-always-0 720-898
gap. Justified a full run.

**Full run, delegated end-to-end overnight** (training -> ONNX export
-> real-app test -> frame re-dump for review, all done unattended while
the user slept): `batch=2`, `reco-gui.exe` closed, stable ~7.48GB/8GB
VRAM the whole run. Stopped naturally at **epoch 225** (best @
**epoch 125**, `patience=100`), 2.55 hours.

Val-set:
```
              Precision  Recall  mAP50  mAP50-95
all (1920)     0.731      0.789  0.736   0.557    (round-4 1280: 0.784/0.770/0.710/0.510)
ball (1920)    0.920      0.444  0.474   0.344    (round-4 1280: 0.985/0.444/0.465/0.321)
```
Ball recall on the val set is exactly unchanged (0.444); mAP50-95
improved meaningfully (box quality), not recall, on this 18-instance
slice.

**Real-app test** (same clip/settings as every prior round;
`trt-cache` cleared first even though the shape matched the already-
cached 8-epoch engine - the cache key's sensitivity to different
*weights* at identical shape was untested, and a silent wrong-weights
reuse would be worse than a crash):

```
                       round-4(1280,e169)  1920(8ep)  1920(FINAL,e225)
overall raw-ball rate   49.1%               42.7%      51.1%   <- best of all checkpoints tested
frames 720-898 gap      0/179               51/179     18/179 (10.1%)
frames 690-719          80%                 100%       80%
mean confidence         0.46                0.41       0.48   <- best
frame 704 confidence    0.55                0.96       0.94
```

**Genuine improvement over round-4** on the primary metric (overall
recall + confidence), and the persistent 720-898 dead zone - exactly
0/179 in every prior test this project has ever run, across
`yolo26s_v3`, round-4, and the imgsz=1536 experiment - is no longer
zero. **Not a clean sweep**: the intermediate 8-epoch checkpoint scored
*higher* than the fully-converged final model on both curated review
windows - an unexplained early-training artifact, not chased further.

**Frame 704 localization "doesn't improve" - root-caused, see below**:
confidence rose across every checkpoint (0.55 -> 0.94), but position
stayed ~25px off in the same direction regardless of training run. Not
a detector quirk - a single mislabeled training example, see the
dedicated section right after this one.

Checkpoint: `round4/runs/yolo26s_v4_imgsz1920/weights/{best.pt,best.onnx}`
(ONNX verified: `1x3x1920x1920` in, `1x300x6` out, correct names).
Frames 3705-3715 (right camera) re-dumped with this model for physical
review - labels + the fixed curve-aware ROI overlay (see the ROI
section elsewhere in this repo's memory) both visible.

**This is now the best-tested checkpoint of the whole project** on the
real-app metric - not a solved recall problem, but a real step forward.
Not yet promoted to "the" production default.

## Root cause of the "frame-704 offset": one mislabeled training example, not a model/pipeline bug (2026-08-15)

User declined to keep training until this was explained - correctly,
since a real pipeline bug would invalidate every result above.
Investigated properly:

1. Compared *every* detection in frame 704 against its nearest ground-
   truth box (LS task 1951/`winC_014`, the same frame) - person/referee
   matched almost perfectly (<1-11px, normal noise); only the ball was
   off by 23px, far outside that band. Rules out a general coordinate/
   letterbox bug, which would hit every class uniformly.
2. Checked 3 other ball labels elsewhere in the training set
   (`winC_030`, `winC_029`, `winC_012`) against their own images - all
   3 sat correctly on their visible ball. Not a systemic labeling-
   convention problem across the dataset.
3. Zoomed into frame 704 at high magnification with a pixel-grid
   overlay: neither the model's prediction nor the LS "ground truth"
   actually touched the visible ball - both sat ~23-28px above it. The
   LS annotation's `origin` field read `"prediction-changed"` with the
   *same confidence score (0.55)* as the original AI pre-label - a
   human had technically touched the box (enough to flip the origin
   flag) without ever moving it onto the real ball. One understandable
   miss among dozens of boxes in a busy frame.

**`winC_014` (= frame 704) has one bad ball label, and it's in the
training set every checkpoint this session was trained on** - so all 4
(round-4, 1536, 1920-8ep, 1920-final) partially learned/reproduced this
one wrong position on this one specific (in-training-set) frame. That's
why the "offset" looked consistent across 4 very different runs - they
were graded against the same flawed answer key on an image they'd all
memorized a piece of. Not a fair generalization test, same lesson as
the "30/30 winC self-check isn't evidence of generalization" note in
the Round 4 section above.

**Fixed both copies**: `round4/labels/train/4abed4f2-winC_014.txt` line
16 (`1 0.466927 0.648611 0.015625 0.017361`, center moved ~28px down in
y, box widened slightly to actually enclose the ball) and the LS
source annotation (`PATCH /api/annotations/385/`, ball entry `det_15`).

**Not yet done**: this fixes one label, doesn't retroactively change
the 4 already-trained checkpoints. Doesn't by itself justify an
immediate retrain (1 of ~140 ball instances).

## Systematic QA pass across every ball label - winC_014 is an isolated miss, not a pattern (2026-08-15, same session)

Ran the QA pass the open question above called for. One-off script
(session scratchpad, not committed) loads the best checkpoint
(`yolo26s_v4_imgsz1920`, imgsz=1920), runs inference on every image
with a ball label (train + val), matches each label to its nearest
predicted ball box, flags low-IoU/no-match cases.

**214 ball instances checked** (149 train + 14 val images with a ball
label): **OK 164 (76.6%), LOW_IOU 16 (7.5%), MISSED 34 (15.9%)**.
MISSED is almost certainly the already-known recall gap (model finds
no ball at all, even at conf>=0.10), not a labeling concern.

Visually checked 5 of the 16 LOW_IOU cases (smallest-distance ones
most likely to be genuine mislabels, plus the single largest-distance
one): `winC_014` itself still flags (expected - the *current*
checkpoint trained on the *old* wrong label before this session's fix);
2 cases were label-correct but had **multiple balls in one frame**
(model picked a different real ball than the labeled one - a dataset
ambiguity, not an error); 2 cases were label-correct with a **model
false positive** elsewhere in the frame; 1 (2905px distance) was
label-correct with an unrelated weak false positive far away.

**Zero new label errors found.** `winC_014` looks genuinely isolated,
consistent with the earlier 3/3 spot-check. Didn't exhaustively check
the remaining 11 LOW_IOU cases, but the pattern held cleanly across the
full distance range sampled - a broader systemic problem looks
unlikely. **Answers the open question: safe to keep training/using
this dataset without a full manual re-audit.**

(Revised later the same day - see `SESSION_HANDOFF.md`'s "Full
independent ball-label QA audit" entry / [[project_yolo26n_training_pipeline]]:
a follow-up pass using an independent model lineage + a full visual
contact-sheet scan found 6 real mislabels, all fixed. This QA pass's
own blind spot was checking only a same-family model's agreement with
labels it was trained on.)

## Ai Learning batch (Berghem Sport J011-1) - 4 new source videos exported to Label Studio (2026-08-17)

Not a training round - a new data-collection batch, logged here for
provenance since it feeds the next round. 4 new raw videos in
`D:\VOETBAL_VIDEO\Berghem Sport J011-1\Ai Learning`: 2 recordings
(`0001` = 2026-05-30, `0005` = 2026-06-03), each L+R, 3840x2880 HEVC,
~20.4 min. `select_ball_rich_frames.py` (interval=3s, top-k=25,
conf=0.15), one camera tag per video (`0001_left`/`0001_right`/
`0005_left`/`0005_right`):

```
0001_left:  408/408 candidates had >=1 ball, kept 25
0001_right: 250/408 candidates had >=1 ball, kept 25
0005_left:  408/408 candidates had >=1 ball, kept 25
0005_right: 399/408 candidates had >=1 ball, kept 25
```

**Model: `yolo26s_v4_imgsz1920` (imgsz=1920), not `soccana.pt`.** User
initially asked for the established soccana teacher-model convention,
then corrected mid-session to "the latest ONNX" - of the two
undocumented-as-"latest" candidates on disk, chose the documented,
real-app-tested `round4/runs/yolo26s_v4_imgsz1920/weights/best.pt`
over the newer-by-timestamp but unvalidated
`yolo26s_tiled1920_full/weights/best.onnx` (needs tiled L/R inference
not built into this script or any production path yet - see the
"SAHI-style left/right tiled-1920 training" section above). Ran via
the `.pt` weights on GPU/torch (this machine's `onnxruntime` has no
GPU execution provider installed) - identical weights to `best.onnx`,
just a faster local backend for the pre-labeling pass itself.

Flattened (100 images) and pushed to **LS project 24, "Ai Learning -
yolo26s_v4_imgsz1920 pre-labels"** - 100 tasks, 100 predictions, 0
annotations yet (awaiting the user's review pass). Re-hit the known
"import response has no `task_ids` on this LS instance" quirk (see
[[project_yolo26n_training_pipeline]], first hit 2026-08-09) in a
fresh driver script that didn't check for it first - no data lost
(images uploaded fine, just needed a follow-up pass to attach
predictions via `GET /api/tasks` filename-matching instead). Full
narrative in `SESSION_HANDOFF.md`'s 2026-08-17 entry.

## Merged training set across all reviewed data, all LS projects (2026-08-21)

User asked for one training set pooling every reviewed image across
*all* LS projects, including projects still mid-review - not just the
latest round's dedicated project. Checked all 6 projects on the LS
instance via the API:

| id | title | tasks | reviewed |
|---|---|---|---|
| 24 | Ai Learning - yolo26s | 100 | 100 |
| 19 | 02 RPC - ball-rich pre-labels (soccana) | 200 | 34 |
| 18 | 01 Vierluik - ball-rich pre-labels (soccana) | 556 | 10 |
| 16 | yolo26s rough_v6_1280_b4_e300 predictions | 200 | 0 |
| 9 | yolo11 football (soccana) - RECO test | 150 | 4 |
| 8 | Finetuned yolo26n (rough v1) | 258 | 258 |

Excluded 16 (0 reviewed - pure unreviewed model predictions, would add
noise not signal) and 9 (a 4-task test project) on the user's call.
Kept 8/18/19/24 = **402 reviewed images total** (vs. round 4's 258).

**Real bug caught while verifying, not just assumed correct**: the
existing local `training/ai_learning_dataset/` copy of project 24's
labels does NOT match project 24's actual current annotations - its
`classes.txt` claims `0 person / 1 ball / 2 referee` but the label
files underneath hold class-frequency counts (1534/591/4) that don't
correspond to *either* class ordering once cross-checked against a
freshly re-pulled official export (`ball, person, referee` order per
`classes.txt`, counts 144/1288/119 for the same 100 tasks) - it's
stale/from an earlier, uncorrected point in the review, not the
finished data. Would have silently trained on wrong review state (or
worse, swapped ball/person if naively remapped) had this not been
cross-checked. Used the fresh re-pulled export instead; the stale
local copy is untouched, not deleted, in case its provenance matters
later.

**Image pairing**: LS's YOLO export bundles labels only (no image
bytes, as `prepare_yolo_train_split_from_ls_export.py`'s docstring
already notes), and label filenames get a per-task uuid prefix
assigned at *upload* time (confirmed stable across repeated exports of
the same task - not regenerated per-export). For project 8, the
matching pre-upload flat image set already existed locally
(`training/_archive/finetuned_n_preds/ls_flat/images/`, uuid-prefixed
filenames already matching). For project 24, the local pre-upload
images existed but *without* the uuid prefix
(`training/ai_learning_dataset/ls_flat/images/`) - resolved by
stripping the `XXXXXXXX-` prefix from each export label's stem and
matching the remainder (confirmed via `GET /api/tasks/?project=24`:
each task's `data.image` is literally `/data/upload/<project>/<uuid>-
<original filename>.jpg`, i.e. the uuid prefix *is* the original
filename with a collision-avoidance prefix bolted on at upload). For
18/19 (uploaded from elsewhere - probably the other PC, no local trace
here) there was no pre-upload copy at all; downloaded the 10+34
actually-annotated images directly from the LS instance instead (cheap
at that count - not the full 756 unreviewed tasks).

**New `scripts/merge_yolo_datasets.py`**: `prepare_yolo_train_split_
from_ls_export.py` only ever handled one project at a time. The new
script takes multiple `--raw-source` (a raw LS export + matching
images, remapped from LS's ball/person/referee order same as the
existing script) and `--ready-source` (an already-prepared dataset
like `round4`, pooled as-is - remapping it again would have silently
swapped classes, exactly the bug caught above) entries, pools every
source's pairs, and does *one* global shuffle+split - not a separate
split per source, which would bias val toward whichever source ran
last and could leave a small source with zero val representation.

Produced `training/merged_v1/`: 342 train / 60 val (402 total, 15%
val). Verified: total person/ball/referee counts (4650/387/365) equal
the sum of each source's individually-confirmed counts exactly; zero
zero-byte images; each source contributes to train (vierluik, only 10
images total, landed 0 in this particular val shuffle - plausible at
that sample size, not a bug, but worth a stratified split later if it
matters).

**Not started yet**: the actual `yolo detect train` run - staging and
verification only this session, training itself needs a separate
go-ahead (it's a long GPU-bound run). Base checkpoint choice
(continue from round 4's, or start fresh given the ~1.5x larger and
more diverse pool) also not decided yet.

### Extended the same set to SAHI-style tiled-1920 (same session)

User reminder not to forget the tiled-1920 technique from 2026-08-15/16
(see SESSION_HANDOFF.md's "SAHI-style left/right tiled training" entry
- a real, measured ~50% relative win on ball recall/mAP50-95, but
stayed a one-off scratchpad script, round4-only). Reused for the full
402-image merged set this time - new `scripts/tile_yolo_dataset.py`
(committed properly, not scratchpad), same crop geometry (two
overlapping 2880x2880 crops per 3840x2880 source, left x=[0,2880]/
right x=[960,3840], resized to 1920x1920).

Verified before trusting it, same discipline as the merge itself:
confirmed all four source projects are uniformly 3840x2880 first (the
crop geometry is a fixed-pixel scheme, not aspect-adaptive - would
silently misalign on a differently-sized source), then drew a sample
of transformed boxes back onto their tiles and checked visually (kept
at `training/merged_v1_tiled_1920_verify_samples/`) - tight and
correct on both an already-verified round4 tile and a never-tiled
ai_learning tile.

`training/merged_v1_tiled_1920/`: 342/60 train/val source images ->
684/120 tiles (804 total).

Still needs a matching tiled-inference pipeline to use a checkpoint
trained on this in any production path - not built yet, same caveat as
the original round. Training itself also not started.

## Tiled-1920 training on the merged multi-project set - real-footage regression found (2026-08-21/22)

Full run on `training/merged_v1_tiled_1920/` (804 tiles), fresh from
`yolo26s.pt`, same hyperparams as round-4's own tiled run (`batch=2,
imgsz=1920, workers=2, patience=100`). 8-epoch smoke test first
(healthy - losses down monotonically, mAP50-95 0.30->0.55, no
crashes), then the full run per user go-ahead ("duidelijk als alles
gezond is start dan maar de volle patience"). Early-stopped at epoch
288 (best @ 188), 11.2 hours.

Val-set (all-class aggregate, not ball-specific):
```
              Precision  Recall  mAP50  mAP50-95
old (round4 tiled1920, best@59)   0.767   0.854  0.838   0.611
new (merged tiled1920, best@188)  0.882   0.816  0.864   0.647
```
Precision and mAP50-95 up, recall down slightly - on the aggregate
number. Exported to ONNX (`1x3x1920x1920` in, `1x300x6` out, correct
`{0:person,1:ball,2:referee}` names) and real-footage tested: 900
extracted frames per camera (t=100-130s, 03 OJC, raw left/right camera
files - NOT the historically-tracked stitched-panorama frame-720-898
window, since Rust-side tiled dual-inference doesn't exist in
production yet; this test tiles each raw camera frame the same way
`tile_yolo_dataset.py` does and runs both tiles through the checkpoint
via the ultralytics Python API directly, not the CLI/GUI app), `conf=0.1`
(matches reco-detect's documented production threshold):

```
                                   left rate  left conf  right rate  right conf
old (round4 tiled1920)               12.3%      0.28       92.0%       0.53
new (merged tiled1920, 2026-08-21)    4.7%      0.55       94.2%       0.63
```

**Not a clean win.** Right camera (where the ball spent most of this
clip) is flat-to-better and clearly more confident. Left camera is a
real recall regression - less than half the raw-ball hits the old
checkpoint found, despite each hit being much more confident. Pattern:
new model is pickier, not simply worse - it drops marginal/low-confidence
calls the old model still caught.

## Non-tiled 1920 training on the same merged set, to isolate the cause (2026-08-22)

Open question after the tiled result: is the left-camera regression
from the tiling method, or from the merged dataset itself? Round-4's
own non-tiled 1920 run (2026-08-14/15) was a genuine win at the time,
so a non-tiled run on the *same* merged data as the tiled round, using
`training/merged_v1/` (already on disk, no new data prep), isolates
the variable. Same discipline: 8-epoch smoke test (healthy, mAP50-95
0.19->0.52 by epoch 8, no crashes) then full `patience=100` run per
user go-ahead ("voer de niet-tiled merged-run uit" / "als de smoke
test goed is, start dan zelf de volledige test"). Ran the full 300
epochs without early-stopping this time (4.46 hours) - `patience=100`
never triggered.

Val-set (all-class aggregate):
```
              Precision  Recall  mAP50  mAP50-95
old (round4 1920 non-tiled, best@59)     0.767   0.854  0.838   0.611
new (merged 1920 non-tiled, best@210)    0.848   0.822  0.873   0.640
```

Real-footage test, same clip/methodology as above but no tiling (full
3840x2880 frame, ultralytics letterboxes to 1920x1920 itself):

```
                                        left rate  left conf  right rate  right conf
old (round4 imgsz1920 non-tiled)          11.4%      0.28       81.9%       0.64
new (merged imgsz1920 non-tiled, 08-22)    5.0%      0.45       82.2%       0.77
```

**Conclusion: the regression is the merged dataset, not the tiling.**
Both independently-trained checkpoints (tiled and non-tiled) on the
same merged data show essentially the same effect - left-camera raw-ball
rate roughly halved (11-12% -> ~5%) while right-camera stays flat and
confidence rises substantially across the board. If tiling were the
cause the non-tiled run wouldn't reproduce it; it does, closely. Most
likely explanation: combining projects 8/18/19/24 shifted the ball
example distribution in a way that makes the model systematically more
conservative on left-camera-style scenes specifically.

## Root cause found: the merged data has a real "easy ball" size bias (2026-08-22, same session)

Checked `training/merged_v1/labels/train` per source project (by
filename prefix - `round4`, `ai_learning`, `rpc`, `vierluik`), counting
ball (class 1) instances and mean normalized bbox area:

```
source        images  ball instances  balls/image  mean ball size (px, approx)
round4         220     173             0.79          232
ai_learning     85     122             1.44          251  (+8%)
rpc             27      19             0.70          331  (+43%)
vierluik        10       9             0.90          295  (+27%)
```

**`round4` - the only source that actually contains the 03 OJC match/
camera this real-footage test uses - has the smallest average ball
size of all four sources.** All three newly-merged sources skew
larger/easier. `ai_learning` in particular was deliberately curated as
"ball-richest frames" via `select_ball_rich_frames.py`'s teacher-model
scoring, which selects for clearly-visible (and thus typically larger)
balls by construction, not a representative sample of ball difficulty.

Replacing ~46% of round4's training images with proportionally larger/
easier ball examples explains the exact pattern seen in both real-
footage tests: higher confidence everywhere (the model learned clean,
large balls better) but lower recall specifically on hard/small cases
like the left camera's - the same class of case round4-only training
already struggled with, now pushed further by a training distribution
that's even less representative of it.

**This mirrors round-4's own original finding, inverted**: round-4
concluded "more hard-frame data > augmentation" for improving ball
recall. This round accidentally did the opposite - added more *easy*
data, and recall regressed as a direct, predictable consequence, not a
tiling artifact or a training instability.

**Not shipping either new checkpoint. Next step, not started**: any
future round on this merged pool should deliberately balance for
ball-size/difficulty (e.g. weight or filter for small/hard ball
examples specifically) rather than just adding more images -
`ai_learning`-style "ball-richest" curation is good for finding *any*
ball to bootstrap labels quickly, but is the wrong selection criterion
for a training set meant to improve recall on hard cases.

**Not shipping either new checkpoint (tiled or non-tiled) as a
replacement.** Checkpoints kept for reference:
`training/merged_v1_tiled_1920/runs/full_patience100/weights/best.pt`,
`training/merged_v1/runs/full_patience100_nontiled/weights/best.pt`.
Next step, not started: root-cause the merged-data recall regression
before another training attempt, rather than trying further blind
training variants.
