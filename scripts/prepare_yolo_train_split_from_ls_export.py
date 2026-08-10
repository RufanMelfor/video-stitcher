#!/usr/bin/env python3
"""Turn a Label Studio YOLO-format export (labels/*.txt + classes.txt,
no images - LS's export endpoint doesn't bundle image bytes) plus the
matching flat image set already uploaded to LS (see
package_yolo_for_labelstudio.py's `ls_flat/images/` output) into the
flat train/val layout `ultralytics` expects.

Unlike prepare_yolo_train_split.py (which reads export_yolo_labels.py's
per-camera images/<camera>/ layout), this script's input is already
flat and camera-prefixed (LS tasks were uploaded with left_/right_
filenames to avoid the basename-collision problem) - no per-camera
directory structure to flatten.

Also does a class remap: LS's soccana-derived label set is
0=ball, 1=person, 2=referee. This project's convention (matching
export_yolo_labels.py) is 0=person, 1=ball, 2=referee - reordered to
put referee last but otherwise kept as its own class (NOT folded into
person - referee is a real, separately-corrected class in the human
review, 141 instances in the "Finetuned yolo26n (rough v1)" project as
of 2026-08-10, more than ball's 123. An earlier version of this script
folded referee into person; every training round before rough_v7 was
trained without it as a result). Default mapping below encodes the
reorder; override with --class-map if the LS export's classes.txt ever
differs.

Usage:
  python3 prepare_yolo_train_split_from_ls_export.py \
      --ls-export finetuned_yolo26n_roughv1_ls_export \
      --images finetuned_n_preds/ls_flat/images \
      --out finetuned_yolo26n_roughv1_train \
      --val-fraction 0.15
"""

import argparse
import shutil
import sys
from pathlib import Path

# LS class index -> our class index. person=0, ball=1, referee=2 (kept
# as its own class, not folded into person - see the module docstring).
DEFAULT_CLASS_MAP = {0: 1, 1: 0, 2: 2}  # ball->1, person->0, referee->2
OUR_CLASS_NAMES = ["person", "ball", "referee"]


def remap_label_file(src: Path, dst: Path, class_map: dict[int, int]) -> None:
    lines_out = []
    for line in src.read_text().splitlines():
        if not line.strip():
            continue
        parts = line.split()
        old_cls = int(parts[0])
        if old_cls not in class_map:
            sys.exit(f"{src}: unknown class id {old_cls}, no mapping given")
        parts[0] = str(class_map[old_cls])
        lines_out.append(" ".join(parts))
    dst.write_text("\n".join(lines_out) + ("\n" if lines_out else ""), encoding="utf-8")


def main():
    p = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    p.add_argument("--ls-export", type=Path, required=True, help="Extracted LS YOLO export dir (has labels/, classes.txt)")
    p.add_argument("--images", type=Path, required=True, help="Flat dir of matching .jpg images (same stems as the label .txt files)")
    p.add_argument("--out", type=Path, required=True, help="Output dir for the ultralytics-ready dataset")
    p.add_argument("--val-fraction", type=float, default=0.15)
    args = p.parse_args()

    labels_dir = args.ls_export / "labels"
    if not labels_dir.is_dir():
        sys.exit(f"No labels/ dir found at {labels_dir}")

    pairs = []  # (stem, img_path, label_path)
    for label_path in sorted(labels_dir.glob("*.txt")):
        img_path = args.images / f"{label_path.stem}.jpg"
        if not img_path.exists():
            print(f"WARNING: no matching image for {label_path.name}, skipping", file=sys.stderr)
            continue
        pairs.append((label_path.stem, img_path, label_path))

    if not pairs:
        sys.exit("No image/label pairs matched - check --images points at the right flat image set")

    pairs.sort(key=lambda t: t[0])
    n_val = max(1, round(len(pairs) * args.val_fraction))
    train_pairs = pairs[:-n_val]
    val_pairs = pairs[-n_val:]

    for split_name, split_pairs in (("train", train_pairs), ("val", val_pairs)):
        img_dir = args.out / "images" / split_name
        lbl_dir = args.out / "labels" / split_name
        img_dir.mkdir(parents=True, exist_ok=True)
        lbl_dir.mkdir(parents=True, exist_ok=True)
        for stem, img_path, label_path in split_pairs:
            shutil.copy(img_path, img_dir / f"{stem}.jpg")
            remap_label_file(label_path, lbl_dir / f"{stem}.txt", DEFAULT_CLASS_MAP)

    data_yaml = args.out / "data.yaml"
    names_block = "\n".join(f"  {i}: {name}" for i, name in enumerate(OUR_CLASS_NAMES))
    data_yaml.write_text(
        f"path: {args.out.resolve()}\n"
        f"train: images/train\n"
        f"val: images/val\n"
        f"names:\n{names_block}\n",
        encoding="utf-8",
    )

    print(f"{len(train_pairs)} train / {len(val_pairs)} val pairs written to {args.out}")
    print(f"data.yaml: {data_yaml}")


if __name__ == "__main__":
    main()
