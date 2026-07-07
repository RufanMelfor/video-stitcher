#!/usr/bin/env python3
"""Turn hand-clicked correspondences into a deployable reco match.json.

Runs reco-calibrate's `optimize_points` solver on the clicked pairs, then
assembles a match.json using the given lens profile + rig tilt.

Usage:
  clicks_to_match.py <clicks.json> <lens_profile.json> <out_match.json> [rig_tilt_deg=19]

The clicks.json must already be in optimizer/plane format.
"""
import json, subprocess, sys, math, re, os

def num(text, name, default=0.0):
    m = re.search(re.escape(name) + r":\s*([-0-9.eE]+)", text)
    return float(m.group(1)) if m else default

def main():
    if len(sys.argv) < 4:
        print(__doc__); sys.exit(1)
    clicks, lens_path, out = sys.argv[1], sys.argv[2], sys.argv[3]
    rig_deg = float(sys.argv[4]) if len(sys.argv) > 4 else 19.0
    binp = r"D:\cargo-target\video-stitcher\release\examples\optimize_points.exe"
    if not os.path.exists(binp):
        print("Change the path in the Python file first!! Then, build it: cargo build --release -p reco-calibrate --example optimize_points")
        sys.exit(1)

    r = subprocess.run([binp, clicks], capture_output=True, text=True)
    err = r.stderr
    params = {
        "cameraAxisOffset": num(err, "cameraAxisOffset"),
        "intersect":        num(err, "intersect"),
        "xTy":              num(err, "xTy"),
        "xRz":              num(err, "xRz"),
        "zRx":              num(err, "zRx"),
        "xRx":              0.0,
        "zRz":              num(err, "zRz"),
    }
    print(f"optimizer: intersect={params['intersect']:.4f} "
          f"axisOffset={params['cameraAxisOffset']:.4f} residual={num(err,'residual'):.6f}")

    lp = json.load(open(lens_path))
    fe = lp["fisheye_params"]; cm = fe["camera_matrix"]; cd = lp["calib_dimension"]
    uni = {"width": cd["w"], "height": cd["h"],
           "fx": cm[0][0], "fy": cm[1][1], "cx": cm[0][2], "cy": cm[1][2],
           "d": fe["distortion_coeffs"]}
    match = {
        "left_uniforms": uni,
        "right_uniforms": dict(uni),
        "params": params,
        "rig_tilt": math.radians(rig_deg),
        "rig_roll": 0.0,
        "sync_offset": 0,
        "field_roi": {"left": [], "right": []},
    }
    json.dump(match, open(out, "w"), indent=2)
    print(f"wrote {out} (rig_tilt {rig_deg} deg)")

if __name__ == "__main__":
    main()
