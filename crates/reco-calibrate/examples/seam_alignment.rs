//! Measure how well each setup lines the two cameras up, in rendered
//! pixels - the "does it look right" metric, independent of any matched
//! feature points - and optionally refine a setup against it.
//!
//! For every setup and frame, renders both cameras exactly as the export
//! does (lens, placement, `ground_tilt_*` / `top_tilt_*`) into the same
//! view, then inside the region both cameras cover:
//!
//! - measures the vertical disparity per 16-row band
//!   (`row_profile::measure_row_bands`): how many pixels one camera's
//!   content sits above/below the other's - the "lines do not continue
//!   across the seam" defect;
//! - scores ZNCC (1.0 = identical) in the far / mid / near thirds of the
//!   overlap - doubled players and lines lower it.
//!
//! The view always looks straight at the seam (`SingleCameraRenderer`
//! ignores the setup's framing tilt/roll), so every setup is judged from
//! the same viewpoint.
//!
//! `--refine` (experiment): starting from the LAST setup, Nelder-Mead
//! searches `x_ty`, `x_rz`, `z_rx`, `x_rx`, `z_rz`, `ground_tilt_x/z` and
//! `top_tilt_x/z` to maximize the objective on `--frames`, keeping
//! `intersect` and `axis_offset` fixed. The result is scored again on
//! `--holdout` frames (never used in the search) and written to
//! `<output_dir>/refined_setup.json`.
//!
//! The background beyond the pitch is only a thin strip in the wide view,
//! so a second, zoomed view (`--far-pitch` rad above the seam centre,
//! `--far-fov` vertical deg; 0 = off)
//! looks at the far field across the seam and adds its own ZNCC term to
//! the objective (`--far-weight`, default 1 = same as one third).
//!
//! Usage:
//!   cargo run --release -p reco-calibrate --example seam_alignment -- \
//!     <left.mp4> <right.mp4> <output_dir> --frames N1,N2,... \
//!     [--holdout M1,M2,...] [--refine] [--fov-degrees F] \
//!     [--far-pitch P] [--far-fov F] [--far-weight W] \
//!     [--eval-width W] [--eval-height H] <setup.json>...
//!
//! Frame numbers are left-camera frames; the right camera uses each
//! setup's own `sync_offset`. Composites of the first frame are written
//! to `<output_dir>/seam_<setup>.png`.

use std::path::Path;

use argmin::core::{CostFunction, Error, Executor, State};
use argmin::solver::neldermead::NelderMead;
use reco_calibrate::photometric::{self, OverlapMask};
use reco_calibrate::row_profile;
use reco_core::calibration::Calibration;
use reco_core::gpu::GpuContext;
use reco_core::render::renderer::{GroundTilt, TopTilt};
use reco_core::render::scene::SceneGeometry;
use reco_core::render::single_camera::SingleCameraRenderer;
use reco_core::source::YuvFrame;

const ALPHA_THRESHOLD: u8 = 127;
/// Row-band disparity search (same values as `fit_row_profile`).
const BAND_HEIGHT: u32 = 16;
const MAX_DY: i32 = 15;
const MIN_VARIANCE: f32 = 1e-5;
const MIN_CONFIDENCE: f32 = 0.3;
const ZNCC_PATCH: u32 = 16;
/// Nelder-Mead iteration cap for `--refine`.
const REFINE_MAX_ITERS: u64 = 400;

/// Renderers plus decoded frame pairs, shared by every evaluation.
struct EvalContext {
    gpu: GpuContext,
    left_renderer: SingleCameraRenderer,
    right_renderer: SingleCameraRenderer,
    width: u32,
    height: u32,
    aspect: f32,
    fov_degrees: f32,
    /// Zoomed far-field view: pitch above straight-at-the-seam (rad) and
    /// vertical FOV (deg); FOV 0 = off.
    far_pitch: f32,
    far_fov: f32,
    /// Weight of the far-field view's ZNCC next to the three thirds.
    far_weight: f32,
}

/// Alignment quality of one setup over a set of frames.
#[derive(Default)]
struct Score {
    /// Mean ZNCC per third (far, mid, near) over all frames.
    zncc: [f32; 3],
    /// |dy| per measured band, per third, over all frames.
    dy: [Vec<f32>; 3],
    /// Per-frame "row:dy" listing, for the report.
    band_lines: Vec<String>,
    /// Mean ZNCC in the zoomed far-field view (`None` when off).
    far_zoom: Option<f32>,
    /// |dy| per measured band in the zoomed far-field view.
    far_dy: Vec<f32>,
    /// Weight of `far_zoom` in [`Self::objective`].
    far_weight: f32,
}

impl Score {
    /// Single number to maximize: every third counts equally, so the
    /// large, easy far field cannot hide a misaligned near field; the
    /// zoomed far-field view adds its own weighted term, since in the
    /// wide view the background beyond the pitch is only a thin strip.
    fn objective(&self) -> f32 {
        let thirds = self.zncc.iter().sum::<f32>();
        match self.far_zoom {
            Some(z) => (thirds + self.far_weight * z) / (3.0 + self.far_weight),
            None => thirds / 3.0,
        }
    }

    fn print(&self, label: &str) {
        println!("\n== {label}");
        for line in &self.band_lines {
            println!("   {line}");
        }
        for (ti, name) in ["far ", "mid ", "near"].iter().enumerate() {
            let mut dy = self.dy[ti].clone();
            dy.sort_by(f32::total_cmp);
            let median = dy.get(dy.len() / 2).copied().unwrap_or(f32::NAN);
            let max = dy.last().copied().unwrap_or(f32::NAN);
            println!(
                "   {name}: |dy| median {median:5.2} px  max {max:5.2} px  [{} bands]   zncc {:.4}",
                dy.len(),
                self.zncc[ti]
            );
        }
        if let Some(z) = self.far_zoom {
            let mut dy = self.far_dy.clone();
            dy.sort_by(f32::total_cmp);
            let median = dy.get(dy.len() / 2).copied().unwrap_or(f32::NAN);
            let max = dy.last().copied().unwrap_or(f32::NAN);
            println!(
                "   far zoom: |dy| median {median:5.2} px  max {max:5.2} px  [{} bands]   zncc {z:.4}",
                dy.len()
            );
        }
        println!("   objective {:.4}", self.objective());
    }
}

fn tilts(cal: &Calibration, is_right: bool) -> (GroundTilt, TopTilt) {
    let t = &cal.topology;
    let (lens, ground, top) = if is_right {
        (&cal.lenses[1], t.ground_tilt_x, t.top_tilt_x)
    } else {
        (&cal.lenses[0], t.ground_tilt_z, t.top_tilt_z)
    };
    let k = lens.ground_tilt_k() as f32;
    (
        GroundTilt {
            tilt: ground as f32,
            k,
            band_full: t.ground_tilt_band_width as f32,
        },
        TopTilt {
            tilt: top as f32,
            k,
            band_full: t.top_tilt_band_width as f32,
        },
    )
}

/// Render every frame pair with `cal` and score the overlap. With
/// `composite_path`, also writes the first frame's composite.
fn evaluate(
    ctx: &EvalContext,
    cal: &Calibration,
    pairs: &[(YuvFrame, YuvFrame)],
    frame_numbers: &[u64],
    measure_dy: bool,
    composite_path: Option<&str>,
) -> Score {
    let scene = SceneGeometry::new(&cal.topology, &cal.framing, ctx.aspect);
    let (w, h) = (ctx.width, ctx.height);
    let mut score = Score::default();
    let mut zncc_sums = [(0.0f32, 0usize); 3];
    let mut far_sum = (0.0f32, 0usize);

    for (fi, (l, r)) in pairs.iter().enumerate() {
        let (left_rgba, right_rgba) = render_pair(ctx, cal, &scene, ctx.fov_degrees, 0.0, l, r);
        let mask =
            photometric::overlap_mask_from_alpha(&left_rgba, &right_rgba, w, h, ALPHA_THRESHOLD);
        let left_luma = photometric::to_luma(&left_rgba, w, h);
        let right_luma = photometric::to_luma(&right_rgba, w, h);

        let Some((min_row, max_row)) = overlap_rows(&mask) else {
            score
                .band_lines
                .push(format!("frame {}: no overlap", frame_numbers[fi]));
            continue;
        };
        let third = ((max_row - min_row) / 3).max(1);
        let third_of = |row: f32| (((row as u32).saturating_sub(min_row)) / third).min(2) as usize;

        if measure_dy {
            let bands = row_profile::measure_row_bands(
                &left_luma,
                &right_luma,
                &mask,
                BAND_HEIGHT,
                MAX_DY,
                MIN_VARIANCE,
                MIN_CONFIDENCE,
            );
            let mut line = Vec::new();
            for b in bands.iter().flatten() {
                score.dy[third_of(b.row_center)].push(b.dy.abs());
                line.push(format!("{:.0}:{:+.0}", b.row_center, b.dy));
            }
            score.band_lines.push(format!(
                "frame {} bands row:dy  {}",
                frame_numbers[fi],
                line.join(" ")
            ));
        }

        for (ti, sum) in zncc_sums.iter_mut().enumerate() {
            let start = min_row + ti as u32 * third;
            let end = if ti == 2 { max_row + 1 } else { start + third };
            let sub = OverlapMask {
                width: mask.width,
                height: mask.height,
                mask: mask
                    .mask
                    .iter()
                    .enumerate()
                    .map(|(p, &m)| {
                        let y = p as u32 / mask.width;
                        m && y >= start && y < end
                    })
                    .collect(),
            };
            let report =
                photometric::windowed_zncc(&left_luma, &right_luma, &sub, ZNCC_PATCH, MIN_VARIANCE);
            if report.valid_patches > 0 {
                sum.0 += report.mean;
                sum.1 += 1;
            }
        }

        if fi == 0
            && let Some(path) = composite_path
        {
            save_composite(&left_rgba, &right_rgba, w, h, path);
        }

        if ctx.far_fov > 0.0 {
            let (fl, fr) = render_pair(ctx, cal, &scene, ctx.far_fov, ctx.far_pitch, l, r);
            let fmask = photometric::overlap_mask_from_alpha(&fl, &fr, w, h, ALPHA_THRESHOLD);
            let fl_luma = photometric::to_luma(&fl, w, h);
            let fr_luma = photometric::to_luma(&fr, w, h);
            let report =
                photometric::windowed_zncc(&fl_luma, &fr_luma, &fmask, ZNCC_PATCH, MIN_VARIANCE);
            if report.valid_patches > 0 {
                far_sum.0 += report.mean;
                far_sum.1 += 1;
            }
            if measure_dy {
                let bands = row_profile::measure_row_bands(
                    &fl_luma,
                    &fr_luma,
                    &fmask,
                    BAND_HEIGHT,
                    MAX_DY,
                    MIN_VARIANCE,
                    MIN_CONFIDENCE,
                );
                score
                    .far_dy
                    .extend(bands.iter().flatten().map(|b| b.dy.abs()));
            }
            if fi == 0
                && let Some(path) = composite_path
            {
                save_composite(&fl, &fr, w, h, &path.replace(".png", "_far.png"));
            }
        }
    }
    for (ti, (sum, n)) in zncc_sums.iter().enumerate() {
        score.zncc[ti] = if *n > 0 { sum / *n as f32 } else { -1.0 };
    }
    if ctx.far_fov > 0.0 {
        score.far_zoom = Some(if far_sum.1 > 0 {
            far_sum.0 / far_sum.1 as f32
        } else {
            -1.0
        });
    }
    score.far_weight = ctx.far_weight;
    score
}

/// Render both cameras of one frame pair into `scene` at `fov` (vertical,
/// degrees), looking `pitch` radians above straight at the seam.
fn render_pair(
    ctx: &EvalContext,
    cal: &Calibration,
    scene: &SceneGeometry,
    fov: f32,
    pitch: f32,
    l: &YuvFrame,
    r: &YuvFrame,
) -> (Vec<u8>, Vec<u8>) {
    let (lg, lt) = tilts(cal, false);
    let (rg, rt) = tilts(cal, true);
    let left = ctx.left_renderer.render_and_readback_pitched(
        &ctx.gpu,
        scene,
        &cal.lenses[0],
        false,
        fov,
        pitch,
        &l.y,
        &l.u,
        &l.v,
        lg,
        lt,
    );
    let right = ctx.right_renderer.render_and_readback_pitched(
        &ctx.gpu,
        scene,
        &cal.lenses[1],
        true,
        fov,
        pitch,
        &r.y,
        &r.u,
        &r.v,
        rg,
        rt,
    );
    (left, right)
}

/// Decode left frames and the matching right frames (`sync_offset`).
fn load_pairs(
    left: &str,
    right: &str,
    frames: &[u64],
    sync_offset: i64,
) -> Vec<(YuvFrame, YuvFrame)> {
    let right_indices: Vec<u64> = frames
        .iter()
        .map(|f| (*f as i64 + sync_offset).max(0) as u64)
        .collect();
    let l = reco_io::ffmpeg::calibration_io::extract_frames(Path::new(left), frames)
        .expect("failed to extract left frames");
    let r = reco_io::ffmpeg::calibration_io::extract_frames(Path::new(right), &right_indices)
        .expect("failed to extract right frames");
    l.into_iter().zip(r).collect()
}

/// Parameters `--refine` searches, in this order.
const REFINE_PARAMS: [&str; 9] = [
    "x_ty",
    "x_rz",
    "z_rx",
    "x_rx",
    "z_rz",
    "ground_tilt_x",
    "ground_tilt_z",
    "top_tilt_x",
    "top_tilt_z",
];
/// Initial simplex step per parameter (same order).
const REFINE_STEPS: [f64; 9] = [0.002, 0.004, 0.008, 0.008, 0.004, 0.01, 0.01, 0.005, 0.005];

fn get_params(cal: &Calibration) -> Vec<f64> {
    let t = &cal.topology;
    vec![
        t.x_ty,
        t.x_rz,
        t.z_rx,
        t.x_rx,
        t.z_rz,
        t.ground_tilt_x,
        t.ground_tilt_z,
        t.top_tilt_x,
        t.top_tilt_z,
    ]
}

fn with_params(base: &Calibration, p: &[f64]) -> Calibration {
    let mut cal = base.clone();
    let t = &mut cal.topology;
    t.x_ty = p[0];
    t.x_rz = p[1];
    t.z_rx = p[2];
    t.x_rx = p[3];
    t.z_rz = p[4];
    t.ground_tilt_x = p[5];
    t.ground_tilt_z = p[6];
    t.top_tilt_x = p[7];
    t.top_tilt_z = p[8];
    cal
}

struct RefineCost<'a> {
    ctx: &'a EvalContext,
    base: &'a Calibration,
    pairs: &'a [(YuvFrame, YuvFrame)],
    frames: &'a [u64],
}

impl CostFunction for RefineCost<'_> {
    type Param = Vec<f64>;
    type Output = f64;

    fn cost(&self, p: &Self::Param) -> Result<Self::Output, Error> {
        let cal = with_params(self.base, p);
        let score = evaluate(self.ctx, &cal, self.pairs, self.frames, false, None);
        Ok(-(score.objective() as f64))
    }
}

fn parse_frames(value: &str, flag: &str) -> Vec<u64> {
    value
        .split(',')
        .map(|s| {
            s.trim()
                .parse()
                .unwrap_or_else(|_| panic!("{flag}: comma-separated u64"))
        })
        .collect()
}

fn main() {
    reco_io::init();

    let args: Vec<String> = std::env::args().collect();
    if args.len() < 6 {
        eprintln!(
            "Usage: {} <left.mp4> <right.mp4> <output_dir> --frames N1,N2,... \
             [--holdout M1,M2,...] [--refine] [--fov-degrees F] \
             [--eval-width W] [--eval-height H] <setup.json>...",
            args[0]
        );
        std::process::exit(1);
    }
    let (left_path, right_path, output_dir) = (&args[1], &args[2], &args[3]);
    let rest = &args[4..];

    let mut frames: Vec<u64> = Vec::new();
    let mut holdout: Vec<u64> = Vec::new();
    let mut refine = false;
    let mut fov_degrees: f32 = 100.0;
    let mut width: u32 = 1920;
    let mut height: u32 = 1080;
    let mut far_pitch: f32 = 0.46;
    let mut far_fov: f32 = 25.0;
    let mut far_weight: f32 = 1.0;
    let mut setup_paths: Vec<&String> = Vec::new();
    let mut i = 0;
    while i < rest.len() {
        let value = || rest.get(i + 1).expect("flag needs a value");
        match rest[i].as_str() {
            "--frames" => {
                frames = parse_frames(value(), "--frames");
                i += 2;
            }
            "--holdout" => {
                holdout = parse_frames(value(), "--holdout");
                i += 2;
            }
            "--refine" => {
                refine = true;
                i += 1;
            }
            "--fov-degrees" => {
                fov_degrees = value().parse().expect("--fov-degrees: number");
                i += 2;
            }
            "--eval-width" => {
                width = value().parse().expect("--eval-width: integer");
                i += 2;
            }
            "--far-pitch" => {
                far_pitch = value().parse().expect("--far-pitch: number (rad)");
                i += 2;
            }
            "--far-fov" => {
                far_fov = value().parse().expect("--far-fov: number (0 = off)");
                i += 2;
            }
            "--far-weight" => {
                far_weight = value().parse().expect("--far-weight: number");
                i += 2;
            }
            "--eval-height" => {
                height = value().parse().expect("--eval-height: integer");
                i += 2;
            }
            _ => {
                setup_paths.push(&rest[i]);
                i += 1;
            }
        }
    }
    assert!(!frames.is_empty(), "--frames is required");
    assert!(
        !setup_paths.is_empty(),
        "at least one setup.json is required"
    );
    std::fs::create_dir_all(output_dir).expect("cannot create output_dir");

    let setups: Vec<(String, Calibration)> = setup_paths
        .iter()
        .map(|p| {
            let cal = Calibration::from_file(Path::new(p))
                .unwrap_or_else(|e| panic!("cannot load setup {p}: {e}"));
            let name = Path::new(p)
                .file_stem()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| (*p).clone());
            (name, cal)
        })
        .collect();
    let sync_offset = setups[0].1.sync_offset;
    if setups.iter().any(|(_, c)| c.sync_offset != sync_offset) {
        eprintln!("note: setups differ in sync_offset; using the first setup's ({sync_offset})");
    }

    let pairs = load_pairs(left_path, right_path, &frames, sync_offset);
    let holdout_pairs = if holdout.is_empty() {
        Vec::new()
    } else {
        load_pairs(left_path, right_path, &holdout, sync_offset)
    };
    let gpu = GpuContext::new_blocking().expect("no GPU");
    let (in_w, in_h) = (pairs[0].0.width, pairs[0].0.height);
    let aspect = in_w as f32 / in_h as f32;
    let ctx = EvalContext {
        left_renderer: SingleCameraRenderer::new(&gpu, in_w, in_h, width, height, aspect),
        right_renderer: SingleCameraRenderer::new(&gpu, in_w, in_h, width, height, aspect),
        gpu,
        width,
        height,
        aspect,
        fov_degrees,
        far_pitch,
        far_fov,
        far_weight,
    };

    println!(
        "Frames {frames:?} (holdout {holdout:?}), view {width}x{height} at {fov_degrees} deg, \
         far zoom {far_fov} deg pitched {far_pitch:.2} rad (weight {far_weight})"
    );
    println!("dy = vertical disparity in view pixels (0 = lines continue across the seam)");
    println!("zncc = overlap similarity (1.0 = identical; doubled content lowers it)");

    for (name, cal) in &setups {
        let composite = format!("{output_dir}/seam_{name}.png");
        evaluate(&ctx, cal, &pairs, &frames, true, Some(&composite)).print(name);
        if !holdout_pairs.is_empty() {
            evaluate(&ctx, cal, &holdout_pairs, &holdout, true, None)
                .print(&format!("{name} [holdout]"));
        }
    }

    if !refine {
        return;
    }
    let (base_name, base) = setups.last().expect("at least one setup");
    let start = get_params(base);
    let mut simplex = vec![start.clone()];
    for (d, step) in REFINE_STEPS.iter().enumerate() {
        let mut v = start.clone();
        v[d] += step;
        simplex.push(v);
    }
    println!("\nRefining '{base_name}' ({REFINE_MAX_ITERS} iterations max) ...");
    let solver: NelderMead<Vec<f64>, f64> = NelderMead::new(simplex)
        .with_sd_tolerance(1e-6)
        .expect("valid simplex");
    let cost = RefineCost {
        ctx: &ctx,
        base,
        pairs: &pairs,
        frames: &frames,
    };
    let res = Executor::new(cost, solver)
        .configure(|s| s.max_iters(REFINE_MAX_ITERS))
        .run()
        .expect("refinement failed");
    let best = res.state().get_best_param().expect("no best param").clone();
    println!(
        "Refined in {} iterations, mean zncc {:.4}",
        res.state().get_iter(),
        -res.state().get_best_cost()
    );
    for (name, (from, to)) in REFINE_PARAMS.iter().zip(start.iter().zip(&best)) {
        println!("   {name:14} {from:+.5} -> {to:+.5}");
    }

    let refined = with_params(base, &best);
    let composite = format!("{output_dir}/seam_refined.png");
    evaluate(&ctx, &refined, &pairs, &frames, true, Some(&composite)).print("refined");
    if !holdout_pairs.is_empty() {
        evaluate(&ctx, &refined, &holdout_pairs, &holdout, true, None).print("refined [holdout]");
    }
    let out = format!("{output_dir}/refined_setup.json");
    std::fs::write(
        &out,
        serde_json::to_string_pretty(&refined).expect("serialize setup"),
    )
    .expect("cannot write refined setup");
    println!("\nWrote {out}");
}

/// First and last row that has any overlap coverage.
fn overlap_rows(mask: &OverlapMask) -> Option<(u32, u32)> {
    let covered = |y: u32| (0..mask.width).any(|x| mask.mask[(y * mask.width + x) as usize]);
    let first = (0..mask.height).find(|&y| covered(y))?;
    let last = (0..mask.height).rev().find(|&y| covered(y))?;
    Some((first, last))
}

/// Left first, right alpha-blended over it (production draw order).
fn save_composite(left: &[u8], right: &[u8], width: u32, height: u32, path: &str) {
    let mut out = vec![0u8; (width * height * 4) as usize];
    for (o, (l, r)) in out
        .chunks_exact_mut(4)
        .zip(left.chunks_exact(4).zip(right.chunks_exact(4)))
    {
        let ra = r[3] as f32 / 255.0;
        for c in 0..3 {
            o[c] = (r[c] as f32 * ra + l[c] as f32 * (1.0 - ra)).round() as u8;
        }
        o[3] = 255;
    }
    image::RgbaImage::from_raw(width, height, out)
        .expect("rgba buffer size mismatch")
        .save(path)
        .expect("cannot write composite");
}
