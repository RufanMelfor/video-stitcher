#!/usr/bin/env python3
"""Flatten a `export_yolo_labels.py` dataset (images/<camera>/*.jpg +
labels/<camera>/*.txt) into the plain layout `label-studio-converter
import yolo` expects, then optionally run that converter to produce the
Label Studio task-import JSON.

label-studio-converter's YOLO importer (see
label_studio_converter/imports/yolo.py) wants a single flat directory:

    <out>/images/*.jpg
    <out>/labels/*.txt      # same basename as the matching image
    <out>/classes.txt       # one class name per line, line index = class id

This differs from our per-camera dataset layout in two ways: images live
under images/<camera>/ (would collide by basename once flattened - left
and right both produce frame_0000000.jpg etc.), and our classes.txt has
an "id name" prefix per line (this codebase's own convention, see
export_yolo_labels.py) rather than just the bare name the LS importer
expects. This script fixes both.

The actual `label-studio-converter import yolo` step needs
--image-root-url, which depends on how the target Label Studio instance
serves local files (LOCAL_FILES_DOCUMENT_ROOT) - not decided yet as of
writing (the Raspberry Pi this will run on isn't set up). Pass
--image-root-url once that's known; without it this script only does the
flatten/rename step and prints the command to run later.

Usage:
  python3 package_yolo_for_labelstudio.py --dataset pilot_ojc_bgs/dataset --out ls_import
  # once the Pi/Label Studio path convention is known:
  python3 package_yolo_for_labelstudio.py --dataset pilot_ojc_bgs/dataset --out ls_import \\
      --image-root-url "/data/local-files/?d=pilot_ojc_bgs/images" --run-converter
"""

import argparse
import shutil
import subprocess
import sys
from pathlib import Path


def main():
    p = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    p.add_argument("--dataset", type=Path, required=True, help="export_yolo_labels.py output dir")
    p.add_argument("--out", type=Path, required=True, help="Output dir for the flattened images/labels/classes.txt")
    p.add_argument("--image-root-url", default=None,
                   help="Label Studio local-files URL prefix, e.g. '/data/local-files/?d=<subpath>' "
                        "(depends on the target instance's LOCAL_FILES_DOCUMENT_ROOT - leave unset "
                        "to skip running the converter and just prepare the flattened directory)")
    p.add_argument("--run-converter", action="store_true",
                    help="Also invoke `label-studio-converter import yolo` (requires --image-root-url "
                         "and the label-studio-converter package installed)")
    p.add_argument("--to-name", default="image", help="Label Studio labeling-config object name (default: image)")
    p.add_argument("--from-name", default="label", help="Label Studio labeling-config control tag name (default: label)")
    args = p.parse_args()

    if args.run_converter and not args.image_root_url:
        p.error("--run-converter needs --image-root-url")

    classes_file = args.dataset / "classes.txt"
    class_names = [line.split(maxsplit=1)[1] for line in classes_file.read_text().splitlines() if line.strip()]

    images_dir = args.out / "images"
    labels_dir = args.out / "labels"
    images_dir.mkdir(parents=True, exist_ok=True)
    labels_dir.mkdir(parents=True, exist_ok=True)

    count = 0
    for camera_dir in sorted((args.dataset / "images").iterdir()):
        camera = camera_dir.name
        for img_path in sorted(camera_dir.glob("*.jpg")):
            label_path = args.dataset / "labels" / camera / f"{img_path.stem}.txt"
            dest_stem = f"{camera}_{img_path.stem}"
            shutil.copy(img_path, images_dir / f"{dest_stem}.jpg")
            shutil.copy(label_path, labels_dir / f"{dest_stem}.txt")
            count += 1

    # Label Studio's YOLO importer reads classes.txt as one bare name per
    # line (index = class id) - no "id name" prefix like our own convention.
    (args.out / "classes.txt").write_text("\n".join(class_names) + "\n", encoding="utf-8")

    print(f"{count} images flattened into {args.out} (classes: {', '.join(class_names)})")

    converter_cmd = [
        "label-studio-converter", "import", "yolo",
        "-i", str(args.out),
        "-o", str(args.out / "tasks.json"),
        "--to-name", args.to_name,
        "--from-name", args.from_name,
        "--out-type", "predictions",
    ]
    if args.image_root_url:
        converter_cmd += ["--image-root-url", args.image_root_url]

    if args.run_converter:
        print("Running:", " ".join(converter_cmd))
        subprocess.run(converter_cmd, check=True)
        print(f"Label Studio task JSON ready: {args.out / 'tasks.json'}")
    else:
        print("\nNot run yet (needs --image-root-url matching the target Label Studio "
              "instance's local-files serving config). Command to run once that's known:")
        print(" ", " ".join(converter_cmd))


if __name__ == "__main__":
    main()
