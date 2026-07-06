//! Phase 1 validation harness for the experimental, band-limited,
//! per-plane `ground_tilt_x`/`ground_tilt_z` parameters (see
//! `crates/reco-calibrate/FRICTION.md`).
//!
//! Loads a real `reco calibrate --debug-dir <dir> ...` dump
//! (`matched_points.json`, a serialized `Vec<FrameMatches>`), then grid-
//! searches candidate `(ground_tilt_x, ground_tilt_z)` pairs, pre-warping
//! each plane's own plane-y coordinate independently via
//! `geometry::band_limited_ground_warp` and re-fitting the existing,
//! completely unmodified 5-parameter Nelder-Mead optimizer against the
//! warped points. Pre-warping the input is mathematically identical to
//! `apply_transformations` applying the same warp internally, so this
//! validates the new parameters without touching the optimizer/config/CLI
//! at all.
//!
//! This is the second iteration of this experiment: the first used one
//! *shared* scalar for both planes. Now that band-limiting has removed
//! the `cam_d`-aliasing failure mode (see FRICTION.md), letting each plane
//! have its own correction may extract more real signal, since the two
//! planes are not otherwise assumed symmetric (different rotations,
//! different overlap-side offsets).
//!
//! Reports, per candidate pair: the 5 fitted core parameters and trimmed
//! reprojection error split into a near-field bucket (the one that's
//! currently broken) and a far-field bucket - a lower aggregate residual
//! alone is not treated as success (see FRICTION.md's homography-overfit
//! false positive). Only the best few candidates are printed in full; the
//! whole grid is scanned for the summary comparison.
//!
//! Usage: cargo run --release -p reco-calibrate --example fit_ground_tilt -- <matched_points.json>
//!
//! `K` below is the focal-scale constant (`fy / (2 * width)`, see
//! `geometry::warp_ground_y`'s doc comment) for this project's DJI Osmo
//! Action 4 rig: `fy=1457.07`, `width=3840` (both cameras - same model,
//! same resolution, confirmed identical via `reco calibrate`'s own log:
//! "embedded lens (from ClipMeta): focal=1457.07 ..." printed once per
//! side). If this harness is pointed at a different rig, update `K`
//! (or split into `K_X`/`K_Z` if the two cameras differ).

use reco_calibrate::geometry::{self, OptParams};
use reco_calibrate::optimizer;
use reco_calibrate::types::{CalibrationConfig, FrameMatches, MatchedPoint};
use reco_core::calibration::PlaneLayout;

/// Plane-y magnitude above which a point counts as "near-field" - matches
/// the `sigma_y` scale used throughout this project's seam weighting.
const NEAR_FIELD_THRESHOLD: f64 = 0.08;

/// Focal-scale constant for both planes on this rig (see module doc).
const K: f64 = 1457.07 / (2.0 * 3840.0);

struct Candidate {
    cx: f64,
    cz: f64,
    near_err: f64,
    far_err: f64,
    layout: PlaneLayout,
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 2 {
        eprintln!("Usage: {} <matched_points.json>", args[0]);
        std::process::exit(1);
    }

    let json = std::fs::read_to_string(&args[1])
        .unwrap_or_else(|e| panic!("failed to read {}: {e}", args[1]));
    let frames: Vec<FrameMatches> = serde_json::from_str(&json)
        .unwrap_or_else(|e| panic!("failed to parse {} as Vec<FrameMatches>: {e}", args[1]));

    let points: Vec<MatchedPoint> = frames.iter().flat_map(|f| f.points.clone()).collect();
    eprintln!(
        "Loaded {} points from {} frames",
        points.len(),
        frames.len()
    );
    if points.len() < 6 {
        eprintln!("too few points to fit anything meaningful - aborting");
        std::process::exit(1);
    }

    let is_near = |p: &MatchedPoint| {
        p.left[1].abs() > NEAR_FIELD_THRESHOLD || p.right[1].abs() > NEAR_FIELD_THRESHOLD
    };
    let near: Vec<MatchedPoint> = points.iter().copied().filter(is_near).collect();
    let far: Vec<MatchedPoint> = points.iter().copied().filter(|p| !is_near(p)).collect();
    eprintln!(
        "  near-field points (|y| > {NEAR_FIELD_THRESHOLD}): {}",
        near.len()
    );
    eprintln!("  far-field points: {}", far.len());
    if near.is_empty() {
        eprintln!(
            "\nWARNING: no near-field points in this dataset - ground_tilt has nothing to fit \
             against. Re-run `reco calibrate` with more --frames (FRICTION.md point 6 found 15 \
             frames were needed to reach near-field matches on this footage)."
        );
    }

    let config = CalibrationConfig::default();

    eprintln!("\n=== Baseline (today's flat-plane model, cx=cz=0.0) ===");
    let (baseline_layout, baseline_residual) =
        optimizer::optimize(&points, &config).expect("baseline optimizer should converge");
    let (base_near, base_far) = split_residual(&baseline_layout, &near, &far, 0.0, 0.0);
    print_result(
        "cx=+0.00 cz=+0.00 (baseline)",
        &baseline_layout,
        base_near,
        base_far,
    );
    eprintln!("  (production trimmed_seam_weighted residual: {baseline_residual:.6})");

    eprintln!("\n=== 2D grid search over (ground_tilt_x, ground_tilt_z) ===");
    let steps: Vec<f64> = (-5..=5).map(|i| i as f64 * 0.04).collect(); // -0.20..=0.20 step 0.04

    let mut results: Vec<Candidate> = Vec::new();
    let mut failed = 0usize;

    for &cx in &steps {
        for &cz in &steps {
            if cx == 0.0 && cz == 0.0 {
                continue;
            }
            let warped: Vec<MatchedPoint> = points
                .iter()
                .map(|p| MatchedPoint {
                    left: [
                        p.left[0],
                        geometry::band_limited_ground_warp(p.left[1], cx, K),
                    ],
                    right: [
                        p.right[0],
                        geometry::band_limited_ground_warp(p.right[1], cz, K),
                    ],
                    left_pixel_nx: p.left_pixel_nx,
                    right_pixel_nx: p.right_pixel_nx,
                })
                .collect();

            let Ok((layout, _residual)) = optimizer::optimize(&warped, &config) else {
                failed += 1;
                continue;
            };

            let (near_err, far_err) = split_residual(&layout, &near, &far, cx, cz);
            results.push(Candidate {
                cx,
                cz,
                near_err,
                far_err,
                layout,
            });
        }
    }

    eprintln!(
        "Scanned {} combinations ({} failed to converge)",
        steps.len() * steps.len() - 1,
        failed
    );

    results.sort_by(|a, b| a.near_err.partial_cmp(&b.near_err).unwrap());

    eprintln!("\nTop 10 candidates by near-field residual:");
    for cand in results.iter().take(10) {
        print_result(
            &format!("cx={:+.2} cz={:+.2}", cand.cx, cand.cz),
            &cand.layout,
            cand.near_err,
            cand.far_err,
        );
    }

    if let Some(best) = results.first() {
        eprintln!(
            "\n=== Best candidate: cx={:+.3} cz={:+.3} ===",
            best.cx, best.cz
        );
        print_result("best", &best.layout, best.near_err, best.far_err);

        eprintln!("\nCompare against baseline core parameters:");
        eprintln!(
            "  baseline: cam_d={:.4} intersect={:.4} x_ty={:.5} x_rz={:.4} z_rx={:.4}",
            baseline_layout.camera_axis_offset,
            baseline_layout.intersect,
            baseline_layout.x_ty,
            baseline_layout.x_rz,
            baseline_layout.z_rx,
        );
        eprintln!(
            "  best:     cam_d={:.4} intersect={:.4} x_ty={:.5} x_rz={:.4} z_rx={:.4}",
            best.layout.camera_axis_offset,
            best.layout.intersect,
            best.layout.x_ty,
            best.layout.x_rz,
            best.layout.z_rx,
        );

        eprintln!("\nFar-field residual range across all candidates:");
        let min_far = results
            .iter()
            .map(|c| c.far_err)
            .fold(f64::INFINITY, f64::min);
        let max_far = results
            .iter()
            .map(|c| c.far_err)
            .fold(f64::NEG_INFINITY, f64::max);
        eprintln!("  min={min_far:.6} max={max_far:.6} baseline={base_far:.6}");

        let cam_d_min = results
            .iter()
            .map(|c| c.layout.camera_axis_offset)
            .fold(f64::INFINITY, f64::min);
        let cam_d_max = results
            .iter()
            .map(|c| c.layout.camera_axis_offset)
            .fold(f64::NEG_INFINITY, f64::max);
        eprintln!("cam_d range across all candidates: min={cam_d_min:.4} max={cam_d_max:.4}");

        let bound = 0.3;
        if (cam_d_min - 0.1).abs() < 0.005 || (cam_d_max - bound).abs() < 0.005 {
            eprintln!(
                "\nWARNING: cam_d reaches its bound ({:.1}-{bound}) somewhere in this grid - \
                 this codebase's own documented tell for a bad/under-constrained fit \
                 (see FRICTION.md's XFeat-matching failure). Check which candidate this is \
                 before trusting it.",
                0.1
            );
        }
    }
}

fn print_result(label: &str, layout: &PlaneLayout, near_err: f64, far_err: f64) {
    eprintln!(
        "  {label}: near={near_err:.6} far={far_err:.6}  \
         cam_d={:.4} intersect={:.4} x_ty={:.5} x_rz={:.4} z_rx={:.4}",
        layout.camera_axis_offset, layout.intersect, layout.x_ty, layout.x_rz, layout.z_rx,
    );
}

/// Evaluate the fitted layout's (unweighted) reprojection error separately
/// on the near-field and far-field point buckets, using `ground_tilt_x =
/// Some(cx)` / `ground_tilt_z = Some(cz)` so the evaluation matches exactly
/// what the optimizer minimized against the pre-warped points (warping the
/// input once, up front, is mathematically identical to
/// `apply_transformations` warping internally).
fn split_residual(
    layout: &PlaneLayout,
    near: &[MatchedPoint],
    far: &[MatchedPoint],
    cx: f64,
    cz: f64,
) -> (f64, f64) {
    let params = OptParams {
        x_ty: layout.x_ty,
        intersect: layout.intersect,
        cam_d: layout.camera_axis_offset,
        x_rz: layout.x_rz,
        z_rx: layout.z_rx,
        z_rz: None,
        x_rx: None,
        ground_tilt_x: if cx == 0.0 { None } else { Some(cx) },
        ground_tilt_z: if cz == 0.0 { None } else { Some(cz) },
        k_x: K,
        k_z: K,
    };
    let near_err: f64 = geometry::per_point_reprojection_error(near, &params)
        .iter()
        .sum();
    let far_err: f64 = geometry::per_point_reprojection_error(far, &params)
        .iter()
        .sum();
    (near_err, far_err)
}
