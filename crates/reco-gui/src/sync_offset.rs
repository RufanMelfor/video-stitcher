//! Standalone sync-offset detection background job.
//!
//! Mirrors the IMU-then-audio priority `reco_calibrate::video::calibrate_videos`
//! uses internally (same as `reco-cli`'s `calibrate` subcommand: try IMU
//! telemetry cross-correlation first, fall back to audio cross-correlation),
//! but stopping there instead of continuing into AKAZE feature matching and
//! the optimizer. Much cheaper when only the sync offset needs
//! (re-)detecting - e.g. after re-clipping footage without touching an
//! already-good rig calibration.
//!
//! The job reports each step it enters over the same channel as the final
//! result, so the GUI can show a progress window while it runs (the IMU
//! telemetry parse and audio extraction can each take tens of seconds).

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender};

use reco_calibrate::CalibrationConfig;
use reco_calibrate::pipeline::{CalibrationPipeline, VideoInfo};
use reco_io::ffmpeg::calibration_io;

/// Result of a standalone sync-offset computation.
pub struct SyncOffsetResult {
    pub frames: i64,
    /// Which method produced the result, for status-line display.
    pub method: &'static str,
}

/// Message from the sync-offset background thread to the render tick.
pub enum SyncOffsetMsg {
    /// The job entered a new step; human-readable, shown in the progress
    /// window's step line and appended to its log.
    Progress(String),
    /// The job finished (successfully, with an error, or cancelled).
    /// Always the last message on the channel.
    Done(Result<SyncOffsetResult, String>),
}

/// Error text the job finishes with when it stopped because `cancel` was set.
pub const CANCELLED: &str = "cancelled";

/// Spawn a background computation of just the temporal sync offset
/// between two videos. Step updates and the final result come back over
/// an `mpsc` channel that the render tick polls (non-blocking `try_recv`).
///
/// `cancel` is checked between steps only - the IMU telemetry parse and
/// audio extraction themselves are not interruptible - so the job can
/// keep running for up to one step after it is set.
pub fn spawn_compute_sync_offset(
    left: PathBuf,
    right: PathBuf,
    cancel: Arc<AtomicBool>,
) -> Receiver<SyncOffsetMsg> {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let result = compute_sync_offset(&left, &right, &cancel, &tx);
        match &result {
            Ok(r) => log::info!(
                "Sync-offset detection complete: {} frames ({})",
                r.frames,
                r.method
            ),
            Err(e) if e == CANCELLED => log::info!("Sync-offset detection cancelled"),
            Err(e) => log::error!("Sync-offset detection failed: {e}"),
        }
        tx.send(SyncOffsetMsg::Done(result)).ok();
    });
    rx
}

fn compute_sync_offset(
    left: &Path,
    right: &Path,
    cancel: &AtomicBool,
    tx: &Sender<SyncOffsetMsg>,
) -> Result<SyncOffsetResult, String> {
    let step = |text: String| -> Result<(), String> {
        if cancel.load(Ordering::Relaxed) {
            return Err(CANCELLED.to_string());
        }
        log::info!("Sync-offset detection: {text}");
        tx.send(SyncOffsetMsg::Progress(text)).ok();
        Ok(())
    };

    step("Reading video headers".to_string())?;
    let left_probe = calibration_io::probe_video(left).map_err(|e| e.to_string())?;
    let right_probe = calibration_io::probe_video(right).map_err(|e| e.to_string())?;
    step(format!(
        "Left {}x{} @ {:.3} fps, {} frames - right {}x{} @ {:.3} fps, {} frames",
        left_probe.width,
        left_probe.height,
        left_probe.fps,
        left_probe.total_frames,
        right_probe.width,
        right_probe.height,
        right_probe.fps,
        right_probe.total_frames,
    ))?;

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
    step("Detecting camera and lens profiles".to_string())?;
    pipeline.detect_profiles().map_err(|e| e.to_string())?;

    step("IMU sync: reading camera telemetry (can take a minute)".to_string())?;
    if let Some(frames) = pipeline.imu_sync().map_err(|e| e.to_string())? {
        step(format!("IMU sync: {frames} frames"))?;
        return Ok(SyncOffsetResult {
            frames,
            method: "IMU",
        });
    }

    let sample_rate = 44_100u32;
    step("No usable IMU sync - extracting left audio".to_string())?;
    let left_samples =
        calibration_io::extract_audio_pcm(left, sample_rate).map_err(|e| e.to_string())?;
    step("Extracting right audio".to_string())?;
    let right_samples =
        calibration_io::extract_audio_pcm(right, sample_rate).map_err(|e| e.to_string())?;
    step("Cross-correlating audio".to_string())?;
    let frames = pipeline
        .audio_sync(&left_samples, &right_samples, sample_rate)
        .map_err(|e| e.to_string())?;
    step(format!("Audio sync: {frames} frames"))?;
    Ok(SyncOffsetResult {
        frames,
        method: "audio",
    })
}
