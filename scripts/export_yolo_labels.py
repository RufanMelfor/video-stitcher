#!/usr/bin/env python3
"""Turn a `reco stitch --events detections.jsonl` run into a YOLO-format
pre-labeled dataset, ready for human review/correction in a labeling tool
(Roboflow, CVAT, Label Studio, ...).

The idea: run the *current* model over real match footage via `reco stitch
--model yolo26n.onnx --events detections.jsonl`, then use this script to
turn those detections into YOLO `.txt` labels paired with the actual raw
camera frames - so a human only has to fix the model's mistakes instead of
drawing every box from scratch.

Usage:
  python3 export_yolo_labels.py \\
      --events detections.jsonl --left left.mp4 --right right.mp4 \\
      --out pilot_dataset --sample-every 30 --min-confidence 0.35

Output layout (per camera, since detections are per-camera-frame, not
stitched-panorama space):
  <out>/images/left/frame_000123.jpg
  <out>/labels/left/frame_000123.txt   # YOLO format: class_id cx cy w h (normalized)
  <out>/images/right/...
  <out>/labels/right/...
  <out>/classes.txt                    # class_id -> name, for import into a labeling tool

class_id convention: the model's raw output is standard 80-class COCO
indices (0 = person, 32 = sports ball), *not* the "0 = ball, 1 = person"
described in `MappedDetection::class_id`'s doc comment
(crates/reco-core/src/detect/director.rs) - verified empirically against a
real detections.jsonl run (class 0 boxes are person-shaped, ~0.37 aspect
ratio, one per player; class 32 boxes are near-square, ~0.81 aspect ratio,
matching a ball). That doc comment appears to describe a different/planned
reduced-class model, not this one - don't trust it without checking.

Everything else COCO detects on football footage (tennis racket, frisbee,
kite, backpack, chair, ...) is noise from running a general-purpose model
on a domain it wasn't trained for - filtered out here rather than fed into
the training set as mislabeled examples. Output labels are remapped to a
clean local 2-class scheme: 0 = person, 1 = ball.
"""

import argparse
import json
import subprocess
import sys
from collections import defaultdict
from pathlib import Path

# COCO class id -> (output class id, name). Only these two are kept;
# everything else in the raw detections is dropped (see module docstring).
COCO_TO_OUTPUT = {0: (0, "person"), 32: (1, "ball")}
CLASS_NAMES = {out_id: name for out_id, name in COCO_TO_OUTPUT.values()}


def load_detections_by_frame(events_path: Path) -> dict[int, list[dict]]:
    """Frame index -> list of MappedDetection dicts (from the last
    `detections_raw` event seen for that frame; there's normally only one)."""
    by_frame: dict[int, list[dict]] = {}
    with open(events_path, encoding="utf-8") as f:
        for line in f:
            line = line.strip()
            if not line:
                continue
            ev = json.loads(line)
            if ev.get("kind") != "detections_raw":
                continue
            by_frame[ev["frame_index"]] = ev.get("detections", [])
    return by_frame


def probe_fps(video_path: Path) -> float:
    out = subprocess.run(
        [
            "ffprobe", "-v", "error", "-select_streams", "v:0",
            "-show_entries", "stream=r_frame_rate",
            "-of", "default=noprint_wrappers=1:nokey=1", str(video_path),
        ],
        capture_output=True, text=True, check=True,
    ).stdout.strip()
    num, _, den = out.partition("/")
    return float(num) / float(den or 1)


def extract_frame(video_path: Path, frame_index: int, fps: float, out_path: Path) -> bool:
    """Seek to frame_index's timestamp and grab exactly one frame.
    Input-side seek (-ss before -i) is fast but keyframe-snapped, which is
    fine here - a pre-label a few frames off from the requested index is
    still a valid (image, boxes) pair for training, just not frame-exact."""
    timestamp = frame_index / fps
    result = subprocess.run(
        [
            "ffmpeg", "-y", "-ss", f"{timestamp:.3f}", "-i", str(video_path),
            "-frames:v", "1", "-q:v", "2", str(out_path),
        ],
        capture_output=True, text=True,
    )
    return result.returncode == 0 and out_path.exists()


def write_yolo_label(detections: list[dict], camera: str, min_confidence: float, out_path: Path) -> int:
    lines = []
    for det in detections:
        if det.get("camera") != camera:
            continue
        if det.get("confidence", 0.0) < min_confidence:
            continue
        mapped = COCO_TO_OUTPUT.get(det["class_id"])
        if mapped is None:
            continue  # not person/ball - COCO noise class, drop it
        class_id, _name = mapped
        cx, cy = det["camera_center"]
        w, h = det["camera_size"]
        # Clamp: a box centered near the frame edge can nominally extend
        # past [0, 1] - YOLO label format expects values in range.
        cx, cy = min(max(cx, 0.0), 1.0), min(max(cy, 0.0), 1.0)
        w, h = min(w, 1.0), min(h, 1.0)
        lines.append(f"{class_id} {cx:.6f} {cy:.6f} {w:.6f} {h:.6f}")
    out_path.write_text("\n".join(lines) + ("\n" if lines else ""), encoding="utf-8")
    return len(lines)


def main():
    p = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    p.add_argument("--events", type=Path, required=True, help="detections.jsonl from `reco stitch --events`")
    p.add_argument("--left", type=Path, required=True, help="Left camera source video")
    p.add_argument("--right", type=Path, required=True, help="Right camera source video")
    p.add_argument("--out", type=Path, required=True, help="Output dataset directory")
    p.add_argument("--sample-every", type=int, default=30,
                    help="Only export every Nth frame that has detections (default: 30, ~1/sec at 30fps)")
    p.add_argument("--min-confidence", type=float, default=0.35,
                    help="Drop detections below this confidence from the pre-labels (default: 0.35)")
    p.add_argument("--max-samples", type=int, default=0,
                    help="Stop after this many sampled frames (0 = no limit)")
    args = p.parse_args()

    by_frame = load_detections_by_frame(args.events)
    if not by_frame:
        print(f"No detections_raw events found in {args.events}", file=sys.stderr)
        sys.exit(1)

    frame_indices = sorted(by_frame.keys())[:: args.sample_every]
    if args.max_samples:
        frame_indices = frame_indices[: args.max_samples]
    print(f"{len(by_frame)} frames with detections in events file; sampling {len(frame_indices)} "
          f"(every {args.sample_every}) for pre-labeling.")

    left_fps = probe_fps(args.left)
    right_fps = probe_fps(args.right)

    for camera in ("left", "right"):
        (args.out / "images" / camera).mkdir(parents=True, exist_ok=True)
        (args.out / "labels" / camera).mkdir(parents=True, exist_ok=True)

    (args.out / "classes.txt").write_text(
        "\n".join(f"{cid} {name}" for cid, name in sorted(CLASS_NAMES.items())) + "\n",
        encoding="utf-8",
    )

    total_boxes = defaultdict(int)
    exported = defaultdict(int)
    for frame_index in frame_indices:
        detections = by_frame[frame_index]
        for camera, video_path, fps, api_camera in (
            ("left", args.left, left_fps, "Left"),
            ("right", args.right, right_fps, "Right"),
        ):
            stem = f"frame_{frame_index:07d}"
            img_path = args.out / "images" / camera / f"{stem}.jpg"
            lbl_path = args.out / "labels" / camera / f"{stem}.txt"
            if not extract_frame(video_path, frame_index, fps, img_path):
                print(f"  [skip] {camera} frame {frame_index}: ffmpeg extraction failed", file=sys.stderr)
                continue
            n = write_yolo_label(detections, api_camera, args.min_confidence, lbl_path)
            total_boxes[camera] += n
            exported[camera] += 1

    for camera in ("left", "right"):
        print(f"{camera}: {exported[camera]} frames exported, {total_boxes[camera]} pre-label boxes total "
              f"({total_boxes[camera] / max(exported[camera], 1):.1f} boxes/frame avg)")
    print(f"\nDataset ready at: {args.out}")
    print("Next: import images/+labels/ into a labeling tool (Roboflow, CVAT, Label Studio) "
          "for human review - correct the model's mistakes instead of labeling from scratch.")


if __name__ == "__main__":
    main()
