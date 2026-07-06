//! Phase 1 validation harness for per-camera INTRINSIC correction via a
//! photometric (ZNCC) objective - a different hypothesis than
//! `examples/fit_photometric.rs`, which only ever varies placement
//! (extrinsics) and holds each camera's `fx/fy/cx/cy/d` fixed as given by
//! the lens profile.
//!
//! Motivation: the user manually tuned the `left_uniforms`/`right_uniforms`
//! intrinsic values (not the placement sliders) in rig-calib and got a
//! near-perfect overlap by eye. Intrinsics drive the GPU undistortion step
//! that runs *before* any placement math - a slightly wrong intrinsic
//! (plausible: this rig's `cx`/`cy` are exactly `width/2`/`height/2`,
//! which looks like a generic default rather than a measured principal
//! point) would distort each camera's "rectilinear" image in a way no
//! amount of placement tuning can undo, and KB4 distortion's nonlinearity
//! is strongest at the wide field angles that correspond to the near-field
//! edges where the seam problem lives.
//!
//! This harness holds placement FIXED at the existing AKAZE-fitted seed
//! layout and searches `fx`/`fy`/`cx`/`cy` per camera (8 parameters total)
//! by default, maximizing patch-based ZNCC over the overlap region -
//! isolating whether intrinsic correction alone can reproduce what the
//! user found by hand, independent of the placement question already
//! explored in `fit_photometric.rs`. Pass `--fit-distortion` to also
//! search the four KB4 distortion coefficients (`d0..d3`) per camera (16
//! parameters total) - the near-field seam lives at wide field angles,
//! exactly where KB4's higher-order terms dominate over the "affine-ish"
//! fx/fy/cx/cy, so this is the more direct lever if the 8-parameter
//! search alone doesn't move the visual result.
//!
//! Standalone example only. Does NOT touch reco-cli, rig-calib, the
//! production calibration pipeline, or any config/CLI flag.
//!
//! Known limitation: unlike `fit_photometric.rs`, this harness has no
//! independent AKAZE-residual cross-check, because the existing AKAZE
//! matches were detected against the OLD (unperturbed) undistortion - a
//! true independent check would require re-running detection through the
//! candidate intrinsics, out of scope for this pass. Success here rests on
//! the ZNCC checklist and, most importantly, the visual diff-heatmap dump.
//!
//! Usage:
//! ```text
//! cargo run --release -p reco-calibrate --example fit_photometric_intrinsics -- \
//!   <left.mp4> <right.mp4> <match.json> <output_dir> \
//!   [--frame N] [--sync-offset N] [--fov-degrees F] \
//!   [--eval-width W] [--eval-height H] [--fit-distortion]
//! ```
//!
//! Never deletes anything under `<output_dir>`.

use argmin::core::{CostFunction, Error as ArgminError, Executor, State};
use argmin::solver::neldermead::NelderMead;
use reco_calibrate::photometric::{self, OverlapMask, ZnccReport};
use reco_core::calibration::{CameraParams, MatchCalibration};
use reco_core::gpu::GpuContext;
use reco_core::render::scene::SceneGeometry;
use reco_core::render::single_camera::SingleCameraRenderer;

/// Symmetric bound on `fx`/`fy` deltas, as a fraction of the baseline
/// value - e.g. `0.05` allows +-5%. Focal length manufacturing tolerance
/// is typically small relative to the nominal value.
const FOCAL_DELTA_FRAC: f64 = 0.05;
/// Symmetric bound on `cx`/`cy` deltas, in pixels. A few tens of pixels
/// is a plausible per-unit principal-point offset for a consumer action
/// camera - notably, this rig's baseline `cx`/`cy` are exactly
/// `width/2`/`height/2`, which looks like a default rather than a
/// measured value, so there's reason to expect real slack here.
const PRINCIPAL_POINT_DELTA_PX: f64 = 40.0;
/// Symmetric bound on each KB4 distortion coefficient's delta (`d0..d3`),
/// applied uniformly despite the coefficients' very different nominal
/// magnitudes (baseline ~[0.155, 0.137, -0.094, 0.004]) - a coarse Phase 1
/// choice, not derived from any per-coefficient sensitivity analysis.
/// Only used when `--fit-distortion` is passed.
const DISTORTION_DELTA_ABS: f64 = 0.03;

const ALPHA_THRESHOLD: u8 = 127;
const PATCH_SIZE: u32 = 16;
const MIN_PATCH_VARIANCE: f32 = 1e-5;
const MIN_OVERLAP_FRACTION: f64 = 0.05;
const MIN_VALID_PATCHES: usize = 8;
const NO_OVERLAP_PENALTY: f64 = 10.0;

/// Relative delta above which a fitted intrinsic correction is flagged as
/// suspicious - i.e. large enough that it's more likely exploiting the
/// photometric objective than correcting a real per-unit manufacturing
/// deviation.
const SUSPICIOUS_RELATIVE_DELTA: f64 = 0.5;

fn main() {
    reco_io::init();

    let args: Vec<String> = std::env::args().collect();
    if args.len() < 5 {
        eprintln!(
            "Usage: {} <left.mp4> <right.mp4> <match.json> <output_dir> \
             [--frame N] [--sync-offset N] [--fov-degrees F] \
             [--eval-width W] [--eval-height H] [--fit-distortion]",
            args[0]
        );
        std::process::exit(1);
    }

    let left_path = &args[1];
    let right_path = &args[2];
    let match_json_path = &args[3];
    let output_dir = &args[4];
    let flags = &args[5..];

    let target_frame: u64 = flag_value(flags, "--frame")
        .and_then(|s| s.parse().ok())
        .unwrap_or(300);
    let sync_offset: u64 = flag_value(flags, "--sync-offset")
        .and_then(|s| s.parse().ok())
        .unwrap_or(85);
    let fov_degrees: f32 = flag_value(flags, "--fov-degrees")
        .and_then(|s| s.parse().ok())
        .unwrap_or(100.0);
    let eval_width: u32 = flag_value(flags, "--eval-width")
        .and_then(|s| s.parse().ok())
        .unwrap_or(1280);
    let eval_height: u32 = flag_value(flags, "--eval-height")
        .and_then(|s| s.parse().ok())
        .unwrap_or(720);
    let fit_distortion = flags.iter().any(|f| f == "--fit-distortion");
    println!(
        "Searching: fx/fy/cx/cy per camera{}",
        if fit_distortion {
            " + d0..d3 per camera (--fit-distortion)"
        } else {
            " (pass --fit-distortion to also search KB4 coefficients)"
        }
    );

    std::fs::create_dir_all(output_dir).expect("failed to create output_dir");

    let json_str = std::fs::read_to_string(match_json_path).expect("failed to read match.json");
    let cal: MatchCalibration = serde_json::from_str(&json_str).expect("invalid match.json");
    let baseline_left = cal.left.clone();
    let baseline_right = cal.right.clone();

    println!("Baseline intrinsics (from embedded lens metadata / match.json):");
    print_camera_params("left", &baseline_left);
    print_camera_params("right", &baseline_right);
    if (baseline_left.cx - baseline_left.width as f64 / 2.0).abs() < 0.5
        && (baseline_left.cy - baseline_left.height as f64 / 2.0).abs() < 0.5
    {
        println!(
            "  NOTE: left cx/cy sit exactly at width/2, height/2 - looks like a default \
             centering assumption rather than a measured principal point."
        );
    }

    // --- Load one real frame pair (mirrors examples/dump_undistorted.rs) ---
    let mut left_dec =
        reco_io::ffmpeg::decoder::VideoDecoder::open(std::path::Path::new(left_path)).unwrap();
    let mut right_dec =
        reco_io::ffmpeg::decoder::VideoDecoder::open(std::path::Path::new(right_path)).unwrap();
    for _ in 0..target_frame {
        left_dec.next_frame().unwrap();
    }
    for _ in 0..(target_frame + sync_offset) {
        right_dec.next_frame().unwrap();
    }
    let left_yuv = left_dec.next_frame().unwrap().expect("no left frames");
    let right_yuv = right_dec.next_frame().unwrap().expect("no right frames");
    assert_eq!(
        (left_yuv.width, left_yuv.height),
        (right_yuv.width, right_yuv.height),
        "this harness assumes both cameras share one resolution/aspect"
    );

    let gpu = GpuContext::new_blocking().expect("no GPU");
    let aspect = left_yuv.width as f32 / left_yuv.height as f32;
    let left_renderer = SingleCameraRenderer::new(
        &gpu,
        left_yuv.width,
        left_yuv.height,
        eval_width,
        eval_height,
        aspect,
    );
    let right_renderer = SingleCameraRenderer::new(
        &gpu,
        right_yuv.width,
        right_yuv.height,
        eval_width,
        eval_height,
        aspect,
    );

    // Placement is held FIXED at the AKAZE-fitted seed for this whole
    // experiment - only intrinsics vary. Built once since it never changes.
    let scene = SceneGeometry::from_layout_with_aspect(&cal.layout, aspect);

    let ctx = RenderCtx {
        gpu: &gpu,
        left_renderer: &left_renderer,
        right_renderer: &right_renderer,
        left_yuv: &left_yuv,
        right_yuv: &right_yuv,
        scene: &scene,
        fov_degrees,
        eval_width,
        eval_height,
    };

    println!("\n=== Baseline (embedded-lens-metadata intrinsics, seed placement) ===");
    let (seed_report, seed_mask, seed_left_rgba, seed_right_rgba, seed_left_luma, seed_right_luma) =
        render_and_score(&ctx, &baseline_left, &baseline_right);
    print_report("baseline", &seed_report, seed_mask.coverage_fraction());

    println!("\n=== Intrinsics refinement (ZNCC), placement held fixed ===");
    let bounds: Vec<(f64, f64)> = build_bounds(&baseline_left, &baseline_right, fit_distortion);
    let param_names = param_names(fit_distortion);
    let n = bounds.len();

    let cost = IntrinsicsCost {
        ctx,
        baseline_left: baseline_left.clone(),
        baseline_right: baseline_right.clone(),
        bounds: bounds.clone(),
        fit_distortion,
    };

    let zero = vec![0.0; n];
    let per_camera = if fit_distortion { 8 } else { 4 };
    let mut starts: Vec<Vec<f64>> = vec![
        zero.clone(),
        perturb(&zero, 0, bounds[0].1 * 0.3),
        perturb(&zero, 1, bounds[1].1 * 0.3),
        perturb(&zero, 2, bounds[2].1 * 0.5),
        perturb(&zero, 3, bounds[3].1 * 0.5),
        perturb(&zero, per_camera + 2, bounds[per_camera + 2].1 * 0.5),
        perturb(&zero, per_camera + 3, bounds[per_camera + 3].1 * 0.5),
    ];
    if fit_distortion {
        starts.push(perturb(&zero, 4, bounds[4].1 * 0.5)); // left d0
        starts.push(perturb(&zero, 7, bounds[7].1 * 0.5)); // left d3
        starts.push(perturb(
            &zero,
            per_camera + 4,
            bounds[per_camera + 4].1 * 0.5,
        )); // right d0
    }
    let max_iters = if fit_distortion { 150 } else { 60 };

    let mut converged: Vec<(Vec<f64>, f64)> = Vec::new();
    for start in &starts {
        if let Some(result) = run_nelder_mead(&cost, start, max_iters) {
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

    let (refined_left, refined_right) =
        deltas_to_cameras(&best_vec, &baseline_left, &baseline_right, fit_distortion);
    println!("Refined intrinsics:");
    print_camera_params("left", &refined_left);
    print_camera_params("right", &refined_right);

    let (
        refined_report,
        refined_mask,
        refined_left_rgba,
        refined_right_rgba,
        refined_left_luma,
        refined_right_luma,
    ) = render_and_score(&ctx, &refined_left, &refined_right);
    print_report("refined", &refined_report, refined_mask.coverage_fraction());

    println!("\n=== Checklist (read all of these before trusting the result) ===");
    println!(
        "1. ZNCC mean: baseline={:.4} -> refined={:.4} (higher is better; 1.0 = perfect)",
        seed_report.mean, refined_report.mean
    );

    println!("2. Intrinsic deltas (baseline -> refined):");
    let per_camera_names: &[&str] = if fit_distortion {
        &["fx", "fy", "cx", "cy", "d0", "d1", "d2", "d3"]
    } else {
        &["fx", "fy", "cx", "cy"]
    };
    for (side, base) in [("left", &baseline_left), ("right", &baseline_right)] {
        let offset = if side == "left" { 0 } else { per_camera };
        for (i, name) in per_camera_names.iter().enumerate() {
            let base_val = match *name {
                "fx" => base.fx,
                "fy" => base.fy,
                "cx" => base.cx,
                "cy" => base.cy,
                "d0" => base.d[0],
                "d1" => base.d[1],
                "d2" => base.d[2],
                _ => base.d[3],
            };
            check_delta(
                &format!("{side}.{name}"),
                base_val,
                base_val + best_vec[offset + i],
            );
        }
    }

    println!("3. Spread across {} converged starts:", converged.len());
    for (i, name) in param_names.iter().enumerate() {
        let vals: Vec<f64> = converged.iter().map(|(p, _)| p[i]).collect();
        let min = vals.iter().cloned().fold(f64::INFINITY, f64::min);
        let max = vals.iter().cloned().fold(f64::NEG_INFINITY, f64::max);
        println!(
            "   {name} delta: min={min:.3} max={max:.3} spread={:.3}",
            max - min
        );
    }

    println!("4. Bound-pegging check:");
    for (i, name) in param_names.iter().enumerate() {
        check_bound_pegging(name, best_vec[i], bounds[i]);
    }

    println!(
        "5. No independent AKAZE-residual cross-check for this experiment (see module doc) - \
         judge primarily from the visual diff-heatmap dump below."
    );

    println!("\n=== Visual dump -> {output_dir} ===");
    save_rgba(
        &seed_left_rgba,
        eval_width,
        eval_height,
        &format!("{output_dir}/baseline_left.png"),
    );
    save_rgba(
        &seed_right_rgba,
        eval_width,
        eval_height,
        &format!("{output_dir}/baseline_right.png"),
    );
    save_mask(
        &seed_mask,
        &format!("{output_dir}/baseline_overlap_mask.png"),
    );
    let seed_heatmap = render_heatmap(&seed_left_luma, &seed_right_luma, &seed_mask);
    seed_heatmap
        .save(format!("{output_dir}/baseline_diff_heatmap.png"))
        .unwrap();

    save_rgba(
        &refined_left_rgba,
        eval_width,
        eval_height,
        &format!("{output_dir}/refined_left.png"),
    );
    save_rgba(
        &refined_right_rgba,
        eval_width,
        eval_height,
        &format!("{output_dir}/refined_right.png"),
    );
    save_mask(
        &refined_mask,
        &format!("{output_dir}/refined_overlap_mask.png"),
    );
    let refined_heatmap = render_heatmap(&refined_left_luma, &refined_right_luma, &refined_mask);
    refined_heatmap
        .save(format!("{output_dir}/refined_diff_heatmap.png"))
        .unwrap();

    let mut sbs = image::RgbaImage::new(eval_width * 2, eval_height);
    image::imageops::replace(&mut sbs, &seed_heatmap, 0, 0);
    image::imageops::replace(&mut sbs, &refined_heatmap, eval_width as i64, 0);
    sbs.save(format!("{output_dir}/comparison_before_after.png"))
        .unwrap();

    println!(
        "Done. Inspect {output_dir}/comparison_before_after.png (baseline left, refined right) \
         before trusting any of the numbers above."
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

fn print_camera_params(label: &str, cam: &CameraParams) {
    println!(
        "  {label}: fx={:.3} fy={:.3} cx={:.3} cy={:.3} d={:?}",
        cam.fx, cam.fy, cam.cx, cam.cy, cam.d
    );
}

fn print_report(label: &str, report: &ZnccReport, coverage_fraction: f64) {
    println!(
        "  {label}: ZNCC mean={:.4} min={:.4} max={:.4} valid_patches={} skipped={} \
         coverage={:.1}%",
        report.mean,
        report.min,
        report.max,
        report.valid_patches,
        report.skipped_patches,
        coverage_fraction * 100.0
    );
}

fn check_delta(name: &str, baseline: f64, refined: f64) {
    let abs_delta = refined - baseline;
    let rel = if baseline.abs() > 1e-9 {
        (abs_delta / baseline).abs()
    } else {
        abs_delta.abs()
    };
    let warn = if rel > SUSPICIOUS_RELATIVE_DELTA {
        "  WARNING: large relative swing - check for overfitting"
    } else {
        ""
    };
    println!(
        "   {name}: {baseline:.4} -> {refined:.4} (delta={abs_delta:+.4}, rel={:.1}%){warn}",
        rel * 100.0
    );
}

fn check_bound_pegging(name: &str, delta: f64, bound: (f64, f64)) {
    let (lo, hi) = bound;
    let range = hi - lo;
    if (delta - lo).abs() < range * 0.02 || (delta - hi).abs() < range * 0.02 {
        println!(
            "   WARNING: {name} delta={delta:.3} is pegged at its bound [{lo:.3}, {hi:.3}] - \
             the search wants to go further; bounds may be too tight, or this is a bad fit."
        );
    } else {
        println!("   {name} delta={delta:.3} within bounds [{lo:.3}, {hi:.3}], not pegged.");
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
/// difference, red = high difference), black outside the mask.
fn render_heatmap(left_luma: &[f32], right_luma: &[f32], mask: &OverlapMask) -> image::RgbaImage {
    let mut img = image::RgbaImage::new(mask.width, mask.height);
    for y in 0..mask.height {
        for x in 0..mask.width {
            let idx = (y * mask.width + x) as usize;
            if mask.mask[idx] {
                let diff = (left_luma[idx] - right_luma[idx]).abs().clamp(0.0, 1.0);
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

/// Parameter names in the same order `deltas_to_cameras`/`build_bounds`
/// use: `[left.fx, left.fy, left.cx, left.cy, (left.d0..d3,) right.fx, ...]`.
fn param_names(fit_distortion: bool) -> Vec<String> {
    let base = ["fx", "fy", "cx", "cy"];
    let dist = ["d0", "d1", "d2", "d3"];
    let mut names = Vec::new();
    for side in ["left", "right"] {
        for n in base {
            names.push(format!("{side}.{n}"));
        }
        if fit_distortion {
            for n in dist {
                names.push(format!("{side}.{n}"));
            }
        }
    }
    names
}

/// Bounds in the same per-camera-block order as `param_names`/
/// `deltas_to_cameras`.
fn build_bounds(
    baseline_left: &CameraParams,
    baseline_right: &CameraParams,
    fit_distortion: bool,
) -> Vec<(f64, f64)> {
    let mut bounds = Vec::new();
    for base in [baseline_left, baseline_right] {
        bounds.push((-base.fx * FOCAL_DELTA_FRAC, base.fx * FOCAL_DELTA_FRAC));
        bounds.push((-base.fy * FOCAL_DELTA_FRAC, base.fy * FOCAL_DELTA_FRAC));
        bounds.push((-PRINCIPAL_POINT_DELTA_PX, PRINCIPAL_POINT_DELTA_PX));
        bounds.push((-PRINCIPAL_POINT_DELTA_PX, PRINCIPAL_POINT_DELTA_PX));
        if fit_distortion {
            for _ in 0..4 {
                bounds.push((-DISTORTION_DELTA_ABS, DISTORTION_DELTA_ABS));
            }
        }
    }
    bounds
}

fn deltas_to_cameras(
    p: &[f64],
    base_left: &CameraParams,
    base_right: &CameraParams,
    fit_distortion: bool,
) -> (CameraParams, CameraParams) {
    let per_camera = if fit_distortion { 8 } else { 4 };

    let mut left = base_left.clone();
    left.fx += p[0];
    left.fy += p[1];
    left.cx += p[2];
    left.cy += p[3];
    if fit_distortion {
        for i in 0..4 {
            left.d[i] += p[4 + i];
        }
    }

    let mut right = base_right.clone();
    right.fx += p[per_camera];
    right.fy += p[per_camera + 1];
    right.cx += p[per_camera + 2];
    right.cy += p[per_camera + 3];
    if fit_distortion {
        for i in 0..4 {
            right.d[i] += p[per_camera + 4 + i];
        }
    }

    (left, right)
}

/// Shared render inputs, cheap to clone (all shared references + Copy
/// scalars) so `IntrinsicsCost` can carry its own copy for argmin's
/// `Executor`, which requires `Clone` to run multiple starts.
#[derive(Clone, Copy)]
struct RenderCtx<'a> {
    gpu: &'a GpuContext,
    left_renderer: &'a SingleCameraRenderer,
    right_renderer: &'a SingleCameraRenderer,
    left_yuv: &'a reco_core::source::YuvFrame,
    right_yuv: &'a reco_core::source::YuvFrame,
    scene: &'a SceneGeometry,
    fov_degrees: f32,
    eval_width: u32,
    eval_height: u32,
}

type RenderScoreResult = (
    ZnccReport,
    OverlapMask,
    Vec<u8>,
    Vec<u8>,
    Vec<f32>,
    Vec<f32>,
);

fn render_and_score(
    ctx: &RenderCtx<'_>,
    left_cam: &CameraParams,
    right_cam: &CameraParams,
) -> RenderScoreResult {
    let left_rgba = ctx.left_renderer.render_and_readback(
        ctx.gpu,
        ctx.scene,
        left_cam,
        false,
        ctx.fov_degrees,
        &ctx.left_yuv.y,
        &ctx.left_yuv.u,
        &ctx.left_yuv.v,
    );
    let right_rgba = ctx.right_renderer.render_and_readback(
        ctx.gpu,
        ctx.scene,
        right_cam,
        true,
        ctx.fov_degrees,
        &ctx.right_yuv.y,
        &ctx.right_yuv.u,
        &ctx.right_yuv.v,
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
    let report = if mask.coverage_fraction() < MIN_OVERLAP_FRACTION {
        ZnccReport::empty()
    } else {
        photometric::windowed_zncc(
            &left_luma,
            &right_luma,
            &mask,
            PATCH_SIZE,
            MIN_PATCH_VARIANCE,
        )
    };

    (report, mask, left_rgba, right_rgba, left_luma, right_luma)
}

#[derive(Clone)]
struct IntrinsicsCost<'a> {
    ctx: RenderCtx<'a>,
    baseline_left: CameraParams,
    baseline_right: CameraParams,
    bounds: Vec<(f64, f64)>,
    fit_distortion: bool,
}

impl CostFunction for IntrinsicsCost<'_> {
    type Param = Vec<f64>;
    type Output = f64;

    fn cost(&self, p: &Self::Param) -> Result<Self::Output, ArgminError> {
        let (left_cam, right_cam) = deltas_to_cameras(
            p,
            &self.baseline_left,
            &self.baseline_right,
            self.fit_distortion,
        );
        let (report, mask, ..) = render_and_score(&self.ctx, &left_cam, &right_cam);

        let penalty = bounds_penalty(p, &self.bounds);

        if mask.coverage_fraction() < MIN_OVERLAP_FRACTION
            || report.valid_patches < MIN_VALID_PATCHES
        {
            return Ok(NO_OVERLAP_PENALTY + penalty);
        }

        Ok((1.0 - report.mean as f64) + penalty)
    }
}

/// Quadratic out-of-bounds penalty, mirroring
/// `reco_calibrate::optimizer`'s private `bounds_penalty`.
fn bounds_penalty(p: &[f64], bounds: &[(f64, f64)]) -> f64 {
    let scale = 1e-2; // deltas are in pixel units (much larger magnitude than
    // the placement harness's radian/normalized-unit params), so this
    // penalty scale is correspondingly smaller to keep it comparable to
    // the ~[0,1]-scale ZNCC cost term.
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
    cost: &IntrinsicsCost<'_>,
    start: &[f64],
    max_iters: u64,
) -> Option<(Vec<f64>, f64)> {
    let simplex = build_simplex(start, &cost.bounds);
    let solver: NelderMead<Vec<f64>, f64> =
        NelderMead::new(simplex).with_sd_tolerance(1e-6).ok()?;

    let res = Executor::new(cost.clone(), solver)
        .configure(|state| state.max_iters(max_iters))
        .run()
        .ok()?;

    let p = res.state().get_best_param()?.clone();
    let f = res.state().get_best_cost();
    Some((p, f))
}
