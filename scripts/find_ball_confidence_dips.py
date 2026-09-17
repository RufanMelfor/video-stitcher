#!/usr/bin/env python3
r"""Find CONFIRMED ball tracks in raw match footage, follow each one until
the detector loses it, and package the loss-moment frames (plus context)
for Label Studio - a hard-negative source complementary to
pick_training_frames.py.

# Why this is a different signal than pick_training_frames.py

pick_training_frames.py's blind_spot group samples ~180 points spread
sparsely across a whole match (--min-gap defaults to 120s) and asks "did the
model miss the ball here". That is the right tool for finding the model's
general blind spots, but it structurally cannot see a loss like this one: a
ball sitting still, in plain sight, that the model detects on frame N,
loses on frame N+1..N+k, and detects again on frame N+k+1 with no motion,
occlusion, or lighting change in between - e.g. a ball rolling onto a white
field line loses the sharp dark-grass/white-ball contrast the model leans
on. Sparse point-sampling almost never lands inside a multi-frame window
that short. This script instead decodes sequentially (like
raw_camera_debug.rs / `reco-cli ai-debug-raw`) and explicitly tracks one
ball at a time forward from a confirmed sighting.

# Why tracking must start from a CONFIRMED ball, not just any candidate

An earlier version of this script picked "the ball" as whichever
ball-class candidate had the highest confidence in a frame with no
established track yet - including low-confidence false positives (a
cone, a line marking, a sponsor board corner). Once locked onto that
position, it would keep re-finding the SAME static fixture on every later
frame (a fixture doesn't move, so it's always "close" to its own last
position) and report the fixture's own confidence noise as a "ball lost"
event. In practice this shipped a batch of Label Studio tasks where every
box was on a cone or a line, never a ball - a real failure caught only by
the user reviewing the actual uploaded predictions, not by any earlier
testing here (see this project's own docs/YOLO26_Training.md and repeated
"always regenerate fresh evidence, don't defend a claim from memory"
convention - this script itself violated that until the user caught it).

The fix (`build_ball_tracks`): a track may only START at a frame where a
ball-class candidate clears `--anchor-conf`, a much higher bar (default
0.5) than the usual 0.1-0.2 production floor - high enough that a false
positive clearing it is rare, so reaching it is real evidence of a ball.
Once anchored, the track is extended forward by spatial proximity only
(nearest candidate within `--max-track-dist`, regardless of that
candidate's own confidence - a LOW-confidence detection of the already-
confirmed real ball is exactly the interesting signal, not a reason to
drop the track). The track's end - where no candidate remains within
range for more than `--max-gap-frames` - is the label-worthy moment: a
ball the detector was confidently tracking, that it then failed to keep
finding. `--min-track-frames` additionally discards any track that
barely got established (an anchor whose very next frame already failed
to reconnect is itself suspect, not a real tracked ball).

Detection confidence is read from the SAME kind of raw, untiled,
un-ROI-filtered single-frame inference `reco-cli ai-debug-raw` uses (see
that command's own doc comment for why raw camera-space, not projected/
stitched space, is the only domain that matches what the detector actually
saw) - the ultralytics Python API directly, no tracker, no ROI filter, no
ROI-anchor logic. This deliberately excludes every tracker-level reason the
ball can appear "lost" in a real export (BallTracker's coast budget,
player-anchor gate, RoiFilteredDetector's in/out-of-ROI test) - see this
repo's docs/YOLO26_Training.md and crates/reco-autocam/src/roi_filter.rs
for those. A frame where the raw detector itself never dropped below
threshold is not a detector weakness, whatever a higher-level tracker did
with that same detection - labeling tracker-only "losses" would add frames
the detector already handles correctly, diluting rather than improving the
next training round.

Usage (default scans start-secs to the end of the video - a full match in
one run; pass --duration-secs for a shorter, specific clip instead):
  python scripts/find_ball_confidence_dips.py \
      --video "<path>/RIGHT/DJI_..._R01.MP4" --camera right \
      --model "D:\VOETBAL_VIDEO\RECO\training\round7_tiled_1920\runs\full_patience100\weights\best.pt" \
      --out dataset_dir

Output matches pick_training_frames.py's convention (images/<camera>/,
labels/<camera>/, classes.txt) so upload_to_labelstudio.py and
package_yolo_for_labelstudio.py both work on it unchanged. Each kept
frame's label file holds the model's own prediction (every class, not just
ball) as a starting point for human correction in Label Studio - same
convention pick_training_frames.py uses, never hand the labeler an empty
file when the model already has a plausible guess for person/referee boxes
even where it missed the ball.

Ball-only vs. all 3 classes: this script keeps training 3 classes
(person/ball/referee) exactly like every other tool in this project -
see docs/YOLO26_Training.md and train_class_weighted.py's --ball-weight
for the deliberate choice to fix ball-class imbalance via a loss-weight
knob, not by dropping the other classes. A frame selected here still needs
its person/referee boxes for FieldPanner's cluster-based autocam framing
downstream - dropping those classes would break more than it fixes.

NOT run as part of writing this script - point it at a real match's raw
camera source and a checkpoint yourself (or ask me to).
"""

import argparse
import json
import math
import subprocess
import sys
from pathlib import Path

CLASS_NAMES = ["person", "ball", "referee"]
PERSON_CLS, BALL_CLS, REFEREE_CLS = 0, 1, 2


def probe_fps(video_path: Path) -> float:
    """Real bug found 2026-09-15: `-of csv=p=0` on this ffprobe query
    returns a trailing comma (e.g. `30000/1001,`, not `30000/1001`) - CSV
    output always terminates each row with its field separator here since
    there's only one field selected, `p=0` only suppresses the section
    name, not the trailing separator. The naive `"30000/1001,".split("/")`
    gave `["30000", "1001,"]`, and `float("1001,")` crashed with
    ValueError on every real invocation of this function (it was dead
    code - never actually called - until the extract_frames cache-
    completeness fix started using it, which is how this was first
    exercised for real and caught). Stripping trailing punctuation from
    the denominator before parsing fixes it without depending on a
    different -of format."""
    out = subprocess.run(
        ["ffprobe", "-v", "error", "-select_streams", "v:0",
         "-show_entries", "stream=r_frame_rate", "-of", "csv=p=0", str(video_path)],
        capture_output=True, text=True, check=True,
    )
    num, den = out.stdout.strip().split("/")
    return float(num) / float(den.rstrip(","))


def probe_duration(video_path: Path) -> float:
    """Total stream duration in seconds - used when --duration-secs is
    omitted so a run covers the video from --start-secs to its actual end,
    instead of silently defaulting to a fixed clip length (see
    --duration-secs's help text: a real user complaint was every dip-search
    run only ever covering the first ~10 minutes of a match, because the
    old fixed 600s default was never overridden)."""
    out = subprocess.run(
        ["ffprobe", "-v", "error", "-show_entries", "format=duration",
         "-of", "csv=p=0", str(video_path)],
        capture_output=True, text=True, check=True,
    )
    return float(out.stdout.strip())


def extract_frames(video_path: Path, start_secs: float, duration_secs: float,
                    out_dir: Path) -> list[Path]:
    """Sequentially decode every frame in [start_secs, start_secs+duration_secs)
    to numbered JPEGs, one ffmpeg call (not one seek per frame like
    pick_training_frames.py's sparse sampling) - a dip search needs every
    consecutive frame, not spaced probes, so this is the one place this
    script intentionally does NOT reuse that script's extraction approach.

    A real bug found 2026-09-15: this function used to treat ANY existing
    f*.jpg files in out_dir as "already fully extracted" and return early,
    with no check that the count matched what the requested
    [start_secs, start_secs+duration_secs) window should actually contain.
    A user-initiated TaskStop on an earlier in-progress run (mid-extraction,
    at 4K/HEVC 10-bit this is CPU-bound and takes minutes per 300s of
    source) left a PARTIAL frame set on disk (754 of an expected ~8992).
    The next run on the same --out/--cache-dir silently reused those 754
    frames as if extraction were complete, and every downstream track/dip
    search ran on a tiny, truncated slice of the requested window with no
    error or warning - it reported "0 tracks found", which looked like a
    plausible (if disappointing) real result instead of the truncation bug
    it actually was. Sequential ffmpeg decode of hours of HEVC footage is
    exactly the kind of long-running step a user might reasonably interrupt
    (or that might die for any other reason - disk full, process killed),
    so silent partial-cache reuse is a real, recurring risk, not just a
    one-off tooling accident from this session.

    Fixed: the expected frame count for this window (ffprobe's stream fps
    x duration_secs, ceil'd) is written to `_expected_count.txt` next to
    the extracted frames on a successful full run. A cached dir is only
    reused if that count file exists AND matches the number of frames
    actually on disk - any mismatch (missing marker, wrong count) means
    stale/partial data, and the directory is cleared and fully
    re-extracted rather than trusted."""
    out_dir.mkdir(parents=True, exist_ok=True)
    marker = out_dir / "_expected_count.txt"
    existing = sorted(out_dir.glob("f*.jpg"))
    if existing and marker.exists():
        try:
            expected = int(marker.read_text(encoding="utf-8").strip())
        except ValueError:
            expected = -1
        if expected == len(existing):
            return existing
        print(f"  cached frames in {out_dir} incomplete/stale "
              f"({len(existing)} on disk, {expected} expected) - "
              f"re-extracting", flush=True)
    elif existing:
        print(f"  {len(existing)} cached frame(s) in {out_dir} have no "
              f"completeness marker (from before this check existed, or a "
              f"prior interrupted run) - re-extracting to be safe", flush=True)
    for f in existing:
        f.unlink()
    marker.unlink(missing_ok=True)

    fps = probe_fps(video_path)
    expected_count = math.ceil(duration_secs * fps)

    pattern = str(out_dir / "f%06d.jpg")
    subprocess.run(
        [
            "ffmpeg", "-y", "-v", "error",
            "-ss", str(start_secs), "-i", str(video_path),
            "-t", str(duration_secs),
            "-q:v", "1", "-vsync", "0",
            pattern,
        ],
        check=True,
    )
    result = sorted(out_dir.glob("f*.jpg"))
    if len(result) != expected_count:
        # ffmpeg exited 0 (check=True didn't fire) but produced a
        # different count than expected - e.g. the source is shorter than
        # start_secs+duration_secs, or a variable-frame-rate quirk. Not
        # necessarily wrong, but worth surfacing rather than silently
        # writing a marker that then papers over a real problem on the
        # NEXT run too.
        print(f"  NOTE: extracted {len(result)} frames, expected "
              f"~{expected_count} (fps={fps:.3f}) - if the source is "
              f"shorter than requested this is normal, otherwise "
              f"investigate before trusting results", flush=True)
    marker.write_text(str(len(result)), encoding="utf-8")
    return result


# Must match tile_yolo_dataset.py's own constants exactly - see that
# script's doc comment for why tiling exists at all (source frames are
# 3840x2880; a single square imgsz letterboxes ~25% away and scales
# 0.5x, tiling instead scales 0.67x with zero letterbox waste) and why
# it's fixed ("we trainen altijd op een vierkant van 1920 dat staat vast
# als een huis, mag ook niet wijzigen" - user, 2026-09-15).
TILE_SIZE = 1920
CROP_SIZE = 2880
ORIG_W = 3840
ORIG_H = 2880
TILES = [("L", 0), ("R", ORIG_W - CROP_SIZE)]


def score_frames(paths: list[Path], model_path: Path, imgsz: int, device: str) -> dict:
    """Run inference at a near-zero confidence floor (0.001) so a real dip
    below the production threshold is visible instead of silently filtered
    before this script ever sees it - same reasoning as
    compare_checkpoints_real_footage.py's --distribution mode. Keeps every
    detected box (not just ball) so kept frames can carry person/referee
    boxes into their label file too.

    A real bug found 2026-09-15, more significant than any prior fix in
    this file: this used to run model.predict() on the WHOLE, un-tiled
    3840x2880 frame at imgsz=1920 - ultralytics resizes that to 1920x1440
    (longest side to imgsz, aspect preserved, zero-padded to a square
    internally), a 0.5x scale with letterbox waste. But round7's
    checkpoint (like every checkpoint this project trains) was trained
    EXCLUSIVELY on tile_yolo_dataset.py's left/right 1920x1920 SQUARE
    tiles - each a 2880x2880 crop of the source resized to 1920x1920, a
    0.67x scale with NO letterbox waste (see that script's doc comment:
    an ~18px ball becomes ~9px full-frame vs ~12px tiled). Feeding the
    model a representation it was never trained on is a real train/
    inference mismatch, not just a resolution/speed tradeoff - confirmed
    via round7's own args.yaml (imgsz: 1920) and by inspecting actual
    training image dimensions (1920x1920, not 1920x1440). The user
    confirmed training always uses a fixed 1920x1920 square tile and that
    is NOT going to change - so this function must tile at inference
    time to match, not the other way around.

    Fixed: each source frame is split into the SAME two overlapping
    2880x2880 crops (`TILES`, matching tile_yolo_dataset.py's own
    constants exactly) and scored separately at imgsz=1920 (now always
    genuinely 1920x1920, square, no letterbox). Detections from each
    tile are transformed back to full-frame-normalized xywhn coordinates
    before being returned, so every downstream consumer of this
    function's output (build_ball_tracks, format_label_lines, etc.)
    keeps working in full-frame space unchanged. A ball-class candidate
    inside the L/R overlap band (x in [960, 2880] source pixels) can
    legitimately appear in BOTH tiles' detections - `_dedupe_tile_boxes`
    collapses near-duplicate same-class boxes from the two tiles into
    one (keeping the higher-confidence one) before returning, so
    build_ball_tracks doesn't see a phantom double-detection at every
    overlap-band position.

    Resize filter: Image.BILINEAR, NOT Image.LANCZOS (tile_yolo_dataset.py
    uses LANCZOS to build training data, and an earlier version of this
    fix matched that - measured 2-6x higher ball confidence with LANCZOS
    on round7's checkpoint). Reverted to BILINEAR 2026-09-15 once RECO's
    actual production inference paths were checked directly: EVERY one
    (wgpu_preprocess.rs's GPU compute shader, the CPU fallback in
    detectors/cpu.rs, and the TensorRT/NPP path in npp_interop.rs) uses
    bilinear resize, not Lanczos - so the training pipeline and RECO's
    own production detector already disagree on this, independently of
    anything in this file. This script's own doc comment claims to
    simulate "the SAME kind of raw... inference reco-cli ai-debug-raw
    uses" - matching PRODUCTION's filter is the correct choice for that
    stated goal, even though it scores lower confidence than LANCZOS
    would. User confirmed 2026-09-15: BILINEAR is now the standing
    choice for every future resize in this project's Python tooling too
    ("alles moet dan vanaf nu BILINEAR zijn") - the LANCZOS-vs-BILINEAR
    mismatch between tile_yolo_dataset.py and RECO's production code is
    a known, NOT-yet-resolved inconsistency (see
    project_ball_confidence_dip_tool.md), not something this file
    decides on its own.

    `device` is passed to ultralytics explicitly (never left to its own
    auto-selection) - a real question from the user mid-run ("gebeurt dit
    nu op de GPU?") showed relying on ultralytics' implicit device pick was
    the wrong call: this script never confirmed which device it landed on,
    only assumed it."""
    from ultralytics import YOLO
    from PIL import Image

    model = YOLO(str(model_path))
    print(f"  scoring {len(paths)} frames (2 tiles each) with {model_path.name} "
          f"on device={device} (names={model.names})", flush=True)
    out = {}
    for i, p in enumerate(paths, 1):
        im = Image.open(p)
        if im.size != (ORIG_W, ORIG_H):
            sys.exit(f"{p}: expected {ORIG_W}x{ORIG_H} source frame, got "
                      f"{im.size} - tiled inference assumes this project's "
                      f"fixed source resolution (see tile_yolo_dataset.py)")

        all_boxes = []
        for _tile_name, crop_x0 in TILES:
            crop = im.crop((crop_x0, 0, crop_x0 + CROP_SIZE, CROP_SIZE)) \
                     .resize((TILE_SIZE, TILE_SIZE), Image.BILINEAR)
            res = model.predict(crop, imgsz=imgsz, conf=0.001, device=device,
                                 verbose=False)[0]
            for b in res.boxes:
                tcx, tcy, tw, th = [float(v) for v in b.xywhn[0]]
                # tile-normalized -> tile-pixel -> full-frame-pixel -> full-frame-normalized
                px_cx = crop_x0 + tcx * CROP_SIZE
                px_cy = tcy * CROP_SIZE
                px_w = tw * CROP_SIZE
                px_h = th * CROP_SIZE
                all_boxes.append({
                    "cls": int(b.cls), "conf": float(b.conf),
                    "xywhn": [px_cx / ORIG_W, px_cy / ORIG_H,
                              px_w / ORIG_W, px_h / ORIG_H],
                })
        out[p.name] = _dedupe_tile_boxes(all_boxes)
        if i % 200 == 0:
            print(f"    {i}/{len(paths)}", flush=True)
    return out


def _dedupe_tile_boxes(boxes: list[dict], dist_thresh: float = 0.01) -> list[dict]:
    """Collapse near-duplicate same-class boxes that came from BOTH tiles
    of the L/R overlap band (source x in [960, 2880], present in both
    TILES' crops - see score_frames) into one, keeping the
    higher-confidence detection. Without this, every real object sitting
    in the overlap band would show up as two near-identical detections in
    the merged full-frame result, which build_ball_tracks would then see
    as two separate nearby candidates on the same frame - harmless for
    _closest_ball_near (picks the nearest one), but doubles anchor
    candidates and pollutes any future per-frame box counting. Boxes
    outside the overlap band are never close enough (in full-frame
    normalized space) to a same-class box from the other tile to
    collide, so this is safe to run unconditionally on every frame's
    combined box list rather than only on overlap-band boxes."""
    boxes = sorted(boxes, key=lambda b: -b["conf"])
    kept: list[dict] = []
    for b in boxes:
        if any(k["cls"] == b["cls"] and _dist(k["xywhn"], b["xywhn"]) <= dist_thresh
               for k in kept):
            continue
        kept.append(b)
    return kept


def _dist(a: list[float], b: list[float]) -> float:
    """Euclidean distance between two xywhn centers (a, b each [cx, cy, w, h] -
    only cx/cy matter here)."""
    return ((a[0] - b[0]) ** 2 + (a[1] - b[1]) ** 2) ** 0.5


def merge_spans(spans: list[tuple[int, int]], merge_gap_frames: int) -> list[tuple[int, int]]:
    """Merge (start, end_inclusive) spans that overlap or are within
    merge_gap_frames of each other into single, larger spans.

    A track with several in-track dips close together (e.g. a ball
    repeatedly grazing the confidence threshold while sitting near a
    field line - see find_confidence_dips_in_track) produces many short
    spans whose own +/-context_frames margins can overlap or abut,
    silently producing one long run of near-duplicate frames once the
    per-event sampling cap is applied independently to each - the cap
    only limits frames WITHIN one span, not across several that turn out
    to be adjacent. Merging BEFORE the cap is applied (see this
    function's caller) is what actually bounds a cluster of nearby dips
    to a small, evenly-sampled set, the way a labeler doing this by hand
    would treat "12 near-identical dips in the same 50 frames" as one
    thing to review, not 12."""
    if not spans:
        return []
    ordered = sorted(spans)
    merged = [list(ordered[0])]
    for start, end in ordered[1:]:
        if start - merged[-1][1] <= merge_gap_frames:
            merged[-1][1] = max(merged[-1][1], end)
        else:
            merged.append([start, end])
    return [(s, e) for s, e in merged]


def format_label_lines(boxes: list[dict], tracked_ball: dict | None,
                        other_class_conf: float = 0.25) -> tuple[str, list[float]]:
    """Build one frame's YOLO label file content - the fix for a real bug
    found by the user reviewing actual Label Studio output (task #2248,
    frame 232): a frame's raw `boxes` list is EVERY detection at a
    near-zero 0.001 confidence floor (see score_frames), a hundred-plus
    entries including dozens of low-confidence ball-class false
    positives (cones, line markings) that were never part of any
    confirmed track. Writing that raw list straight to a label file - as
    an earlier version of this script did - hands the labeler a frame
    covered in "ball" boxes that aren't the ball, which is worse than no
    pre-label at all: it looks like confirmed model output, not noise.

    The fix: ball-class (`BALL_CLS`) boxes are dropped entirely and
    replaced with AT MOST ONE box - `tracked_ball`, the position
    `build_ball_tracks` actually confirmed for this specific frame (via
    anchor + movement check + spatial continuity - see that function's
    doc comment). `tracked_ball=None` (this frame is in the track's
    surrounding --context-frames but not one the track was actually
    extended on) means no ball box at all is written - an honest "the
    confirmed track doesn't cover this exact frame" rather than
    guessing. Person/referee boxes are NOT run through the tracker (out
    of scope - this script only tracks the ball) but ARE filtered to
    `other_class_conf` (default 0.25, matching pick_training_frames.py's
    own --person-conf default) since the same near-zero floor problem
    applies to them, just never caused a *wrong-class* bug the way an
    untracked ball box did.

    Also returns a list of per-line confidences, same order as the label
    lines, for the CALLER to write to a separate `.conf` sidecar file
    (added 2026-09-15, see main()) - deliberately NOT appended as a 6th
    column in the .txt itself, since that file is standard 5-column YOLO
    format consumed by this project's actual training pipeline
    (ultralytics via train_class_weighted.py) - a 6th column risks
    breaking or being silently misinterpreted by that dataloader if one
    of these label files were ever used for training directly. The
    confidence value only matters for showing a human reviewer in Label
    Studio (see upload_to_labelstudio.py's --show-confidence), not for
    training itself."""
    lines = []
    confs = []
    for b in boxes:
        if b["cls"] != BALL_CLS and b["conf"] >= other_class_conf:
            lines.append(f"{b['cls']} {b['xywhn'][0]:.6f} {b['xywhn'][1]:.6f} "
                         f"{b['xywhn'][2]:.6f} {b['xywhn'][3]:.6f}")
            confs.append(b["conf"])
    if tracked_ball is not None:
        xywhn = tracked_ball["xywhn"]
        lines.append(f"{BALL_CLS} {xywhn[0]:.6f} {xywhn[1]:.6f} {xywhn[2]:.6f} {xywhn[3]:.6f}")
        confs.append(tracked_ball["conf"])
    return "\n".join(lines) + "\n", confs


def _closest_ball_near(boxes: list[dict], ref_xywhn: list[float], max_dist: float) -> dict | None:
    """Among this frame's ball-class candidates, the one closest to
    ref_xywhn - but only if it's within max_dist. Used only to CONTINUE
    an already-established track (see build_ball_tracks), never to start
    one - see that function's doc comment for why starting a track this
    way was the actual bug in an earlier version of this script."""
    balls = [b for b in boxes if b["cls"] == BALL_CLS]
    if not balls:
        return None
    candidate = min(balls, key=lambda b: _dist(b["xywhn"], ref_xywhn))
    return candidate if _dist(candidate["xywhn"], ref_xywhn) <= max_dist else None


def build_ball_tracks(paths: list[Path], dets: dict, anchor_conf: float,
                       max_track_dist: float = 0.05,
                       max_gap_frames: int = 3,
                       movement_check_frames: int = 15,
                       min_movement_dist: float = 0.01) -> list[dict]:
    """Find confirmed ball tracks and report where each one ends.

    # Two bugs this replaces (both hit on real footage, not just theory)

    1. An early version tracked "the ball" starting from whatever
       ball-class candidate happened to have the highest confidence in a
       frame with no established track - including a low-confidence
       false positive (a cone, a line marking, a sponsor board corner).
       Once "locked onto" that position, every later frame's nearby-
       candidate search kept re-finding the SAME static fixture and
       reported ITS confidence noise as a "dip". Fixed by only starting
       a track from a high-confidence anchor (`anchor_conf`, default 0.7)
       - see below.

    2. The `anchor_conf` fix alone was NOT enough: real testing (not
       just review) found the detector itself gives a stationary cone/
       pylon near the sideline a confidence HIGH ENOUGH to clear even a
       0.5 anchor bar (0.51 observed on real footage) - a genuine
       detector weakness, not a tooling bug, but one this script must
       still defend against. A pylon sits still; a ball in active play
       essentially never does for long. So an anchor is additionally
       required to actually MOVE: within `movement_check_frames` frames
       of the anchor, some reconnect must land at least
       `min_movement_dist` away from the anchor's own position. An
       anchor that fails this (near-zero movement for its whole early
       window, or no reconnect at all) is discarded outright - not
       demoted to a shorter track, thrown away entirely, since a
       stationary "ball" for that long is almost certainly not one.

    # The anchor + movement + spatial-continuity combination

    A track may ONLY begin at a frame where a ball-class candidate clears
    `anchor_conf`, AND that anchor must then be confirmed as moving (see
    above) before the track is accepted at all. Once accepted, the track
    is extended forward frame-by-frame via `_closest_ball_near` (nearest
    candidate within `max_track_dist`, regardless of that candidate's own
    confidence - once anchored+confirmed, a low-confidence detection of
    the SAME real ball is exactly the signal this tool wants to find, not
    a reason to drop it). `max_gap_frames` lets the track survive a
    handful of frames with NO candidate at all within range (a truly
    missed detection, not reconnecting to something else) before being
    considered ended.

    A track's end - the last frame it was actively extended on, before
    either the gap budget ran out or the scanned window ended - is the
    label-worthy moment: a confirmed real ball that the detector then
    failed to find nearby for a sustained stretch. This deliberately
    keeps "ball left the frame" and "track's end" as the same signal
    rather than trying to distinguish them the way an even earlier
    bracketed-dip approach did - once anchored AND movement-confirmed on
    a real ball, ANY sustained loss of it is worth showing a labeler,
    whether the ball is still on-screen (a detection failure) or has
    left (nothing to correct, but harmless to include - a human discards
    it in seconds, which costs far less than this tool silently
    manufacturing non-ball training frames again).

    Returns one dict per track: `anchor_idx` (where it started),
    `last_seen_idx` (last frame actively extended), `end_idx` (first
    frame with no reconnect - where the ball was confirmed missing), and
    `frames` (frame_idx -> {"xywhn": ..., "conf": ...} for every extended
    frame - the per-frame confidence readings let
    `find_confidence_dips_in_track` find low-confidence stretches INSIDE
    an otherwise-continuous track, e.g. a ball resting on a field line
    that the detector keeps finding, just with degraded confidence -
    without that safeguard being confused for the track actually ending)."""
    n = len(paths)
    box_lists = [dets.get(p.name, []) for p in paths]

    tracks = []
    i = 0
    while i < n:
        anchor = None
        for b in box_lists[i]:
            if b["cls"] == BALL_CLS and b["conf"] >= anchor_conf:
                if anchor is None or b["conf"] > anchor["conf"]:
                    anchor = b
        if anchor is None:
            i += 1
            continue

        # Movement confirmation: search forward (without yet committing to
        # a track) for a reconnect within movement_check_frames that has
        # moved at least min_movement_dist from the anchor. A stationary
        # false positive (see bug #2 above) never produces one and this
        # anchor frame is simply skipped, not retried - if it really is a
        # false positive, re-anchoring on it a frame later would just
        # repeat the same failure.
        ref = anchor["xywhn"]
        moved = False
        probe = i + 1
        while probe < min(n, i + 1 + movement_check_frames):
            hit = _closest_ball_near(box_lists[probe], ref, max_track_dist)
            if hit is not None and _dist(hit["xywhn"], anchor["xywhn"]) >= min_movement_dist:
                moved = True
                break
            probe += 1
        if not moved:
            i += 1
            continue

        # Extend the track forward from this confirmed, moving anchor.
        frames = {i: {"xywhn": ref, "conf": anchor["conf"]}}
        last_seen = i
        j = i + 1
        gap = 0
        while j < n:
            hit = _closest_ball_near(box_lists[j], ref, max_track_dist)
            if hit is not None:
                ref = hit["xywhn"]
                frames[j] = {"xywhn": ref, "conf": hit["conf"]}
                last_seen = j
                gap = 0
            else:
                gap += 1
                if gap > max_gap_frames:
                    break
            j += 1
        end_idx = last_seen + gap if last_seen + gap < n else n - 1

        tracks.append({
            "anchor_idx": i,
            "last_seen_idx": last_seen,
            "end_idx": end_idx,
            "frames": frames,
        })
        # Resume scanning for the NEXT track's anchor only after this
        # one's confirmed extent - re-anchoring inside an already-tracked
        # span would just re-discover the same ball.
        i = last_seen + 1

    return tracks


def find_confidence_dips_in_track(track: dict, conf_threshold: float,
                                   min_run_frames: int, max_run_frames: int) -> list[dict]:
    """Within one CONFIRMED track (see build_ball_tracks), find stretches
    where the tracked ball's own confidence drops below conf_threshold
    while the track itself stays alive (the detector keeps finding
    something within max_track_dist of the ball's last position, just at
    low confidence) - this is the ball-on-a-field-line scenario this
    script was originally built for: a real ball, confirmed by the
    track's anchor, that the detector still nominally sees but would not
    have surfaced at the production confidence floor.

    This is safe to do only because the caller already established that
    every frame in `track["frames"]` is spatially continuous with a
    confirmed anchor - unlike the very first version of this script,
    which computed confidence dips directly off unverified per-frame
    detections and could dip on a false positive's own confidence noise
    instead of the real ball's.

    A frame the track has no entry for at all (a genuine gap, inside
    max_gap_frames) counts as confidence 0 here - a true miss is at least
    as bad as a low-confidence hit, and should not silently disappear
    from the sweep.

    Returns one dict per bracketed dip within the track: `run_start`/
    `run_end` (frame indices, end exclusive), `run_len`, `before_conf`/
    `after_conf` (confidence just outside the dip), `min_conf_in_run`."""
    lo, hi = track["anchor_idx"], track["last_seen_idx"]
    frame_conf = [track["frames"].get(idx, {}).get("conf", 0.0) for idx in range(lo, hi + 1)]

    n = len(frame_conf)
    above = [c >= conf_threshold for c in frame_conf]

    dips = []
    i = 0
    while i < n:
        if above[i]:
            i += 1
            continue
        run_start = i
        while i < n and not above[i]:
            i += 1
        run_end = i
        run_len = run_end - run_start

        # Bracketed within the track: both sides need an above-threshold
        # frame inside this same confirmed track (not the very start/end
        # of the track itself, which is a different kind of event -
        # entering/leaving the tracked ball's confident zone, not a dip
        # inside it).
        has_before = run_start > 0
        has_after = run_end < n
        if has_before and has_after and min_run_frames <= run_len <= max_run_frames:
            dips.append({
                "run_start": lo + run_start,
                "run_end": lo + run_end,
                "run_len": run_len,
                "before_conf": frame_conf[run_start - 1],
                "after_conf": frame_conf[run_end],
                "min_conf_in_run": min(frame_conf[run_start:run_end]),
            })

    return dips


def main():
    p = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    p.add_argument("--video", type=Path, required=True, help="Raw single-camera source video")
    p.add_argument("--camera", choices=["left", "right"], required=True,
                   help="Camera label used in output paths/filenames")
    p.add_argument("--model", type=Path, required=True, help="Detector weights (.pt)")
    p.add_argument("--out", type=Path, required=True, help="Dataset output dir")
    p.add_argument("--cache-dir", type=Path, default=None,
                   help="Where extracted frames + detections are cached "
                        "(default: <out>/.cache) - keep it to re-scan for free "
                        "with different dip thresholds")
    p.add_argument("--start-secs", type=float, default=0.0)
    p.add_argument("--duration-secs", type=float, default=None,
                   help="Span to scan sequentially, starting at --start-secs. "
                        "Default: scan to the end of the video (probed via "
                        "ffprobe) - a full match in one run, so results "
                        "aren't silently limited to an arbitrary early clip "
                        "(a real problem with the old fixed 600s/10min "
                        "default: every run only ever covered the start of "
                        "the match unless this flag was remembered and set "
                        "by hand). Sequential decode + per-frame inference "
                        "is expensive - pass an explicit value to scan a "
                        "shorter, specific clip instead.")
    p.add_argument("--anchor-conf", type=float, default=0.7,
                   help="Confidence a ball-class candidate must clear to "
                        "START a new track (default 0.7 - well above the "
                        "usual 0.1-0.2 production floor). NOTE: this alone "
                        "is not sufficient - real footage showed a "
                        "stationary pylon/cone clearing even 0.5 - see "
                        "--movement-check-frames/--min-movement-dist and "
                        "build_ball_tracks's doc comment for the full "
                        "defense. Once anchored+movement-confirmed, a "
                        "track is extended by spatial proximity regardless "
                        "of the extending candidate's own confidence - low "
                        "confidence on an already-confirmed ball IS the "
                        "signal this tool looks for")
    p.add_argument("--movement-check-frames", type=int, default=15,
                   help="An anchor must have a reconnect moving at least "
                        "--min-movement-dist within this many frames, or "
                        "it's discarded as likely a stationary false "
                        "positive (a pylon/cone) rather than a real ball - "
                        "default 15 = 0.5s at 30fps, generous enough for a "
                        "ball that's briefly stationary at the anchor "
                        "moment itself (e.g. a dead-ball situation) to "
                        "still confirm once play resumes")
    p.add_argument("--min-movement-dist", type=float, default=0.01,
                   help="Normalized xywhn distance (default 0.01, ~1%% of "
                        "frame width/height) an anchor's reconnect must "
                        "move within --movement-check-frames to confirm "
                        "the anchor as a real, moving ball rather than a "
                        "stationary fixture")
    p.add_argument("--max-track-dist", type=float, default=0.05,
                   help="Max normalized xywhn distance (default 0.05, ~5%% "
                        "of frame width/height) a candidate may be from the "
                        "track's last position and still extend it - small "
                        "enough to reject a jump to an unrelated static "
                        "fixture, generous enough for a fast-moving ball "
                        "between frames at 30fps")
    p.add_argument("--max-gap-frames", type=int, default=3,
                   help="How many consecutive frames with no candidate in "
                        "range a track survives before counting as ended "
                        "(default 3 - a couple of genuinely missed "
                        "detections shouldn't end the track, but this stays "
                        "well below --min-track-frames so it can't "
                        "single-handedly manufacture a track)")
    p.add_argument("--min-track-frames", type=int, default=5,
                   help="Minimum frames a track must have been actively "
                        "extended over (anchor to last_seen, inclusive) to "
                        "be reported - filters out an anchor that "
                        "immediately lost its own reconnect, which usually "
                        "means the anchor frame itself was a fluke rather "
                        "than a real, trackable ball")
    p.add_argument("--conf-threshold", type=float, default=0.15,
                   help="Confidence floor a tracked ball's own detections "
                        "must clear inside an otherwise-continuous track "
                        "(default 0.15, close to reco-detect's documented "
                        "production default) - a stretch below this INSIDE "
                        "a confirmed track (e.g. a ball resting on a field "
                        "line, still nominally found but at low confidence) "
                        "is exported as its own event via "
                        "find_confidence_dips_in_track, in addition to a "
                        "track's own end")
    p.add_argument("--min-run-frames", type=int, default=1,
                   help="Minimum consecutive below-threshold frames inside "
                        "a track to count as a dip worth exporting")
    p.add_argument("--max-run-frames", type=int, default=15,
                   help="Maximum consecutive below-threshold frames inside "
                        "a track still counted as a dip rather than "
                        "something else going on (default 15 = 0.5s at "
                        "30fps)")
    p.add_argument("--end-window-frames", type=int, default=10,
                   help="For a track's own end (the ball was confirmed, "
                        "then lost), only frames within this many frames of "
                        "end_idx are eligible for export - default 10 "
                        "(~0.3s at 30fps). Without this, sampling spread "
                        "evenly across the WHOLE track (anchor to end) "
                        "mostly returns frames where the ball was already "
                        "confidently found, since a track can run for many "
                        "seconds before it's lost - a real user complaint "
                        "after reviewing actual Label Studio output: most "
                        "exported frames already had a correct ball "
                        "detection, not the loss moment being searched "
                        "for. This does not apply to in-track confidence "
                        "dips (find_confidence_dips_in_track), which are "
                        "already local to where confidence actually dropped.")
    p.add_argument("--context-frames", type=int, default=2,
                   help="Extra frames kept on each side of an exported "
                        "event's span, for temporal context when labeling")
    p.add_argument("--merge-gap-frames", type=int, default=10,
                   help="Events (a track's own end, or an in-track "
                        "confidence dip) whose gap from the previous "
                        "event's end is at most this many frames get "
                        "merged into one span BEFORE --max-frames-per-track "
                        "sampling is applied (default 10 = ~0.3s at 30fps) "
                        "- see merge_spans's doc comment: a track can have "
                        "many nearby in-track dips (e.g. a ball repeatedly "
                        "grazing the confidence threshold near a field "
                        "line) whose individual +/- --context-frames "
                        "margins would otherwise overlap into one long run "
                        "of near-duplicate frames, each capped "
                        "independently instead of as the one cluster they "
                        "really are")
    p.add_argument("--max-frames-per-track", type=int, default=12,
                   help="Cap on frames exported per merged span (its own "
                        "span plus --context-frames on each end) - a long "
                        "span would otherwise export near-duplicate frames "
                        "of the same ball; this samples evenly across the "
                        "span instead, always keeping both endpoints so "
                        "the labeler sees the transition into and out of "
                        "the ball being lost, not just the noisy middle")
    p.add_argument("--imgsz", type=int, default=1920)
    p.add_argument("--device", type=str, default=None,
                   help="Device passed to ultralytics (e.g. 'cuda:0', "
                        "'cpu'). Default: 'cuda:0' if torch reports CUDA "
                        "available, else 'cpu' - always resolved and "
                        "printed explicitly, never left to ultralytics' "
                        "own implicit auto-selection (see score_frames's "
                        "doc comment for why: this script previously never "
                        "confirmed which device it actually ran on).")
    p.add_argument("--reselect-only", action="store_true",
                   help="Skip extraction and inference, re-scan the cache "
                        "with different --anchor-conf/--max-track-dist/etc.")
    args = p.parse_args()

    cache_root = args.cache_dir or (args.out / ".cache")
    frames_dir = cache_root / "frames"
    dets_file = cache_root / "detections.json"
    cache_root.mkdir(parents=True, exist_ok=True)

    device = args.device
    if device is None:
        import torch
        device = "cuda:0" if torch.cuda.is_available() else "cpu"
    print(f"Using device={device} "
          f"({'CUDA available' if device.startswith('cuda') else 'CPU'})", flush=True)

    if args.reselect_only:
        paths = sorted(frames_dir.glob("f*.jpg"))
        if not paths:
            sys.exit(f"--reselect-only given but no cached frames in {frames_dir}")
        dets = json.loads(dets_file.read_text(encoding="utf-8"))
    else:
        duration_secs = args.duration_secs
        if duration_secs is None:
            total = probe_duration(args.video)
            duration_secs = total - args.start_secs
            if duration_secs <= 0:
                sys.exit(f"--start-secs={args.start_secs} is at or past the "
                          f"video's probed duration ({total:.1f}s)")
        print(f"Extracting frames from {args.video.name} "
              f"[{args.start_secs}s, {args.start_secs + duration_secs}s)...", flush=True)
        paths = extract_frames(args.video, args.start_secs, duration_secs, frames_dir)
        print(f"{len(paths)} frames extracted. Scoring...", flush=True)
        dets = score_frames(paths, args.model, args.imgsz, device)
        dets_file.write_text(json.dumps(dets), encoding="utf-8")

    tracks = build_ball_tracks(paths, dets, args.anchor_conf,
                                args.max_track_dist, args.max_gap_frames,
                                args.movement_check_frames, args.min_movement_dist)
    kept_tracks = [t for t in tracks
                   if t["last_seen_idx"] - t["anchor_idx"] + 1 >= args.min_track_frames]
    print(f"\nFound {len(tracks)} anchored ball track(s) "
          f"(anchor_conf>={args.anchor_conf}), {len(kept_tracks)} of them "
          f">= --min-track-frames={args.min_track_frames} frames long:")
    for t in kept_tracks:
        span = t["last_seen_idx"] - t["anchor_idx"] + 1
        print(f"  anchor={t['anchor_idx']} last_seen={t['last_seen_idx']} "
              f"end={t['end_idx']} (tracked {span} frame(s))")

    if not kept_tracks:
        print("\nNothing to export.")
        return

    # Two kinds of label-worthy event, both anchored on a CONFIRMED track
    # so neither can be a false-positive fixture (see build_ball_tracks's
    # doc comment): (a) the track's own end - the ball was confirmed, then
    # the detector stopped finding anything nearby; (b) a confidence dip
    # INSIDE an otherwise-continuous track - the ball stayed nominally
    # findable but dropped below the usable confidence floor for a
    # stretch (the field-line scenario this script was built for).
    #
    # Each kept frame index is mapped to the CONFIRMED track position for
    # that frame (when the track was actually extended there - a context
    # frame just outside a track's own span has no confirmed position,
    # `None`). format_label_lines uses this to keep only the verified
    # ball box, never every raw ball-class candidate in the frame - see
    # that function's doc comment for the real bug (a bad Label Studio
    # upload) this fixes.
    # Each event dict: span_start/span_end (inclusive), event_type
    # ("track_end" or "in_track_dip"), min_conf (the lowest confidence
    # seen in this event - for track_end, the last confirmed frame's own
    # confidence, since that's the last thing the detector was sure of
    # before losing the ball), and track_anchor_idx (which track this
    # event belongs to, for grouping). Added 2026-09-15 so a downstream
    # selector (e.g. select_top_frames.py) can prioritize by severity/type
    # without re-deriving them from build_ball_tracks - previously only
    # (span_start, span_end) tuples were kept, and min_conf/type only ever
    # existed as console print statements, not in dip_summary.json.
    events: list[dict] = []
    keep_indices: dict[int, dict | None] = {}
    for t in kept_tracks:
        # The track's own end is the loss moment we actually want, not the
        # whole anchor-to-end span - see --end-window-frames's help text.
        # Sampling the full span (an earlier version of this script did)
        # spends most of --max-frames-per-track on frames near the anchor,
        # where the ball was already confidently found, exactly the
        # opposite of what this tool is for. Clip to a window around
        # end_idx instead; never extend past anchor_idx (a short track can
        # have end_idx close to anchor_idx already).
        end_window_start = max(t["anchor_idx"], t["end_idx"] - args.end_window_frames)
        last_frame = t["frames"].get(t["last_seen_idx"], {})
        last_conf = last_frame.get("conf", 0.0)
        # xywhn: a representative ball position for this event, so a
        # downstream selector can do spatial clustering (e.g. "these 5
        # dips are all the same recurring spot on the field line") without
        # re-reading label files. track_end uses the last confirmed
        # position (the ball's last known spot before it was lost);
        # in_track_dip uses the position just before the dip started
        # (frame run_start-1, the closest confirmed position to the dip
        # itself - the dip's own frames have no confirmed position by
        # definition, see find_confidence_dips_in_track).
        track_events = [{
            "span_start": end_window_start, "span_end": t["end_idx"],
            "event_type": "track_end", "min_conf": last_conf,
            "track_anchor_idx": t["anchor_idx"],
            "xywhn": last_frame.get("xywhn"),
        }]
        for d in find_confidence_dips_in_track(t, args.conf_threshold,
                                                args.min_run_frames, args.max_run_frames):
            before_frame = t["frames"].get(d["run_start"] - 1, {})
            track_events.append({
                "span_start": d["run_start"], "span_end": d["run_end"] - 1,
                "event_type": "in_track_dip", "min_conf": d["min_conf_in_run"],
                "track_anchor_idx": t["anchor_idx"],
                "xywhn": before_frame.get("xywhn"),
            })
            print(f"  in-track dip: frames [{d['run_start']}, {d['run_end']}) "
                  f"min_conf={d['min_conf_in_run']:.2f}")
        events.extend(track_events)
        track_spans = [(e["span_start"], e["span_end"]) for e in track_events]
        merged_spans = merge_spans(track_spans, args.merge_gap_frames)
        if len(merged_spans) != len(track_spans):
            print(f"  merged this track's {len(track_spans)} event(s) into "
                  f"{len(merged_spans)} span(s) (--merge-gap-frames={args.merge_gap_frames})")
        for span_start, span_end in merged_spans:
            # Sample from frames the track actually confirmed a position
            # on (t["frames"], clipped to this span) FIRST, not blindly
            # across the whole [span_start, span_end] range - a track can
            # have small internal gaps (within --max-gap-frames) with no
            # confirmed position, and evenly sampling the raw index range
            # can land squarely on one of those, exporting a frame with no
            # ball box even though nearby frames in the same span have
            # one. A labeler reviewing this tool's output found exactly
            # that: ~28% of exported frames with no ball suggestion at
            # all, purely from unlucky sampling, not because the ball was
            # genuinely unconfirmed for that whole surrounding area.
            confirmed = sorted(idx for idx in t["frames"] if span_start <= idx <= span_end)
            if len(confirmed) > args.max_frames_per_track:
                # Evenly sample across the CONFIRMED frames only - see
                # --max-frames-per-track's help text. Always keeps both
                # endpoints so the labeler still sees the confirmed
                # anchor and the loss moment, not just the tracked
                # middle.
                step = (len(confirmed) - 1) / (args.max_frames_per_track - 1)
                confirmed = [confirmed[round(i * step)] for i in range(args.max_frames_per_track)]
            for idx in confirmed:
                keep_indices[idx] = t["frames"][idx]
            # Context frames are added separately, outside the sampling
            # above - these are the deliberately-unconfirmed margin
            # (before the anchor / after the loss) that lets a labeler
            # see the transition, not a sampling artifact; `None` here is
            # honest, not a bug.
            lo = max(0, span_start - args.context_frames)
            hi = min(len(paths), span_end + args.context_frames + 1)
            for idx in range(lo, span_start):
                keep_indices.setdefault(idx, t["frames"].get(idx))
            for idx in range(span_end + 1, hi):
                keep_indices.setdefault(idx, t["frames"].get(idx))

    args.out.mkdir(parents=True, exist_ok=True)
    (args.out / "classes.txt").write_text(
        "\n".join(f"{i} {n}" for i, n in enumerate(CLASS_NAMES)) + "\n", encoding="utf-8")
    img_dir = args.out / "images" / args.camera
    lbl_dir = args.out / "labels" / args.camera

    import shutil

    # Real bug found 2026-09-15: this used to just mkdir(exist_ok=True)
    # and write the current run's keep_indices on top of whatever was
    # already there - a re-run with a DIFFERENT keep_indices set (e.g.
    # after changing --anchor-conf, or after this file's own tiled-
    # inference fix changed which candidates get anchored at all) left
    # stale dip_*.jpg/.txt pairs from the PREVIOUS run sitting right next
    # to the new ones, silently mixed together with no way to tell which
    # is which by filename alone. Caught by the user asking to visually
    # review output: two label files 3 frames apart showed the SAME
    # tracked ball jumping to a completely different screen position,
    # which traced back to one file being ~3 hours older than its
    # neighbor - an untiled run's leftover output, not a real tracking
    # discontinuity. Fixed: wipe this run's own images/<camera> and
    # labels/<camera> dirs before writing, so a directory only ever
    # holds exactly one run's output. classes.txt/dip_summary.json/.cache
    # are untouched (already correctly overwritten in place, and .cache
    # is deliberately reused across runs, not a stale-output risk).
    if img_dir.exists():
        shutil.rmtree(img_dir)
    if lbl_dir.exists():
        shutil.rmtree(lbl_dir)
    img_dir.mkdir(parents=True, exist_ok=True)
    lbl_dir.mkdir(parents=True, exist_ok=True)

    for idx, tracked_ball in sorted(keep_indices.items()):
        path = paths[idx]
        boxes = dets.get(path.name, [])
        stem = f"dip_{path.stem}"
        shutil.copy2(path, img_dir / f"{stem}.jpg")
        label_text, confs = format_label_lines(boxes, tracked_ball)
        (lbl_dir / f"{stem}.txt").write_text(label_text, encoding="utf-8")
        # Confidence sidecar, one float per label line in the SAME order
        # - see format_label_lines's doc comment for why this isn't a
        # 6th .txt column. Only written when there's at least one label
        # line, matching the .txt's own "no ball box" honesty (an empty
        # .txt has no corresponding .conf either).
        if confs:
            (lbl_dir / f"{stem}.conf").write_text(
                "\n".join(f"{c:.6f}" for c in confs) + "\n", encoding="utf-8")

    (args.out / "dip_summary.json").write_text(json.dumps({
        "video": str(args.video),
        "camera": args.camera,
        "model": str(args.model),
        "anchor_conf": args.anchor_conf,
        "conf_threshold": args.conf_threshold,
        "track_count": len(tracks),
        "kept_track_count": len(kept_tracks),
        "tracks": [{"anchor_idx": t["anchor_idx"], "last_seen_idx": t["last_seen_idx"],
                    "end_idx": t["end_idx"]} for t in kept_tracks],
        "events": events,
        "frames_kept": len(keep_indices),
    }, indent=2), encoding="utf-8")

    print(f"\nKept {len(keep_indices)} frame(s) across {len(events)} event(s) "
          f"from {len(kept_tracks)} confirmed track(s) -> {args.out}")
    print("Reminder: these frames' label files hold the model's OWN prediction "
          "(often empty for the ball, by construction) - verify/correct every "
          "one in Label Studio, don't upload as ground truth as-is.")


if __name__ == "__main__":
    sys.exit(main())
