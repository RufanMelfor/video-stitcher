#!/usr/bin/env python3
r"""Real-footage ball-detection comparison between two YOLO checkpoints,
same methodology every prior tiled-1920 training round used (see
YOLO26_Training.md's "Root cause found" and "Tiled-1920 training on
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
