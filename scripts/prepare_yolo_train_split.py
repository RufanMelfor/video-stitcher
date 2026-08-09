#!/usr/bin/env python3
"""Turn an `export_yolo_labels.py` dataset (images/<camera>/*.jpg +
labels/<camera>/*.txt) into the flat train/val layout `ultralytics`
expects for `yolo train`, plus the `data.yaml` it needs to find them.

export_yolo_labels.py's own layout keeps images per-camera
(images/left/, images/right/) since detections are per-camera-frame, not
stitched-panorama space - useful for review (see
package_yolo_for_labelstudio.py) but ultralytics just wants one flat
images/ dir split into train/ and val/ subdirs, each mirrored under
labels/. This script does that flattening + split, camera-prefixing
filenames the same way package_yolo_for_labelstudio.py does to avoid the
same left/right basename-collision problem (both cameras produce
frame_0000000.jpg etc.).

Split is a simple deterministic slice (last N% of the sorted frame list
is val) rather than random shuffling - keeps repeated runs reproducible
without needing a seed, and for a small "rough first pass" dataset this
is more than adequate; revisit with a proper random/stratified split
once the training set is large enough for that to matter.

Usage:
  python3 prepare_yolo_train_split.py --dataset pilot_ojc_bgs_v2/dataset --out train_v2 --val-fraction 0.15
"""

import argparse
import shutil
import sys
from pathlib import Path


def main():
    p = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    p.add_argument("--dataset", type=Path, required=True, help="export_yolo_labels.py output dir")
    p.add_argument("--out", type=Path, required=True, help="Output dir for the ultralytics-ready dataset")
    p.add_argument("--val-fraction", type=float, default=0.15,
                   help="Fraction of images held out for validation (default: 0.15)")
    args = p.parse_args()

    classes_file = args.dataset / "classes.txt"
    if not classes_file.exists():
        sys.exit(f"No classes.txt found at {classes_file}")
    class_names = [line.split(maxsplit=1)[1] for line in classes_file.read_text().splitlines() if line.strip()]

    pairs = []  # (dest_stem, img_path, label_path)
    for camera_dir in sorted((args.dataset / "images").iterdir()):
        camera = camera_dir.name
        for img_path in sorted(camera_dir.glob("*.jpg")):
            label_path = args.dataset / "labels" / camera / f"{img_path.stem}.txt"
            if not label_path.exists():
                continue
            pairs.append((f"{camera}_{img_path.stem}", img_path, label_path))

    if not pairs:
        sys.exit(f"No image/label pairs found under {args.dataset}")

    # Deterministic split: sort by dest_stem, last val-fraction goes to val.
    pairs.sort(key=lambda t: t[0])
    n_val = max(1, round(len(pairs) * args.val_fraction))
    train_pairs = pairs[:-n_val]
    val_pairs = pairs[-n_val:]

    for split_name, split_pairs in (("train", train_pairs), ("val", val_pairs)):
        img_dir = args.out / "images" / split_name
        lbl_dir = args.out / "labels" / split_name
        img_dir.mkdir(parents=True, exist_ok=True)
        lbl_dir.mkdir(parents=True, exist_ok=True)
        for dest_stem, img_path, label_path in split_pairs:
            shutil.copy(img_path, img_dir / f"{dest_stem}.jpg")
            shutil.copy(label_path, lbl_dir / f"{dest_stem}.txt")

    data_yaml = args.out / "data.yaml"
    names_block = "\n".join(f"  {i}: {name}" for i, name in enumerate(class_names))
    data_yaml.write_text(
        f"path: {args.out.resolve()}\n"
        f"train: images/train\n"
        f"val: images/val\n"
        f"names:\n{names_block}\n",
        encoding="utf-8",
    )

    print(
        f"{len(train_pairs)} train / {len(val_pairs)} val images "
        f"({len(pairs)} total) -> {args.out}"
    )
    print(f"data.yaml written: {data_yaml}")


if __name__ == "__main__":
    main()
