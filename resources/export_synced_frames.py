#!/usr/bin/env python3
"""Export a synced left/right frame pair from raw stereo footage as PNGs.

Given a frame number on the LEFT video, computes the matching RIGHT frame
using the rig's sync_offset (same convention as reco-cli's --sync-offset /
MatchCalibration's sync_offset: positive = right video is ahead by N
frames, so right_frame = left_frame + sync_offset - see
crates/reco-cli/src/main.rs and crates/reco-calibrate/examples/
dump_undistorted.rs for the Rust-side equivalent) and extracts both as
RAW (still-fisheye, not undistorted) PNG stills via ffmpeg.

Output is meant to be loaded directly into resources/click_line_calib_v1.html
(Left:/Right: file inputs) - that tool does its own in-browser undistort,
so no GPU/--debug-dir step is needed just to get clickable stills.

Usage:
  export_synced_frames.py <left.mp4> <right.mp4> <sync_offset> <left_frame> <out_dir>

  left_frame may be an integer frame number, or HH:MM:SS(.ms) / MM:SS for
  a timestamp on the left video (converted via the left video's own fps).

Example (OJC match, sync_offset=4, see resources/match_werkplaats.json-style
per-clip calibration json for this rig):
  python export_synced_frames.py \\
    "D:/VOETBAL_VIDEO/Berghem Sport J011-1/03 OJC -Bergem Sport 04072026/L/DJI_20260704095935_0028_D_L01.MP4" \\
    "D:/VOETBAL_VIDEO/Berghem Sport J011-1/03 OJC -Bergem Sport 04072026/R/DJI_20260704095935_0029_D_R01.MP4" \\
    4 10000 "D:/VOETBAL_VIDEO/CALIB/OJC_test/line_clicks"
"""
import subprocess
import sys
import os


def ffprobe_fps(video):
    out = subprocess.check_output(
        [
            "ffprobe", "-v", "error", "-select_streams", "v:0",
            "-show_entries", "stream=r_frame_rate",
            "-of", "default=noprint_wrappers=1:nokey=1", video,
        ],
        text=True,
    ).strip()
    num, den = out.split("/")
    return float(num) / float(den)


def parse_left_frame(arg, fps):
    if ":" in arg:
        parts = [float(p) for p in arg.split(":")]
        secs = 0.0
        for p in parts:
            secs = secs * 60 + p
        return round(secs * fps)
    return int(arg)


def extract_frame(video, frame_idx, fps, out_path):
    """Fast (coarse) seek near the target, then a short accurate seek to
    land on the exact time - avoids decoding the whole file from frame 0
    for frames deep into a multi-GB recording."""
    t = frame_idx / fps
    coarse = max(0.0, t - 5.0)
    fine = t - coarse
    subprocess.run(
        [
            "ffmpeg", "-y", "-hide_banner", "-loglevel", "error",
            "-ss", f"{coarse:.3f}", "-i", video,
            "-ss", f"{fine:.3f}", "-frames:v", "1", "-q:v", "2",
            out_path,
        ],
        check=True,
    )


def main():
    if len(sys.argv) != 6:
        print(__doc__)
        sys.exit(1)
    left_video, right_video, sync_offset_s, left_frame_arg, out_dir = sys.argv[1:6]
    sync_offset = int(sync_offset_s)

    left_fps = ffprobe_fps(left_video)
    right_fps = ffprobe_fps(right_video)
    if abs(left_fps - right_fps) > 1e-6:
        print(f"WARNING: fps mismatch left={left_fps:.4f} right={right_fps:.4f} - "
              f"sync_offset is defined in frames, this will be off")

    left_frame = parse_left_frame(left_frame_arg, left_fps)
    right_frame = left_frame + sync_offset

    os.makedirs(out_dir, exist_ok=True)
    left_out = os.path.join(out_dir, f"frame{left_frame}_left.png")
    right_out = os.path.join(out_dir, f"frame{right_frame}_right.png")

    print(f"left:  frame {left_frame} (t={left_frame / left_fps:.3f}s, fps={left_fps:.3f}) -> {left_out}")
    print(f"right: frame {right_frame} (t={right_frame / right_fps:.3f}s, fps={right_fps:.3f}) -> {right_out} "
          f"(sync_offset={sync_offset})")

    extract_frame(left_video, left_frame, left_fps, left_out)
    extract_frame(right_video, right_frame, right_fps, right_out)
    print("done")


if __name__ == "__main__":
    main()
