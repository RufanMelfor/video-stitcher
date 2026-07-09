//! Auto-calibrate background thread wiring.
//!
//! Runs `reco_calibrate::video::calibrate_videos` off the UI thread so
//! feature detection/matching (which can take several seconds on 4K
//! footage) never blocks the render loop. Progress is marshalled back
//! onto the Slint event loop via `slint::invoke_from_event_loop`; the
//! final result comes back over an `mpsc` channel that the render tick
//! polls (non-blocking `try_recv`).

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::sync::mpsc::Receiver;

use reco_calibrate::pipeline::{CalibrationPipeline, VideoInfo};
use reco_calibrate::video::{CalibrateVideosError, CalibrateVideosOptions};
use reco_calibrate::{CalibrationConfig, CalibrationResult};
use reco_core::calibration::CameraParams;
use reco_io::ffmpeg::calibration_io;

/// Tuning knobs surfaced to the UI for auto-calibrate. Mirrors the
/// sliders reco-gui exposes (calibration frame count, AKAZE detection
/// threshold/band, end-of-clip skip, IMU rotation seeds).
pub struct AutoCalibrateParams {
    pub left: PathBuf,
    pub right: PathBuf,
    pub start_secs: f64,
    pub num_frames: usize,
    pub akaze_threshold: f64,
    pub detect_y_min: f64,
    pub detect_y_max: f64,
    pub skip_end_secs: f64,
    pub use_imu_seeds: bool,
    /// Force the optimizer to solve the right camera's independent pitch
    /// (x_rx), bypassing the IMU differential-pitch>2deg auto-enable gate.
    /// Needed on rigs without a native gyro, where that gate never fires.
    pub force_x_rx: bool,
    /// Force the optimizer to solve the left camera's independent roll
    /// (z_rz). No IMU signal maps to this angle, so there's no auto-enable
    /// gate to bypass - it's manual-only.
    pub force_z_rz: bool,
    /// Max frame width for AKAZE feature detection (see
    /// `reco_calibrate::types::AkazeConfig::detect_max_width`). `0`
    /// detects at full source resolution instead of the 1920px default.
    pub detect_max_width: u32,
    pub existing_left_params: Option<CameraParams>,
    pub existing_right_params: Option<CameraParams>,
}

/// A running auto-calibrate job: the interrupt flag (set to cancel) and
/// the channel the render tick polls for the final result.
pub struct AutoCalibrateHandle {
    pub rx: Receiver<Result<CalibrationResult, CalibrateVideosError>>,
    pub interrupted: Arc<AtomicBool>,
}

/// Spawn auto-calibrate on a background thread.
///
/// `on_progress` is called from the background thread; it must
/// marshal onto the Slint event loop itself (via
/// `slint::invoke_from_event_loop`) before touching any UI state. The
/// `DetectionPreview` argument is the annotated AKAZE keypoint image for
/// the frame pair that just finished (only present on `FeatureMatching`
/// progress updates).
pub fn spawn_auto_calibrate(
    params: AutoCalibrateParams,
    on_progress: impl Fn(String, String, Option<f32>, Option<reco_calibrate::preview::DetectionPreview>)
    + Send
    + 'static,
) -> AutoCalibrateHandle {
    let interrupted = Arc::new(AtomicBool::new(false));
    let (tx, rx) = std::sync::mpsc::channel();

    let interrupted_bg = Arc::clone(&interrupted);
    std::thread::spawn(move || {
        log::info!(
            "Auto-calibrate: {} frames, skip=[{:.1}s, -{:.0}s], imu_seeds={}, force_x_rx={}, \
             force_z_rz={}, akaze={}, detect_y=[{:.2}, {:.2}], detect_max_width={}",
            params.num_frames,
            params.start_secs,
            params.skip_end_secs,
            params.use_imu_seeds,
            params.force_x_rx,
            params.force_z_rz,
            params.akaze_threshold,
            params.detect_y_min,
            params.detect_y_max,
            params.detect_max_width,
        );

        let mut config = CalibrationConfig {
            num_frames: params.num_frames,
            skip_start_secs: params.start_secs,
            skip_end_secs: params.skip_end_secs,
            use_imu_rotation_seeds: params.use_imu_seeds,
            ..Default::default()
        };
        config.akaze.threshold = params.akaze_threshold;
        config.akaze.detect_y_min = params.detect_y_min;
        config.akaze.detect_y_max = params.detect_y_max;
        config.akaze.detect_max_width = params.detect_max_width;
        if params.force_x_rx {
            config.optimizer.enable_x_rx = true;
        }
        if params.force_z_rz {
            config.optimizer.enable_z_rz = true;
        }

        if params.existing_left_params.is_some() {
            log::info!("Re-calibrating with user-picked lens profiles");
        }

        let result = reco_calibrate::video::calibrate_videos(
            &params.left,
            &params.right,
            CalibrateVideosOptions {
                config: Some(config),
                left_params: params.existing_left_params,
                right_params: params.existing_right_params,
                ..Default::default()
            },
            &mut |progress| {
                on_progress(
                    format!("{:?}", progress.step),
                    progress.detail.clone(),
                    progress.fraction,
                    progress.preview.clone(),
                );
            },
            &interrupted_bg,
        );

        match &result {
            Ok(r) => log::info!("Auto-calibration complete: {} matches", r.total_matches),
            Err(e) => log::error!("Auto-calibration failed: {e}"),
        }
        tx.send(result).ok();
    });

    AutoCalibrateHandle { rx, interrupted }
}

/// Result of a standalone sync-offset computation.
pub struct SyncOffsetResult {
    pub frames: i64,
    /// Which method produced the result, for status-line display.
    pub method: &'static str,
}

/// Spawn a background computation of just the temporal sync offset
/// between two videos - the same IMU-then-audio priority `Auto-Calibrate`
/// uses internally (mirrors `reco-cli`'s `calibrate` subcommand: try IMU
/// telemetry cross-correlation first, fall back to audio cross-
/// correlation), but stopping there instead of continuing into AKAZE
/// feature matching and the optimizer. Much cheaper when only the sync
/// offset needs (re-)detecting - e.g. after re-clipping footage without
/// touching an already-good rig calibration.
pub fn spawn_compute_sync_offset(
    left: PathBuf,
    right: PathBuf,
) -> Receiver<Result<SyncOffsetResult, String>> {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let result = compute_sync_offset(&left, &right);
        match &result {
            Ok(r) => log::info!(
                "Sync-offset detection complete: {} frames ({})",
                r.frames,
                r.method
            ),
            Err(e) => log::error!("Sync-offset detection failed: {e}"),
        }
        tx.send(result).ok();
    });
    rx
}

fn compute_sync_offset(left: &Path, right: &Path) -> Result<SyncOffsetResult, String> {
    let left_probe = calibration_io::probe_video(left).map_err(|e| e.to_string())?;
    let right_probe = calibration_io::probe_video(right).map_err(|e| e.to_string())?;

    let left_info = VideoInfo {
        path: left.to_path_buf(),
        width: left_probe.width,
        height: left_probe.height,
        fps: left_probe.fps,
        total_frames: left_probe.total_frames,
    };
    let right_info = VideoInfo {
        path: right.to_path_buf(),
        width: right_probe.width,
        height: right_probe.height,
        fps: right_probe.fps,
        total_frames: right_probe.total_frames,
    };

    let mut pipeline =
        CalibrationPipeline::new(left_info, right_info, CalibrationConfig::default());
    // Needed for `has_native_gyro`, which gates `imu_sync` - see its own
    // doc comment on why IMU sync is unreliable on quaternion-only cameras.
    pipeline.detect_profiles().map_err(|e| e.to_string())?;

    if let Some(frames) = pipeline.imu_sync().map_err(|e| e.to_string())? {
        return Ok(SyncOffsetResult {
            frames,
            method: "IMU",
        });
    }

    let sample_rate = 44_100u32;
    let left_samples =
        calibration_io::extract_audio_pcm(left, sample_rate).map_err(|e| e.to_string())?;
    let right_samples =
        calibration_io::extract_audio_pcm(right, sample_rate).map_err(|e| e.to_string())?;
    let frames = pipeline
        .audio_sync(&left_samples, &right_samples, sample_rate)
        .map_err(|e| e.to_string())?;
    Ok(SyncOffsetResult {
        frames,
        method: "audio",
    })
}
