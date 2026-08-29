#!/usr/bin/env python3
"""Pick training-candidate frames from raw match footage, aimed at what the
current detector gets *wrong*.

Why not just sample more frames: adding data does not automatically help.
This project measured the opposite once - a merged round that added three
sources regressed real-footage ball recall, because the added frames held
balls 8-43% larger than the originals. Volume was never the lever;
composition is. So this script selects two groups on purpose:

  uncertain_ball  the model found a plausibly sized ball but is unsure
  blind_spot      the model found no ball at all while many players are on
                  the pitch, so play is happening and a ball is very likely
                  in frame - the model's actual failures

Ball plausibility matters: at 3840x2880 a real ball is roughly 18 px tall,
so a "ball" of 80-170 px is a false positive. Without the size cap those
false positives dominate the large-ball end of any size-stratified pick,
which is exactly what happened on the first run of this script.

Frames are pulled with a keyframe seek per candidate rather than decoding
the match linearly - minutes instead of hours for a full match. Candidate
frames and their detections are cached, so re-selecting with different
parameters costs no GPU time at all.

IMPORTANT - what a frame must be: reco runs detection on each camera's own
raw frame, before stitching (see session/detection_dispatch.rs, which
dispatches CameraId::Left and CameraId::Right separately). Training data
must therefore be raw per-camera frames. Stitched panoramas, and above all
the exported virtual-camera video, are a different domain and will not help.

Output matches export_yolo_labels.py's convention (images/<camera>/,
labels/<camera>/, classes.txt), so upload_to_labelstudio.py and
package_yolo_for_labelstudio.py both work on it unchanged.

Usage:
  python3 pick_training_frames.py \\
      --match-dir "/path/to/Match Name" --out dataset_dir \\
      --model best.pt --per-camera 15
  # re-select from the cache without re-running anything expensive:
  python3 pick_training_frames.py --match-dir ... --out ... --model ... \\
      --reselect-only --per-camera 25
"""

import argparse
import json
import shutil
import subprocess
import sys
from pathlib import Path

CLASS_NAMES = ["person", "ball", "referee"]
PERSON_CLS, BALL_CLS = 0, 1


def probe_duration(path: Path) -> float:
    out = subprocess.run(
        ["ffprobe", "-v", "error", "-show_entries", "format=duration",
         "-of", "csv=p=0", str(path)],
        capture_output=True, text=True, check=True)
    return float(out.stdout.strip())


def segments(cam_dir: Path):
    """Chained segments with their start offset on the joined timeline."""
    files = sorted(set(list(cam_dir.glob("*.MP4")) + list(cam_dir.glob("*.mp4"))))
    if not files:
        raise SystemExit(f"no video segments in {cam_dir}")
    out, t = [], 0.0
    for f in files:
        d = probe_duration(f)
        out.append((f, t, d))
        t += d
    return out, t


def locate(segs, t: float):
    for f, start, dur in segs:
        if t < start + dur:
            return f, t - start
    f, start, dur = segs[-1]
    return f, min(t - start, dur - 1.0)


def grab(video: Path, offset: float, dest: Path, decoder: str) -> bool:
    cmd = ["ffmpeg", "-v", "error", "-ss", f"{offset:.3f}"]
    if decoder:
        cmd += ["-c:v", decoder]
    cmd += ["-i", str(video), "-frames:v", "1", "-q:v", "2", "-y", str(dest)]
    subprocess.run(cmd, capture_output=True, text=True)
    return dest.exists() and dest.stat().st_size > 0


def extract(args, cam_dir: Path, cam: str, cache: Path):
    segs, total = segments(cam_dir)
    lo, hi = args.edge_skip, total - args.edge_skip
    if hi <= lo:
        raise SystemExit(f"{cam_dir} is shorter than 2x --edge-skip")
    step = (hi - lo) / max(1, args.candidates - 1)
    cache.mkdir(parents=True, exist_ok=True)
    print(f"  {total / 60:.1f} min over {len(segs)} segment(s)", flush=True)
    out = []
    for i in range(args.candidates):
        t = lo + i * step
        video, offset = locate(segs, t)
        dest = cache / f"{cam}_{int(t):05d}s.jpg"
        if dest.exists() or grab(video, offset, dest, args.decoder):
            out.append(dest)
        else:
            print(f"    seek failed at {t:.0f}s", flush=True)
        if (i + 1) % 30 == 0:
            print(f"    extracted {i + 1}/{args.candidates}", flush=True)
    return out


def score(paths, model_path: Path, imgsz: int, conf: float):
    from ultralytics import YOLO
    model = YOLO(str(model_path))
    print(f"  scoring {len(paths)} frames with {model_path.name} "
          f"(names={model.names})", flush=True)
    out = {}
    for i, p in enumerate(paths, 1):
        res = model.predict(str(p), imgsz=imgsz, conf=conf, verbose=False)[0]
        out[p.name] = [
            {"cls": int(b.cls), "conf": float(b.conf),
             "xywhn": [float(v) for v in b.xywhn[0]]}
            for b in res.boxes
        ]
        if i % 100 == 0:
            print(f"    {i}/{len(paths)}", flush=True)
    return out


def spaced(cands, n, key, min_gap):
    """Greedy top-n by `key`, skipping picks within `min_gap` seconds."""
    out = []
    for r in sorted(cands, key=key):
        if all(abs(r["t"] - c["t"]) >= min_gap for c in out):
            out.append(r)
        if len(out) == n:
            return out
    for r in sorted(cands, key=key):  # relax rather than return a short set
        if r not in out:
            out.append(r)
        if len(out) == n:
            break
    return out


def select(records, args):
    """Split the kept frames between the two groups described in the module doc."""
    with_ball = [r for r in records if r["has_ball"]]
    blind = [r for r in records if not r["has_ball"] and r["persons"] >= args.crowd_min]

    n_blind = round(args.per_camera * args.blind_fraction)
    n_ball = args.per_camera - n_blind

    with_ball.sort(key=lambda r: r["ball_h_px"])
    m = len(with_ball)
    bins = [with_ball[:m // 3], with_ball[m // 3:2 * m // 3], with_ball[2 * m // 3:]]
    want = [n_ball - 2 * (n_ball // 3), n_ball // 3, n_ball // 3]
    chosen = []
    for b, k in zip(bins, want):
        for r in spaced(b, k, lambda r: -r["persons"], args.min_gap):
            r["group"] = "uncertain_ball"
            chosen.append(r)
    for r in spaced(blind, n_blind, lambda r: -r["persons"], args.min_gap):
        r["group"] = "blind_spot"
        chosen.append(r)
    return sorted(chosen, key=lambda r: r["t"])


def main():
    p = argparse.ArgumentParser(description=__doc__,
                                formatter_class=argparse.RawDescriptionHelpFormatter)
    p.add_argument("--match-dir", type=Path, required=True, action="append",
                   help="Match folder holding the per-camera subdirs (repeatable)")
    p.add_argument("--camera-dir", action="append", default=None,
                   help="Per-camera subdir names (default: LEFT RIGHT)")
    p.add_argument("--out", type=Path, required=True, help="Dataset output dir")
    p.add_argument("--model", type=Path, required=True, help="Teacher weights (.pt)")
    p.add_argument("--cache-dir", type=Path, default=None,
                   help="Where candidate frames and detections are cached "
                        "(default: <out>/.cache) - keep it to re-select for free")
    p.add_argument("--candidates", type=int, default=180,
                   help="Probe points per camera, spread over the match (default: 180)")
    p.add_argument("--per-camera", type=int, default=15, help="Frames kept per camera")
    p.add_argument("--blind-fraction", type=float, default=0.47,
                   help="Share of the kept frames taken from the blind-spot group "
                        "(default: 0.47, i.e. 7 of 15)")
    p.add_argument("--edge-skip", type=float, default=90.0,
                   help="Seconds ignored at both ends (warm-up / pack-up)")
    p.add_argument("--min-gap", type=float, default=120.0,
                   help="Minimum seconds between two picks, so one phase of play "
                        "cannot dominate the set")
    p.add_argument("--crowd-min", type=int, default=14,
                   help="Players needed before a ball-less frame counts as "
                        "'play is happening'")
    p.add_argument("--ball-px", type=float, nargs=2, default=(5.0, 30.0),
                   metavar=("MIN", "MAX"),
                   help="Plausible ball height in source pixels; anything outside is "
                        "a false positive (default: 5 30, for a ~18px ball at 3840x2880)")
    p.add_argument("--frame-height", type=int, default=2880,
                   help="Source frame height, for the --ball-px conversion")
    p.add_argument("--person-conf", type=float, default=0.25)
    p.add_argument("--conf", type=float, default=0.05,
                   help="Detection floor. Deliberately low: a high floor throws away "
                        "exactly the hard balls this set is meant to collect")
    p.add_argument("--imgsz", type=int, default=1920)
    p.add_argument("--decoder", default="hevc_cuvid",
                   help="Explicit ffmpeg decoder (empty string to let ffmpeg choose). "
                        "The generic -hwaccel cuda flag silently falls back to software "
                        "decode on some setups, so name the decoder instead")
    p.add_argument("--reselect-only", action="store_true",
                   help="Skip extraction and inference, re-select from the cache")
    args = p.parse_args()

    cameras = args.camera_dir or ["LEFT", "RIGHT"]
    cache_root = args.cache_dir or (args.out / ".cache")
    dets_file = cache_root / "detections.json"
    cache_root.mkdir(parents=True, exist_ok=True)
    dets = json.loads(dets_file.read_text(encoding="utf-8")) if dets_file.exists() else {}

    for sub in ("images", "labels"):
        shutil.rmtree(args.out / sub, ignore_errors=True)
    args.out.mkdir(parents=True, exist_ok=True)
    (args.out / "classes.txt").write_text(
        "\n".join(f"{i} {n}" for i, n in enumerate(CLASS_NAMES)) + "\n", encoding="utf-8")

    summary = {}
    for match_dir in args.match_dir:
        tag = match_dir.name.split()[0]
        for cam_sub in cameras:
            cam = cam_sub.lower()
            cam_cache = cache_root / tag / cam
            print(f"\n=== {match_dir.name} / {cam}", flush=True)

            if args.reselect_only:
                paths = sorted(cam_cache.glob("*.jpg"))
            else:
                paths = extract(args, match_dir / cam_sub, cam, cam_cache)
                todo = [p for p in paths if f"{tag}/{cam}/{p.name}" not in dets]
                if todo:
                    for name, boxes in score(todo, args.model, args.imgsz, args.conf).items():
                        dets[f"{tag}/{cam}/{name}"] = boxes
                    dets_file.write_text(json.dumps(dets), encoding="utf-8")

            lo_px, hi_px = args.ball_px
            records = []
            for path in paths:
                boxes = dets.get(f"{tag}/{cam}/{path.name}")
                if boxes is None:
                    continue
                persons = sum(1 for b in boxes
                              if b["cls"] == PERSON_CLS and b["conf"] >= args.person_conf)
                balls = [b for b in boxes if b["cls"] == BALL_CLS
                         and lo_px <= b["xywhn"][3] * args.frame_height <= hi_px]
                best = max(balls, key=lambda b: b["conf"]) if balls else None
                records.append({
                    "path": path, "boxes": boxes, "persons": persons,
                    "t": int(path.stem.split("_")[-1].rstrip("s")),
                    "ball_h_px": best["xywhn"][3] * args.frame_height if best else 0.0,
                    "ball_conf": best["conf"] if best else 0.0,
                    "has_ball": best is not None,
                })

            chosen = select(records, args)
            img_dir, lbl_dir = args.out / "images" / cam, args.out / "labels" / cam
            img_dir.mkdir(parents=True, exist_ok=True)
            lbl_dir.mkdir(parents=True, exist_ok=True)
            for r in chosen:
                stem = f"{tag}_{r['path'].stem}"
                shutil.copy2(r["path"], img_dir / f"{stem}.jpg")
                (lbl_dir / f"{stem}.txt").write_text("\n".join(
                    f"{b['cls']} {b['xywhn'][0]:.6f} {b['xywhn'][1]:.6f} "
                    f"{b['xywhn'][2]:.6f} {b['xywhn'][3]:.6f}" for b in r["boxes"]
                ) + "\n", encoding="utf-8")

            groups = {}
            for r in chosen:
                groups[r["group"]] = groups.get(r["group"], 0) + 1
            print(f"  kept {len(chosen)} of {len(records)} candidates {groups}", flush=True)
            summary[f"{tag}/{cam}"] = [
                {"t": r["t"], "group": r["group"], "persons": r["persons"],
                 "ball_conf": round(r["ball_conf"], 3),
                 "ball_h_px": round(r["ball_h_px"], 1),
                 "file": f"{tag}_{r['path'].stem}.jpg"}
                for r in chosen
            ]

    (args.out / "selection_summary.json").write_text(
        json.dumps(summary, indent=2), encoding="utf-8")
    print(f"\ndone -> {args.out}", flush=True)


if __name__ == "__main__":
    sys.exit(main())
