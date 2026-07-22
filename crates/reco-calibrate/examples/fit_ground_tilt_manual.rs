//! Phase 1 validation harness for fitting `ground_tilt_x`/`ground_tilt_z`
//! (see `crates/reco-calibrate/src/geometry.rs` and `FRICTION.md` points
//! 8-11) against manually-clicked field-line correspondences instead of
//! automatic AKAZE/photometric measurement - see `crates/reco-calibrate/
//! src/line_seam.rs` for the motivation and the pixel-to-seam-point math.
//!
//! Unlike `fit_ground_tilt.rs` (which grid-searches against sparse,
//! low-confidence AKAZE near-field points), this harness's input points
//! come from [`reco_calibrate::line_seam::ManualLinesFile`]: for each real
//! field line, the user clicks two points along it in the left camera's
//! own GPU-undistorted frame and two points along its continuation in the
//! right camera's own frame (`reco calibrate --debug-dir <dir> ...`
//! already dumps per-camera undistorted debug frames to click on - see
//! FRICTION.md point 2). Each line is extrapolated to its own camera's
//! seam column and turned into one high-trust `MatchedPoint`, so a
//! two-line input yields two points - not two-per-line - matching what a
//! 2-parameter (`ground_tilt_x`, `ground_tilt_z`) fit actually needs.
//!
//! With only 1 line (1 point), fitting 2 independent parameters is
//! under-determined - this harness detects that and falls back to a
//! single shared parameter (`cx = cz`), the same fallback shape as
//! FRICTION.md point 8 before point 10 split it into two.
//!
//! The base 5-7 placement parameters are loaded from an existing
//! `match.json` and held completely fixed - this harness only ever
//! searches `ground_tilt_x`/`ground_tilt_z`, via the same style of plain
//! grid search `fit_ground_tilt.rs` already uses (cheap and transparent
//! for a 1-2 parameter fit over a handful of points - no need for argmin
//! here).
//!
//! Usage:
//! ```text
//! cargo run --release -p reco-calibrate --example fit_ground_tilt_manual -- \
//!   <match.json> <manual_lines.json> [--matched-points <akaze_matched_points.json>]
//! ```
//!
//! `manual_lines.json` format:
//! ```json
//! {
//!   "lines": [
//!     { "left":  { "p1": [px, py], "p2": [px, py] },
//!       "right": { "p1": [px, py], "p2": [px, py] } }
//!   ]
//! }
//! ```
//! Points are pixel coordinates in each camera's own GPU-undistorted
//! frame (the same frame AKAZE detects on) - NOT the raw fisheye source.
//!
//! `crates/reco-calibrate/tools/manual_line_picker.html` is a small,
//! dependency-free static page (open directly in any browser, nothing
//! uploaded anywhere) for clicking the two points per line on the
//! `--debug-dir`-dumped PNGs and exporting exactly this JSON format.

use reco_calibrate::geometry::{self, OptParams};
use reco_calibrate::line_seam::ManualLinesFile;
use reco_calibrate::types::{FrameMatches, MatchedPoint};
use reco_core::calibration::{Calibration, Framing, Topology};

/// Plane-y magnitude above which a point counts as "near-field" - matches
/// `fit_ground_tilt.rs` and this project's `sigma_y` scale convention.
const NEAR_FIELD_THRESHOLD: f64 = 0.08;

/// Grid search range and step for the independent-parameter (2-line+) case.
const GRID_RANGE: f64 = 0.3;
const GRID_STEP: f64 = 0.005;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 3 {
        eprintln!(
            "Usage: {} <match.json> <manual_lines.json> [--matched-points <akaze_matched_points.json>]",
            args[0]
        );
        std::process::exit(1);
    }
    let match_json_path = &args[1];
    let manual_lines_path = &args[2];
    let akaze_points_path = flag_value(&args[3..], "--matched-points");

    let cal: Calibration = {
        let s = std::fs::read_to_string(match_json_path)
            .unwrap_or_else(|e| panic!("failed to read {match_json_path}: {e}"));
        serde_json::from_str(&s)
            .unwrap_or_else(|e| panic!("failed to parse {match_json_path} as Calibration: {e}"))
    };
    let base_topology = cal.topology.clone();
    let base_framing = cal.framing.clone();

    let manual: ManualLinesFile = {
        let s = std::fs::read_to_string(manual_lines_path)
            .unwrap_or_else(|e| panic!("failed to read {manual_lines_path}: {e}"));
        serde_json::from_str(&s).unwrap_or_else(|e| {
            panic!("failed to parse {manual_lines_path} as ManualLinesFile: {e}")
        })
    };

    let k_x = cal.lenses[1].ground_tilt_k(); // x-plane = right camera
    let k_z = cal.lenses[0].ground_tilt_k(); // z-plane = left camera
    eprintln!("Focal-scale constants from match.json: k_x={k_x:.4} k_z={k_z:.4}");

    let manual_points = manual.to_matched_points(
        cal.lenses[0].width,
        cal.lenses[0].height,
        cal.lenses[1].width,
        cal.lenses[1].height,
        base_topology.intersect,
    );
    eprintln!(
        "Loaded {} manually-clicked line(s) -> {} seam-continuity point(s)",
        manual.lines.len(),
        manual_points.len()
    );
    if manual_points.is_empty() {
        eprintln!("no lines in {manual_lines_path} - nothing to fit");
        std::process::exit(1);
    }

    let base_err = summed_reprojection_error(
        &manual_points,
        &base_topology,
        &base_framing,
        0.0,
        0.0,
        k_x,
        k_z,
    );
    eprintln!(
        "\nBaseline (ground_tilt_x=ground_tilt_z=0.0): manual continuity error = {base_err:.8}"
    );

    let (best_cx, best_cz, best_err) = if manual_points.len() < 2 {
        eprintln!(
            "\nWARNING: only {} point(s) - fitting 2 independent parameters (ground_tilt_x, \
             ground_tilt_z) from 1 constraint is under-determined. Falling back to a single \
             shared parameter (cx = cz), same fallback FRICTION.md point 8 used before point 10 \
             split it into two.",
            manual_points.len()
        );
        let (c, err) = grid_search_1d(&manual_points, &base_topology, &base_framing, k_x, k_z);
        (c, c, err)
    } else {
        grid_search_2d(&manual_points, &base_topology, &base_framing, k_x, k_z)
    };

    // A fit landing exactly on the grid edge isn't a converged answer - it
    // means the true minimum (if one exists at all) lies outside the range
    // that was searched, or the objective improves without bound (a
    // degenerate fit - see FRICTION.md point 19, where this was first
    // observed on real data with only 1 clicked line). Either way, silently
    // reporting the boundary value as "the" result is misleading.
    let pinned = |c: f64| (c.abs() - GRID_RANGE).abs() < GRID_STEP;
    if pinned(best_cx) || pinned(best_cz) {
        eprintln!(
            "\n*** WARNING: fitted value hit the search boundary (+/-{GRID_RANGE}) - this is NOT \
             a converged result. Re-run with a wider GRID_RANGE before trusting this number, or \
             add more independent clicked lines (a single-line fit has nothing to stop it \
             reaching for whatever value zeroes that one constraint, however implausible). ***"
        );
    }

    eprintln!("\n=== Fitted result ===");
    eprintln!(
        "ground_tilt_x = {best_cx:+.4} (theta = {:+.1} deg)",
        best_cx.atan().to_degrees()
    );
    eprintln!(
        "ground_tilt_z = {best_cz:+.4} (theta = {:+.1} deg)",
        best_cz.atan().to_degrees()
    );
    eprintln!(
        "manual continuity error: {base_err:.8} -> {best_err:.8} ({:+.1}%)",
        100.0 * (best_err - base_err) / base_err.max(1e-12)
    );

    if let Some(path) = akaze_points_path {
        let frames: Vec<FrameMatches> = {
            let s = std::fs::read_to_string(path)
                .unwrap_or_else(|e| panic!("failed to read {path}: {e}"));
            serde_json::from_str(&s)
                .unwrap_or_else(|e| panic!("failed to parse {path} as Vec<FrameMatches>: {e}"))
        };
        let akaze_points: Vec<MatchedPoint> =
            frames.iter().flat_map(|f| f.points.clone()).collect();
        let is_near = |p: &MatchedPoint| {
            p.left[1].abs() > NEAR_FIELD_THRESHOLD || p.right[1].abs() > NEAR_FIELD_THRESHOLD
        };
        let far: Vec<MatchedPoint> = akaze_points
            .iter()
            .copied()
            .filter(|p| !is_near(p))
            .collect();
        let far_before =
            summed_reprojection_error(&far, &base_topology, &base_framing, 0.0, 0.0, k_x, k_z);
        let far_after = summed_reprojection_error(
            &far,
            &base_topology,
            &base_framing,
            best_cx,
            best_cz,
            k_x,
            k_z,
        );
        eprintln!(
            "\nFar-field cross-check ({} AKAZE far-field points from {path}):",
            far.len()
        );
        eprintln!(
            "  before: {far_before:.8}  after: {far_after:.8}  ({:+.4}% - should be ~0 by \
             construction, band_limited_ground_warp is provably identity beyond \
             GROUND_TILT_BAND_FULL)",
            100.0 * (far_after - far_before) / far_before.max(1e-12)
        );
    } else {
        eprintln!(
            "\n(no --matched-points given - skipping far-field cross-check; \
             band_limited_ground_warp guarantees far-field is untouched by construction, but \
             verifying on real data is this project's own standing rule - see FRICTION.md)"
        );
    }
}

#[allow(clippy::too_many_arguments)]
fn summed_reprojection_error(
    points: &[MatchedPoint],
    base_topology: &Topology,
    base_framing: &Framing,
    cx: f64,
    cz: f64,
    k_x: f64,
    k_z: f64,
) -> f64 {
    if points.is_empty() {
        return 0.0;
    }
    let params = OptParams {
        x_ty: base_topology.x_ty,
        intersect: base_topology.intersect,
        cam_d: base_framing.axis_offset,
        x_rz: base_topology.x_rz,
        z_rx: base_topology.z_rx,
        z_rz: Some(base_topology.z_rz),
        x_rx: Some(base_topology.x_rx),
        ground_tilt_x: if cx == 0.0 { None } else { Some(cx) },
        ground_tilt_z: if cz == 0.0 { None } else { Some(cz) },
        k_x,
        k_z,
    };
    geometry::per_point_reprojection_error(points, &params)
        .iter()
        .sum()
}

fn grid_search_2d(
    points: &[MatchedPoint],
    base_topology: &Topology,
    base_framing: &Framing,
    k_x: f64,
    k_z: f64,
) -> (f64, f64, f64) {
    let steps = ((2.0 * GRID_RANGE / GRID_STEP) as i64) + 1;
    let mut best = (0.0, 0.0, f64::INFINITY);
    for i in 0..steps {
        let cx = -GRID_RANGE + i as f64 * GRID_STEP;
        for j in 0..steps {
            let cz = -GRID_RANGE + j as f64 * GRID_STEP;
            let err =
                summed_reprojection_error(points, base_topology, base_framing, cx, cz, k_x, k_z);
            if err < best.2 {
                best = (cx, cz, err);
            }
        }
    }
    best
}

fn grid_search_1d(
    points: &[MatchedPoint],
    base_topology: &Topology,
    base_framing: &Framing,
    k_x: f64,
    k_z: f64,
) -> (f64, f64) {
    let steps = ((2.0 * GRID_RANGE / GRID_STEP) as i64) + 1;
    let mut best = (0.0, f64::INFINITY);
    for i in 0..steps {
        let c = -GRID_RANGE + i as f64 * GRID_STEP;
        let err = summed_reprojection_error(points, base_topology, base_framing, c, c, k_x, k_z);
        if err < best.1 {
            best = (c, err);
        }
    }
    best
}

fn flag_value<'a>(args: &'a [String], name: &str) -> Option<&'a str> {
    args.iter()
        .position(|a| a == name)
        .and_then(|i| args.get(i + 1))
        .map(String::as_str)
}
