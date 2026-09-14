#!/usr/bin/env python3
"""Merge multiple sources into one ultralytics-ready train/val split -
one global shuffle across every source's pooled images, not a separate
split per source (which would bias val toward whichever source runs
last and under-represent small sources in val entirely).

Two kinds of source, given as repeatable --raw-source/--ready-source:

  --raw-source NAME=EXPORT_DIR;IMAGES_DIR
      A raw Label Studio YOLO export (labels/*.txt + classes.txt, no
      image bytes - see prepare_yolo_train_split_from_ls_export.py's
      docstring) plus its matching flat image dir. Separated by a
      semicolon, not a colon - a Windows drive letter (e.g. "C:\") would
      otherwise split in the wrong place. Remapped from LS's
      soccana-derived class order (0=ball, 1=person, 2=referee) to this
      project's (0=person, 1=ball, 2=referee) - same DEFAULT_CLASS_MAP,
      kept in sync manually since duplicating one 3-line dict isn't
      worth an import-time coupling between two independent CLI tools.

  --ready-source NAME=DATASET_DIR
      An already-prepared dataset in this project's own class order
      (has images/{train,val} + labels/{train,val} - i.e. a previous
      round's output, e.g. training/round4). Pooled as-is, no remap -
      remapping already-correct labels a second time would silently
      swap classes (this is exactly the bug this script's own
      development caught in a differently-mislabeled local copy: a
      classes.txt claiming 0=person/1=ball while the label files
      underneath still held raw ball=1288/person=144/referee=119 counts
      that didn't match at all once cross-checked against a fresh LS
      export - see docs/YOLO26_Training.md's merged-training-set entry).

Usage:
  python3 merge_yolo_datasets.py \
      --raw-source round18=verify18/labels_export;verify18/images \
      --raw-source round19=verify19/labels_export;verify19/images \
      --raw-source ai_learning=verify24/labels_export;verify24/images \
      --ready-source round4=training/round4 \
      --out training/merged_v1 \
      --val-fraction 0.15
"""

import argparse
import shutil
import sys
from pathlib import Path

# LS class index -> our class index. Must match
# prepare_yolo_train_split_from_ls_export.py's DEFAULT_CLASS_MAP exactly.
DEFAULT_CLASS_MAP = {0: 1, 1: 0, 2: 2}  # ball->1, person->0, referee->2
OUR_CLASS_NAMES = ["person", "ball", "referee"]


def remap_label_lines(src_text: str, class_map: dict[int, int], context: str) -> str:
    lines_out = []
    for line in src_text.splitlines():
        if not line.strip():
            continue
        parts = line.split()
        old_cls = int(parts[0])
        if old_cls not in class_map:
            sys.exit(f"{context}: unknown class id {old_cls}, no mapping given")
        parts[0] = str(class_map[old_cls])
        lines_out.append(" ".join(parts))
    return "\n".join(lines_out) + ("\n" if lines_out else "")


def parse_raw_source(spec: str) -> tuple[str, Path, Path]:
    name, rest = spec.split("=", 1)
    export_dir, images_dir = rest.split(";", 1)
    return name, Path(export_dir), Path(images_dir)


def parse_ready_source(spec: str) -> tuple[str, Path]:
    name, dataset_dir = spec.split("=", 1)
    return name, Path(dataset_dir)


def collect_raw_source(name: str, export_dir: Path, images_dir: Path) -> list[tuple[str, Path, str]]:
    """Returns (unique_stem, image_path, remapped_label_text) triples."""
    labels_dir = export_dir / "labels"
    if not labels_dir.is_dir():
        sys.exit(f"[{name}] no labels/ dir found at {labels_dir}")
    out = []
    for label_path in sorted(labels_dir.glob("*.txt")):
        img_path = images_dir / f"{label_path.stem}.jpg"
        if not img_path.exists():
            print(f"[{name}] WARNING: no matching image for {label_path.name}, skipping", file=sys.stderr)
            continue
        remapped = remap_label_lines(label_path.read_text(), DEFAULT_CLASS_MAP, f"{name}/{label_path.name}")
        # Prefix the stem with the source name so two sources can never
        # collide on filename even if their upstream uuids ever did.
        out.append((f"{name}_{label_path.stem}", img_path, remapped))
    print(f"[{name}] {len(out)} raw pairs collected (remapped)")
    return out


def collect_ready_source(name: str, dataset_dir: Path) -> list[tuple[str, Path, str]]:
    out = []
    for split in ("train", "val"):
        img_dir = dataset_dir / "images" / split
        lbl_dir = dataset_dir / "labels" / split
        if not img_dir.is_dir():
            continue
        for img_path in sorted(img_dir.glob("*.jpg")):
            label_path = lbl_dir / f"{img_path.stem}.txt"
            text = label_path.read_text() if label_path.exists() else ""
            out.append((f"{name}_{img_path.stem}", img_path, text))
    print(f"[{name}] {len(out)} ready pairs collected (no remap)")
    return out


def main():
    p = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    p.add_argument("--raw-source", action="append", default=[], metavar="NAME=EXPORT_DIR;IMAGES_DIR")
    p.add_argument("--ready-source", action="append", default=[], metavar="NAME=DATASET_DIR")
    p.add_argument("--out", type=Path, required=True)
    p.add_argument("--val-fraction", type=float, default=0.15)
    p.add_argument("--seed", type=int, default=42, help="Deterministic shuffle seed")
    args = p.parse_args()

    if not args.raw_source and not args.ready_source:
        sys.exit("Need at least one --raw-source or --ready-source")

    pool: list[tuple[str, Path, str]] = []
    for spec in args.raw_source:
        name, export_dir, images_dir = parse_raw_source(spec)
        pool.extend(collect_raw_source(name, export_dir, images_dir))
    for spec in args.ready_source:
        name, dataset_dir = parse_ready_source(spec)
        pool.extend(collect_ready_source(name, dataset_dir))

    if not pool:
        sys.exit("No image/label pairs collected from any source")

    import random

    rng = random.Random(args.seed)
    pool.sort(key=lambda t: t[0])  # deterministic order before shuffling
    rng.shuffle(pool)

    n_val = max(1, round(len(pool) * args.val_fraction))
    val_pairs = pool[:n_val]
    train_pairs = pool[n_val:]

    for split_name, split_pairs in (("train", train_pairs), ("val", val_pairs)):
        img_dir = args.out / "images" / split_name
        lbl_dir = args.out / "labels" / split_name
        img_dir.mkdir(parents=True, exist_ok=True)
        lbl_dir.mkdir(parents=True, exist_ok=True)
        for stem, img_path, label_text in split_pairs:
            shutil.copy(img_path, img_dir / f"{stem}.jpg")
            (lbl_dir / f"{stem}.txt").write_text(label_text, encoding="utf-8")

    data_yaml = args.out / "data.yaml"
    names_block = "\n".join(f"  {i}: {name}" for i, name in enumerate(OUR_CLASS_NAMES))
    data_yaml.write_text(
        f"path: {args.out.resolve()}\n"
        f"train: images/train\n"
        f"val: images/val\n"
        f"names:\n{names_block}\n",
        encoding="utf-8",
    )

    print(f"\n{len(pool)} total pairs from {len(args.raw_source) + len(args.ready_source)} sources")
    print(f"{len(train_pairs)} train / {len(val_pairs)} val written to {args.out}")
    print(f"data.yaml: {data_yaml}")


if __name__ == "__main__":
    main()
