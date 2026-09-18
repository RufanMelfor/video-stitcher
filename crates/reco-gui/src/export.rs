//! Export worker thread.
//!
//! Runs a [`StitchJob`](reco_io::StitchJob) on a background thread,
//! pumping progress back to the Slint UI via `invoke_from_event_loop`.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use reco_core::calibration::Calibration;
use reco_core::render::overlay::{OverlayFrame, OverlayFrameSource};

use crate::RecoApp;
use crate::scoreboard_import::{MatchLoggerExport, SyncAnchor};

/// Snapshot needed to drive a time-varying scoreboard through export - the
/// parsed Match Logger event log plus its video<->wall-clock sync anchor.
/// Cloned from `AppState` before spawning the worker thread, same reason
/// as `AutocamUiConfig` above. Replaces `scoreboard_state`'s one frozen
/// snapshot with a per-frame replay when present (see `run_export`).
#[derive(Debug, Clone)]
pub struct ScoreboardReplay {
    pub export: MatchLoggerExport,
    pub anchor: SyncAnchor,
}

/// Adapts a shared, running [`reco_scoreboard::ScoreboardRuntime`] to
/// [`OverlayFrameSource`] so the encode session can pull frames from it
/// (`try_frame`, needs `&mut self`) while the progress callback
/// independently pushes replayed state into it (`update`, only needs
/// `&self`) from the same underlying runtime - `update()`'s effect is a
/// non-blocking channel send, so the two never actually contend.
///
/// No longer gates on a "first push landed" flag (an earlier version
/// of this did) - that only masked the symptom and raced against the
/// browser's own async capture loop besides (the flag could flip true
/// before a *fresh* screenshot reflecting the push had actually been
/// captured, so the very next pull could still hand back the stale
/// placeholder). The real fix is at the source: `run_export` now seeds
/// `ScoreboardRuntime::start_with_state` with the *correct* frame-0
/// state up front, so the very first screenshot the browser ever
/// takes already matches - see the `initial_scoreboard_state` doc
/// comment there.
struct SharedScoreboardSource(Arc<Mutex<reco_scoreboard::ScoreboardRuntime>>);

impl OverlayFrameSource for SharedScoreboardSource {
    fn try_frame(&mut self) -> Result<Option<OverlayFrame>, String> {
        self.0
            .lock()
            .map_err(|_| "scoreboard runtime lock poisoned".to_string())?
            .try_frame()
    }
}

/// Result published by the export thread.
#[derive(Debug)]
pub enum ExportOutcome {
    /// Finished successfully - carries (frames, output path).
    Ok(u64, PathBuf),
    /// Export was cancelled by the user.
    Cancelled,
    /// Export failed with the structured error.
    Failed(reco_io::stitch_job::StitchError),
}

/// Autocam + panner settings captured from the GUI export panel.
///
/// Slint properties can only be read on the UI thread, so the caller
/// snapshots them into this struct before spawning the worker thread.
/// String fields (`tracking_mode`, `framing`, `cluster_mode`) carry the
/// Slint combo values and are mapped to the reco-autocam enums inside
/// [`run_export`].
///
/// In an AI-less build (`--no-default-features`, no detection backend) the
/// fields are only snapshotted and never read, so dead-code is allowed for
/// that configuration alone.
#[cfg_attr(not(feature = "autocam"), allow(dead_code))]
#[derive(Debug, Clone)]
pub struct AutocamUiConfig {
    /// Master toggle for the AI tracking pipeline.
    pub enabled: bool,
    /// Path to the YOLO model (ONNX/engine/NCNN dir). Empty = no-op.
    pub model_path: String,
    /// `"field"`, `"ball"`, or `"sweep"`.
    pub tracking_mode: String,
    /// Run the detector every N frames.
    pub detection_interval: u32,
    /// Max distance (radians) a raw ball detection may be from the
    /// nearest tracked player and still be accepted by the ball tracker,
    /// before the panner ever sees it. See
    /// `reco_autocam::AutocamConfig::player_anchor_max_rad`.
    pub player_anchor_rad: f32,
    /// Ball tracker's coast budget (seconds) - how long it holds the
    /// last known ball position through a detection gap (including a
    /// brief field-ROI exit) before declaring the track lost. See
    /// `reco_autocam::AutocamConfig::ball_coast_secs`.
    pub ball_coast_secs: f32,
    /// Max panorama distance (radians) at which a *new* ball track may
    /// start from the main player group; `0` disables the gate. See
    /// `reco_autocam::AutocamConfig::ball_acquire_max_dist_from_cluster`.
    pub ball_acquire_max_dist_from_cluster: f32,
    /// Tracked frames before the acquisition gate arms itself. See
    /// `reco_autocam::AutocamConfig::ball_acquire_established_frames`.
    pub ball_acquire_established_frames: u32,
    /// Confidence needed to follow a long jump; `0` disables. See
    /// `reco_autocam::AutocamConfig::ball_jump_confidence`.
    pub ball_jump_confidence: f32,
    /// Apparent ball speed limit, radians per processed frame; `0`
    /// disables. See
    /// `reco_autocam::AutocamConfig::ball_max_speed_rad_per_tick`.
    pub ball_max_speed: f32,
    /// Ball detection pitch ceiling (radians); `0` disables. See
    /// `reco_autocam::AutocamConfig::ball_max_pitch`.
    pub ball_max_pitch: f32,
    /// Player pitch ceiling for the action cluster; `0` disables. See
    /// `reco_autocam::panners::FieldPannerConfig::max_player_pitch`.
    pub max_player_pitch: f32,
    /// Seconds to hold the aim on a lost ball; `0` releases at once.
    /// See `reco_autocam::panners::FieldPannerConfig::ball_hold_secs`.
    pub ball_hold_secs: f32,
    /// Detector confidence floor `[0,1]` - a raw detection (any class)
    /// below this score never reaches the tracker. See
    /// `reco_autocam::AutocamConfig::confidence_threshold`. Applies to
    /// every tracking mode; Ball mode no longer silently forces a
    /// higher floor of its own.
    pub confidence_threshold: f32,
    /// Lookahead buffer depth in seconds (0 = off).
    pub lookahead_secs: f64,
    /// Downconvert the lookahead pool to 8-bit NV12 even for 10-bit
    /// sources, roughly halving its VRAM cost at some cost to gradient
    /// smoothness in the final render (the same buffered frames feed
    /// both AI tracking and the stitch render - see
    /// `reco_core::session::vram_pool::LookaheadBitDepth`). Off by
    /// default; no effect on already-8-bit sources.
    pub lookahead_reduced_bit_depth: bool,
    /// Run the detector's inference call on a dedicated background
    /// thread instead of blocking the export loop (see
    /// `reco_core::async_detect`). Only takes effect when
    /// `lookahead_secs > 0`. Builds a second, separate detector
    /// instance - a modest extra VRAM cost, not a doubling of the whole
    /// session (measured ~400MB / ~8% on the reference machine, for a
    /// measured ~1.2-1.4x export speedup - see
    /// `docs/async-detect-benchmark-v054.md`). Off by default: opt in
    /// on cards with VRAM headroom to spare.
    pub async_detect: bool,
    /// Preset name used as the config base; visible knobs overlay it.
    pub preset: String,
    /// `"action"` or `"frame_all"`.
    pub framing: String,
    /// Horizontal-only pan (hold pitch level).
    pub lock_pitch: bool,
    /// `"density"` or `"trimmed_mean"`.
    pub cluster_mode: String,
    /// Density-peak neighborhood / trim window, radians.
    pub cluster_bandwidth_rad: f32,
    /// Soft dead-zone radius, radians.
    pub dead_zone_rad: f32,
    /// Ball-vs-cluster blend weight (0..1); forced to 1.0 in ball mode.
    pub ball_weight: f32,
    /// Max panorama distance (radians) the ball may be from the player
    /// cluster and still blend into the aim; beyond it the ball is treated
    /// as off-the-action and ignored (Action framing only).
    pub ball_max_dist_from_cluster: f32,
    /// Tight / wide / default field-of-view, degrees.
    pub fov_tight: f32,
    pub fov_wide: f32,
    pub fov_default: f32,
    /// Zoom-target smoothing rate (EMA alpha per frame). See
    /// `reco_autocam::panners::FieldPannerConfig::fov_alpha`.
    pub fov_alpha: f32,
    /// Cluster-position smoothing rate (EMA alpha per frame). See
    /// `reco_autocam::panners::FieldPannerConfig::cluster_alpha`.
    pub cluster_alpha: f32,
    /// Lookahead reactivity multiplier (>= 1.0). See
    /// `reco_autocam::panners::FieldPannerConfig::lookahead_reactivity`.
    pub lookahead_reactivity: f32,
}

/// Telemetry sink that forwards snapshots to the Slint UI thread.
struct ExportTelemetrySink {
    window: slint::Weak<RecoApp>,
}

impl reco_core::telemetry::TelemetrySink for ExportTelemetrySink {
    fn on_snapshot(&mut self, snap: &reco_core::telemetry::TelemetrySnapshot) {
        let snap = snap.clone();
        let weak = self.window.clone();
        let _ = slint::invoke_from_event_loop(move || {
            if let Some(app) = weak.upgrade() {
                app.set_telem_fps_avg(snap.fps_average);
                app.set_telem_fps_recent(snap.fps_recent);
                app.set_telem_decode_ms(snap.avg_decode_ms);
                app.set_telem_stitch_ms(snap.avg_stitch_ms);
                app.set_telem_readback_ms(snap.avg_readback_ms);
                app.set_telem_submit_ms(snap.avg_submit_ms);
                app.set_telem_total_ms(snap.avg_total_ms);
                app.set_telem_p99_ms(snap.p99_total_ms);
                app.set_telem_detection_ms(snap.avg_detection_ms);
                app.set_telem_active_tracks(snap.active_tracks as i32);
                app.set_telem_ball_pct(snap.ball_presence_pct);
                app.set_telem_det_per_frame(snap.detections_per_frame);
                app.set_telem_gpu_name(snap.gpu_name.clone().into());
                app.set_telem_bottleneck(
                    snap.bottleneck
                        .map(|s| s.to_string())
                        .unwrap_or_default()
                        .into(),
                );
            }
        });
    }
}

/// Run a StitchJob on the worker thread.
#[allow(clippy::too_many_arguments)]
pub fn run_export(
    left: reco_io::stitch_job::InputPath,
    right: reco_io::stitch_job::InputPath,
    cal: Calibration,
    output: PathBuf,
    stream_url: Option<String>,
    replay_enabled: bool,
    events_path: Option<PathBuf>,
    width: u32,
    height: u32,
    codec_str: String,
    quality_str: String,
    blend: f32,
    blend_flip_direction: bool,
    multiband_blend_enabled: bool,
    seam_offset: f32,
    start_secs: f32,
    end_secs: f32,
    cut_ranges: Vec<reco_io::cut_range::CutRange>,
    // `Some((fade_secs, hold_secs))` shows a "PAUZE" dip-to-black
    // transition at every cut-range boundary. See
    // `StitchJob::pause_overlay`'s doc comment.
    pause_overlay: Option<(f32, f32)>,
    autocam: AutocamUiConfig,
    scoreboard_package: Option<reco_scoreboard::ScoreboardPackage>,
    scoreboard_state: Option<serde_json::Value>,
    scoreboard_replay: Option<ScoreboardReplay>,
    scoreboard_placement: reco_core::render::overlay::OverlayPlacement,
    scoreboard_style: crate::scoreboard_import::ScoreboardStyle,
    app_weak: slint::Weak<RecoApp>,
    interrupted: &AtomicBool,
    last_progress_at: Arc<Mutex<Option<Instant>>>,
) -> ExportOutcome {
    use reco_io::output::{Codec, Quality};

    let codec: Codec = codec_str.parse().unwrap_or_default();
    let quality: Quality = quality_str.parse().unwrap_or_default();

    // Densified so RoiFilteredDetector's point-in-polygon test follows
    // the true (curved, in raw-distorted space) field boundary instead
    // of straight-lining between the calibration's few stored vertices -
    // see `FieldRoi::densified`'s doc comment.
    #[cfg(feature = "autocam")]
    let field_roi = cal
        .field_roi
        .as_ref()
        .map(|roi| roi.densified(&cal.lenses[0], &cal.lenses[1]));

    // Manual AI-tracking pitch safety margin (see
    // `reco_core::calibration::AutocamPitchLimits`'s doc comment) -
    // applied to the director's output only, via
    // `StitchCore::set_autocam_pitch_limits` inside the `on_session`
    // hook below. Independent of `field_roi`/autocam-enabled gating
    // above; harmless to set even with tracking off (unused until a
    // panner is attached).
    #[cfg(feature = "autocam")]
    let autocam_pitch_limits = cal.autocam_pitch_limits;

    let post_status = |text: String| {
        let weak = app_weak.clone();
        let _ = slint::invoke_from_event_loop(move || {
            if let Some(app) = weak.upgrade() {
                app.set_export_status_text(text.into());
            }
        });
    };

    post_status("Probing source...".into());

    use reco_core::source::FrameSource;
    let fps = reco_io::adapters::FfmpegFileSource::frame_rate(left.first_path())
        .map(|(n, d)| if d != 0 { n as f64 / d as f64 } else { 30.0 })
        .unwrap_or(30.0);
    if let Ok(source) = reco_io::adapters::FfmpegFileSource::open_from_inputs(&left, &right, 0)
        && let Some(full_total) = source.total_frames()
    {
        let start_frames = if start_secs > 0.0 {
            (start_secs as f64 * fps) as u64
        } else {
            0
        };
        let end_frames = if end_secs > 0.0 {
            (end_secs as f64 * fps) as u64
        } else {
            full_total
        };
        let naive_total = end_frames.saturating_sub(start_frames);
        // `naive_total` (the plain start/end span) is what "remaining
        // time" used to divide against unconditionally - wrong the
        // moment cut ranges are in play (it never subtracted the
        // excluded time) and, once the pause overlay shipped, also
        // missing its added hold frames. `planned_output_frames`
        // mirrors StitchJob::run's own frame-count math exactly; only
        // fall back to the naive span if it can't compute (e.g. an
        // invalid cut range - StitchJob::run will surface that error
        // properly once the export actually starts).
        let overlay_cfg = pause_overlay.and_then(|(fade_secs, hold_secs)| {
            reco_core::render::pause_overlay::PauseOverlayConfig::new(fade_secs, hold_secs, "PAUZE")
                .ok()
        });
        let range_total = reco_io::cut_range::planned_output_frames(
            start_secs as f64,
            (end_secs > 0.0).then_some(end_secs as f64),
            &cut_ranges,
            fps,
            None,
            overlay_cfg.as_ref(),
        )
        .unwrap_or(naive_total);
        let weak = app_weak.clone();
        let _ = slint::invoke_from_event_loop(move || {
            if let Some(app) = weak.upgrade() {
                app.set_export_frames_total(range_total as i32);
            }
        });
    }

    post_status("Opening encoder and decoders...".into());

    let progress_weak = app_weak.clone();
    let progress_start = Instant::now();
    let progress_last_at = Arc::clone(&last_progress_at);
    let effective_output = stream_url
        .as_ref()
        .filter(|u| !u.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| output.clone());
    let format = reco_io::output::Format::for_output(&effective_output.to_string_lossy());
    if format.is_streaming() {
        log::info!("Streaming to {}", effective_output.display());
    }

    // Snapshot the settings actually used for this export into the
    // output container's "comment" tag, so a batch of test exports
    // with varying AI/blend settings stays self-describing without a
    // separate sidecar file (`ffprobe -show_entries format_tags`).
    let metadata_comment = serde_json::json!({
        "reco_export": {
            "codec": codec_str,
            "quality": quality_str,
            "resolution": format!("{width}x{height}"),
            "blend_width": blend,
            "blend_flip_direction": blend_flip_direction,
            "multiband_blend": multiband_blend_enabled,
            "seam_offset": seam_offset,
            "autocam": {
                "enabled": autocam.enabled,
                "model_path": &autocam.model_path,
                "tracking_mode": &autocam.tracking_mode,
                "detection_interval": autocam.detection_interval,
                "player_anchor_rad": autocam.player_anchor_rad,
                "ball_coast_secs": autocam.ball_coast_secs,
                "ball_acquire_max_dist_from_cluster": autocam.ball_acquire_max_dist_from_cluster,
                "ball_acquire_established_frames": autocam.ball_acquire_established_frames,
                "ball_jump_confidence": autocam.ball_jump_confidence,
                "ball_max_speed": autocam.ball_max_speed,
                "ball_max_pitch": autocam.ball_max_pitch,
                "max_player_pitch": autocam.max_player_pitch,
                "ball_hold_secs": autocam.ball_hold_secs,
                "confidence_threshold": autocam.confidence_threshold,
                "lookahead_secs": autocam.lookahead_secs,
                "lookahead_reduced_bit_depth": autocam.lookahead_reduced_bit_depth,
                "preset": &autocam.preset,
                "framing": &autocam.framing,
                "lock_pitch": autocam.lock_pitch,
                "cluster_mode": &autocam.cluster_mode,
                "cluster_bandwidth_rad": autocam.cluster_bandwidth_rad,
                "dead_zone_rad": autocam.dead_zone_rad,
                "ball_weight": autocam.ball_weight,
                "ball_max_dist_from_cluster": autocam.ball_max_dist_from_cluster,
                "fov_tight": autocam.fov_tight,
                "fov_wide": autocam.fov_wide,
                "fov_default": autocam.fov_default,
                "fov_alpha": autocam.fov_alpha,
                "cluster_alpha": autocam.cluster_alpha,
            }
        }
    })
    .to_string();

    let mut job = reco_io::StitchJob::with_calibration(
        left.clone(),
        right.clone(),
        cal,
        effective_output.clone(),
    )
    .codec(codec)
    .quality(quality)
    .format(format)
    .resolution(width, height)
    .blend_width(blend)
    .blend_flip_direction(blend_flip_direction)
    .multiband_blend_enabled(multiband_blend_enabled)
    .seam_offset(seam_offset)
    .metadata_comment(metadata_comment);

    // Real video-time fps for the replay below - distinct from the
    // encode-throughput `fps` computed inside the progress closure for the
    // ETA display (frames processed per wall-clock second), which is a
    // different quantity despite the name collision.
    let video_fps = fps;
    // Precomputed once, *before* the scoreboard runtime starts below (see
    // `initial_scoreboard_state` right after): the same output-frame ->
    // source-seconds mapping the real export uses
    // (`reco_io::cut_range::output_frame_to_source_secs`). Replaces a
    // naive `start_secs + frames/video_fps` formula that was exact for a
    // plain start/end trim but drifted behind by roughly a cut's own
    // duration after each one - visible as the on-screen clock stalling
    // for a long stretch right after a mid-match pause instead of
    // resuming immediately. `validate_cut_ranges` failing here (a
    // malformed cut) falls back to treating the whole export as one
    // unbroken span rather than blocking the replay entirely -
    // `StitchJob::run` still surfaces the real error through the normal
    // export-failure path.
    let replay_keep_windows = reco_io::cut_range::validate_cut_ranges(cut_ranges.clone())
        .map(|sorted| {
            reco_io::cut_range::keep_windows(
                start_secs as f64,
                (end_secs > 0.0).then_some(end_secs as f64),
                &sorted,
            )
        })
        .unwrap_or_else(|_| {
            vec![(
                start_secs as f64,
                (end_secs > 0.0).then_some(end_secs as f64),
            )]
        });
    let mut replay_window_limits =
        reco_io::cut_range::window_limits(&replay_keep_windows, video_fps, None);
    if let Some((fade_secs, hold_secs)) = pause_overlay
        && let Ok(overlay_cfg) =
            reco_core::render::pause_overlay::PauseOverlayConfig::new(fade_secs, hold_secs, "PAUZE")
    {
        reco_io::cut_range::extend_for_pause_overlay(
            &replay_keep_windows,
            &mut replay_window_limits,
            video_fps,
            u64::MAX,
            &overlay_cfg,
        );
    }

    // The state to seed the scoreboard runtime's *very first* rendered
    // frame with. When a live Match Logger replay is configured, this is
    // the correct state for output frame 0 (not `scoreboard_state`'s
    // usually-unrelated frozen live-preview snapshot, and not the
    // package's own placeholder HTML either) - computed and pushed
    // *before* the runtime's first screenshot is ever taken, so there's
    // no async gap for a wrong frame to show in. An earlier fix tried to
    // paper over this by withholding overlay frames until the on_progress
    // closure's first push landed, but that raced the browser's own
    // async capture loop (the "ready" flag could flip true before a
    // *fresh* screenshot reflecting the push had actually been captured)
    // and was found not to reliably fix it - this is the real fix, at
    // the source.
    let initial_scoreboard_state = match scoreboard_replay.as_ref() {
        Some(replay) => {
            let video_seconds = reco_io::cut_range::output_frame_to_source_secs(
                0,
                &replay_keep_windows,
                &replay_window_limits,
                video_fps,
            );
            let state =
                crate::scoreboard_import::state_at(&replay.export, &replay.anchor, video_seconds);
            Some(crate::scoreboard_import::apply_style(
                state,
                &scoreboard_style,
            ))
        }
        None => scoreboard_state,
    };

    // Scoreboard runtime is started before `.on_progress` (not inline with
    // `.on_session` below, as PR #474 originally had it) so a shared
    // handle can also be captured into the progress callback - that's what
    // lets a Match Logger replay actually change the on-screen score/
    // clock/cards over the course of the export instead of freezing
    // whatever `initial_scoreboard_state` held at the moment export started.
    let scoreboard_shared: Option<Arc<Mutex<reco_scoreboard::ScoreboardRuntime>>> =
        match scoreboard_package {
            Some(package) => {
                let design_size = (
                    package.manifest.viewport.width,
                    package.manifest.viewport.height,
                );
                match reco_scoreboard::ScoreboardRuntime::start_with_state(
                    package,
                    30,
                    initial_scoreboard_state,
                ) {
                    Ok(runtime) => {
                        // Render at (roughly) the resolution this frame
                        // will actually end up at on screen, instead of
                        // always at the package's full design
                        // resolution - avoids relying on GPU
                        // minification for text quality when the
                        // scoreboard is placed smaller than full-frame.
                        // Export output resolution and placement are
                        // both fixed for the whole run, so this is set
                        // once here, not re-sent per frame.
                        let render_scale = reco_core::render::overlay::contain_fit_render_scale(
                            design_size,
                            (width, height),
                            scoreboard_placement,
                        );
                        if let Err(error) = runtime.set_render_scale(render_scale) {
                            log::warn!(
                                "Scoreboard: could not apply render scale {render_scale:.3}: \
                                 {error} (falling back to full design resolution)"
                            );
                        }
                        Some(Arc::new(Mutex::new(runtime)))
                    }
                    Err(error) => {
                        log::error!("Scoreboard disabled for export: {error}");
                        let weak = app_weak.clone();
                        let message = error.to_string();
                        let _ = slint::invoke_from_event_loop(move || {
                            if let Some(app) = weak.upgrade() {
                                app.set_scoreboard_error_text(message.into());
                            }
                        });
                        None
                    }
                }
            }
            None => None,
        };
    let replay_runtime = scoreboard_shared.clone();
    let mut last_replay_frames: Option<u64> = None;

    // Diagnostic: which scoreboard source (if any) this export actually
    // has, so a stuck/wrong on-screen scoreboard can be told apart from
    // "no replay was ever wired up" at a glance in the log instead of
    // by guessing. See the update() logging below for the other half.
    if replay_runtime.is_some() {
        match scoreboard_replay.as_ref() {
            Some(replay) => log::info!(
                "Scoreboard: live Match Logger replay active ({} events)",
                replay.export.event_count()
            ),
            None => log::info!(
                "Scoreboard: showing a single frozen snapshot only (no Match Logger \
                 replay wired up - scoreboard_import/scoreboard_sync_anchor not both \
                 set when export started)"
            ),
        }
    } else {
        log::info!("Scoreboard: disabled for this export");
    }

    job = job.on_progress(move |p: &reco_core::session::types::FrameProgress| {
        let frames = p.frames_completed;
        let elapsed = progress_start.elapsed().as_secs_f64();
        let fps = if elapsed > 0.0 {
            frames as f64 / elapsed
        } else {
            0.0
        };
        *progress_last_at.lock().unwrap() = Some(Instant::now());

        if let (Some(replay), Some(runtime)) = (scoreboard_replay.as_ref(), &replay_runtime) {
            // Throttle to roughly 1/sec of *video* time, not every
            // encoded frame - each push is a real JS eval in the headless
            // browser.
            let due = last_replay_frames
                .is_none_or(|last| frames.saturating_sub(last) as f64 >= video_fps.max(1.0));
            if due {
                let first_push = last_replay_frames.is_none();
                last_replay_frames = Some(frames);
                let video_seconds = reco_io::cut_range::output_frame_to_source_secs(
                    frames,
                    &replay_keep_windows,
                    &replay_window_limits,
                    video_fps,
                );
                let state = crate::scoreboard_import::state_at(
                    &replay.export,
                    &replay.anchor,
                    video_seconds,
                );
                let state = crate::scoreboard_import::apply_style(state, &scoreboard_style);
                match runtime.lock() {
                    Ok(runtime) => match runtime.update(&state) {
                        Ok(()) => {
                            if first_push {
                                log::info!(
                                    "Scoreboard: first replay push succeeded (video_seconds={video_seconds:.1}s)"
                                );
                            }
                        }
                        // Previously silently swallowed (`let _ =`) - a
                        // failure here is exactly what would leave the
                        // on-screen scoreboard stuck on its last-good
                        // (or, if this is the very first push, its
                        // built-in placeholder) state for the rest of
                        // the export with no visible sign why.
                        Err(error) => {
                            log::warn!(
                                "Scoreboard: replay push failed at video_seconds={video_seconds:.1}s: {error}"
                            );
                        }
                    },
                    Err(_) => {
                        log::warn!("Scoreboard: replay push skipped, runtime lock poisoned");
                    }
                }
            }
        }

        let weak = progress_weak.clone();
        let _ = slint::invoke_from_event_loop(move || {
            if let Some(app) = weak.upgrade() {
                app.set_export_frames_done(frames as i32);
                let total = app.get_export_frames_total();
                if total > 0 {
                    app.set_export_progress(frames as f32 / total as f32);
                }
                let eta = if total > 0 && fps > 0.0 {
                    let remaining = (total as f64 - frames as f64) / fps;
                    let mins = remaining as u64 / 60;
                    let secs = remaining as u64 % 60;
                    format!(" - ~{mins}:{secs:02} remaining")
                } else {
                    String::new()
                };
                app.set_export_status_text(format!("Frame {frames} ({fps:.0} fps){eta}").into());
            }
        });
    });

    if let Some(shared) = scoreboard_shared {
        // Registered as a layer (not `on_session` + `set_overlay_source`
        // directly) so it composites cleanly with the "PAUZE" pause
        // overlay below when both are enabled, always drawn on top of
        // it - see `StitchJob::overlay_layer`'s doc comment.
        job = job.overlay_layer(
            Box::new(SharedScoreboardSource(shared)),
            scoreboard_placement,
        );
    }

    if start_secs > 0.0 {
        let skip_frames = (start_secs as f64 * fps) as u64;
        post_status(format!(
            "Seeking to {start_secs:.0}s (skipping ~{skip_frames} frames)..."
        ));
        job = job.start_time(start_secs as f64);
    }
    if end_secs > 0.0 {
        job = job.end_time(end_secs as f64);
    }
    if !cut_ranges.is_empty() {
        log::info!(
            "Cut ranges: {} excluded ({})",
            cut_ranges.len(),
            cut_ranges
                .iter()
                .map(|r| format!("{:.1}-{:.1}s", r.start_secs, r.end_secs))
                .collect::<Vec<_>>()
                .join(", ")
        );
        job = job.cut_ranges(cut_ranges);
    }
    if let Some((fade_secs, hold_secs)) = pause_overlay {
        job = job.pause_overlay(fade_secs, hold_secs);
    }

    if replay_enabled {
        let replay_path = effective_output.with_extension("replay.mkv");
        log::info!("Replay recording: {}", replay_path.display());
        job = job.with_replay_recording(&replay_path);
    }

    if let Some(ref ep) = events_path {
        log::info!("Pipeline events: {}", ep.display());
        job = job.events(ep);
    }

    let finalizing_weak = app_weak.clone();
    job = job.on_finalizing(move || {
        let weak = finalizing_weak;
        let _ = slint::invoke_from_event_loop(move || {
            if let Some(app) = weak.upgrade() {
                app.set_export_status_text("Finalizing output file...".into());
            }
        });
    });

    let telem_weak = app_weak.clone();
    job = job.on_session(move |session, _source| {
        let sink = ExportTelemetrySink { window: telem_weak };
        session.telemetry_mut().set_sink(Box::new(sink), 30);
    });

    // Lookahead buffers N future frames so the panner can centered-smooth
    // its pose stream. It is the dominant quality lever for AI panning, so
    // the GUI defaults it on; the baked dead_zone_rad assumes it is active.
    #[cfg(feature = "autocam")]
    if autocam.enabled && autocam.lookahead_secs > 0.0 {
        job = job.lookahead(autocam.lookahead_secs);
        if autocam.lookahead_reduced_bit_depth {
            job = job.lookahead_reduced_bit_depth(true);
        }
    }

    #[cfg(feature = "autocam")]
    if autocam.enabled && !autocam.model_path.is_empty() {
        if events_path.is_some() {
            job = job.ai_run_config(
                autocam.model_path.clone(),
                reco_core::calibration::AutocamDefaults {
                    tracking_mode: autocam.tracking_mode.clone(),
                    detection_interval: autocam.detection_interval,
                    player_anchor_rad: autocam.player_anchor_rad,
                    ball_coast_secs: autocam.ball_coast_secs,
                    ball_acquire_max_dist_from_cluster: autocam
                        .ball_acquire_max_dist_from_cluster,
                    ball_acquire_established_frames: autocam.ball_acquire_established_frames,
                    ball_jump_confidence: autocam.ball_jump_confidence,
                    ball_max_speed: autocam.ball_max_speed,
                    ball_max_pitch: autocam.ball_max_pitch,
                    max_player_pitch: autocam.max_player_pitch,
                    ball_hold_secs: autocam.ball_hold_secs,
                    lookahead_secs: autocam.lookahead_secs,
                    lookahead_reduced_bit_depth: autocam.lookahead_reduced_bit_depth,
                    preset: autocam.preset.clone(),
                    framing: autocam.framing.clone(),
                    lock_pitch: autocam.lock_pitch,
                    cluster_mode: autocam.cluster_mode.clone(),
                    cluster_bandwidth_rad: autocam.cluster_bandwidth_rad,
                    dead_zone_rad: autocam.dead_zone_rad,
                    ball_weight: autocam.ball_weight,
                    ball_max_dist_from_cluster: autocam.ball_max_dist_from_cluster,
                    fov_tight: autocam.fov_tight,
                    fov_wide: autocam.fov_wide,
                    fov_default: autocam.fov_default,
                    fov_alpha: autocam.fov_alpha,
                    cluster_alpha: autocam.cluster_alpha,
                    confidence_threshold: autocam.confidence_threshold,
                    lookahead_reactivity: autocam.lookahead_reactivity,
                },
            );
        }
        let ac = autocam.clone();
        let status_weak = app_weak.clone();
        let pitch_limits = autocam_pitch_limits;
        job = job.on_session(move |session, source| {
            session.set_autocam_pitch_limits(pitch_limits);
            let info = source.info();
            let mode = match ac.tracking_mode.as_str() {
                "ball" => reco_autocam::TrackingMode::Ball,
                "sweep" => reco_autocam::TrackingMode::Sweep,
                _ => reco_autocam::TrackingMode::Field,
            };
            let is_10bit =
                source.gpu_pixel_format() == reco_core::render::renderer::GpuPixelFormat::P010;

            // Map the GUI knobs onto FieldPannerConfig, leaving every other
            // field at its validated default. Ball mode's ball_weight=1.0 is
            // owned by setup_autocam, so we pass the slider value as-is.
            let framing = match ac.framing.as_str() {
                "frame_all" => reco_autocam::panners::FramingMode::FrameAll,
                _ => reco_autocam::panners::FramingMode::Action,
            };
            let cluster_mode = match ac.cluster_mode.as_str() {
                "trimmed_mean" => reco_autocam::panners::ClusterMode::TrimmedMean,
                _ => reco_autocam::panners::ClusterMode::Density,
            };
            let panner_cfg = reco_autocam::panners::FieldPannerConfig {
                framing,
                cluster_mode,
                cluster_bandwidth_rad: ac.cluster_bandwidth_rad,
                dead_zone_rad: ac.dead_zone_rad,
                ball_weight: ac.ball_weight,
                ball_max_dist_from_cluster: ac.ball_max_dist_from_cluster,
                lock_pitch: ac.lock_pitch,
                fov_tight: ac.fov_tight,
                fov_wide: ac.fov_wide,
                fov_default: ac.fov_default,
                fov_alpha: ac.fov_alpha,
                cluster_alpha: ac.cluster_alpha,
                // 0 means "off"; the panner's own "count everyone" is
                // infinity, so translate rather than passing 0 through.
                max_player_pitch: if ac.max_player_pitch > 0.0 {
                    ac.max_player_pitch
                } else {
                    f32::INFINITY
                },
                ball_hold_secs: ac.ball_hold_secs,
                // Preset is the base; the visible knobs above overlay it
                // (they mirror the preset until the user tweaks them).
                ..reco_autocam::panners::FieldPannerConfig::from_preset_name(&ac.preset)
                    .unwrap_or_default()
            };

            let mut autocam_config = reco_autocam::AutocamConfig::new(&ac.model_path)
                .with_tracking_mode(mode)
                .with_detection_interval(ac.detection_interval as u64)
                .with_10bit(is_10bit)
                .with_player_anchor_rad(ac.player_anchor_rad)
                .with_ball_coast_secs(ac.ball_coast_secs)
                .with_confidence_threshold(ac.confidence_threshold);
            // 0 is the "off" encoding everywhere this value travels
            // (slider, saved calibration, CLI flag).
            autocam_config.ball_acquire_max_dist_from_cluster =
                Some(ac.ball_acquire_max_dist_from_cluster).filter(|d| *d > 0.0);
            autocam_config.ball_acquire_established_frames =
                Some(ac.ball_acquire_established_frames as u64);
            autocam_config.ball_jump_confidence = Some(ac.ball_jump_confidence);
            autocam_config.ball_max_speed_rad_per_tick = Some(ac.ball_max_speed);
            // 0 means "off"; the tracker's own "accept everything" is
            // infinity, so translate rather than passing 0 through.
            autocam_config.ball_max_pitch = if ac.ball_max_pitch > 0.0 {
                Some(ac.ball_max_pitch)
            } else {
                None
            };
            autocam_config.field_panner_config = Some(panner_cfg);
            let autocam_config = if let Some(roi) = field_roi.as_ref() {
                autocam_config.with_field_roi(roi.clone())
            } else {
                autocam_config
            };
            let result = reco_autocam::setup_autocam(
                session,
                &autocam_config,
                info.fps as f32,
                source.is_gpu_resident(),
                None,
            );
            // --async-detect equivalent. Only meaningful once tracking
            // is confirmed active and the buffered/export loop is in
            // play (lookahead > 0) - a second, separate detector
            // instance moved onto reco-core's async worker thread. See
            // `AutocamUiConfig::async_detect`'s doc comment for the
            // measured cost/benefit.
            #[cfg(feature = "ort")]
            if matches!(result, Ok(true)) && ac.async_detect && ac.lookahead_secs > 0.0 {
                match reco_autocam::CpuYoloDetector::with_config(
                    &ac.model_path,
                    autocam_config.confidence_threshold.unwrap_or(0.10),
                    Vec::new(),
                ) {
                    Ok(inference_detector) => {
                        let queue_depth = ((ac.lookahead_secs * info.fps).ceil() as usize).max(2);
                        session.enable_async_detect(Box::new(inference_detector), queue_depth);
                        log::info!(
                            "Export: async detect thread active (queue depth {queue_depth})"
                        );
                    }
                    Err(e) => log::warn!(
                        "Async AI detection: could not load a second detector instance ({e}), \
                         continuing with synchronous detection"
                    ),
                }
            }
            let banner: String = match result {
                Ok(true) => "AI tracking: active".into(),
                Ok(false) => {
                    "AI tracking unavailable - build with --features tensorrt, or use CPU decode"
                        .into()
                }
                Err(e) => format!("AI tracking failed: {e}"),
            };
            log::info!("Export: {banner}");
            let weak = status_weak.clone();
            let _ = slint::invoke_from_event_loop(move || {
                if let Some(app) = weak.upgrade() {
                    // Banner goes to the export status only, never the main
                    // status bar. This async set can land after the export
                    // completion handler and would otherwise clobber the final
                    // outcome (e.g. the VRAM error) shown there.
                    app.set_export_status_text(banner.into());
                }
            });
        });
    }
    // Defense in depth: the GUI blocks Start when AI is on with no model,
    // but if that guard is ever bypassed, say so rather than silently
    // producing a tracking-free export.
    #[cfg(feature = "autocam")]
    if autocam.enabled && autocam.model_path.is_empty() {
        log::warn!("AI tracking enabled but no model selected; exporting WITHOUT tracking");
    }
    #[cfg(not(feature = "autocam"))]
    let _ = &autocam;

    match job.run(interrupted) {
        Ok(r) => ExportOutcome::Ok(r.frames_processed, output),
        Err(e) => {
            if interrupted.load(Ordering::Relaxed) {
                ExportOutcome::Cancelled
            } else {
                ExportOutcome::Failed(e)
            }
        }
    }
}
