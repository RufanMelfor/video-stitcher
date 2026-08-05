#!/usr/bin/env python3
"""Package a `export_yolo_labels.py` dataset (images/<camera>/*.jpg +
labels/<camera>/*.txt) into CVAT's classic "YOLO 1.1" import format
(obj.data + obj.names + train.txt + obj_train_data/), so it can be loaded
straight into a CVAT task for human review.

CVAT's YOLO 1.1 importer (datumaro's `YoloBase`, see
cvat/apps/dataset_manager/formats/yolo.py) expects:
  obj.data          - "classes = N", "train = data/train.txt", "names = data/obj.names"
  obj.names         - class names, one per line, index = line number
  train.txt         - one relative image path per line
  obj_train_data/   - images + matching .txt labels (same basename)

Usage:
  python3 package_yolo_for_cvat.py --dataset pilot_ojc_bgs/dataset --out cvat_import.zip
"""

import argparse
import shutil
import zipfile
from pathlib import Path


def main():
    p = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    p.add_argument("--dataset", type=Path, required=True, help="export_yolo_labels.py output dir")
    p.add_argument("--out", type=Path, required=True, help="Output zip path")
    args = p.parse_args()

    classes_file = args.dataset / "classes.txt"
    class_names = [line.split(maxsplit=1)[1] for line in classes_file.read_text().splitlines() if line.strip()]

    staging = args.out.parent / (args.out.stem + "_staging")
    if staging.exists():
        shutil.rmtree(staging)
    data_dir = staging / "obj_train_data"
    data_dir.mkdir(parents=True)

    train_lines = []
    for camera_dir in sorted((args.dataset / "images").iterdir()):
        camera = camera_dir.name
        for img_path in sorted(camera_dir.glob("*.jpg")):
            label_path = args.dataset / "labels" / camera / f"{img_path.stem}.txt"
            dest_stem = f"{camera}_{img_path.stem}"
            shutil.copy(img_path, data_dir / f"{dest_stem}.jpg")
            shutil.copy(label_path, data_dir / f"{dest_stem}.txt")
            train_lines.append(f"data/obj_train_data/{dest_stem}.jpg")

    (staging / "obj.names").write_text("\n".join(class_names) + "\n", encoding="utf-8")
    (staging / "train.txt").write_text("\n".join(train_lines) + "\n", encoding="utf-8")
    (staging / "obj.data").write_text(
        f"classes = {len(class_names)}\ntrain = data/train.txt\nnames = data/obj.names\n",
        encoding="utf-8",
    )

    with zipfile.ZipFile(args.out, "w", zipfile.ZIP_DEFLATED) as zf:
        for file in staging.rglob("*"):
            if file.is_file():
                zf.write(file, file.relative_to(staging))

    shutil.rmtree(staging)
    print(f"{len(train_lines)} images packaged into {args.out} (classes: {', '.join(class_names)})")


if __name__ == "__main__":
    main()
