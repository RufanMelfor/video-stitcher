//! Generate an AKAZE matched-points file from raw fisheye PNG frame pairs.
//!
//! `examples/dump_points.rs` needs the source *videos*, which aren't always
//! available (multi-GB match recordings live on a separate drive). This
//! harness runs the same pipeline from the already-extracted raw frame PNGs
//! that `resources/export_synced_frames.py` produces - e.g. the committed
//! pairs under `resources/test-data/frames/` - so the far-field cross-check
//! in `examples/fit_ground_tilt_manual.rs` (`--matched-points`) can run from
//! data that ships with the repo (FRICTION.md point 20).
//!
//! Pairs are discovered by filename: every `frame<N>_left.png` is matched
//! with the `frame<M>_right.png` that follows it in numeric order (the
//! export script already applied `sync_offset` when extracting, so the pair
//! counts and ordering line up by construction; `N`/`M` differ by the
//! offset).
//!
//! The full [`reco_calibrate::calibrate`] pipeline runs - GPU undistort with
//! the match.json intrinsics, AKAZE detect/match/filter with production
//! defaults - and `CalibrationResult::per_frame` is written out, which is
//! exactly the `Vec<FrameMatches>` shape `--matched-points` deserializes.
//! The freshly optimized placement parameters are printed too, as a free
//! sanity comparison against the loaded match.json.
//!
//! Usage:
//! ```text
//! cargo run --release -p reco-calibrate --example match_png_frames -- \
//!   <match.json> <frames_dir> <out_matched_points.json>
//! ```

use std::path::{Path, PathBuf};

use reco_calibrate::types::{CalibrationConfig, YuvFrame};
use reco_core::calibration::Calibration;
use reco_core::gpu::GpuContext;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() != 4 {
        eprintln!(
            "Usage: {} <match.json> <frames_dir> <out_matched_points.json>",
            args[0]
        );
        std::process::exit(1);
    }
    let (match_path, frames_dir, out_path) = (&args[1], &args[2], &args[3]);

    let cal: Calibration = {
        let s = std::fs::read_to_string(match_path)
            .unwrap_or_else(|e| panic!("failed to read {match_path}: {e}"));
        serde_json::from_str(&s)
            .unwrap_or_else(|e| panic!("failed to parse {match_path} as Calibration: {e}"))
    };

    let (left_paths, right_paths) = discover_pairs(Path::new(frames_dir));
    if left_paths.is_empty() {
        eprintln!("no frame<N>_left.png / frame<N>_right.png pairs found in {frames_dir}");
        std::process::exit(1);
    }
    eprintln!("Found {} frame pair(s) in {frames_dir}", left_paths.len());

    let mut frames: Vec<(YuvFrame, YuvFrame)> = Vec::with_capacity(left_paths.len());
    for (lp, rp) in left_paths.iter().zip(right_paths.iter()) {
        eprintln!(
            "  loading {} + {}",
            lp.file_name().unwrap().to_string_lossy(),
            rp.file_name().unwrap().to_string_lossy()
        );
        frames.push((load_png_as_yuv(lp), load_png_as_yuv(rp)));
    }

    let gpu = pollster::block_on(GpuContext::new()).expect("no GPU available");
    eprintln!("GPU: {}", gpu.gpu_name());

    let config = CalibrationConfig::default();
    let result =
        reco_calibrate::calibrate(&gpu, &frames, &cal.lenses[0], &cal.lenses[1], &config)
            .unwrap_or_else(|e| panic!("calibration pipeline failed: {e}"));

    eprintln!(
        "\n{} total matches across {} frame pair(s), confidence {:.0}%",
        result.total_matches,
        result.frames_used,
        result.confidence * 100.0
    );
    if let Some(q) = &result.quality {
        eprintln!(
            "quality: mean_reproj={:.6} trimmed={:.6} angular={:.6}",
            q.mean_reprojection_error, q.trimmed_reprojection_error, q.angular_error
        );
    }
    let (fresh_topology, loaded_topology) = (&result.calibration.topology, &cal.topology);
    let (fresh_framing, loaded_framing) = (&result.calibration.framing, &cal.framing);
    eprintln!("\nFresh fit vs loaded {match_path} (sanity comparison, not written anywhere):");
    eprintln!(
        "  cam_d     {:+.4} vs {:+.4}\n  intersect {:+.4} vs {:+.4}\n  x_ty      {:+.4} vs {:+.4}\n  x_rz      {:+.4} vs {:+.4}\n  z_rx      {:+.4} vs {:+.4}",
        fresh_framing.axis_offset,
        loaded_framing.axis_offset,
        fresh_topology.intersect,
        loaded_topology.intersect,
        fresh_topology.x_ty,
        loaded_topology.x_ty,
        fresh_topology.x_rz,
        loaded_topology.x_rz,
        fresh_topology.z_rx,
        loaded_topology.z_rx,
    );

    let json = serde_json::to_string_pretty(&result.per_frame).unwrap();
    std::fs::write(out_path, &json).unwrap_or_else(|e| panic!("failed to write {out_path}: {e}"));
    eprintln!(
        "\nWrote {} frame(s) of matched points to {out_path} - feed to \
         fit_ground_tilt_manual via --matched-points",
        result.per_frame.len()
    );
}

/// Find `frame<N>_left.png` / `frame<N>_right.png` files and pair them in
/// numeric order. The counts must match - the export script writes both
/// halves of each pair or neither.
fn discover_pairs(dir: &Path) -> (Vec<PathBuf>, Vec<PathBuf>) {
    let mut lefts: Vec<(u64, PathBuf)> = Vec::new();
    let mut rights: Vec<(u64, PathBuf)> = Vec::new();
    for entry in std::fs::read_dir(dir)
        .unwrap_or_else(|e| panic!("cannot read {}: {e}", dir.display()))
        .flatten()
    {
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        let Some(rest) = name.strip_prefix("frame") else {
            continue;
        };
        if let Some(num) = rest.strip_suffix("_left.png") {
            if let Ok(n) = num.parse::<u64>() {
                lefts.push((n, path));
            }
        } else if let Some(num) = rest.strip_suffix("_right.png")
            && let Ok(n) = num.parse::<u64>()
        {
            rights.push((n, path));
        }
    }
    lefts.sort_by_key(|(n, _)| *n);
    rights.sort_by_key(|(n, _)| *n);
    assert_eq!(
        lefts.len(),
        rights.len(),
        "unpaired frames: {} left vs {} right PNGs",
        lefts.len(),
        rights.len()
    );
    (
        lefts.into_iter().map(|(_, p)| p).collect(),
        rights.into_iter().map(|(_, p)| p).collect(),
    )
}

/// Load an RGB(A) PNG and convert to limited-range BT.709 YUV420P - the
/// exact convention the undistort shader expects (`build_gpu_uniforms` is
/// called with `is_full_range = false`, and `fisheye.wgsl` unpacks with the
/// BT.709 matrix), matching what the H.264 video decode path produces.
fn load_png_as_yuv(path: &Path) -> YuvFrame {
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
    // Per-pixel full-resolution Cb/Cr, box-averaged 2x2 below. f32 keeps the
    // intermediate rounding out of the subsample average.
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
