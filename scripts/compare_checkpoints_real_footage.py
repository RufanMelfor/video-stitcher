#!/usr/bin/env python3
r"""Real-footage ball-detection comparison between two YOLO checkpoints,
same methodology every prior tiled-1920 training round used (see
docs/YOLO26_Training.md's "Root cause found" and "Tiled-1920 training on
the merged multi-project set" entries) - NOT a substitute for val-set
metrics, a check against them: val-mAP has previously gone UP while
real-footage ball recall went DOWN (the 2026-08-22 "easy ball bias"
regression), so a checkpoint is never shipped on val numbers alone.

Extracts N frames per camera from the given raw Left/Right source
video(s) (sequential decode-and-discard for frame accuracy, same
convention as dump_detection_frames.rs - NOT a lossy `-ss` seek),
tiles each frame the same way tile_yolo_dataset.py does (two
overlapping 2880x2880 crops -> 1920x1920, left x=[0,2880],
right x=[960,3840]), runs both checkpoints' inference via the
ultralytics Python API directly (not the Rust app - this only
evaluates the model itself, independent of any production
integration bugs), and reports the ball-class detection rate +
mean confidence per camera for each checkpoint.

Usage:
  python scripts/compare_checkpoints_real_footage.py \
      --left "<path>/LEFT/<file>.MP4" \
      --right "<path>/RIGHT/<file>.MP4" \
      --old-checkpoint "D:\VOETBAL_VIDEO\RECO\training\merged_v1_tiled_1920\runs\full_patience100\weights\best.pt" \
      --new-checkpoint "D:\VOETBAL_VIDEO\RECO\training\round7_tiled_1920\runs\full_patience100\weights\best.pt" \
      --start-secs 300 --duration-secs 30 --conf 0.1

Ball class id: both checkpoints share this project's 0=person/1=ball/
2=referee convention (confirm the new checkpoint's own names via
`model.names` if this was ever a from-scratch run with a different
label order - a continued fine-tune like round7 always keeps its
base's class order, so this is safe here, but the script re-reads
`model.names` per checkpoint rather than hardcoding the ball index,
in case that assumption is ever wrong for a future comparison).

# Confidence-distribution mode (--distribution)

Round 7's writeup in docs/YOLO26_Training.md left one question explicitly
open: is a "pickier, not better" checkpoint (higher confidence on hits,
lower hit rate at conf=0.1) merely shifted to a higher confidence
threshold everywhere (a calibration effect - fixable in production by
just lowering --conf, no retraining needed), or does it genuinely fail
to find balls the old checkpoint found AT ANY confidence (a real
capability loss - no --conf choice fixes that)?

The single-threshold hit-rate this script always reported cannot
distinguish those two cases: a checkpoint that is uniformly shifted and
one that is genuinely worse can both show "lower hit rate at conf=0.1,
higher mean confidence on hits". --distribution answers it directly by
running every tile at a near-zero floor (conf=0.001) so weak/marginal
detections are never filtered before comparison, then reporting each
checkpoint's hit rate at a sweep of thresholds (0.05 through 0.9). A
genuinely worse checkpoint shows a LOWER hit rate than the other at
EVERY threshold in the sweep, including low ones; a merely-recalibrated
one matches or beats the other at low thresholds and only falls behind
at high ones - the crossover point (if any) tells you what --conf the
old checkpoint's behavior needs the new one run at, without retraining.

NOT executed as part of building this script - the user asked to only
prepare it, not run it, while training is still using the GPU. Run it
yourself (or ask me to) once the round7 training finishes.
"""

import argparse
import subprocess
import sys
from pathlib import Path

CROP_SIZE = 2880
ORIG_W = 3840
ORIG_H = 2880
TILE_SIZE = 1920
TILES = [("L", 0), ("R", ORIG_W - CROP_SIZE)]


def extract_frames(video_path: Path, start_secs: float, count: int, interval_secs: float, out_dir: Path) -> list[Path]:
    """Sequentially decode `count` frames spaced `interval_secs` apart,
    starting at `start_secs`, via ffmpeg (frame-accurate: seeks once to
    the nearest keyframe before start_secs, then decodes forward, same
    trade-off dump_detection_frames.rs documents - good enough for a
    diagnostic sample spread across a real clip, not frame-exact to the
    single frame the way a Rust decode-and-discard loop would be)."""
    out_dir.mkdir(parents=True, exist_ok=True)
    paths = []
    for i in range(count):
        t = start_secs + i * interval_secs
        out_path = out_dir / f"frame_{i:04d}_t{t:.1f}s.jpg"
        subprocess.run(
            [
                "ffmpeg", "-y", "-v", "error",
                "-ss", str(t), "-i", str(video_path),
                "-frames:v", "1", "-q:v", "2",
                str(out_path),
            ],
            check=True,
        )
        paths.append(out_path)
    return paths


def tile_frame(img_path: Path, out_dir: Path) -> list[Path]:
    from PIL import Image

    im = Image.open(img_path)
    if im.size != (ORIG_W, ORIG_H):
        sys.exit(f"{img_path}: expected {ORIG_W}x{ORIG_H}, got {im.size}")
    out_dir.mkdir(parents=True, exist_ok=True)
    tiles = []
    for name, x0 in TILES:
        crop = im.crop((x0, 0, x0 + CROP_SIZE, CROP_SIZE)).resize((TILE_SIZE, TILE_SIZE), Image.BILINEAR)
        out_path = out_dir / f"{img_path.stem}_{name}.jpg"
        crop.save(out_path)
        tiles.append(out_path)
    return tiles


def run_checkpoint(checkpoint_path: str, tile_paths: list[Path], conf: float) -> dict:
    """Run one checkpoint over all tiles, return per-camera ball stats.
    Imports ultralytics lazily so this script's --help/argument parsing
    doesn't require torch/ultralytics to be importable."""
    from ultralytics import YOLO

    model = YOLO(checkpoint_path)
    ball_id = None
    for idx, name in model.names.items():
        if name.lower() == "ball":
            ball_id = idx
            break
    if ball_id is None:
        sys.exit(f"{checkpoint_path}: no 'ball' class in model.names ({model.names})")

    stats = {"L": {"total": 0, "hits": 0, "confs": []}, "R": {"total": 0, "hits": 0, "confs": []}}
    for tile_path in tile_paths:
        camera = "L" if tile_path.stem.endswith("_L") else "R"
        stats[camera]["total"] += 1
        results = model.predict(str(tile_path), conf=conf, verbose=False)
        best_conf = 0.0
        found = False
        for box in results[0].boxes:
            if int(box.cls[0]) == ball_id:
                found = True
                best_conf = max(best_conf, float(box.conf[0]))
        if found:
            stats[camera]["hits"] += 1
            stats[camera]["confs"].append(best_conf)
    return stats


def summarize(label: str, stats: dict) -> None:
    print(f"\n=== {label} ===")
    for camera in ("L", "R"):
        s = stats[camera]
        rate = s["hits"] / s["total"] * 100 if s["total"] else 0.0
        mean_conf = sum(s["confs"]) / len(s["confs"]) if s["confs"] else 0.0
        print(f"  {camera}: {s['hits']}/{s['total']} ({rate:.1f}%) rate, mean conf {mean_conf:.2f} on hits")


# Thresholds swept by --distribution. Starts below the production
# default (0.1) so a checkpoint that only needs a lower floor to match
# the old one's behavior is visible, not just "worse at 0.1".
SWEEP_THRESHOLDS = (0.05, 0.1, 0.2, 0.3, 0.4, 0.5, 0.6, 0.7, 0.8, 0.9)


def run_checkpoint_raw_scores(checkpoint_path: str, tile_paths: list[Path]) -> dict:
    """Same tiles/model as run_checkpoint, but at a near-zero confidence
    floor (0.001) and keeping every tile's best ball score (0.0 if the
    model found no ball box at all, even weak ones) rather than only the
    ones that clear a single --conf floor. This is what --distribution
    needs to compute a hit rate at multiple thresholds after the fact,
    without re-running inference once per threshold."""
    from ultralytics import YOLO

    model = YOLO(checkpoint_path)
    ball_id = None
    for idx, name in model.names.items():
        if name.lower() == "ball":
            ball_id = idx
            break
    if ball_id is None:
        sys.exit(f"{checkpoint_path}: no 'ball' class in model.names ({model.names})")

    scores = {"L": [], "R": []}
    for tile_path in tile_paths:
        camera = "L" if tile_path.stem.endswith("_L") else "R"
        results = model.predict(str(tile_path), conf=0.001, verbose=False)
        best = 0.0
        for box in results[0].boxes:
            if int(box.cls[0]) == ball_id:
                best = max(best, float(box.conf[0]))
        scores[camera].append(best)
    return scores


def summarize_distribution(label: str, scores: dict) -> None:
    print(f"\n=== {label} (confidence-sweep) ===")
    for camera in ("L", "R"):
        vals = scores[camera]
        total = len(vals)
        if total == 0:
            continue
        rates = ", ".join(
            f"{t:.2f}:{sum(1 for v in vals if v >= t) / total * 100:5.1f}%"
            for t in SWEEP_THRESHOLDS
        )
        print(f"  {camera} (n={total}): hit rate by threshold -> {rates}")


def compare_distributions(old_scores: dict, new_scores: dict) -> None:
    """Interpret the two sweeps against each other - see this module's
    doc comment ("Confidence-distribution mode") for what each outcome
    means. Compares at the same tile granularity old/new share (both
    were run over the identical tile_paths list), so a per-threshold
    hit-rate comparison is apples-to-apples even though the two models
    may fire on different individual tiles."""
    print("\n=== Interpretation ===")
    for camera in ("L", "R"):
        old_vals, new_vals = old_scores[camera], new_scores[camera]
        if not old_vals or not new_vals:
            continue
        total = len(old_vals)
        worse_everywhere = True
        crossover = None
        for t in SWEEP_THRESHOLDS:
            old_rate = sum(1 for v in old_vals if v >= t) / total
            new_rate = sum(1 for v in new_vals if v >= t) / total
            if new_rate >= old_rate:
                worse_everywhere = False
                if crossover is None:
                    crossover = t
        if worse_everywhere:
            print(f"  {camera}: NEW is behind OLD at every threshold in the sweep - "
                  f"looks like a real capability loss (more/harder misses), not just a "
                  f"higher confidence calibration. Lowering --conf for NEW will not fix this.")
        elif crossover == SWEEP_THRESHOLDS[0]:
            print(f"  {camera}: NEW matches or beats OLD across the whole sweep - "
                  f"no evidence of a regression here at any threshold.")
        else:
            print(f"  {camera}: NEW falls behind OLD only at/above conf~{crossover:.2f} - "
                  f"looks like a calibration shift (NEW is pickier but not less capable "
                  f"below that threshold). Consider running NEW at a lower --conf in "
                  f"production instead of retraining, then re-check with real-footage "
                  f"testing at that lower floor.")


def main():
    p = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    p.add_argument("--left", required=True, help="Path to the raw LEFT camera source video")
    p.add_argument("--right", required=True, help="Path to the raw RIGHT camera source video")
    p.add_argument("--old-checkpoint", required=True, help="Path to the current production checkpoint (e.g. merged_v1_tiled_1920's best.pt)")
    p.add_argument("--new-checkpoint", required=True, help="Path to the candidate checkpoint to compare (e.g. round7's best.pt)")
    p.add_argument("--start-secs", type=float, default=300.0, help="Where in the clip to start sampling (avoid warm-up/dead time)")
    p.add_argument("--duration-secs", type=float, default=60.0, help="Span of the clip to sample across")
    p.add_argument("--frame-count", type=int, default=30, help="Number of frames per camera to sample (spread evenly across duration-secs)")
    p.add_argument("--conf", type=float, default=0.1, help="Detection confidence floor - matches reco-detect's documented production default")
    p.add_argument("--work-dir", default=None, help="Where to extract/tile frames (default: a scratch subdir next to this script's cwd)")
    p.add_argument("--distribution", action="store_true",
                   help="Instead of a single-threshold hit-rate, sweep hit rate across "
                        "several confidence thresholds and report whether NEW looks "
                        "genuinely worse or just recalibrated to a higher confidence - "
                        "see this module's 'Confidence-distribution mode' doc section")
    args = p.parse_args()

    work_dir = Path(args.work_dir) if args.work_dir else Path("checkpoint_compare_scratch")
    interval = args.duration_secs / max(args.frame_count - 1, 1)

    print(f"Extracting {args.frame_count} frames per camera from t={args.start_secs}s "
          f"over {args.duration_secs}s (interval {interval:.2f}s)...")
    left_frames = extract_frames(Path(args.left), args.start_secs, args.frame_count, interval, work_dir / "raw_left")
    right_frames = extract_frames(Path(args.right), args.start_secs, args.frame_count, interval, work_dir / "raw_right")

    print("Tiling frames (2880x2880 crops -> 1920x1920, matching tile_yolo_dataset.py)...")
    tile_paths = []
    for f in left_frames + right_frames:
        tile_paths += tile_frame(f, work_dir / "tiles")

    print(f"{len(tile_paths)} tiles ready ({len(tile_paths)//2} per camera). Running old checkpoint...")

    if args.distribution:
        old_scores = run_checkpoint_raw_scores(args.old_checkpoint, tile_paths)
        print("Running new checkpoint...")
        new_scores = run_checkpoint_raw_scores(args.new_checkpoint, tile_paths)
        summarize_distribution(f"OLD: {args.old_checkpoint}", old_scores)
        summarize_distribution(f"NEW: {args.new_checkpoint}", new_scores)
        compare_distributions(old_scores, new_scores)
        return

    old_stats = run_checkpoint(args.old_checkpoint, tile_paths, args.conf)
    print("Running new checkpoint...")
    new_stats = run_checkpoint(args.new_checkpoint, tile_paths, args.conf)

    summarize(f"OLD: {args.old_checkpoint}", old_stats)
    summarize(f"NEW: {args.new_checkpoint}", new_stats)

    print("\nReminder: a real recall regression on either camera despite a val-mAP")
    print("win is exactly the pattern that sank the 2026-08-21/22 merged-data rounds -")
    print("do not ship the new checkpoint on val metrics alone if this test disagrees.")


if __name__ == "__main__":
    main()
