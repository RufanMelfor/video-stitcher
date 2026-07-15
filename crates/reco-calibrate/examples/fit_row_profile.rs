//! Phase 1 validation harness for the row-varying local disparity
//! correction (`reco_calibrate::row_profile`) - see that module's doc
//! comment for the full motivation.
//!
//! Unlike every other `fit_photometric*.rs` harness, this one does NOT
//! search for new global placement/intrinsic parameters. It takes the
//! EXISTING calibration as fixed and measures + corrects the residual
//! local (per-row) disparity on top of it, entirely in rendered-pixel
//! space. Because the correction lives in pixel space (not the abstract
//! `PlaneLayout`), it cannot be expressed as a `match.json` and cannot be
//! tested via `reco stitch` - this harness builds its own full composite
//! (left-over-right alpha blend, matching production's blend math) so
//! the before/after comparison is still a real full-frame visual, not
//! just an isolated diff heatmap.
//!
//! Critically - learned the hard way earlier this session - this harness
//! checks alignment quality in THREE independent height bands (near/mid/
//! far thirds of the overlap region), not just the specific bands the
//! profile was fit from, and against HELD-OUT frames never used in
//! fitting, specifically to catch the "helped one region, silently broke
//! another" and "overfit to the frames it saw" failure modes that
//! invalidated every earlier photometric experiment today.
//!
//! Usage:
//! ```text
//! cargo run --release -p reco-calibrate --example fit_row_profile -- \
//!   <left.mp4> <right.mp4> <match.json> <output_dir> \
//!   --frames N1,N2,N3,N4 [--sync-offset N] [--fov-degrees F] \
//!   [--eval-width W] [--eval-height H]
//! ```
//! Pass at least 2 frames; the LAST one is held out from fitting and used
//! purely for the generalization check. Never deletes anything under
//! `<output_dir>`.

use reco_calibrate::photometric;
use reco_calibrate::row_profile::{self, RowProfile};
use reco_core::calibration::Calibration;
use reco_core::gpu::GpuContext;
use reco_core::render::scene::SceneGeometry;
use reco_core::render::single_camera::SingleCameraRenderer;

const ALPHA_THRESHOLD: u8 = 127;
/// Height of each row band measured (pixels, at eval resolution).
const BAND_HEIGHT: u32 = 16;
/// Maximum vertical search radius per band (pixels).
const MAX_DY: i32 = 15;
/// Minimum luma variance to trust a band's ZNCC match.
const MIN_VARIANCE: f32 = 1e-5;
/// Minimum ZNCC score to trust a band's best-shift match at all.
const MIN_CONFIDENCE: f32 = 0.3;
/// Max allowed frame-to-frame spread (pixels) before a band is rejected
/// as unstable (likely a moving object, not static geometry).
const MAX_DY_SPREAD: f32 = 4.0;
/// Rows beyond the outermost measured band over which the correction
/// tapers smoothly to exactly zero.
const TAPER_ROWS: f32 = 24.0;
/// ZNCC patch size for the multi-band before/after quality check
/// (independent of `BAND_HEIGHT`, which is only for measuring disparity).
const CHECK_PATCH_SIZE: u32 = 16;
const CHECK_MIN_PATCH_VARIANCE: f32 = 1e-5;

fn main() {
    reco_io::init();

    let args: Vec<String> = std::env::args().collect();
    if args.len() < 5 {
        eprintln!(
            "Usage: {} <left.mp4> <right.mp4> <match.json> <output_dir> \
             --frames N1,N2,N3,N4 [--sync-offset N] [--fov-degrees F] \
             [--eval-width W] [--eval-height H]",
            args[0]
        );
        std::process::exit(1);
    }

    let left_path = &args[1];
    let right_path = &args[2];
    let match_json_path = &args[3];
    let output_dir = &args[4];
    let flags = &args[5..];

    let left_frame_indices: Vec<u64> = flag_value(flags, "--frames")
        .expect("--frames N1,N2,... is required (at least 2, last one held out)")
        .split(',')
        .map(|s| {
            s.trim()
                .parse()
                .expect("--frames must be a comma-separated list of u64")
        })
        .collect();
    assert!(
        left_frame_indices.len() >= 2,
        "need at least 2 frames: fit on all but the last, hold out the last for generalization check"
    );
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

    std::fs::create_dir_all(output_dir).expect("failed to create output_dir");

    let json_str = std::fs::read_to_string(match_json_path).expect("failed to read match.json");
    let cal: Calibration = serde_json::from_str(&json_str).expect("invalid match.json");

    println!(
        "Loading {} frame pairs (last one held out for generalization check): left={:?} right={:?}",
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
    let scene = SceneGeometry::new(&cal.topology, &cal.framing, aspect);

    // --- Render every frame's left/right/mask/luma once, up front ---
    struct FrameRender {
        left_rgba: Vec<u8>,
        right_rgba: Vec<u8>,
        left_luma: Vec<f32>,
        right_luma: Vec<f32>,
        mask: photometric::OverlapMask,
    }
    let render_frame = |left_yuv: &reco_core::source::YuvFrame,
                        right_yuv: &reco_core::source::YuvFrame| {
        let left_rgba = left_renderer.render_and_readback(
            &gpu,
            &scene,
            &cal.lenses[0],
            false,
            fov_degrees,
            &left_yuv.y,
            &left_yuv.u,
            &left_yuv.v,
            reco_core::render::renderer::GroundTilt::default(),
            reco_core::render::renderer::TopTilt::default(),
        );
        let right_rgba = right_renderer.render_and_readback(
            &gpu,
            &scene,
            &cal.lenses[1],
            true,
            fov_degrees,
            &right_yuv.y,
            &right_yuv.u,
            &right_yuv.v,
            reco_core::render::renderer::GroundTilt::default(),
            reco_core::render::renderer::TopTilt::default(),
        );
        let mask = photometric::overlap_mask_from_alpha(
            &left_rgba,
            &right_rgba,
            eval_width,
            eval_height,
            ALPHA_THRESHOLD,
        );
        let left_luma = photometric::to_luma(&left_rgba, eval_width, eval_height);
        let right_luma = photometric::to_luma(&right_rgba, eval_width, eval_height);
        FrameRender {
            left_rgba,
            right_rgba,
            left_luma,
            right_luma,
            mask,
        }
    };

    let renders: Vec<FrameRender> = left_frames
        .iter()
        .zip(right_frames.iter())
        .map(|(l, r)| render_frame(l, r))
        .collect();

    let n_fit = renders.len() - 1;
    println!("\n=== Measuring row-band disparity on {n_fit} fitting frame(s) ===");
    let per_frame_bands: Vec<Vec<Option<row_profile::RowBandDisparity>>> = renders[..n_fit]
        .iter()
        .map(|r| {
            row_profile::measure_row_bands(
                &r.left_luma,
                &r.right_luma,
                &r.mask,
                BAND_HEIGHT,
                MAX_DY,
                MIN_VARIANCE,
                MIN_CONFIDENCE,
            )
        })
        .collect();

    let combined = row_profile::combine_across_frames(&per_frame_bands, MAX_DY_SPREAD);
    let n_measured = combined.iter().filter(|b| b.is_some()).count();
    let n_total = combined.len();
    println!("  {n_measured}/{n_total} bands measured and stable across frames");
    for b in combined.iter().flatten() {
        println!(
            "    row~{:.0}: dy={:+.2}px confidence={:.3}",
            b.row_center, b.dy, b.confidence
        );
    }
    if n_measured == 0 {
        eprintln!("ERROR: no stable bands measured - aborting");
        std::process::exit(1);
    }

    let profile = RowProfile::fit(&combined, eval_height, TAPER_ROWS);

    // --- Multi-band before/after check on EVERY frame, including the held-out one ---
    println!("\n=== Near/mid/far band quality check (independent of what was optimized) ===");
    for (i, r) in renders.iter().enumerate() {
        let label = if i < n_fit {
            format!("frame {} (fit)", left_frame_indices[i])
        } else {
            format!(
                "frame {} (HELD OUT - not used in fitting)",
                left_frame_indices[i]
            )
        };
        println!("  {label}:");
        let corrected_right_rgba =
            profile.apply_vertical_shift(&r.right_rgba, eval_width, eval_height);
        let corrected_right_luma =
            photometric::to_luma(&corrected_right_rgba, eval_width, eval_height);
        let corrected_mask = photometric::overlap_mask_from_alpha(
            &r.left_rgba,
            &corrected_right_rgba,
            eval_width,
            eval_height,
            ALPHA_THRESHOLD,
        );

        report_band_thirds("    before", &r.left_luma, &r.right_luma, &r.mask);
        report_band_thirds(
            "    after ",
            &r.left_luma,
            &corrected_right_luma,
            &corrected_mask,
        );
    }

    // --- Visual dump: full composite before/after on the held-out frame ---
    println!(
        "\n=== Visual dump (held-out frame {}) -> {output_dir} ===",
        left_frame_indices[n_fit]
    );
    let held_out = &renders[n_fit];
    let before_composite = composite_over(
        &held_out.left_rgba,
        &held_out.right_rgba,
        eval_width,
        eval_height,
    );
    let corrected_right =
        profile.apply_vertical_shift(&held_out.right_rgba, eval_width, eval_height);
    let after_composite = composite_over(
        &held_out.left_rgba,
        &corrected_right,
        eval_width,
        eval_height,
    );

    save_rgba(
        &before_composite,
        eval_width,
        eval_height,
        &format!("{output_dir}/composite_before.png"),
    );
    save_rgba(
        &after_composite,
        eval_width,
        eval_height,
        &format!("{output_dir}/composite_after.png"),
    );

    let mut sbs = image::RgbaImage::new(eval_width, eval_height * 2);
    let before_img =
        image::RgbaImage::from_raw(eval_width, eval_height, before_composite.clone()).unwrap();
    let after_img =
        image::RgbaImage::from_raw(eval_width, eval_height, after_composite.clone()).unwrap();
    image::imageops::replace(&mut sbs, &before_img, 0, 0);
    image::imageops::replace(&mut sbs, &after_img, 0, eval_height as i64);
    sbs.save(format!("{output_dir}/comparison_stacked.png"))
        .unwrap();

    // Also dump the profile shape itself as a simple text file for reference.
    let profile_txt: String = (0..eval_height)
        .step_by(8)
        .map(|row| format!("{row}\t{:.3}\n", profile.dy_at(row)))
        .collect();
    std::fs::write(format!("{output_dir}/profile.tsv"), profile_txt).unwrap();

    println!(
        "Done. Inspect {output_dir}/comparison_stacked.png (before on top, after on bottom, \
         held-out frame never used in fitting) - and read the near/mid/far report above before \
         trusting this: it must show improvement in EVERY band, on EVERY frame including the \
         held-out one, or this has the same 'moved the problem elsewhere' flaw as every earlier \
         photometric experiment today."
    );
}

/// Report ZNCC quality independently in the near/mid/far thirds of the
/// mask's actual row extent - NOT the bands the profile was fit from -
/// so a fix local to one region can't hide a regression in another.
fn report_band_thirds(
    label: &str,
    left_luma: &[f32],
    right_luma: &[f32],
    mask: &photometric::OverlapMask,
) {
    let rows_with_coverage: Vec<u32> = (0..mask.height)
        .filter(|&y| (0..mask.width).any(|x| mask.mask[(y * mask.width + x) as usize]))
        .collect();
    if rows_with_coverage.is_empty() {
        println!("{label}: no overlap coverage at all");
        return;
    }
    let min_row = *rows_with_coverage.first().unwrap();
    let max_row = *rows_with_coverage.last().unwrap();
    let span = max_row - min_row;
    let third = (span / 3).max(1);

    let names = ["far ", "mid ", "near"];
    let mut summary = Vec::new();
    for (i, name) in names.iter().enumerate() {
        let band_start = min_row + i as u32 * third;
        let band_end = if i == 2 {
            max_row + 1
        } else {
            min_row + (i as u32 + 1) * third
        };
        let sub_mask = photometric::OverlapMask {
            width: mask.width,
            height: mask.height,
            mask: (0..mask.height)
                .flat_map(|y| {
                    (0..mask.width).map(move |x| {
                        y >= band_start && y < band_end && mask.mask[(y * mask.width + x) as usize]
                    })
                })
                .collect(),
        };
        let report = photometric::windowed_zncc(
            left_luma,
            right_luma,
            &sub_mask,
            CHECK_PATCH_SIZE,
            CHECK_MIN_PATCH_VARIANCE,
        );
        summary.push(format!(
            "{name}={:.4}({}p)",
            report.mean, report.valid_patches
        ));
    }
    println!("{label}: {}", summary.join("  "));
}

/// Standard alpha-over composite, matching the production stitch pass's
/// draw order (left first, then right blended on top with straight
/// alpha-over) - see `reco_core::render::renderer`'s blend state.
fn composite_over(left: &[u8], right: &[u8], width: u32, height: u32) -> Vec<u8> {
    let mut out = vec![0u8; (width * height * 4) as usize];
    for i in 0..(width * height) as usize {
        let lo = i * 4;
        let l = &left[lo..lo + 4];
        let r = &right[lo..lo + 4];
        let ra = r[3] as f32 / 255.0;
        for c in 0..3 {
            let v = r[c] as f32 * ra + l[c] as f32 * (1.0 - ra);
            out[lo + c] = v.round().clamp(0.0, 255.0) as u8;
        }
        let a = ra + (l[3] as f32 / 255.0) * (1.0 - ra);
        out[lo + 3] = (a * 255.0).round().clamp(0.0, 255.0) as u8;
    }
    out
}

fn flag_value<'a>(flags: &'a [String], name: &str) -> Option<&'a str> {
    flags
        .iter()
        .position(|f| f == name)
        .and_then(|i| flags.get(i + 1))
        .map(|s| s.as_str())
}

fn save_rgba(rgba: &[u8], width: u32, height: u32, path: &str) {
    image::RgbaImage::from_raw(width, height, rgba.to_vec())
        .expect("rgba buffer size mismatch")
        .save(path)
        .unwrap();
}
