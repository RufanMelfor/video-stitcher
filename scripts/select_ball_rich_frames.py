#!/usr/bin/env python3
"""Extract candidate frames from raw match footage at a fixed interval,
run a teacher model (soccana by default) over them, and keep only the
frames richest in ball detections - for building a training set that
specifically targets the ball-detection weak spot instead of uniform
time-sampling.

Output layout matches export_yolo_labels.py's convention (images/<camera>/,
labels/<camera>/, classes.txt) so downstream tooling
(package_yolo_for_labelstudio.py, prepare_yolo_train_split_from_ls_export.py)
works unchanged. Labels use this project's 3-class scheme:
0=person, 1=ball, 2=referee (soccana's native order, verified via
model.names: 0=Player, 1=Ball, 2=Referee - an identity remap here, but
kept explicit since it's a different source than
prepare_yolo_train_split_from_ls_export.py's mapping, which reads an
LS project's own classes.txt - a different, ball-first order set at
labeling-config time, not soccana's raw output order).

Usage:
  python3 select_ball_rich_frames.py \\
      --video left.mp4 --camera left --out dataset_dir \\
      --model soccana.pt --interval 3 --top-k 150 --conf 0.15
"""

import argparse
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path

# soccana's native class order (verified via model.names, not assumed:
# {0: 'Player', 1: 'Ball', 2: 'Referee'}) -> this project's (person=0,
# ball=1, referee=2). Happens to be an identity mapping, but kept explicit
# since it's a different source than prepare_yolo_train_split_from_ls_export.py's
# mapping (that one reads an LS project's own classes.txt, which used a
# different order - ball first - set at labeling-config time, not
# soccana's raw output order. Don't assume the two match.)
SOCCANA_TO_OURS = {0: 0, 1: 1, 2: 2}  # player->person, ball->ball, referee->referee
OUR_CLASS_NAMES = ["person", "ball", "referee"]


def extract_candidates(video_path: Path, interval_s: float, tmp_dir: Path) -> list[Path]:
    """Single sequential decode pass at fps=1/interval_s - much faster than
    per-frame seeks for a few hundred sparse samples across a long video.

    Uses NVDEC (`-c:v hevc_cuvid`) for the decode - measured ~1.35-1.4x
    realtime on this machine's RTX 3060 Ti vs ~1.0x for plain software
    decode, and vs. no actual speedup from the more generic `-hwaccel cuda`
    flag alone (it silently fell back to software decode here - confirmed
    via the "Stream mapping" line showing "hevc (native)" instead of the
    cuvid decoder). All source footage in this project is HEVC as of
    2026-08-10 (03/01/02 match folders all checked) - if a future source
    turns out to be H.264, swap to `h264_cuvid` for that call instead."""
    pattern = tmp_dir / "cand_%06d.jpg"
    result = subprocess.run(
        [
            "ffmpeg", "-y", "-c:v", "hevc_cuvid", "-i", str(video_path),
            "-vf", f"fps=1/{interval_s}", "-q:v", "2", str(pattern),
        ],
        capture_output=True, text=True,
    )
    if result.returncode != 0:
        sys.exit(f"ffmpeg extraction failed for {video_path}:\n{result.stderr[-2000:]}")
    return sorted(tmp_dir.glob("cand_*.jpg"))


def process_video(
    video: Path, camera: str, out: Path, model, interval: float, top_k: int, conf: float, imgsz: int
) -> None:
    """Core pipeline for one video, reusable with a preloaded `model` so a
    driver script processing many videos doesn't reload the checkpoint
    every time (see the bottom of this file for a multi-video example)."""
    with tempfile.TemporaryDirectory(prefix="ball_rich_") as tmp:
        tmp_dir = Path(tmp)
        print(f"[{video.name}/{camera}] extracting candidates every {interval}s...")
        candidates = extract_candidates(video, interval, tmp_dir)
        print(f"[{video.name}/{camera}] {len(candidates)} candidate frames, running model...")

        scored = []  # (ball_count, max_ball_conf, frame_path, boxes)
        for i, frame_path in enumerate(candidates):
            result = model.predict(str(frame_path), conf=conf, imgsz=imgsz, verbose=False)[0]
            boxes = []
            ball_confs = []
            for box in result.boxes:
                cls_id = int(box.cls[0])
                bconf = float(box.conf[0])
                x1, y1, x2, y2 = box.xyxy[0].tolist()
                boxes.append((cls_id, bconf, x1, y1, x2, y2))
                if cls_id == 1:  # soccana's ball class (verified: {0: Player, 1: Ball, 2: Referee})
                    ball_confs.append(bconf)
            scored.append((len(ball_confs), max(ball_confs, default=0.0), frame_path, boxes, result.orig_shape))
            if (i + 1) % 50 == 0:
                print(f"  ...{i + 1}/{len(candidates)}")

        scored.sort(key=lambda t: (t[0], t[1]), reverse=True)
        ball_positive = sum(1 for s in scored if s[0] > 0)
        keep = [s for s in scored if s[0] > 0][:top_k]
        print(f"[{video.name}/{camera}] {ball_positive}/{len(candidates)} candidates had >=1 ball, keeping {len(keep)}")

        img_out = out / "images" / camera
        lbl_out = out / "labels" / camera
        img_out.mkdir(parents=True, exist_ok=True)
        lbl_out.mkdir(parents=True, exist_ok=True)

        for idx, (ball_count, max_conf, frame_path, boxes, (h, w)) in enumerate(keep):
            stem = f"frame_{idx:06d}"
            dest_img = img_out / f"{stem}.jpg"
            # shutil.move (not Path.replace/os.replace) - the temp dir is on
            # C: and the dataset output is typically on D:, and os.replace
            # refuses cross-drive moves on Windows (WinError 17).
            shutil.move(str(frame_path), str(dest_img))
            lines = []
            for cls_id, bconf, x1, y1, x2, y2 in boxes:
                our_cls = SOCCANA_TO_OURS.get(cls_id)
                if our_cls is None:
                    continue
                cx = (x1 + x2) / 2 / w
                cy = (y1 + y2) / 2 / h
                bw = (x2 - x1) / w
                bh = (y2 - y1) / h
                lines.append(f"{our_cls} {cx:.6f} {cy:.6f} {bw:.6f} {bh:.6f}")
            (lbl_out / f"{stem}.txt").write_text("\n".join(lines) + ("\n" if lines else ""), encoding="utf-8")

    classes_file = out / "classes.txt"
    if not classes_file.exists():
        classes_file.write_text(
            "\n".join(f"{i} {name}" for i, name in enumerate(OUR_CLASS_NAMES)) + "\n", encoding="utf-8"
        )
    print(f"[{video.name}/{camera}] done -> {img_out}")


def main():
    p = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    p.add_argument("--video", type=Path, required=True)
    p.add_argument("--camera", required=True, help="Label for this camera side, e.g. 'left'/'right' (used as the output subdir name)")
    p.add_argument("--out", type=Path, required=True, help="export_yolo_labels.py-style dataset dir (images/<camera>/, labels/<camera>/, classes.txt)")
    p.add_argument("--model", default="soccana.pt", help="Teacher model weights (default: soccana.pt, must be on disk/resolvable by ultralytics)")
    p.add_argument("--interval", type=float, default=3.0, help="Seconds between candidate frames (default: 3.0)")
    p.add_argument("--top-k", type=int, default=150, help="Max frames to keep, ranked by ball richness (default: 150)")
    p.add_argument("--conf", type=float, default=0.15, help="Detection confidence threshold (default: 0.15)")
    p.add_argument("--imgsz", type=int, default=1280)
    args = p.parse_args()

    from ultralytics import YOLO

    model = YOLO(args.model)
    process_video(args.video, args.camera, args.out, model, args.interval, args.top_k, args.conf, args.imgsz)


if __name__ == "__main__":
    main()
