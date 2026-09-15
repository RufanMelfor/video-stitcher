#!/usr/bin/env python3
"""Export specific, known Label Studio task IDs' human annotations to a
flat YOLO-format dataset (images/ + labels/ + classes.txt), for merging
into an existing training set.

# Why this exists, not LS's own project-wide export

Label Studio's own export endpoints (YOLO, YOLO_WITH_IMAGES, etc. - see
`ls.api('/api/projects/<id>/export/formats')`) export the WHOLE project's
tasks, not a specific subset. This project's Label Studio instance
mixes many rounds' tasks in one project (project 24 held 419 tasks as
of 2026-09-15, only a small subset of which - 91, from
find_ball_confidence_dips.py + select_top_frames.py's capped selection
- are the ones a given training round actually wants). Re-exporting and
re-filtering the whole project every time a small new batch is ready is
wasteful and risks accidentally pulling in unrelated/unfinished
annotations. This script instead takes explicit task IDs and pulls only
those.

# Annotation selection within a task

A task can have multiple annotations (re-submissions, multiple
annotators) - by default this script takes the FIRST non-cancelled one
per task (`was_cancelled: False`), matching what a human would consider
"the" finished annotation for a single-annotator project like this one.
Each region's `origin` field (`prediction`, `prediction-changed`,
`manual`) is NOT filtered on - an unmodified `prediction` region the
human reviewed and left as-is (by submitting the task) is just as much
part of the finished annotation as one they drew from scratch; origin is
informational, not a signal to exclude on.

Usage:
  python scripts/export_ls_annotations_by_task.py \\
      --task-ids 2296 2297 2298 \\
      --url http://192.168.191.204:8080 --token-file token.txt \\
      --out D:/CLAUDE/ls_export_batch1

NOT run as part of writing this script - point it at real Label Studio
task IDs yourself (or ask me to).
"""

import argparse
import json
import sys
from pathlib import Path
from urllib import error, request

# This project's fixed 3-class scheme (see docs/YOLO26_Training.md,
# every other script in this repo) - same order as
# prepare_yolo_train_split_from_ls_export.py's target mapping.
CLASS_NAMES = ["person", "ball", "referee"]
CLASS_INDEX = {name: i for i, name in enumerate(CLASS_NAMES)}


class LabelStudio:
    """Same minimal API client as upload_to_labelstudio.py - duplicated
    rather than imported, since that script is a CLI tool, not a
    library, and this script needs only `api()` and image download, not
    upload_image/tasks_by_filename."""

    def __init__(self, url: str, token: str):
        self.url = url.rstrip("/")
        self.hdr = {"Authorization": f"Token {token}"}

    def api(self, path, data=None, method=None):
        body = json.dumps(data).encode() if data is not None else None
        req = request.Request(f"{self.url}{path}", data=body,
                              method=method or ("POST" if body else "GET"))
        for k, v in self.hdr.items():
            req.add_header(k, v)
        if body:
            req.add_header("Content-Type", "application/json")
        with request.urlopen(req, timeout=120) as r:
            raw = r.read()
        return json.loads(raw) if raw else {}

    def download(self, path: str) -> bytes:
        req = request.Request(f"{self.url}{path}")
        for k, v in self.hdr.items():
            req.add_header(k, v)
        with request.urlopen(req, timeout=300) as r:
            return r.read()


def region_to_yolo_line(region: dict) -> str | None:
    """LS rectanglelabels region (top-left x/y/width/height, percent of
    image) -> one YOLO label line (class-index center-x/y/w/h, 0..1).
    Returns None for a region this project doesn't have a class for
    (skip rather than crash - a stray/typo'd label should not fail the
    whole export)."""
    v = region["value"]
    labels = v.get("rectanglelabels", [])
    if not labels:
        return None
    cls_name = labels[0]
    if cls_name not in CLASS_INDEX:
        print(f"  WARNING: unknown class '{cls_name}', skipping this region", file=sys.stderr)
        return None
    cls_idx = CLASS_INDEX[cls_name]
    x0 = v["x"] / 100.0
    y0 = v["y"] / 100.0
    w = v["width"] / 100.0
    h = v["height"] / 100.0
    cx = x0 + w / 2
    cy = y0 + h / 2
    return f"{cls_idx} {cx:.6f} {cy:.6f} {w:.6f} {h:.6f}"


def main():
    p = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    p.add_argument("--task-ids", type=int, nargs="+", required=True,
                   help="Explicit Label Studio task IDs to export (not a project-wide export)")
    p.add_argument("--url", required=True, help="Label Studio base URL")
    p.add_argument("--token-file", type=Path, required=True,
                   help="File holding the API token")
    p.add_argument("--out", type=Path, required=True, help="Output dataset dir")
    p.add_argument("--camera", choices=["left", "right", "auto"], default="auto",
                   help="Camera subdirectory for images/labels - 'auto' "
                        "infers left/right from the task's own filename "
                        "prefix (this project's upload convention: "
                        "'left_...'/'right_...'), sys.exit if a filename "
                        "doesn't start with either")
    args = p.parse_args()

    token = args.token_file.read_text(encoding="utf-8").strip()
    ls = LabelStudio(args.url, token)

    args.out.mkdir(parents=True, exist_ok=True)
    (args.out / "classes.txt").write_text(
        "\n".join(f"{i} {n}" for i, n in enumerate(CLASS_NAMES)) + "\n", encoding="utf-8")

    exported, skipped_no_annotation, skipped_cancelled_only = 0, [], []
    for tid in args.task_ids:
        task = ls.api(f"/api/tasks/{tid}/")
        annotations = [a for a in task.get("annotations", []) if not a.get("was_cancelled")]
        if not annotations:
            if any(task.get("annotations", [])):
                skipped_cancelled_only.append(tid)
            else:
                skipped_no_annotation.append(tid)
            continue
        annotation = annotations[0]  # see module doc comment on annotation selection

        image_field = task.get("data", {}).get("image", "")
        # Uploaded images are stored as `<uuid>-<original name>` (see
        # upload_to_labelstudio.py's tasks_by_filename) - strip the uuid
        # prefix back off to recover the real filename.
        orig_name = image_field.rsplit("/", 1)[-1]
        if "-" in orig_name:
            orig_name = orig_name.split("-", 1)[1]
        stem = Path(orig_name).stem

        if args.camera == "auto":
            if orig_name.startswith("left"):
                camera = "left"
            elif orig_name.startswith("right"):
                camera = "right"
            else:
                sys.exit(f"task {tid}: filename '{orig_name}' doesn't start with "
                          f"left/right - pass --camera explicitly instead of 'auto'")
        else:
            camera = args.camera

        lines = [ln for r in annotation["result"]
                 if (ln := region_to_yolo_line(r)) is not None]

        img_dir = args.out / "images" / camera
        lbl_dir = args.out / "labels" / camera
        img_dir.mkdir(parents=True, exist_ok=True)
        lbl_dir.mkdir(parents=True, exist_ok=True)

        img_bytes = ls.download(image_field)
        (img_dir / f"{stem}.jpg").write_bytes(img_bytes)
        (lbl_dir / f"{stem}.txt").write_text(
            "\n".join(lines) + ("\n" if lines else ""), encoding="utf-8")
        exported += 1
        if exported % 10 == 0:
            print(f"  exported {exported}/{len(args.task_ids)}", flush=True)

    print(f"\nExported {exported} annotated task(s) -> {args.out}")
    if skipped_no_annotation:
        print(f"Skipped {len(skipped_no_annotation)} task(s) with no annotation at all: {skipped_no_annotation}")
    if skipped_cancelled_only:
        print(f"Skipped {len(skipped_cancelled_only)} task(s) whose only annotation(s) were cancelled/skipped: {skipped_cancelled_only}")


if __name__ == "__main__":
    sys.exit(main())
