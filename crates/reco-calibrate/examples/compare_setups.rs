//! Score several setup files against the same matched feature points.
//!
//! Runs the Auto Calibrate pipeline once (same AKAZE detection, matching
//! and filtering as the GUI's Auto Calibrate), then evaluates every given
//! setup's geometry on the matched points that run produced, plus the
//! fresh result of the run itself. Answers "is a hand-tuned setup better
//! or worse than Auto Calibrate *by Auto Calibrate's own measure*?":
//!
//! - hand-tuned scores lower on the optimizer objective -> the optimizer
//!   is stuck or lacks the parameters to get there;
//! - hand-tuned scores higher -> the objective does not measure what a
//!   person sees in the stitch, and needs to change.
//!
//! Plane-y warps (`ground_tilt_*`, `top_tilt_*` with their band widths)
//! are applied exactly as `fisheye.wgsl` does at render time, so manual
//! tilt corrections count towards the score. `seam_offset` and the
//! `framing` tilt/roll are not part of the point geometry (they move the
//! seam / the output view, not where the two cameras' content lands
//! relative to each other) and are reported but not scored.
//!
//! Usage:
//!   cargo run --release -p reco-calibrate --features io --example compare_setups -- \
//!     <left.mp4> <right.mp4> <num_frames> <start_secs> <setup.json>...
//!
//! `start_secs` mirrors the GUI's anchor: frames are sampled from there to
//! the end of the (first) file. Lenses and sync offset come from the first
//! setup, so every setup is scored on identical undistorted points.
//!
//! Optional detection overrides (environment variables): `RECO_CAL_MAX_KP`,
//! `RECO_CAL_DETECT_MAX_WIDTH` (0 = full resolution), `RECO_CAL_Y_MIN`,
//! `RECO_CAL_Y_MAX`, `RECO_CAL_LOWE`, and `RECO_CAL_END_SECS` (stop
//! sampling at this time instead of the end of the file).

use std::path::Path;
use std::sync::atomic::AtomicBool;

use reco_calibrate::CalibrationConfig;
use reco_calibrate::geometry::{self, OptParams};
use reco_calibrate::types::MatchedPoint;
use reco_calibrate::video::{CalibrateVideosOptions, calibrate_videos};
use reco_core::calibration::Calibration;

/// Ramp start shared by the ground and top warps (`fisheye.wgsl`).
const BAND_START: f64 = 0.08;
/// Optimizer defaults (`OptimizerConfig::default`), so the objective
/// printed here is the exact number Nelder-Mead minimizes.
const SEAM_SIGMA: f64 = 0.08;
const SEAM_SIGMA_Y: f64 = 0.08;
const TRIM_FRACTION: f64 = 0.3;

fn smoothstep(edge0: f64, edge1: f64, x: f64) -> f64 {
    let t = ((x - edge0) / (edge1 - edge0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// `fisheye.wgsl` `band_limited_ground_warp` (t > 0, near field).
fn ground_warp(t: f64, c: f64, k: f64, band_full: f64) -> f64 {
    if t <= 0.0 || c == 0.0 {
        return t;
    }
    let w = smoothstep(BAND_START, band_full.max(BAND_START + 0.01), t);
    t + w * (geometry::warp_ground_y(t, c, k) - t)
}

/// `fisheye.wgsl` `band_limited_top_warp` (t < 0, top of frame).
fn top_warp(t: f64, c: f64, k: f64, band_full: f64) -> f64 {
    if t >= 0.0 || c == 0.0 {
        return t;
    }
    let w = smoothstep(BAND_START, band_full.max(BAND_START + 0.01), -t);
    t + w * (geometry::warp_ground_y(t, c, k) - t)
}

/// Apply a setup's plane-y warps to the raw matched points. The x-plane
/// (`MatchedPoint::left`) holds the right camera (`lenses[1]`), the
/// z-plane (`MatchedPoint::right`) the left camera (`lenses[0]`).
fn warp_points(points: &[MatchedPoint], cal: &Calibration) -> Vec<MatchedPoint> {
    let t = &cal.topology;
    let k_x = cal.lenses[1].ground_tilt_k();
    let k_z = cal.lenses[0].ground_tilt_k();
    points
        .iter()
        .map(|mp| {
            let mut out = *mp;
            let y = ground_warp(mp.left[1], t.ground_tilt_x, k_x, t.ground_tilt_band_width);
            out.left[1] = top_warp(y, t.top_tilt_x, k_x, t.top_tilt_band_width);
            let y = ground_warp(mp.right[1], t.ground_tilt_z, k_z, t.ground_tilt_band_width);
            out.right[1] = top_warp(y, t.top_tilt_z, k_z, t.top_tilt_band_width);
            out
        })
        .collect()
}

fn opt_params(cal: &Calibration) -> OptParams {
    let t = &cal.topology;
    OptParams {
        x_ty: t.x_ty,
        intersect: t.intersect,
        cam_d: cal.framing.axis_offset,
        x_rz: t.x_rz,
        z_rx: t.z_rx,
        z_rz: Some(t.z_rz),
        x_rx: Some(t.x_rx),
        // Already applied by `warp_points`, with the setup's own band width.
        ground_tilt_x: None,
        ground_tilt_z: None,
        k_x: 1.0,
        k_z: 1.0,
    }
}

fn percentile(sorted: &[f64], p: f64) -> f64 {
    if sorted.is_empty() {
        return f64::NAN;
    }
    let idx = ((sorted.len() - 1) as f64 * p).round() as usize;
    sorted[idx]
}

/// Per-point misalignment in source pixels: `per_point_reprojection_error`
/// is the sum of two squared plane-unit distances (both ray directions);
/// plane width 1.0 spans the source image width.
fn pixel_errors(points: &[MatchedPoint], params: &OptParams, width_px: f64) -> Vec<f64> {
    geometry::per_point_reprojection_error(points, params)
        .into_iter()
        .map(|e| (e / 2.0).sqrt() * width_px)
        .collect()
}

fn score(name: &str, cal: &Calibration, points: &[MatchedPoint], width_px: f64) {
    let warped = warp_points(points, cal);
    let params = opt_params(cal);
    let objective = geometry::trimmed_seam_weighted_reprojection_error(
        &warped,
        &params,
        SEAM_SIGMA,
        SEAM_SIGMA_Y,
        TRIM_FRACTION,
    );
    let errs = pixel_errors(&warped, &params, width_px);

    println!("\n== {name}");
    println!(
        "   optimizer objective (lower = better): {objective:.6e}   [{} points]",
        points.len()
    );
    let mut all = errs.clone();
    all.sort_by(f64::total_cmp);
    println!(
        "   all points   px: median {:6.2}  p75 {:6.2}  p90 {:6.2}",
        percentile(&all, 0.5),
        percentile(&all, 0.75),
        percentile(&all, 0.9)
    );

    // Bands by the left camera's plane y (z-plane), matching the warp
    // convention: t < -0.08 top of frame, |t| <= 0.08 horizon band,
    // t > 0.08 near field.
    let bands = [
        ("top     ", f64::NEG_INFINITY, -BAND_START),
        ("horizon ", -BAND_START, BAND_START),
        ("near    ", BAND_START, f64::INFINITY),
    ];
    for (label, lo, hi) in bands {
        let mut v: Vec<f64> = points
            .iter()
            .zip(&errs)
            .filter(|(mp, _)| mp.right[1] >= lo && mp.right[1] < hi)
            .map(|(_, e)| *e)
            .collect();
        v.sort_by(f64::total_cmp);
        println!(
            "   {label} px: median {:6.2}  p75 {:6.2}  p90 {:6.2}   [{} points]",
            percentile(&v, 0.5),
            percentile(&v, 0.75),
            percentile(&v, 0.9),
            v.len()
        );
    }
    let t = &cal.topology;
    println!(
        "   (not scored) seam_offset {:.4}  framing tilt {:.4} roll {:.4}",
        t.seam_offset, cal.framing.tilt, cal.framing.roll
    );
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 6 {
        eprintln!(
            "Usage: {} <left.mp4> <right.mp4> <num_frames> <start_secs> <setup.json>...",
            args[0]
        );
        std::process::exit(1);
    }
    let num_frames: usize = args[3].parse().expect("num_frames must be an integer");
    let start_secs: f64 = args[4].parse().expect("start_secs must be a number");
    let setups: Vec<(String, Calibration)> = args[5..]
        .iter()
        .map(|p| {
            let cal = Calibration::from_file(Path::new(p))
                .unwrap_or_else(|e| panic!("cannot load setup {p}: {e}"));
            let name = Path::new(p)
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| p.clone());
            (name, cal)
        })
        .collect();
    let reference = &setups[0].1;

    // Same knobs the GUI's Auto Calibrate passes by default, optionally
    // overridden per run to try detection variants on the same footage.
    let mut config = CalibrationConfig {
        num_frames,
        skip_start_secs: start_secs,
        ..Default::default()
    };
    let env_f64 = |key: &str| std::env::var(key).ok().and_then(|v| v.parse::<f64>().ok());
    if let Some(v) = env_f64("RECO_CAL_MAX_KP") {
        config.akaze.max_keypoints = v as usize;
    }
    if let Some(v) = env_f64("RECO_CAL_DETECT_MAX_WIDTH") {
        config.akaze.detect_max_width = v as u32;
    }
    if let Some(v) = env_f64("RECO_CAL_Y_MIN") {
        config.akaze.detect_y_min = v;
    }
    if let Some(v) = env_f64("RECO_CAL_Y_MAX") {
        config.akaze.detect_y_max = v;
    }
    if let Some(v) = env_f64("RECO_CAL_LOWE") {
        config.matching.lowe_ratio = v;
    }
    if let Some(end_secs) = env_f64("RECO_CAL_END_SECS") {
        let probe = reco_io::ffmpeg::calibration_io::probe_video(Path::new(&args[1]))
            .expect("cannot probe left video");
        let duration = probe.total_frames as f64 / probe.fps;
        config.skip_end_secs = (duration - end_secs).max(0.0);
    }
    eprintln!(
        "config: max_keypoints {} detect_max_width {} detect_y [{:.2}, {:.2}] lowe {:.2}",
        config.akaze.max_keypoints,
        config.akaze.detect_max_width,
        config.akaze.detect_y_min,
        config.akaze.detect_y_max,
        config.matching.lowe_ratio
    );
    let options = CalibrateVideosOptions {
        config: Some(config),
        left_params: Some(reference.lenses[0].clone()),
        right_params: Some(reference.lenses[1].clone()),
        sync_offset: Some(reference.sync_offset),
        ..Default::default()
    };

    eprintln!("Running Auto Calibrate on {num_frames} frame pairs from {start_secs:.0}s ...");
    let result = calibrate_videos(
        Path::new(&args[1]),
        Path::new(&args[2]),
        options,
        &mut |p| eprintln!("  [{:?}] {}", p.step, p.detail),
        &AtomicBool::new(false),
    )
    .expect("calibration run failed");

    for (i, f) in result.per_frame.iter().enumerate() {
        println!(
            "frame {i}: keypoints L/R {}/{}  ratio {}  spatial {}  ransac {}  kept {}",
            f.keypoints_left,
            f.keypoints_right,
            f.post_ratio_test,
            f.post_spatial_filter,
            f.post_ransac,
            f.points.len()
        );
    }

    let points: Vec<MatchedPoint> = result
        .per_frame
        .iter()
        .flat_map(|f| f.points.iter().copied())
        .collect();
    let width_px = reference.lenses[0].width as f64;
    println!(
        "Matched points: {} over {} frame pairs (residual {:.6e})",
        points.len(),
        result.frames_used,
        result.residual_error
    );

    for (name, cal) in &setups {
        score(name, cal, &points, width_px);
    }
    score("auto (this run)", &result.calibration, &points, width_px);

    // Ablation: take the first setup and swap in the second setup's value
    // for one parameter at a time, to see which parameter accounts for
    // the gap between them.
    if setups.len() >= 2 {
        let (a, b) = (&setups[0].1, &setups[1].1);
        let median_px = |cal: &Calibration| {
            let mut e = pixel_errors(&warp_points(&points, cal), &opt_params(cal), width_px);
            e.sort_by(f64::total_cmp);
            percentile(&e, 0.5)
        };
        type Swap = fn(&mut Calibration, &Calibration);
        let swaps: [(&str, Swap); 9] = [
            ("intersect", |c, o| {
                c.topology.intersect = o.topology.intersect
            }),
            ("axis_offset", |c, o| {
                c.framing.axis_offset = o.framing.axis_offset
            }),
            ("x_ty", |c, o| c.topology.x_ty = o.topology.x_ty),
            ("x_rz", |c, o| c.topology.x_rz = o.topology.x_rz),
            ("z_rx", |c, o| c.topology.z_rx = o.topology.z_rx),
            ("x_rx", |c, o| c.topology.x_rx = o.topology.x_rx),
            ("z_rz", |c, o| c.topology.z_rz = o.topology.z_rz),
            ("ground_tilt", |c, o| {
                c.topology.ground_tilt_x = o.topology.ground_tilt_x;
                c.topology.ground_tilt_z = o.topology.ground_tilt_z;
            }),
            ("top_tilt", |c, o| {
                c.topology.top_tilt_x = o.topology.top_tilt_x;
                c.topology.top_tilt_z = o.topology.top_tilt_z;
            }),
        ];
        println!(
            "\nAblation: '{}' with one parameter taken from '{}' (median px, base {:.2})",
            setups[0].0,
            setups[1].0,
            median_px(a)
        );
        for (name, swap) in swaps {
            let mut c = a.clone();
            swap(&mut c, b);
            println!("   {name:12} -> {:6.2}", median_px(&c));
        }
    }

    // Every point with its position and per-setup error, to see *where*
    // the setups disagree (columns follow the order setups were given).
    println!("\nleft_cam(z) x,y | right_cam(x) x,y | px error per setup");
    let per_setup: Vec<Vec<f64>> = setups
        .iter()
        .map(|(_, cal)| pixel_errors(&warp_points(&points, cal), &opt_params(cal), width_px))
        .collect();
    for (i, mp) in points.iter().enumerate() {
        let errs: Vec<String> = per_setup.iter().map(|e| format!("{:8.2}", e[i])).collect();
        println!(
            "{:7.3} {:7.3} | {:7.3} {:7.3} | {}",
            mp.right[0],
            mp.right[1],
            mp.left[0],
            mp.left[1],
            errs.join(" ")
        );
    }

    let fresh = &result.calibration;
    println!(
        "\nThis run's fit: intersect {:.4} axis_offset {:.4} x_ty {:.4} x_rz {:.4} z_rx {:.4} roll {:.4}",
        fresh.topology.intersect,
        fresh.framing.axis_offset,
        fresh.topology.x_ty,
        fresh.topology.x_rz,
        fresh.topology.z_rx,
        fresh.framing.roll
    );
}
