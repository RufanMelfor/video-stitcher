//! `reco ai-debug-raw`: raw-camera AI debug export subcommand.
//!
//! Thin CLI wiring around [`reco_io::raw_camera_debug::run`] - builds a
//! `CpuYoloDetector` from `--model` (the only detector backend that
//! accepts `DetectorFrame::Cpu`, exactly what decoded source frames
//! already are - see that module's doc comment for why this doesn't
//! reuse `reco_autocam::setup_autocam`'s GPU-detector wiring), resolves
//! the calibration's field ROI, and reports progress on stdout.
//!
//! Produces TWO output files (Left and Right), not one side-by-side
//! composite - see `raw_camera_debug`'s doc comment for why (H.264's
//! 4096px width limit).

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

/// Arguments for the `ai-debug-raw` subcommand.
pub struct AiDebugRawArgs<'a> {
    pub left: &'a str,
    pub right: &'a str,
    pub calibration: &'a str,
    /// Base output path - actual files get `_L`/`_R` inserted before the
    /// extension (see [`auto_output_names`]). Left empty to auto-name
    /// from the left video's filename.
    pub output: &'a str,
    pub model_path: &'a str,
    pub confidence_threshold: Option<f32>,
    pub start_time: Option<f64>,
    /// Stop at this source timestamp (seconds) - see
    /// `reco_io::raw_camera_debug::RawCameraDebugConfig::end_secs`.
    pub end_time: Option<f64>,
    pub sync_offset: i64,
    pub max_frames: Option<u64>,
    /// Time ranges to exclude from the export - see
    /// `reco_io::raw_camera_debug::RawCameraDebugConfig::cut_ranges`'s
    /// doc comment.
    pub cut_ranges: Vec<reco_io::cut_range::CutRange>,
    /// Detect every Nth frame - see
    /// `reco_io::raw_camera_debug::RawCameraDebugConfig::detection_interval`.
    pub detection_interval: u32,
    pub show_field_roi: bool,
    pub encoder_name: Option<&'a str>,
    pub codec: &'a str,
}

/// `left.mp4` -> `(left_ai_debug_raw_L.mp4, left_ai_debug_raw_R.mp4)`.
/// Mirrors `reco-gui`'s `add_highlights_suffix` / the removed stitched
/// overlay's `add_ai_debug_suffix` filename-marker convention, so an
/// auto-named diagnostic export is never mistaken for a real one. Two
/// names, not one, because this produces two separate files (see
/// `raw_camera_debug`'s doc comment for why: H.264's 4096px width
/// limit rules out one side-by-side composite).
fn auto_output_names(left: &str) -> (String, String) {
    let path = Path::new(left);
    let stem = path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "output".to_string());
    let parent = path.parent().filter(|p| !p.as_os_str().is_empty());
    let name = |suffix: &str| -> String {
        let filename = format!("{stem}_ai_debug_raw_{suffix}.mp4");
        match parent {
            Some(dir) => dir.join(filename).to_string_lossy().into_owned(),
            None => filename,
        }
    };
    (name("L"), name("R"))
}

/// Insert a `_L`/`_R` marker before a user-supplied output path's
/// extension, so an explicit `-o` still produces two distinguishable
/// files rather than one overwriting the other.
fn suffixed_output_names(output: &str) -> (String, String) {
    let path = Path::new(output);
    let stem = path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "output".to_string());
    let ext = path
        .extension()
        .map(|e| e.to_string_lossy().into_owned())
        .unwrap_or_else(|| "mp4".to_string());
    let parent = path.parent().filter(|p| !p.as_os_str().is_empty());
    let name = |suffix: &str| -> String {
        let filename = format!("{stem}_{suffix}.{ext}");
        match parent {
            Some(dir) => dir.join(filename).to_string_lossy().into_owned(),
            None => filename,
        }
    };
    (name("L"), name("R"))
}

pub fn run_ai_debug_raw(
    args: AiDebugRawArgs<'_>,
    interrupted: &Arc<AtomicBool>,
) -> anyhow::Result<()> {
    eprintln!(
        "WARNING: ai-debug-raw is a DIAGNOSTIC export drawing the model's raw detections \
         directly on the unstitched Left/Right camera feeds - two separate output files, one \
         per camera. It is NOT a stitched match export and is NOT meant for real distribution."
    );

    let (output_left, output_right) = if args.output.is_empty() {
        auto_output_names(args.left)
    } else {
        suffixed_output_names(args.output)
    };

    let cal = reco_core::calibration::Calibration::from_file(Path::new(args.calibration))?;
    let (field_roi_left, field_roi_right) = if args.show_field_roi {
        match &cal.field_roi {
            Some(roi) if roi.left.len() >= 3 || roi.right.len() >= 3 => {
                let densified = roi.densified(&cal.lenses[0], &cal.lenses[1]);
                (
                    (!densified.left.is_empty()).then_some(densified.left),
                    (!densified.right.is_empty()).then_some(densified.right),
                )
            }
            _ => (None, None),
        }
    } else {
        (None, None)
    };

    let codec =
        reco_io::ffmpeg::encoder::VideoCodec::from_str_loose(args.codec).unwrap_or_else(|| {
            log::warn!("Unknown codec '{}', defaulting to H.264", args.codec);
            reco_io::ffmpeg::encoder::VideoCodec::default()
        });
    let encoder_config = reco_io::ffmpeg::encoder::EncoderConfig {
        encoder_name: args.encoder_name.map(str::to_string),
        codec,
        ..Default::default()
    };

    let to_input = |s: &str| -> reco_io::stitch_job::InputPath {
        let parts: Vec<std::path::PathBuf> = s
            .split(';')
            .filter(|p| !p.is_empty())
            .map(std::path::PathBuf::from)
            .collect();
        if parts.len() > 1 {
            reco_io::stitch_job::InputPath::Chained(parts)
        } else {
            reco_io::stitch_job::InputPath::Single(std::path::PathBuf::from(s))
        }
    };

    // The detector's own label metadata resolves the real ball class
    // id - never assume COCO's ordering (see `setup_autocam`'s doc
    // comment for why: most of reco's own production checkpoints use a
    // different ordering, e.g. `reco-yolo26s-football`:
    // 0=person, 1=ball, 2=referee).
    let confidence = args.confidence_threshold.unwrap_or(0.10);
    let detector =
        reco_autocam::CpuYoloDetector::with_config(args.model_path, confidence, Vec::new())
            .map_err(|e| anyhow::anyhow!("failed to load model {}: {e}", args.model_path))?;
    let ball_class_id = detector
        .class_names()
        .iter()
        .position(|n| n.eq_ignore_ascii_case("ball") || n.eq_ignore_ascii_case("sports ball"))
        .map(|idx| idx as u16);
    match ball_class_id {
        Some(id) => log::info!("Resolved ball class id {id} from model labels"),
        None => log::warn!(
            "Model labels don't name a 'ball' class; ball-colored boxes will not be drawn \
             (every detection draws in the 'other' color instead)"
        ),
    }
    let input_size = detector.input_size();

    // GPU NV12 preprocessing (the same wgpu compute-shader path
    // `setup_autocam` wraps the real export's detector in): moves the
    // CPU-side resize/letterbox/normalize - the actual bottleneck
    // behind "zero GPU utilization" reports, not the ORT inference call
    // itself - onto the GPU. A throwaway `GpuContext` is created the
    // same validated way the real app does (`GpuContext::new_blocking`),
    // since this standalone diagnostic has no existing device to reuse.
    // Any failure here (no adapter, driver issue) falls back to the
    // original all-CPU `CpuYoloDetector` path rather than failing the
    // whole export - GPU preprocessing is a speed upgrade, never a
    // correctness requirement.
    let (detector, gpu): (Box<dyn reco_core::detect::detector::UnifiedDetector>, _) =
        match reco_core::gpu::GpuContext::new_blocking() {
            Ok(gpu) => {
                log::info!(
                    "ai-debug-raw: GPU NV12 preprocessing enabled ({}, {})",
                    gpu.gpu_name(),
                    gpu.backend_name(),
                );
                // `0, 0` defers sizing the preprocessor to the first
                // real `WgpuNv12` frame it sees: this diagnostic tool
                // doesn't know the source frame resolution yet (that's
                // resolved inside `raw_camera_debug::run`, from the
                // decoder, once Left/Right are opened), unlike
                // `setup_autocam`'s call site where a session is
                // already open. See `WgpuPreprocessingDetector::new`'s
                // doc comment.
                let wrapper = reco_autocam::WgpuPreprocessingDetector::new(
                    Box::new(detector),
                    gpu.device().clone(),
                    gpu.queue().clone(),
                    input_size,
                    0,
                    0,
                );
                (Box::new(wrapper), Some(gpu))
            }
            Err(e) => {
                log::warn!(
                    "ai-debug-raw: GPU context init failed ({e}), falling back to CPU \
                     preprocessing (slower, same detections)"
                );
                (Box::new(detector), None)
            }
        };

    let config = reco_io::raw_camera_debug::RawCameraDebugConfig {
        left: to_input(args.left),
        right: to_input(args.right),
        output_left: std::path::PathBuf::from(&output_left),
        output_right: std::path::PathBuf::from(&output_right),
        start_secs: args.start_time.unwrap_or(0.0),
        end_secs: args.end_time,
        sync_offset_right: args.sync_offset,
        max_frames: args.max_frames,
        cut_ranges: args.cut_ranges,
        detection_interval: args.detection_interval,
        ball_class_id,
        confidence_threshold: args.confidence_threshold,
        field_roi_left,
        field_roi_right,
        encoder: encoder_config,
        gpu,
    };

    // `run` processes Left fully, then Right - see its own doc comment
    // for why. `p.frames_done` counts within the current camera's own
    // pass, not a combined total, so a fresh `ProgressReporter` per
    // pass (reset when `p.camera` changes) keeps the printed count from
    // visibly resetting to 0 mid-stream without explanation.
    let mut progress = crate::helpers::ProgressReporter::new(30);
    let mut current_camera = None;
    let frames = reco_io::raw_camera_debug::run(config, detector, interrupted, |p| {
        if current_camera != Some(p.camera) {
            current_camera = Some(p.camera);
            println!("\n{} pass:", p.camera);
            progress = crate::helpers::ProgressReporter::new(30);
        }
        progress.report(p.frames_done);
    })
    .map_err(|e| anyhow::anyhow!("{e}"))?;

    println!("\nDone: {frames} frames -> {output_left} and {output_right}");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auto_output_names_append_l_r_suffixes_next_to_left_video() {
        assert_eq!(
            auto_output_names("left.mp4"),
            (
                "left_ai_debug_raw_L.mp4".to_string(),
                "left_ai_debug_raw_R.mp4".to_string()
            )
        );
    }

    #[test]
    fn auto_output_names_keep_the_left_videos_directory() {
        let (l, r) = auto_output_names("videos/left.mp4");
        assert_eq!(
            l,
            Path::new("videos")
                .join("left_ai_debug_raw_L.mp4")
                .to_string_lossy()
        );
        assert_eq!(
            r,
            Path::new("videos")
                .join("left_ai_debug_raw_R.mp4")
                .to_string_lossy()
        );
    }

    #[test]
    fn auto_output_names_handle_no_extension() {
        assert_eq!(
            auto_output_names("left"),
            (
                "left_ai_debug_raw_L.mp4".to_string(),
                "left_ai_debug_raw_R.mp4".to_string()
            )
        );
    }

    #[test]
    fn suffixed_output_names_insert_l_r_before_extension() {
        assert_eq!(
            suffixed_output_names("out.mp4"),
            ("out_L.mp4".to_string(), "out_R.mp4".to_string())
        );
    }

    #[test]
    fn suffixed_output_names_keep_the_directory() {
        let (l, r) = suffixed_output_names("videos/out.mp4");
        assert_eq!(l, Path::new("videos").join("out_L.mp4").to_string_lossy());
        assert_eq!(r, Path::new("videos").join("out_R.mp4").to_string_lossy());
    }
}
