//! Visual + numeric verification of `PlaneLayout::ground_tilt_x`/`ground_tilt_z`
//! production wiring (FRICTION.md point 20's "how to actually make progress"
//! entry, and the `PlaneLayout`/`fisheye.wgsl` wiring done the same day).
//!
//! Renders the real seam region twice with [`SingleCameraRenderer`] - the
//! same isolated-plane harness `fit_photometric.rs` already uses for
//! calibration-quality validation - once with `ground_tilt` zeroed out
//! (baseline) and once with the fitted values from `match.json`, using
//! IDENTICAL frame data both times. This is the project's own standing
//! rule in practice: the math was already GPU-cross-checked against the
//! Rust reference on a synthetic grid
//! (`geometry::wgsl_ground_warp_matches_rust_on_grid`), but "the shader
//! compiles and matches a grid of numbers" isn't the same claim as "the
//! stitched output actually looks different in the near field and
//! identical in the far field on a real photo" - this example checks the
//! second claim.
//!
//! Usage:
//! ```text
//! cargo run --release -p reco-calibrate --example verify_ground_tilt -- \
//!   <match.json> <left.png> <right.png> <output_dir>
//! ```
//!
//! `match.json` should have `groundTiltX`/`groundTiltZ` already set (e.g.
//! `resources/test-data/match_werkplaats.json` after point 20's fit,
//! -0.090/-0.085) - this harness renders that same layout with and
//! without the correction, it doesn't fit anything itself.

use reco_core::calibration::MatchCalibration;
use reco_core::gpu::GpuContext;
use reco_core::render::renderer::{GroundTilt, TopTilt};
use reco_core::render::scene::SceneGeometry;
use reco_core::render::single_camera::SingleCameraRenderer;
use reco_core::source::YuvFrame;

const FOV_DEGREES: f32 = 100.0;
const EVAL_WIDTH: u32 = 1280;
const EVAL_HEIGHT: u32 = 1280;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() != 5 {
        eprintln!(
            "Usage: {} <match.json> <left.png> <right.png> <output_dir>",
            args[0]
        );
        std::process::exit(1);
    }
    let (match_path, left_path, right_path, out_dir) = (&args[1], &args[2], &args[3], &args[4]);
    std::fs::create_dir_all(out_dir).expect("failed to create output_dir");

    let cal: MatchCalibration = {
        let s = std::fs::read_to_string(match_path)
            .unwrap_or_else(|e| panic!("failed to read {match_path}: {e}"));
        serde_json::from_str(&s)
            .unwrap_or_else(|e| panic!("failed to parse {match_path} as MatchCalibration: {e}"))
    };
    eprintln!(
        "Loaded ground_tilt_x={:+.4} ground_tilt_z={:+.4} from {match_path}",
        cal.layout.ground_tilt_x, cal.layout.ground_tilt_z
    );
    if cal.layout.ground_tilt_x == 0.0 && cal.layout.ground_tilt_z == 0.0 {
        eprintln!(
            "WARNING: both are 0.0 - baseline and corrected renders will be identical. \
             Pass a match.json with groundTiltX/groundTiltZ already set (see FRICTION.md point 20)."
        );
    }

    let left_yuv = load_png_as_yuv(std::path::Path::new(left_path));
    let right_yuv = load_png_as_yuv(std::path::Path::new(right_path));
    assert_eq!(
        (left_yuv.width, left_yuv.height),
        (right_yuv.width, right_yuv.height),
        "left/right frames must share one resolution"
    );

    let gpu = GpuContext::new_blocking().expect("no GPU");
    let aspect = left_yuv.width as f32 / left_yuv.height as f32;
    let scene = SceneGeometry::from_layout_with_aspect(&cal.layout, aspect);

    let left_renderer = SingleCameraRenderer::new(
        &gpu,
        left_yuv.width,
        left_yuv.height,
        EVAL_WIDTH,
        EVAL_HEIGHT,
        aspect,
    );
    let right_renderer = SingleCameraRenderer::new(
        &gpu,
        right_yuv.width,
        right_yuv.height,
        EVAL_WIDTH,
        EVAL_HEIGHT,
        aspect,
    );

    // Same left<->z-plane / right<->x-plane mapping as
    // Renderer::encode_stitch_pass (crates/reco-core/src/render/renderer.rs)
    // - see that function's comment for why it's not the naive pairing.
    let left_ground_tilt = GroundTilt {
        tilt: cal.layout.ground_tilt_z as f32,
        k: cal.left.ground_tilt_k() as f32,
        band_full: cal.layout.ground_tilt_band_width as f32,
    };
    let right_ground_tilt = GroundTilt {
        tilt: cal.layout.ground_tilt_x as f32,
        k: cal.right.ground_tilt_k() as f32,
        band_full: cal.layout.ground_tilt_band_width as f32,
    };

    let render_pair = |gt_left: GroundTilt, gt_right: GroundTilt| -> Vec<u8> {
        let left_rgba = left_renderer.render_and_readback(
            &gpu,
            &scene,
            &cal.left,
            false,
            FOV_DEGREES,
            &left_yuv.y,
            &left_yuv.u,
            &left_yuv.v,
            gt_left,
            TopTilt::default(),
        );
        let right_rgba = right_renderer.render_and_readback(
            &gpu,
            &scene,
            &cal.right,
            true,
            FOV_DEGREES,
            &right_yuv.y,
            &right_yuv.u,
            &right_yuv.v,
            gt_right,
            TopTilt::default(),
        );
        composite_hard_seam(&left_rgba, &right_rgba)
    };

    eprintln!("Rendering baseline (ground_tilt = 0)...");
    let baseline = render_pair(GroundTilt::default(), GroundTilt::default());
    eprintln!("Rendering corrected (fitted ground_tilt)...");
    let corrected = render_pair(left_ground_tilt, right_ground_tilt);

    let (w, h) = (EVAL_WIDTH, EVAL_HEIGHT);
    save_rgba(&baseline, w, h, &format!("{out_dir}/baseline.png"));
    save_rgba(&corrected, w, h, &format!("{out_dir}/corrected.png"));

    // Amplified abs-diff image (x8, clamped) - makes a subtle shift visible
    // at a glance instead of requiring pixel-peeping two near-identical PNGs.
    let mut diff = vec![0u8; baseline.len()];
    for i in (0..baseline.len()).step_by(4) {
        for c in 0..3 {
            let d = (baseline[i + c] as i32 - corrected[i + c] as i32).unsigned_abs();
            diff[i + c] = (d * 8).min(255) as u8;
        }
        diff[i + 3] = 255;
    }
    save_rgba(&diff, w, h, &format!("{out_dir}/diff_x8.png"));

    // Numeric band-limiting check: mean abs diff in the top 20% of the
    // frame (far field - should be ~0, band_limited_ground_warp is
    // identity there by construction) vs the bottom 20% (near field -
    // should be visibly nonzero whenever ground_tilt != 0).
    let band_mean_diff = |y_frac_lo: f32, y_frac_hi: f32| -> f64 {
        let y0 = (y_frac_lo * h as f32) as u32;
        let y1 = (y_frac_hi * h as f32) as u32;
        let mut sum = 0f64;
        let mut n = 0u64;
        for y in y0..y1 {
            for x in 0..w {
                let i = ((y * w + x) * 4) as usize;
                for c in 0..3 {
                    sum += (baseline[i + c] as f64 - corrected[i + c] as f64).abs();
                    n += 1;
                }
            }
        }
        if n == 0 { 0.0 } else { sum / n as f64 }
    };
    let far = band_mean_diff(0.0, 0.2);
    let near = band_mean_diff(0.8, 1.0);
    eprintln!("\nMean abs pixel diff (0-255 scale), baseline vs corrected:");
    eprintln!("  far field  (top 20%%):    {far:.4}");
    eprintln!("  near field (bottom 20%%): {near:.4}");
    eprintln!(
        "\nWrote {out_dir}/baseline.png, corrected.png, diff_x8.png - inspect visually: far \
         field should look identical, near field should show a real shift."
    );
}

/// Hard-cutover composite (no feathering): right camera's content wherever
/// its coverage alpha says valid, left camera's otherwise. Matches this
/// project's own `--blend 0` diagnostic convention (FRICTION.md) - a
/// visible geometric shift shouldn't be masked by seam feathering.
fn composite_hard_seam(left_rgba: &[u8], right_rgba: &[u8]) -> Vec<u8> {
    let mut out = vec![0u8; left_rgba.len()];
    for i in (0..left_rgba.len()).step_by(4) {
        let right_alpha = right_rgba[i + 3];
        let src = if right_alpha > 127 {
            right_rgba
        } else {
            left_rgba
        };
        out[i..i + 4].copy_from_slice(&src[i..i + 4]);
    }
    out
}

fn save_rgba(rgba: &[u8], w: u32, h: u32, path: &str) {
    image::RgbaImage::from_raw(w, h, rgba.to_vec())
        .expect("RGBA buffer size mismatch")
        .save(path)
        .unwrap_or_else(|e| panic!("failed to save {path}: {e}"));
}

/// Load an RGB(A) PNG and convert to limited-range BT.709 YUV420P - same
/// conversion as `examples/match_png_frames.rs`'s `load_png_as_yuv`
/// (duplicated rather than shared: examples can't import each other, and
/// this ~30-line conversion is small enough that the project's own
/// precedent - `lens::undistort::GpuUndistort` duplicating `quad_vertices`
/// - is to just duplicate rather than force a shared module for it).
fn load_png_as_yuv(path: &std::path::Path) -> YuvFrame {
    let img = image::open(path)
        .unwrap_or_else(|e| panic!("failed to load {}: {e}", path.display()))
        .into_rgb8();
    let (w, h) = (img.width(), img.height());
    assert!(
        w % 2 == 0 && h % 2 == 0,
        "{}: YUV420 needs even dimensions, got {w}x{h}",
        path.display()
    );

    let rgb = img.as_raw();
    let (wu, hu) = (w as usize, h as usize);
    let mut y_plane = vec![0u8; wu * hu];
    let mut cb_full = vec![0f32; wu * hu];
    let mut cr_full = vec![0f32; wu * hu];

    for i in 0..wu * hu {
        let r = rgb[i * 3] as f32 / 255.0;
        let g = rgb[i * 3 + 1] as f32 / 255.0;
        let b = rgb[i * 3 + 2] as f32 / 255.0;
        let y = 0.2126 * r + 0.7152 * g + 0.0722 * b;
        y_plane[i] = (16.0 + 219.0 * y).round().clamp(0.0, 255.0) as u8;
        cb_full[i] = (b - y) / 1.8556;
        cr_full[i] = (r - y) / 1.5748;
    }

    let (hw, hh) = (wu / 2, hu / 2);
    let mut u_plane = vec![0u8; hw * hh];
    let mut v_plane = vec![0u8; hw * hh];
    for by in 0..hh {
        for bx in 0..hw {
            let (x0, y0) = (bx * 2, by * 2);
            let idx = [
                x0 + y0 * wu,
                x0 + 1 + y0 * wu,
                x0 + (y0 + 1) * wu,
                x0 + 1 + (y0 + 1) * wu,
            ];
            let cb = idx.iter().map(|&i| cb_full[i]).sum::<f32>() / 4.0;
            let cr = idx.iter().map(|&i| cr_full[i]).sum::<f32>() / 4.0;
            u_plane[bx + by * hw] = (128.0 + 224.0 * cb).round().clamp(0.0, 255.0) as u8;
            v_plane[bx + by * hw] = (128.0 + 224.0 * cr).round().clamp(0.0, 255.0) as u8;
        }
    }

    YuvFrame {
        y: y_plane,
        u: u_plane,
        v: v_plane,
        width: w,
        height: h,
        timestamp_us: 0,
    }
}
