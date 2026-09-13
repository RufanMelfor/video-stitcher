#!/usr/bin/env python3
"""SAHI-style left/right tiled dataset builder - turns a flat
ultralytics-ready split (images/{train,val} + labels/{train,val}, this
project's 0=person/1=ball/2=referee scheme) into left/right 1920x1920
tiles.

First built as a one-off scratchpad script for the 2026-08-15/16
overnight tiled-training session (round4 only, 258 images -> 516
tiles) - see SESSION_HANDOFF.md's "SAHI-style left/right tiled
training" entry for the full methodology writeup and the ~50%
relative ball recall/mAP50-95 win it measured over the non-tiled 1920
approach. Committed properly this time since it's being reused for a
second, larger round instead of staying a throwaway.

Why tiling at all: this project's source frames are 3840x2880 (4:3).
Fitting that into a single square imgsz (e.g. 1920x1920) letterboxes
~25% of the canvas away and scales the whole frame down by 0.5x (an
~18px ball becomes ~9px). Splitting into two overlapping 2880x2880
crops (left x=[0,2880], right x=[960,3840], a 1920px overlap band so
nothing near the seam is tile-orphaned) and resizing each to 1920x1920
instead only scales by 0.67x (that same ball stays ~12px) with zero
letterbox waste.

Each source image becomes two tiles; a label's box counts for a tile
if its *center* falls inside that tile's x-range (so overlap-band
objects legitimately appear in both tiles), clipped to the tile's
pixel bounds otherwise (a box that's exactly on the tile edge would
otherwise report >1.0-normalized or negative width). A tile keeps its
source image's train/val split - shuffling L/R tiles of the same frame
into different splits would leak near-identical content across the
split.

**Consumes a tiled checkpoint requires tiled inference at runtime**
(split each live frame the same way, run detection on both crops,
merge results) - not wired into any production path yet as of
2026-08-21 (see docs/YOLO26_Training.md / SESSION_HANDOFF.md). This script
only prepares the training data.

Usage:
  python3 tile_yolo_dataset.py --in training/merged_v1 --out training/merged_v1_tiled_1920
"""

import argparse
import sys
from pathlib import Path

from PIL import Image

TILE_SIZE = 1920
CROP_SIZE = 2880  # matches the source frame's own height - a square crop
ORIG_W = 3840
ORIG_H = 2880
# (name, crop_x0) - crop is [crop_x0, crop_x0 + CROP_SIZE) x [0, CROP_SIZE)
TILES = [("L", 0), ("R", ORIG_W - CROP_SIZE)]


def transform_box(cx: float, cy: float, w: float, h: float, crop_x0: int) -> tuple[float, float, float, float] | None:
    """Normalized (0..1, relative to ORIG_W x ORIG_H) box -> normalized
    (0..1, relative to one CROP_SIZE x CROP_SIZE tile) box, or None if
    the box's center doesn't fall in this tile."""
    abs_cx = cx * ORIG_W
    abs_cy = cy * ORIG_H
    abs_w = w * ORIG_W
    abs_h = h * ORIG_H
    crop_x1 = crop_x0 + CROP_SIZE
    if not (crop_x0 <= abs_cx <= crop_x1):
        return None

    x0 = max(abs_cx - abs_w / 2, crop_x0)
    x1 = min(abs_cx + abs_w / 2, crop_x1)
    y0 = max(abs_cy - abs_h / 2, 0.0)
    y1 = min(abs_cy + abs_h / 2, float(CROP_SIZE))
    new_w = x1 - x0
    new_h = y1 - y0
    if new_w <= 0 or new_h <= 0:
        return None

    new_cx = (x0 + x1) / 2 - crop_x0
    new_cy = (y0 + y1) / 2
    return (new_cx / CROP_SIZE, new_cy / CROP_SIZE, new_w / CROP_SIZE, new_h / CROP_SIZE)


def tile_one_image(img_path: Path, label_path: Path, out_img_dir: Path, out_lbl_dir: Path) -> int:
    im = Image.open(img_path)
    if im.size != (ORIG_W, ORIG_H):
        sys.exit(f"{img_path}: expected {ORIG_W}x{ORIG_H}, got {im.size} - this script assumes a uniform source resolution")

    lines = label_path.read_text().splitlines() if label_path.exists() else []
    boxes = []
    for line in lines:
        if not line.strip():
            continue
        parts = line.split()
        boxes.append((int(parts[0]), float(parts[1]), float(parts[2]), float(parts[3]), float(parts[4])))

    tiles_written = 0
    for tile_name, crop_x0 in TILES:
        crop = im.crop((crop_x0, 0, crop_x0 + CROP_SIZE, CROP_SIZE)).resize((TILE_SIZE, TILE_SIZE), Image.LANCZOS)
        out_lines = []
        for cls, cx, cy, w, h in boxes:
            transformed = transform_box(cx, cy, w, h, crop_x0)
            if transformed is None:
                continue
            out_lines.append(f"{cls} {transformed[0]:.6f} {transformed[1]:.6f} {transformed[2]:.6f} {transformed[3]:.6f}")

        stem = f"{img_path.stem}_{tile_name}"
        crop.save(out_img_dir / f"{stem}.jpg", quality=95)
        (out_lbl_dir / f"{stem}.txt").write_text("\n".join(out_lines) + ("\n" if out_lines else ""), encoding="utf-8")
        tiles_written += 1
    return tiles_written


def main():
    p = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    p.add_argument("--in", dest="in_dir", type=Path, required=True, help="Flat dataset dir (images/{train,val}, labels/{train,val})")
    p.add_argument("--out", type=Path, required=True)
    args = p.parse_args()

    class_names = ["person", "ball", "referee"]  # this project's fixed scheme

    total_images = 0
    total_tiles = 0
    for split in ("train", "val"):
        img_dir = args.in_dir / "images" / split
        lbl_dir = args.in_dir / "labels" / split
        if not img_dir.is_dir():
            continue
        out_img_dir = args.out / "images" / split
        out_lbl_dir = args.out / "labels" / split
        out_img_dir.mkdir(parents=True, exist_ok=True)
        out_lbl_dir.mkdir(parents=True, exist_ok=True)
        split_images = 0
        split_tiles = 0
        for img_path in sorted(img_dir.glob("*.jpg")):
            label_path = lbl_dir / f"{img_path.stem}.txt"
            split_tiles += tile_one_image(img_path, label_path, out_img_dir, out_lbl_dir)
            split_images += 1
        total_images += split_images
        total_tiles += split_tiles
        print(f"{split}: {split_images} source images -> {split_tiles} tiles")

    data_yaml = args.out / "data.yaml"
    names_block = "\n".join(f"  {i}: {name}" for i, name in enumerate(class_names))
    data_yaml.write_text(
        f"path: {args.out.resolve()}\n"
        f"train: images/train\n"
        f"val: images/val\n"
        f"names:\n{names_block}\n",
        encoding="utf-8",
    )
    print(f"\n{total_images} source images -> {total_tiles} tiles written to {args.out}")
    print(f"data.yaml: {data_yaml}")


if __name__ == "__main__":
    main()
