//! Phase 1 validation harness for photometric (ZNCC-based direct
//! alignment) calibration refinement, as an alternative objective
//! function to the production AKAZE-feature-based one (see
//! `crates/reco-calibrate/FRICTION.md`, near-field seam misalignment).
//!
//! Renders the left and right camera's contributions SEPARATELY, via
//! `reco_core::render::single_camera::SingleCameraRenderer` (a new,
//! isolated, non-production render path added for this validation only;
//! see its module doc). Masks to the overlap region via alpha
//! thresholding (`reco_calibrate::photometric::overlap_mask_from_alpha`),
//! then maximizes patch-based ZNCC similarity there via the same argmin
//! Nelder-Mead machinery already used by `reco_calibrate::optimizer`,
//! seeded from the AKAZE-fitted layout in an existing `match.json`.
//!
//! **Near-field-banded objective (2026-07-06 revision).** The first
//! version of this harness optimized `windowed_zncc` over the *whole*
//! overlap region and found the visible near-field seam never actually
//! moved despite the aggregate ZNCC improving - because the far larger,
//! already-well-aligned far-field area dominates a uniform mean, drowning
//! out a thin near-field misalignment band. This revision optimizes
//! `reco_calibrate::photometric::windowed_zncc_banded` restricted to the
//! bottom `NEAR_FIELD_ROW_FRAC` of the frame instead, and evaluates
//! across MULTIPLE frame pairs (not just one) to guard against fitting to
//! one frame's incidental content rather than a real alignment
//! improvement. The whole-region metric is still computed and printed
//! for reference/comparison, but no longer drives the optimizer.
//!
//! Standalone example only. Does NOT touch reco-cli, rig-calib,
//! `optimizer.rs`'s production cost function, or any config/CLI flag.
//!
//! Usage:
//! ```text
//! cargo run --release -p reco-calibrate --example fit_photometric -- \
//!   <left.mp4> <right.mp4> <match.json> <output_dir> \
//!   [--frames N1,N2,N3 | --frame N] [--sync-offset N] \
//!   [--matched-points matched_points.json] \
//!   [--fov-degrees F] [--eval-width W] [--eval-height H]
//! ```
//!
//! Never deletes anything under `<output_dir>` - the first evaluated
//! frame's renders (seed and refined) are dumped as PNGs for visual
//! inspection, since the premise of this experiment is that the user's
//! eye already found a better alignment than the existing metric, so the
//! final judgment here should also be visual, not just the printed ZNCC
//! number.

use argmin::core::{CostFunction, Error as ArgminError, Executor, State};
use argmin::solver::neldermead::NelderMead;
use reco_calibrate::geometry::{self, OptParams};
use reco_calibrate::photometric::{self, OverlapMask, ZnccReport};
use reco_calibrate::types::{FrameMatches, MatchedPoint};
use reco_core::calibration::{
    Calibration, DEFAULT_BLEND_WIDTH, DEFAULT_COLOR_MATCH_BAND_WIDTH,
    DEFAULT_COLOR_MATCH_EMA_ALPHA, DEFAULT_COLOR_MATCH_ENABLED, DEFAULT_COLOR_MATCH_GRID_COLS,
    DEFAULT_COLOR_MATCH_GRID_ROWS, DEFAULT_COLOR_MATCH_INTERVAL_FRAMES,
    DEFAULT_COLOR_MATCH_MAX_CHROMA_OFFSET, DEFAULT_COLOR_MATCH_MAX_Y_OFFSET,
    DEFAULT_TILT_BAND_WIDTH, Framing, Lens, Topology,
};
use reco_core::gpu::GpuContext;
use reco_core::render::scene::SceneGeometry;
use reco_core::render::single_camera::SingleCameraRenderer;

/// Bounds for the 5 core parameters, in the same order and with the same
/// values as `reco_calibrate::optimizer`'s (private) `BOUNDS_5`: `[cam_d,
/// intersect, x_ty, x_rz, z_rx]`. Duplicated here (not exported by that
/// module) so this harness's bound-pegging check uses the exact same
/// production tell for a bad/under-constrained fit.
const BOUNDS_5: [(f64, f64); 5] = [
    (0.1, 0.30),
    (0.0, 1.0),
    (-0.1, 0.1),
    (-0.3, 0.3),
    (-0.3, 0.3),
];

/// Overlap mask alpha threshold. Alpha is effectively binary (0 or 255)
/// given `blend_width = 0.0` in `SingleCameraRenderer` - see its module
/// doc - so any mid-range threshold works.
const ALPHA_THRESHOLD: u8 = 127;
/// ZNCC patch size in pixels.
const PATCH_SIZE: u32 = 16;
/// Minimum per-patch luma variance to trust a ZNCC score (guards flat/
/// textureless patches from a degenerate or unstable result).
const MIN_PATCH_VARIANCE: f32 = 1e-5;
/// Minimum overlap coverage fraction; below this the optimizer is
/// probably exploiting a degenerate (near-empty) overlap region.
const MIN_OVERLAP_FRACTION: f64 = 0.05;
/// Minimum valid (non-degenerate) patch count for the same reason.
const MIN_VALID_PATCHES: usize = 8;
/// Large but finite penalty for candidate parameters that collapse the
/// overlap region - keeps the optimizer working with real gradients
/// elsewhere in the simplex rather than immediately diverging.
const NO_OVERLAP_PENALTY: f64 = 10.0;

/// Relative parameter delta (seed vs. refined) above which we print a
/// suspicion warning - a large swing likely means the photometric
/// objective is aliasing information (e.g. rewarding grass-texture
/// repetition) rather than finding a genuinely better alignment.
const SUSPICIOUS_RELATIVE_DELTA: f64 = 0.15;

/// Near-field bucket threshold, matching `examples/fit_ground_tilt.rs`
/// and this project's `sigma_y` convention. Used only by the (separate)
/// AKAZE-residual cross-check.
const NEAR_FIELD_THRESHOLD: f64 = 0.08;

/// Row fraction (from the top) at which the near-field ZNCC band starts,
/// passed to `photometric::windowed_zncc_banded`. `0.75` restricts
/// scoring to roughly the bottom quarter of the rendered frame - visually
/// confirmed (via the seed/refined overlap-mask dumps from the first
/// revision of this harness) to cover the widest part of the overlap
/// "hourglass," where the near-field seam step is visible.
const NEAR_FIELD_ROW_FRAC: f32 = 0.75;

fn main() {
    reco_io::init();

    let args: Vec<String> = std::env::args().collect();
    if args.len() < 5 {
        eprintln!(
            "Usage: {} <left.mp4> <right.mp4> <match.json> <output_dir> \
             [--frames N1,N2,N3 | --frame N] [--sync-offset N] [--matched-points path] \
             [--fov-degrees F] [--eval-width W] [--eval-height H]",
            args[0]
        );
        std::process::exit(1);
    }

    let left_path = &args[1];
    let right_path = &args[2];
    let match_json_path = &args[3];
    let output_dir = &args[4];
    let flags = &args[5..];

    let left_frame_indices: Vec<u64> = if let Some(list) = flag_value(flags, "--frames") {
        list.split(',')
            .map(|s| {
                s.trim()
                    .parse()
                    .expect("--frames must be a comma-separated list of u64")
            })
            .collect()
    } else {
        vec![
            flag_value(flags, "--frame")
                .and_then(|s| s.parse().ok())
                .unwrap_or(300),
        ]
    };
    let sync_offset: u64 = flag_value(flags, "--sync-offset")
        .and_then(|s| s.parse().ok())
        .unwrap_or(85);
    let right_frame_indices: Vec<u64> =
        left_frame_indices.iter().map(|i| i + sync_offset).collect();
    let fov_degrees: f32 = flag_value(flags, "--fov-degrees")
        .and_then(|s| s.parse().ok())
        .unwrap_or(100.0);
    let eval_width: u32 = flag_value(flags, "--eval-width")
        .and_then(|s| s.parse().ok())
        .unwrap_or(1280);
    let eval_height: u32 = flag_value(flags, "--eval-height")
        .and_then(|s| s.parse().ok())
        .unwrap_or(720);
    let matched_points_path = flag_value(flags, "--matched-points");

    std::fs::create_dir_all(output_dir).expect("failed to create output_dir");

    let json_str = std::fs::read_to_string(match_json_path).expect("failed to read match.json");
    let cal: Calibration = serde_json::from_str(&json_str).expect("invalid match.json");
    let seed_topology = cal.topology.clone();
    let seed_framing = cal.framing.clone();

    // --- Load real frame pairs (one per --frames entry) ---
    println!(
        "Loading {} frame pair(s): left={:?} right={:?} (sync_offset={sync_offset})",
        left_frame_indices.len(),
        left_frame_indices,
        right_frame_indices
    );
    let left_frames = reco_io::ffmpeg::calibration_io::extract_frames(
        std::path::Path::new(left_path),
        &left_frame_indices,
    )
    .expect("failed to extract left frames");
    let right_frames = reco_io::ffmpeg::calibration_io::extract_frames(
        std::path::Path::new(right_path),
        &right_frame_indices,
    )
    .expect("failed to extract right frames");
    assert_eq!(
        left_frames.len(),
        right_frames.len(),
        "left/right frame extraction returned different counts"
    );
    assert_eq!(
        (left_frames[0].width, left_frames[0].height),
        (right_frames[0].width, right_frames[0].height),
        "this harness assumes both cameras share one resolution/aspect, like \
         reco_core::render::renderer::Renderer::new already does for the production path"
    );
    println!(
        "  {}x{} per frame",
        left_frames[0].width, left_frames[0].height
    );

    let gpu = GpuContext::new_blocking().expect("no GPU");
    let aspect = left_frames[0].width as f32 / left_frames[0].height as f32;
    let left_renderer = SingleCameraRenderer::new(
        &gpu,
        left_frames[0].width,
        left_frames[0].height,
        eval_width,
        eval_height,
        aspect,
    );
    let right_renderer = SingleCameraRenderer::new(
        &gpu,
        right_frames[0].width,
        right_frames[0].height,
        eval_width,
        eval_height,
        aspect,
    );

    let ctxs: Vec<RenderCtx<'_>> = left_frames
        .iter()
        .zip(right_frames.iter())
        .map(|(left_yuv, right_yuv)| RenderCtx {
            gpu: &gpu,
            left_renderer: &left_renderer,
            right_renderer: &right_renderer,
            left_cam: &cal.lenses[0],
            right_cam: &cal.lenses[1],
            left_yuv,
            right_yuv,
            aspect,
            fov_degrees,
            eval_width,
            eval_height,
        })
        .collect();

    // --- Baseline (AKAZE-fitted seed) ---
    println!("\n=== Baseline (AKAZE-fitted seed layout) ===");
    print_layout("seed", &seed_topology, &seed_framing);
    let seed_results: Vec<RenderScoreResult> = ctxs
        .iter()
        .map(|ctx| render_and_score(ctx, &seed_topology, &seed_framing))
        .collect();
    for (i, r) in seed_results.iter().enumerate() {
        print_report(&format!("seed[frame {}]", left_frame_indices[i]), r);
    }
    let seed_avg_banded = avg_banded_mean(&seed_results);
    println!("  seed average near-field-banded ZNCC mean: {seed_avg_banded:.4}");

    // --- Photometric refinement ---
    println!("\n=== Photometric (ZNCC) refinement, seeded near the AKAZE fit ===");
    let cost = PhotometricCost {
        ctxs: ctxs.clone(),
        bounds: BOUNDS_5.to_vec(),
        fixed_x_rx: seed_topology.x_rx,
        fixed_z_rz: seed_topology.z_rz,
    };

    let seed_vec = vec![
        seed_framing.axis_offset,
        seed_topology.intersect,
        seed_topology.x_ty,
        seed_topology.x_rz,
        seed_topology.z_rx,
    ];
    // Small perturbations around the seed - this harness refines an
    // already-good AKAZE fit, unlike optimizer.rs's wide cold-start grid.
    let starts: Vec<Vec<f64>> = vec![
        seed_vec.clone(),
        perturb(&seed_vec, 0, 0.005),
        perturb(&seed_vec, 0, -0.005),
        perturb(&seed_vec, 1, 0.01),
        perturb(&seed_vec, 1, -0.01),
        perturb(&seed_vec, 3, 0.01),
        perturb(&seed_vec, 4, 0.01),
    ];

    let mut converged: Vec<(Vec<f64>, f64)> = Vec::new();
    for start in &starts {
        if let Some(result) = run_nelder_mead(&cost, start, 60) {
            converged.push(result);
        }
    }
    if converged.is_empty() {
        eprintln!("ERROR: no optimizer start converged - aborting");
        std::process::exit(1);
    }
    converged.sort_by(|a, b| a.1.partial_cmp(&b.1).unwrap());
    let (best_vec, best_cost) = converged[0].clone();
    println!(
        "{} of {} starts converged; best cost={best_cost:.6}",
        converged.len(),
        starts.len()
    );

    let (refined_topology, refined_framing) =
        vec5_to_layout(&best_vec, seed_topology.x_rx, seed_topology.z_rz);
    print_layout("refined", &refined_topology, &refined_framing);
    let refined_results: Vec<RenderScoreResult> = ctxs
        .iter()
        .map(|ctx| render_and_score(ctx, &refined_topology, &refined_framing))
        .collect();
    for (i, r) in refined_results.iter().enumerate() {
        print_report(&format!("refined[frame {}]", left_frame_indices[i]), r);
    }
    let refined_avg_banded = avg_banded_mean(&refined_results);
    println!("  refined average near-field-banded ZNCC mean: {refined_avg_banded:.4}");

    // --- Success-criteria checklist (none of these alone gates success) ---
    println!("\n=== Checklist (read all of these before trusting the result) ===");
    println!(
        "1. Near-field-banded ZNCC mean (the optimized objective, averaged over {} frame(s)): \
         seed={seed_avg_banded:.4} -> refined={refined_avg_banded:.4} (higher is better)",
        ctxs.len()
    );
    let seed_whole_avg = seed_results
        .iter()
        .map(|r| r.whole_report.mean)
        .sum::<f32>()
        / seed_results.len() as f32;
    let refined_whole_avg = refined_results
        .iter()
        .map(|r| r.whole_report.mean)
        .sum::<f32>()
        / refined_results.len() as f32;
    println!(
        "   (for reference, whole-region ZNCC mean: seed={seed_whole_avg:.4} -> \
         refined={refined_whole_avg:.4} - not what was optimized, don't judge success on this alone)"
    );

    println!("2. Core parameter deltas (seed -> refined):");
    check_delta(
        "cam_d",
        seed_framing.axis_offset,
        refined_framing.axis_offset,
    );
    check_delta(
        "intersect",
        seed_topology.intersect,
        refined_topology.intersect,
    );
    check_delta("x_ty", seed_topology.x_ty, refined_topology.x_ty);
    check_delta("x_rz", seed_topology.x_rz, refined_topology.x_rz);
    check_delta("z_rx", seed_topology.z_rx, refined_topology.z_rx);

    println!("3. Spread across {} converged starts:", converged.len());
    for (i, name) in ["cam_d", "intersect", "x_ty", "x_rz", "z_rx"]
        .iter()
        .enumerate()
    {
        let vals: Vec<f64> = converged.iter().map(|(p, _)| p[i]).collect();
        let min = vals.iter().cloned().fold(f64::INFINITY, f64::min);
        let max = vals.iter().cloned().fold(f64::NEG_INFINITY, f64::max);
        println!(
            "   {name}: min={min:.5} max={max:.5} spread={:.5}",
            max - min
        );
    }

    println!("4. Bound-pegging check (cam_d, intersect):");
    check_bound_pegging("cam_d", refined_framing.axis_offset, BOUNDS_5[0]);
    check_bound_pegging("intersect", refined_topology.intersect, BOUNDS_5[1]);

    if let Some(mp_path) = matched_points_path {
        println!("5. Independent AKAZE-metric cross-check ({mp_path}):");
        cross_check_akaze_residual(
            mp_path,
            &seed_topology,
            &seed_framing,
            &refined_topology,
            &refined_framing,
        );
    } else {
        println!("5. (skipped - pass --matched-points to cross-check against the AKAZE residual)");
    }

    // --- Visual dump (never deleted) - first evaluated frame only, to
    // keep output manageable when scoring across many frames.
    println!(
        "\n=== Visual dump (frame {}) -> {output_dir} ===",
        left_frame_indices[0]
    );
    let seed_first = &seed_results[0];
    let refined_first = &refined_results[0];
    save_rgba(
        &seed_first.left_rgba,
        eval_width,
        eval_height,
        &format!("{output_dir}/seed_left.png"),
    );
    save_rgba(
        &seed_first.right_rgba,
        eval_width,
        eval_height,
        &format!("{output_dir}/seed_right.png"),
    );
    save_mask(
        &seed_first.mask,
        &format!("{output_dir}/seed_overlap_mask.png"),
    );
    let seed_heatmap = render_heatmap(
        &seed_first.left_luma,
        &seed_first.right_luma,
        &seed_first.mask,
    );
    seed_heatmap
        .save(format!("{output_dir}/seed_diff_heatmap.png"))
        .unwrap();

    save_rgba(
        &refined_first.left_rgba,
        eval_width,
        eval_height,
        &format!("{output_dir}/refined_left.png"),
    );
    save_rgba(
        &refined_first.right_rgba,
        eval_width,
        eval_height,
        &format!("{output_dir}/refined_right.png"),
    );
    save_mask(
        &refined_first.mask,
        &format!("{output_dir}/refined_overlap_mask.png"),
    );
    let refined_heatmap = render_heatmap(
        &refined_first.left_luma,
        &refined_first.right_luma,
        &refined_first.mask,
    );
    refined_heatmap
        .save(format!("{output_dir}/refined_diff_heatmap.png"))
        .unwrap();

    let mut sbs = image::RgbaImage::new(eval_width * 2, eval_height);
    image::imageops::replace(&mut sbs, &seed_heatmap, 0, 0);
    image::imageops::replace(&mut sbs, &refined_heatmap, eval_width as i64, 0);
    sbs.save(format!("{output_dir}/comparison_before_after.png"))
        .unwrap();

    println!(
        "Done. Inspect {output_dir}/comparison_before_after.png (seed left, refined right) \
         before trusting any of the numbers above - this is the same visual signal the user's \
         eye originally used to judge alignment quality."
    );
}

fn flag_value<'a>(flags: &'a [String], name: &str) -> Option<&'a str> {
    flags
        .iter()
        .position(|f| f == name)
        .and_then(|i| flags.get(i + 1))
        .map(|s| s.as_str())
}

fn perturb(v: &[f64], idx: usize, delta: f64) -> Vec<f64> {
    let mut out = v.to_vec();
    out[idx] += delta;
    out
}

fn print_layout(label: &str, topology: &Topology, framing: &Framing) {
    println!(
        "  {label}: cam_d={:.4} intersect={:.4} x_ty={:.5} x_rz={:.4} z_rx={:.4} x_rx={:.4} z_rz={:.4}",
        framing.axis_offset,
        topology.intersect,
        topology.x_ty,
        topology.x_rz,
        topology.z_rx,
        topology.x_rx,
        topology.z_rz,
    );
}

fn print_report(label: &str, r: &RenderScoreResult) {
    println!(
        "  {label}: banded(near-field) mean={:.4} min={:.4} max={:.4} valid={} skipped={} | \
         whole mean={:.4} valid={} | coverage={:.1}%",
        r.banded_report.mean,
        r.banded_report.min,
        r.banded_report.max,
        r.banded_report.valid_patches,
        r.banded_report.skipped_patches,
        r.whole_report.mean,
        r.whole_report.valid_patches,
        r.mask.coverage_fraction() * 100.0
    );
}

/// Average the near-field-banded ZNCC mean across multiple frames'
/// results - the metric the optimizer actually maximizes.
fn avg_banded_mean(results: &[RenderScoreResult]) -> f32 {
    results.iter().map(|r| r.banded_report.mean).sum::<f32>() / results.len() as f32
}

fn check_delta(name: &str, seed: f64, refined: f64) {
    let abs_delta = refined - seed;
    let rel = if seed.abs() > 1e-9 {
        (abs_delta / seed).abs()
    } else {
        abs_delta.abs()
    };
    let warn = if rel > SUSPICIOUS_RELATIVE_DELTA {
        "  WARNING: large relative swing - check for aliasing/exploited overlap"
    } else {
        ""
    };
    println!(
        "   {name}: {seed:.5} -> {refined:.5} (delta={abs_delta:+.5}, rel={:.1}%){warn}",
        rel * 100.0
    );
}

fn check_bound_pegging(name: &str, value: f64, bound: (f64, f64)) {
    let (lo, hi) = bound;
    if (value - lo).abs() < 0.005 || (value - hi).abs() < 0.005 {
        println!(
            "   WARNING: {name}={value:.4} is pegged at its bound [{lo}, {hi}] - this \
             codebase's own documented tell for a bad/under-constrained fit."
        );
    } else {
        println!("   {name}={value:.4} within bounds [{lo}, {hi}], not pegged.");
    }
}

fn cross_check_akaze_residual(
    path: &str,
    seed_topology: &Topology,
    seed_framing: &Framing,
    refined_topology: &Topology,
    refined_framing: &Framing,
) {
    let json = match std::fs::read_to_string(path) {
        Ok(j) => j,
        Err(e) => {
            eprintln!("   could not read {path}: {e}");
            return;
        }
    };
    let frames: Vec<FrameMatches> = match serde_json::from_str(&json) {
        Ok(f) => f,
        Err(e) => {
            eprintln!("   could not parse {path} as Vec<FrameMatches>: {e}");
            return;
        }
    };
    let points: Vec<MatchedPoint> = frames.iter().flat_map(|f| f.points.clone()).collect();
    let is_near = |p: &MatchedPoint| {
        p.left[1].abs() > NEAR_FIELD_THRESHOLD || p.right[1].abs() > NEAR_FIELD_THRESHOLD
    };
    let near: Vec<MatchedPoint> = points.iter().copied().filter(is_near).collect();
    let far: Vec<MatchedPoint> = points.iter().copied().filter(|p| !is_near(p)).collect();

    let seed_params = layout_to_opt_params(seed_topology, seed_framing);
    let refined_params = layout_to_opt_params(refined_topology, refined_framing);

    let near_seed: f64 = geometry::per_point_reprojection_error(&near, &seed_params)
        .iter()
        .sum();
    let near_refined: f64 = geometry::per_point_reprojection_error(&near, &refined_params)
        .iter()
        .sum();
    let far_seed: f64 = geometry::per_point_reprojection_error(&far, &seed_params)
        .iter()
        .sum();
    let far_refined: f64 = geometry::per_point_reprojection_error(&far, &refined_params)
        .iter()
        .sum();

    println!(
        "   near-field AKAZE residual: seed={near_seed:.6} -> refined={near_refined:.6} \
         ({} points)",
        near.len()
    );
    println!(
        "   far-field AKAZE residual:  seed={far_seed:.6} -> refined={far_refined:.6} \
         ({} points)",
        far.len()
    );
    if far_refined > far_seed * 1.05 {
        println!(
            "   WARNING: far-field AKAZE residual regressed by >5% - the photometric \
             refinement may be trading far-field accuracy for near-field ZNCC."
        );
    }
}

fn layout_to_opt_params(topology: &Topology, framing: &Framing) -> OptParams {
    OptParams {
        x_ty: topology.x_ty,
        intersect: topology.intersect,
        cam_d: framing.axis_offset,
        x_rz: topology.x_rz,
        z_rx: topology.z_rx,
        z_rz: Some(topology.z_rz),
        x_rx: Some(topology.x_rx),
        ground_tilt_x: None,
        ground_tilt_z: None,
        k_x: 1.0,
        k_z: 1.0,
    }
}

fn save_rgba(rgba: &[u8], width: u32, height: u32, path: &str) {
    image::RgbaImage::from_raw(width, height, rgba.to_vec())
        .expect("rgba buffer size mismatch")
        .save(path)
        .unwrap();
}

fn save_mask(mask: &OverlapMask, path: &str) {
    let mut img = image::RgbaImage::new(mask.width, mask.height);
    for y in 0..mask.height {
        for x in 0..mask.width {
            let idx = (y * mask.width + x) as usize;
            let v = if mask.mask[idx] { 255 } else { 0 };
            img.put_pixel(x, y, image::Rgba([v, v, v, 255]));
        }
    }
    img.save(path).unwrap();
}

/// False-colored `|left_luma - right_luma|` inside the mask (blue = low
/// difference, red = high difference), black outside the mask - this is
/// the image the user judges by eye.
fn render_heatmap(left_luma: &[f32], right_luma: &[f32], mask: &OverlapMask) -> image::RgbaImage {
    let mut img = image::RgbaImage::new(mask.width, mask.height);
    for y in 0..mask.height {
        for x in 0..mask.width {
            let idx = (y * mask.width + x) as usize;
            if mask.mask[idx] {
                let diff = (left_luma[idx] - right_luma[idx]).abs().clamp(0.0, 1.0);
                // Amplify for visibility - real misalignment diffs are typically small.
                let amplified = (diff * 4.0).clamp(0.0, 1.0);
                let r = (amplified * 255.0) as u8;
                let b = ((1.0 - amplified) * 255.0) as u8;
                img.put_pixel(x, y, image::Rgba([r, 0, b, 255]));
            } else {
                img.put_pixel(x, y, image::Rgba([0, 0, 0, 255]));
            }
        }
    }
    img
}

fn vec5_to_layout(p: &[f64], x_rx: f64, z_rz: f64) -> (Topology, Framing) {
    let topology = Topology {
        intersect: p[1],
        x_ty: p[2],
        x_rz: p[3],
        z_rx: p[4],
        x_rx,
        z_rz,
        blend_width: DEFAULT_BLEND_WIDTH,
        blend_flip_direction: false,
        seam_offset: 0.0,
        multiband_blend_enabled: false,
        color_match_enabled: DEFAULT_COLOR_MATCH_ENABLED,
        color_match_band_width: DEFAULT_COLOR_MATCH_BAND_WIDTH,
        color_match_grid_cols: DEFAULT_COLOR_MATCH_GRID_COLS,
        color_match_grid_rows: DEFAULT_COLOR_MATCH_GRID_ROWS,
        color_match_interval_frames: DEFAULT_COLOR_MATCH_INTERVAL_FRAMES,
        color_match_ema_alpha: DEFAULT_COLOR_MATCH_EMA_ALPHA,
        color_match_max_y_offset: DEFAULT_COLOR_MATCH_MAX_Y_OFFSET,
        color_match_max_chroma_offset: DEFAULT_COLOR_MATCH_MAX_CHROMA_OFFSET,
        ground_tilt_x: 0.0,
        ground_tilt_z: 0.0,
        top_tilt_x: 0.0,
        top_tilt_z: 0.0,
        ground_tilt_band_width: DEFAULT_TILT_BAND_WIDTH,
        top_tilt_band_width: DEFAULT_TILT_BAND_WIDTH,
    };
    let framing = Framing {
        axis_offset: p[0],
        tilt: 0.0,
        roll: 0.0,
    };
    (topology, framing)
}

/// Shared render inputs, cheap to clone (all shared references + Copy
/// scalars) so `PhotometricCost` can carry its own copy for argmin's
/// `Executor`, which requires `Clone` to run multiple starts.
#[derive(Clone, Copy)]
struct RenderCtx<'a> {
    gpu: &'a GpuContext,
    left_renderer: &'a SingleCameraRenderer,
    right_renderer: &'a SingleCameraRenderer,
    left_cam: &'a Lens,
    right_cam: &'a Lens,
    left_yuv: &'a reco_core::source::YuvFrame,
    right_yuv: &'a reco_core::source::YuvFrame,
    aspect: f32,
    fov_degrees: f32,
    eval_width: u32,
    eval_height: u32,
}

/// Result of rendering and scoring one frame pair at one candidate
/// layout. Carries both the near-field-banded report (what the optimizer
/// maximizes) and the whole-region report (reference/comparison only -
/// see the module doc for why the whole-region metric alone was
/// misleading in the first revision of this harness).
struct RenderScoreResult {
    banded_report: ZnccReport,
    whole_report: ZnccReport,
    mask: OverlapMask,
    left_rgba: Vec<u8>,
    right_rgba: Vec<u8>,
    left_luma: Vec<f32>,
    right_luma: Vec<f32>,
}

fn render_and_score(
    ctx: &RenderCtx<'_>,
    topology: &Topology,
    framing: &Framing,
) -> RenderScoreResult {
    let scene = SceneGeometry::new(topology, framing, ctx.aspect);
    let left_rgba = ctx.left_renderer.render_and_readback(
        ctx.gpu,
        &scene,
        ctx.left_cam,
        false,
        ctx.fov_degrees,
        &ctx.left_yuv.y,
        &ctx.left_yuv.u,
        &ctx.left_yuv.v,
        reco_core::render::renderer::GroundTilt::default(),
        reco_core::render::renderer::TopTilt::default(),
    );
    let right_rgba = ctx.right_renderer.render_and_readback(
        ctx.gpu,
        &scene,
        ctx.right_cam,
        true,
        ctx.fov_degrees,
        &ctx.right_yuv.y,
        &ctx.right_yuv.u,
        &ctx.right_yuv.v,
        reco_core::render::renderer::GroundTilt::default(),
        reco_core::render::renderer::TopTilt::default(),
    );

    let mask = photometric::overlap_mask_from_alpha(
        &left_rgba,
        &right_rgba,
        ctx.eval_width,
        ctx.eval_height,
        ALPHA_THRESHOLD,
    );
    let left_luma = photometric::to_luma(&left_rgba, ctx.eval_width, ctx.eval_height);
    let right_luma = photometric::to_luma(&right_rgba, ctx.eval_width, ctx.eval_height);
    let (banded_report, whole_report) = if mask.coverage_fraction() < MIN_OVERLAP_FRACTION {
        (ZnccReport::empty(), ZnccReport::empty())
    } else {
        let banded = photometric::windowed_zncc_banded(
            &left_luma,
            &right_luma,
            &mask,
            PATCH_SIZE,
            MIN_PATCH_VARIANCE,
            NEAR_FIELD_ROW_FRAC,
        );
        let whole = photometric::windowed_zncc(
            &left_luma,
            &right_luma,
            &mask,
            PATCH_SIZE,
            MIN_PATCH_VARIANCE,
        );
        (banded, whole)
    };

    RenderScoreResult {
        banded_report,
        whole_report,
        mask,
        left_rgba,
        right_rgba,
        left_luma,
        right_luma,
    }
}

#[derive(Clone)]
struct PhotometricCost<'a> {
    ctxs: Vec<RenderCtx<'a>>,
    bounds: Vec<(f64, f64)>,
    fixed_x_rx: f64,
    fixed_z_rz: f64,
}

impl CostFunction for PhotometricCost<'_> {
    type Param = Vec<f64>;
    type Output = f64;

    fn cost(&self, p: &Self::Param) -> Result<Self::Output, ArgminError> {
        let (topology, framing) = vec5_to_layout(p, self.fixed_x_rx, self.fixed_z_rz);
        let penalty = bounds_penalty(p, &self.bounds);

        // Every loaded frame must pass the degeneracy checks on the
        // near-field band specifically - if the candidate layout
        // collapses the near-field overlap on ANY frame, penalize the
        // whole candidate rather than averaging around the failure.
        let mut banded_sum = 0.0f64;
        for ctx in &self.ctxs {
            let r = render_and_score(ctx, &topology, &framing);
            if r.mask.coverage_fraction() < MIN_OVERLAP_FRACTION
                || r.banded_report.valid_patches < MIN_VALID_PATCHES
            {
                return Ok(NO_OVERLAP_PENALTY + penalty);
            }
            banded_sum += r.banded_report.mean as f64;
        }
        let banded_avg = banded_sum / self.ctxs.len() as f64;

        Ok((1.0 - banded_avg) + penalty)
    }
}

/// Quadratic out-of-bounds penalty, mirroring
/// `reco_calibrate::optimizer`'s private `bounds_penalty`.
fn bounds_penalty(p: &[f64], bounds: &[(f64, f64)]) -> f64 {
    let scale = 1e4;
    let mut penalty = 0.0;
    for (i, &val) in p.iter().enumerate() {
        if i >= bounds.len() {
            break;
        }
        let (lo, hi) = bounds[i];
        if val < lo {
            let d = lo - val;
            penalty += scale * d * d;
        } else if val > hi {
            let d = val - hi;
            penalty += scale * d * d;
        }
    }
    penalty
}

/// Build an initial simplex (n+1 vertices) around a start point,
/// mirroring `reco_calibrate::optimizer`'s private `build_simplex`.
fn build_simplex(start: &[f64], bounds: &[(f64, f64)]) -> Vec<Vec<f64>> {
    const SIMPLEX_PERTURBATION: f64 = 0.10;
    let n = start.len();
    let mut vertices: Vec<Vec<f64>> = Vec::with_capacity(n + 1);
    vertices.push(start.to_vec());
    for i in 0..n {
        let mut vertex = start.to_vec();
        let range = bounds[i].1 - bounds[i].0;
        let delta = SIMPLEX_PERTURBATION * range;
        if vertex[i] + delta <= bounds[i].1 {
            vertex[i] += delta;
        } else {
            vertex[i] -= delta;
        }
        vertices.push(vertex);
    }
    vertices
}

fn run_nelder_mead(
    cost: &PhotometricCost<'_>,
    start: &[f64],
    max_iters: u64,
) -> Option<(Vec<f64>, f64)> {
    let simplex = build_simplex(start, &cost.bounds);
    let solver: NelderMead<Vec<f64>, f64> =
        NelderMead::new(simplex).with_sd_tolerance(1e-9).ok()?;

    let res = Executor::new(cost.clone(), solver)
        .configure(|state| state.max_iters(max_iters))
        .run()
        .ok()?;

    let p = res.state().get_best_param()?.clone();
    let f = res.state().get_best_cost();
    Some((p, f))
}
