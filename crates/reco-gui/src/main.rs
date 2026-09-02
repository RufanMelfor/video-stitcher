#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

//! Reco GUI - Slint-based panoramic video stitcher.
//!
//! Opens a Material dark themed window with file pickers for left/right
//! video files and calibration JSON, a GPU-rendered preview panel, and
//! play/pause/seek controls.
//!
//! ## Architecture
//!
//! Slint and reco-core share a single wgpu 28 device. `main()` selects
//! the wgpu 28 backend via `BackendSelector::require_wgpu_28()`, and a
//! `set_rendering_notifier` callback captures Slint's device/queue on
//! `RenderingSetup`. Those handles feed `GpuContext::from_device_queue`,
//! so reco-core renders stitched frames directly into Slint-owned
//! textures with no CPU readback.

mod detect_preview;
mod export;
mod match_folder;
mod playback;
mod preview;
mod scoreboard_import;
mod settings;
mod sync_offset;
mod telemetry_client;
mod toast;
mod waveform;

use std::cell::RefCell;
use std::collections::VecDeque;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use reco_calibrate::{CalibrationConfig, LensProfileInfo, ProfileSource};
use reco_control::pose_control::{PoseControl, PoseControlConfig};
use reco_control::{ControlIntent, PoseIntent};
use reco_core::calibration::Calibration;
use reco_core::geometry::ViewportPosition;
use reco_core::render::overlay::{OverlayFrame, OverlayFrameSource};
use reco_core::wgpu;

use crate::playback::{PlayState, Playback};
use crate::preview::PreviewBridge;
use crate::toast::{Severity, ToastManager};

/// wgpu handles captured from Slint's rendering notifier. Populated once
/// on `RenderingSetup`; used to build `PreviewBridge` when files load.
#[derive(Clone)]
struct SharedGpu {
    device: wgpu::Device,
    queue: wgpu::Queue,
    adapter_info: wgpu::AdapterInfo,
}

slint::include_modules!();

/// Default preview viewport dimensions (used before the first
/// adaptive resize reads the actual preview container size from Slint).
const PREVIEW_WIDTH_DEFAULT: u32 = 1920;
const PREVIEW_HEIGHT_DEFAULT: u32 = 1080;

/// Tick interval for the playback timer (ms).
///
/// Needs to be much smaller than one frame (33ms at 30fps) so the
/// drift-free scheduler in `Playback::tick` can catch up promptly when
/// a scheduled frame boundary is crossed mid-tick. 2ms gives sub-1%
/// timing error at 30fps and is cheap since the tick is a no-op when
/// no frame advance is due.
const TICK_INTERVAL_MS: i64 = 2;

/// FOV clamp range (degrees), matching CLI preview.
const FOV_MIN: f32 = 20.0;
const FOV_MAX: f32 = 150.0;
const FOV_DEFAULT: f32 = 75.0;

/// Mouse drag sensitivity passed to `PoseControlConfig`. `0.287`
/// deg/px = 0.005 rad/px - matches the pre-migration GUI constant
/// (`MOUSE_SENSITIVITY = 0.005`) and the CLI preview's
/// `drag_deg_per_pixel`.
const DRAG_DEG_PER_PIXEL: f32 = 0.287;

/// Exponential smoothing factor passed to `PoseControlConfig`. `0.25`
/// gives a time constant of ~3-4 ticks at 60Hz render rate - fast
/// enough to track input, soft enough to hide per-pixel jitter.
/// Matches the pre-migration GUI constant `CAMERA_SMOOTHING`.
const POSE_SMOOTHING: f32 = 0.25;

/// How long the seek-slider fraction must stay stable before we
/// actually execute the seek. Debouncing is required because every
/// pixel of drag emits a `changed` event, and each seek forces a
/// NVDEC codec reinit that costs ~50ms. Without debouncing, a drag
/// saturates the GPU with hundreds of pending reinits.
const SEEK_DEBOUNCE_MS: u64 = 120;

/// Width of the audio-sync waveform window, in frames of video at the
/// source's own fps - converted to seconds of audio at recompute time
/// since fps is only known once a file is loaded. Frame-based (not a
/// fixed duration) because the whole point is spotting a frame-scale
/// `sync_offset` error as a shifted transient; a multi-second window
/// dilutes that shift into a barely-visible fraction of the display.
const AUDIO_WAVEFORM_WINDOW_FRAMES: f64 = 5.0;
/// Clamp range for the user-adjustable waveform window width (frames).
const AUDIO_WAVEFORM_WINDOW_FRAMES_RANGE: (f64, f64) = (1.0, 300.0);
/// Number of bars drawn per waveform track.
const AUDIO_WAVEFORM_BUCKETS: usize = 240;
/// Minimum wall-clock time between recompute triggers. Each recompute
/// shells out to `ffmpeg` twice (left + right), so this throttles how
/// often that happens during continuous playback.
const AUDIO_WAVEFORM_THROTTLE_MS: u64 = 500;
/// Minimum playhead movement, in frames, that warrants a recompute -
/// also converted to seconds at recompute time. Frame-based for the same
/// reason as the window width: keeps the recenter threshold proportional
/// to the (now much narrower) window instead of the window being fully
/// skipped past between recomputes.
const AUDIO_WAVEFORM_RECENTER_FRAMES: f64 = 3.0;

/// Calibration payload sent from the background worker: the computed
/// match calibration plus the lens profile info each side resolved to,
/// so the GUI can tell the user "we auto-detected GoPro HERO10 Linear 4K"
/// without re-running detection.
struct CalibrationOutput {
    calibration: Calibration,
    confidence: f64,
    total_matches: usize,
    left_lens_profile: Option<LensProfileInfo>,
    right_lens_profile: Option<LensProfileInfo>,
    /// What each camera's metadata probe found for IMU sync/orientation.
    /// See `reco_calibrate::telemetry::ImuDiagnostics`.
    imu_diagnostics: Option<reco_calibrate::telemetry::ImuDiagnostics>,
}

/// Result sent from the calibration background thread. The error is
/// the typed [`reco_calibrate::video::CalibrateVideosError`] now that
/// it is `Clone + Send + Sync` (plan step 7), so the UI thread can
/// pattern-match specific failure modes (`Cancelled`, `NoFrames`,
/// `Io(...)`, etc.) instead of parsing a stringified message.
type CalibrationResult = Result<CalibrationOutput, reco_calibrate::video::CalibrateVideosError>;

/// Headless dev/test preload hook. When `RECO_AUTOLOAD` is set the GUI loads
/// the given left/right videos and calibration on startup (and optionally
/// starts an export via `RECO_AUTOEXPORT`), so the app can be driven under
/// Xvfb for screenshots and CI smoke tests. Inert when the env var is unset.
#[cfg(feature = "automation")]
struct AutoloadSpec {
    left: Vec<PathBuf>,
    right: Vec<PathBuf>,
    cal: PathBuf,
    export: Option<AutoExportSpec>,
}

/// Optional auto-export for [`AutoloadSpec`], from `RECO_AUTOEXPORT`.
#[cfg(feature = "automation")]
struct AutoExportSpec {
    output: PathBuf,
    model: Option<PathBuf>,
    lookahead_secs: f32,
    /// Total number of exports to run back-to-back in one session, from
    /// `RECO_AUTOEXPORT_REPEAT` (default 1). Used to surface cross-export GPU
    /// resource leaks: VRAM is logged between runs, so a leak shows up as a
    /// figure that never returns to baseline. The output path gets a `_N`
    /// suffix per run so runs don't clobber each other.
    repeats: u32,
}

#[cfg(feature = "automation")]
impl AutoloadSpec {
    /// Parse `RECO_AUTOLOAD="left[;left2,...],right[;right2,...],cal.json"`
    /// (segments within a side separated by `;`). Returns `None` when unset or
    /// malformed.
    fn from_env() -> Option<Self> {
        let raw = std::env::var("RECO_AUTOLOAD").ok()?;
        let parts: Vec<&str> = raw.split(',').collect();
        if parts.len() != 3 {
            log::warn!(
                "RECO_AUTOLOAD ignored: expected 'left[;left2],right[;right2],cal.json', got {raw:?}"
            );
            return None;
        }
        let to_paths =
            |s: &str| -> Vec<PathBuf> { s.split(';').map(|p| PathBuf::from(p.trim())).collect() };
        let left = to_paths(parts[0]);
        let right = to_paths(parts[1]);
        let cal = PathBuf::from(parts[2].trim());
        if left
            .iter()
            .chain(right.iter())
            .any(|p| p.as_os_str().is_empty())
        {
            log::warn!("RECO_AUTOLOAD ignored: empty path component");
            return None;
        }
        let export = std::env::var("RECO_AUTOEXPORT")
            .ok()
            .map(|out| AutoExportSpec {
                output: PathBuf::from(out),
                model: std::env::var("RECO_AUTOEXPORT_MODEL")
                    .ok()
                    .map(PathBuf::from),
                lookahead_secs: std::env::var("RECO_AUTOEXPORT_LOOKAHEAD")
                    .ok()
                    .and_then(|v| v.parse().ok())
                    .unwrap_or(0.0),
                repeats: std::env::var("RECO_AUTOEXPORT_REPEAT")
                    .ok()
                    .and_then(|v| v.parse().ok())
                    .filter(|&n| n > 0)
                    .unwrap_or(1),
            });
        Some(Self {
            left,
            right,
            cal,
            export,
        })
    }
}

/// Application state shared between Slint callbacks.
struct AppState {
    left_path: Option<PathBuf>,
    right_path: Option<PathBuf>,
    left_input: Option<reco_io::stitch_job::InputPath>,
    right_input: Option<reco_io::stitch_job::InputPath>,
    /// The match folder selected via "Select Match Folder" (see
    /// `match_folder::scan_match_folder`), if that's how the current
    /// left/right videos were loaded. Used to suggest an export output
    /// path inside the match folder, named after it. Cleared by a manual
    /// left/right pick, since those no longer necessarily correspond to
    /// this folder's `Left`/`Right` subdirectories.
    match_folder: Option<PathBuf>,
    calibration_path: Option<PathBuf>,
    calibration: Option<Calibration>,
    /// Time ranges excluded from the export (e.g. a halftime pause), in
    /// source seconds - same space as `export-start-secs`/`-end-secs`.
    /// Source of truth for the UI's `cut-ranges` property (see
    /// `sync_cut_ranges`); mutated only via the `on_cut_range_*`
    /// handlers, never read back from Slint. Session-only for now - not
    /// yet persisted into the calibration file (deliberately deferred
    /// to a follow-up).
    cut_ranges: Vec<(f64, f64)>,
    playback: Playback,
    bridge: Option<PreviewBridge>,
    scoreboard_packages: Vec<reco_scoreboard::ScoreboardPackage>,
    scoreboard_runtime: Option<reco_scoreboard::ScoreboardRuntime>,
    /// The running `scoreboard_runtime`'s package's declared design
    /// resolution (`manifest.viewport`) - kept alongside it since the
    /// runtime itself doesn't expose its package after start (it moves
    /// into the worker thread). Used to compute the render scale to
    /// pass to `ScoreboardRuntime::set_render_scale` whenever placement
    /// or viewport size changes - see `apply_scoreboard_placement`.
    scoreboard_design_size: Option<(u32, u32)>,
    /// Throttles `apply_scoreboard_render_scale` the same way
    /// `scoreboard_replay_last_push` throttles `push_scoreboard_replay` -
    /// without it, the placement drag handler (which calls it on every
    /// pointer-move tick, unthrottled) sends `SetRenderScale` commands
    /// far faster than the renderer can drain them (each one costs a
    /// CDP viewport resize + a JS `zoom` eval + a fresh capture, ~tens of
    /// ms each with Chrome's own GPU disabled - see
    /// `apply_zoom`/`set_viewport` in reco-scoreboard), overflowing the
    /// 32-slot command queue mid-drag ("HTML renderer command queue is
    /// full" - a real bug hit during the design_size/zoom rework, see
    /// SESSION_HANDOFF's 2026-08-25 entry). Only gates *this* quality-
    /// refinement call - `set_overlay_placement` (the live position/size
    /// feedback the user actually watches while dragging) stays
    /// unthrottled right next to every call site.
    scoreboard_render_scale_last_apply: Option<std::time::Instant>,
    scoreboard_frame: Option<OverlayFrame>,
    scoreboard_frame_dirty: bool,
    scoreboard_error: String,
    /// Parsed Match Logger event log, loaded via the Scoreboard section's
    /// Load button. Session-only for now (same deliberate-deferral
    /// precedent as `cut_ranges` above) - not yet persisted into the
    /// calibration file. Driving the scoreboard from this replaces the
    /// live-manual editor path for as long as it's loaded.
    scoreboard_import: Option<scoreboard_import::MatchLoggerExport>,
    /// Path the currently loaded `scoreboard_import` was read from - kept
    /// alongside it purely so "Save Calibration" can persist a reference
    /// to re-load from, without embedding the whole event log (see
    /// `reco_core::calibration::ScoreboardSettings`'s doc comment for why).
    scoreboard_import_path: Option<PathBuf>,
    /// Video-seconds <-> match-wall-clock anchor for `scoreboard_import`.
    /// Defaults to the log's `video_start` event at `video_seconds: 0.0`
    /// when present; adjustable by scrubbing to the matching frame.
    scoreboard_sync_anchor: Option<scoreboard_import::SyncAnchor>,
    /// Wall-clock throttle for `push_scoreboard_replay` - the replayed
    /// state is recomputed and pushed to the running package on a timer
    /// rather than every render tick, since a scoreboard plausibly changes
    /// at most a few times a second and each push is a real JS eval in the
    /// headless browser.
    scoreboard_replay_last_push: Option<std::time::Instant>,
    /// Overlay position/size set via the "Edit Scoreboard" in-place
    /// editor. Session-only for now, same deliberate-deferral precedent
    /// as `cut_ranges` - applied to the live pipeline immediately on
    /// change and re-applied to export (see `run_export`'s
    /// `scoreboard_placement` argument).
    scoreboard_placement: reco_core::render::overlay::OverlayPlacement,
    /// Team logos / font chosen via the same editor - merged into the
    /// replayed state via `scoreboard_import::apply_style`. Only takes
    /// effect while a Match Logger import is loaded (see that function's
    /// doc comment for why the live-manual editor path can't use it).
    scoreboard_style: scoreboard_import::ScoreboardStyle,
    /// Exactly the ranges last added to `cut_ranges` by the "Auto-cut
    /// kickoff lead-in + pauses" toggle, so turning it off removes only
    /// those - any manually added/edited cut ranges are left alone. Empty
    /// when the toggle is off.
    scoreboard_derived_cut_ranges: Vec<(f64, f64)>,
    /// The `export-start-secs` value last set by that same toggle (see
    /// `scoreboard_import::derived_start_secs`), so turning it off only
    /// resets the field if the user hasn't since edited it by hand -
    /// same "only touch what we own" idea as
    /// `scoreboard_derived_cut_ranges`, just for a single scalar instead
    /// of a list. `None` when the toggle is off or never suggested one
    /// (no `period_start` event to derive from).
    scoreboard_derived_start_secs: Option<f32>,
    /// The `export-end-secs` value last set by that same toggle (see
    /// `scoreboard_import::derived_end_secs`) - same "only touch what we
    /// own" restore/reset behavior as `scoreboard_derived_start_secs`,
    /// mirrored for the trailing (post `match_end`) trim instead of the
    /// leading (pre-kickoff) one. `None` when the toggle is off or never
    /// suggested one (no `match_end` event to derive from).
    scoreboard_derived_end_secs: Option<f32>,
    /// Output path the highlights toggle last suggested (see
    /// `refresh_derived_cut_ranges`) - same "only touch what we own"
    /// contract as the two fields above, so a path the user typed
    /// themselves survives toggling highlights back off.
    scoreboard_derived_output_path: Option<String>,
    recording_tx: Option<std::sync::mpsc::SyncSender<RecordingFrame>>,
    recording_thread: Option<std::thread::JoinHandle<()>>,
    recording_path: Option<PathBuf>,
    recording_frames: u64,
    /// Receives calibration results from the background thread.
    cal_rx: Option<std::sync::mpsc::Receiver<CalibrationResult>>,
    /// Receives the result of a standalone sync-offset detection job (see
    /// `on_compute_sync_offset`), separate from full auto-calibrate.
    sync_offset_job:
        Option<std::sync::mpsc::Receiver<Result<sync_offset::SyncOffsetResult, String>>>,
    /// Set when the in-flight `sync_offset_job` should write its result
    /// straight to `calibration_path` once it resolves, instead of
    /// waiting for an explicit Save (see the "Select Match Folder"
    /// sync-offset prompt in `on_detect_match_folder_sync_offset`).
    /// Safe to auto-save here specifically because it only fires for a
    /// calibration file just created for this match - never the shared
    /// Default Calibration a normal manual detect+edit could touch.
    pending_sync_offset_autosave: bool,
    /// Left/right audio-sync waveform envelopes currently displayed,
    /// downsampled around `audio_envelope_center_secs`. Empty until the
    /// first recompute (see [`AppState::maybe_recompute_audio_envelope`]).
    audio_envelope_left: Vec<f32>,
    audio_envelope_right: Vec<f32>,
    /// User-adjustable window width (frames), clamped to
    /// `AUDIO_WAVEFORM_WINDOW_FRAMES_RANGE`. Defaults to
    /// `AUDIO_WAVEFORM_WINDOW_FRAMES`.
    audio_window_frames: f64,
    /// Playhead time (seconds) the displayed envelope was computed for.
    /// Recompute triggers once the playhead has moved far enough from
    /// this. Starts at `NEG_INFINITY` so the first expand always computes.
    audio_envelope_center_secs: f64,
    /// `Some` while a background extraction job is in flight; polled and
    /// cleared by `maybe_recompute_audio_envelope`.
    audio_envelope_rx: Option<std::sync::mpsc::Receiver<(Vec<f32>, Vec<f32>)>>,
    /// Wall-clock time of the last triggered recompute, for throttling
    /// during continuous playback (each recompute shells out to ffmpeg).
    audio_envelope_triggered_at: Option<Instant>,
    /// wgpu handles captured from Slint's rendering notifier. `None`
    /// until the window has completed its first rendering setup.
    shared_gpu: Option<SharedGpu>,
    /// Headless preload/auto-export hook (RECO_AUTOLOAD); `None` in normal use.
    #[cfg(feature = "automation")]
    autoload: Option<AutoloadSpec>,
    /// Repeat-export driver state (RECO_AUTOEXPORT_REPEAT). When `mode` is set,
    /// the export-completion handler re-triggers another export until `done`
    /// reaches `total`, then quits the event loop. Inert otherwise.
    #[cfg(feature = "automation")]
    auto_export_mode: bool,
    /// Total exports to run back-to-back this session.
    #[cfg(feature = "automation")]
    auto_export_total: u32,
    /// Exports completed so far (the next run is `done + 1`).
    #[cfg(feature = "automation")]
    auto_export_done: u32,
    /// Base output path for repeat exports; each run appends `_N` before the
    /// extension so successive runs do not overwrite each other.
    #[cfg(feature = "automation")]
    auto_export_base: Option<PathBuf>,
    /// Unified pose state machine (target + current + smoothing +
    /// coverage clamping). Replaces the earlier hand-rolled
    /// `yaw/pitch/target_*` fields; all input events (drag, wheel,
    /// slider, reset) feed `PoseControl` and the render loop reads
    /// `pose.current_pose()`.
    pose: PoseControl,
    /// Pending debounced seek: (fraction, time the request was made).
    /// The timer tick executes the seek once the fraction has stopped
    /// changing for `SEEK_DEBOUNCE_MS`.
    pending_seek: Option<(f32, Instant)>,
    /// Last time we pushed a rendered frame to Slint. Used to cap the
    /// smoothing-driven render rate.
    last_render_at: Option<Instant>,
    /// Set by control changes (blend width, rig tilt) that don't go
    /// through the camera-smoothing path but still need a re-render.
    /// Cleared by the timer after it renders.
    preview_dirty: bool,
    /// Set when the user clicks "Remeasure now" in Color Mapping; the
    /// measurement itself only completes on the *next* render call, so
    /// this flags the timer tick to push a confirmation toast with the
    /// fresh L/R values once that render has actually happened, instead
    /// of trying to report a result that doesn't exist yet.
    color_match_remeasure_pending: bool,
    /// Interrupt flag for a running export. Set to true when the user
    /// clicks Cancel; StitchJob checks it between frames and aborts.
    export_interrupted: Arc<AtomicBool>,
    /// Interrupt flag for a running Auto-Calibrate. Set to true by the
    /// progress popup's Cancel button; `calibrate_videos` checks it
    /// between steps (`check_interrupted` in `reco_calibrate::video`)
    /// and aborts with `CalibrateVideosError::Cancelled`.
    calibration_interrupted: Arc<AtomicBool>,
    /// Timestamp of the last time `run_export`'s progress callback
    /// fired. Used by the playback timer to detect when the encoder is
    /// in its post-last-frame finalization phase (av_write_trailer +
    /// index flush can take ~10 seconds) so we can display "Finalizing
    /// output file…" instead of a stale frame count. Shared via Arc so
    /// the worker thread can stamp it without going through
    /// `invoke_from_event_loop`.
    export_last_progress_at: Arc<Mutex<Option<Instant>>>,
    /// Join handle for the export worker. Held so the timer can see
    /// when the export finishes (via try_recv on export_rx).
    export_thread: Option<std::thread::JoinHandle<()>>,
    /// Receives export completion notifications from the worker.
    export_rx: Option<std::sync::mpsc::Receiver<ExportOutcome>>,
    /// Original Topology values — what auto-calibrate produced. Live
    /// calibration sliders edit relative to this so Reset restores.
    cal_baseline: Option<reco_core::calibration::Calibration>,
    /// Persisted user preferences (recent files, default export
    /// settings, AI model path). Loaded at startup from the reco-io
    /// settings namespace and saved on any change via the convenience
    /// `push_*` methods.
    user_settings: crate::settings::GuiSettings,
    /// Last window size we persisted. Used to debounce resize saves -
    /// we only write to disk when the current size actually differs
    /// from the stored value.
    last_persisted_window_size: Option<(u32, u32)>,
    /// Last time we wrote window-size settings. Combined with the
    /// debounce threshold below to avoid thrashing disk during a
    /// drag-resize (Slint reports a new size every pixel).
    last_window_size_save_at: Option<Instant>,
    /// Baseline camera intrinsics from the last successful calibration.
    /// The Lens fine-tune sliders in the Controls panel edit these; the
    /// Reset Lens button restores them. `None` until auto-calibrate or a
    /// manual match.json load populates them.
    cal_baseline_left_params: Option<reco_core::calibration::Lens>,
    cal_baseline_right_params: Option<reco_core::calibration::Lens>,
    /// When true, `clamp_targets` pins yaw/pitch to the coverage boundary
    /// via `CoverageBoundary::safe_clamp` so the viewport never shows
    /// black margins. When false, pan/zoom is unrestricted - useful for
    /// calibration debug where the user wants to see beyond the stitched
    /// region. Bound to the Slint `use-constrained-look` checkbox.
    use_constrained_look: bool,
    /// When true, the preview shows a single camera through orthographic
    /// projection instead of the stitched panorama.
    lens_preview_active: bool,
    /// Which camera to show in lens preview mode ("left" or "right").
    /// Shared with the goal-geometry editor as its "which goal" selector
    /// too (one Left/Right switch for both, per the merged DETECTION
    /// ZONES card).
    lens_preview_side: String,
    /// Lens correction amount for the preview (0.0 = raw, 1.0 = full).
    lens_correction_amount: f32,
    toasts: ToastManager,
    telemetry: Option<telemetry_client::TelemetryClient>,
    /// Floating debug-log window, created lazily on first "Debug" click
    /// and reused (shown/hidden) after that so its position/size stick
    /// across opens. `None` until then.
    debug_window: Option<DebugWindow>,
}

/// Runtime AI capability summary.
///
/// Calls `reco_detect::probe_execution_providers()` to discover which
/// ONNX Runtime execution providers actually load on this machine,
/// not just which were compiled in. Replaces the old compile-time
/// `cfg!()` summary that lied when a feature was baked in but the
/// runtime libraries were missing.
///
/// Returns `(status_text, any_detector_available)`.
fn ai_capability_summary() -> (String, bool) {
    #[cfg(not(feature = "autocam"))]
    return ("AI: disabled (build without autocam feature)".into(), false);

    #[cfg(feature = "autocam")]
    {
        let probe = reco_detect::probe_execution_providers();
        if !probe.is_available() {
            return (
                format!(
                    "AI: unavailable ({})",
                    probe
                        .errors
                        .first()
                        .cloned()
                        .unwrap_or_else(|| "no execution providers loaded".into())
                ),
                false,
            );
        }
        if probe.can_run_on_gpu_frames {
            (
                format!(
                    "AI: {} (hardware decode + inference)",
                    probe.providers.join(", ")
                ),
                true,
            )
        } else {
            (
                format!(
                    "AI: {} (CPU path - works for file export, not live GPU decode)",
                    probe.best_provider()
                ),
                true,
            )
        }
    }
}

use crate::export::ExportOutcome;

fn build_bug_report(state: &AppState, app_weak: &slint::Weak<RecoApp>) -> String {
    let gpu = state
        .bridge
        .as_ref()
        .map(|b| {
            let g = b.engine().gpu();
            format!("{} ({:?})", g.gpu_name(), g.backend_name())
        })
        .unwrap_or_else(|| "no GPU context".into());

    let version = format!(
        "v{}{}",
        env!("CARGO_PKG_VERSION"),
        option_env!("GIT_HASH")
            .filter(|h| !h.is_empty())
            .map(|h| format!(" ({h})"))
            .unwrap_or_default()
    );

    let os = format!("{} {}", std::env::consts::OS, std::env::consts::ARCH);
    let (ai_status, _) = ai_capability_summary();

    let mut report = format!(
        "## Environment\n\
         - Reco {version}\n\
         - OS: {os}\n\
         - GPU: {gpu}\n\
         - {ai_status}\n"
    );

    // Telemetry snapshot if available
    if let Some(app) = app_weak.upgrade() {
        let fps_avg = app.get_telem_fps_avg();
        let fps_recent = app.get_telem_fps_recent();
        let total_ms = app.get_telem_total_ms();
        let bottleneck = app.get_telem_bottleneck().to_string();
        if fps_avg > 0.0 || total_ms > 0.0 {
            report.push_str(&format!(
                "\n## Performance\n\
                 - FPS: {fps_avg:.0} avg / {fps_recent:.0} recent\n\
                 - Frame time: {total_ms:.1} ms\n"
            ));
            if !bottleneck.is_empty() {
                report.push_str(&format!("- Bottleneck: {bottleneck}\n"));
            }
        }

        let cal_confidence = app.get_telem_cal_confidence();
        let cal_matches = app.get_telem_cal_matches();
        if cal_matches > 0 {
            let reproj = app.get_telem_cal_reproj_err();
            report.push_str(&format!(
                "\n## Calibration\n\
                 - Confidence: {:.0}%\n\
                 - Matches: {cal_matches}\n\
                 - Reprojection error: {reproj:.4}\n",
                cal_confidence * 100.0
            ));
        }
    }

    // Redact file paths
    if let Some(cal) = &state.calibration_path {
        let name = cal.file_name().unwrap_or_default().to_string_lossy();
        report.push_str(&format!("\n## Files\n- Calibration: {name}\n"));
    }

    report.push_str(
        "\n## Description\n\
         <!-- What happened? What did you expect? -->\n\n\
         ## Steps to reproduce\n\
         <!-- 1. ... 2. ... 3. ... -->\n",
    );

    if let Some(log_path) = log_file_path()
        && let Ok(contents) = std::fs::read_to_string(&log_path)
    {
        let lines: Vec<&str> = contents.lines().collect();
        let tail = if lines.len() > 200 {
            &lines[lines.len() - 200..]
        } else {
            &lines
        };
        report.push_str("\n## Log (last 200 lines)\n```\n");
        for line in tail {
            report.push_str(line);
            report.push('\n');
        }
        report.push_str("```\n");
    }

    report
}

struct RecordingFrame {
    data: Vec<u8>,
    width: u32,
    height: u32,
    pts_us: i64,
}

impl AppState {
    fn new() -> Self {
        let discovery = reco_scoreboard::discover_installed();
        for issue in &discovery.issues {
            match issue.severity {
                // A later search root re-finding a package an earlier one
                // already provided (e.g. the exe-adjacent bundle build.rs
                // copies next to both debug and release binaries, plus
                // the dev-tree fallback discover_installed also checks in
                // debug builds) isn't a failure - not worth error!-level
                // log noise, let alone a scary red banner in the GUI.
                reco_scoreboard::DiscoveryIssueSeverity::Info => log::debug!(
                    "Scoreboard package overlap at {}: {}",
                    issue.path.display(),
                    issue.message
                ),
                reco_scoreboard::DiscoveryIssueSeverity::Warning => log::warn!(
                    "Skipping scoreboard package {}: {}",
                    issue.path.display(),
                    issue.message
                ),
            }
        }
        // Only surface an issue in the GUI when it actually left the
        // feature unusable (no packages found at all) - a Warning
        // alongside at least one successfully loaded package (or any
        // Info-only overlap) isn't something the user needs to act on.
        let scoreboard_error = if discovery.packages.is_empty() {
            discovery
                .issues
                .iter()
                .find(|issue| issue.severity == reco_scoreboard::DiscoveryIssueSeverity::Warning)
                .map(|issue| issue.message.clone())
                .unwrap_or_default()
        } else {
            String::new()
        };
        Self {
            left_path: None,
            right_path: None,
            left_input: None,
            right_input: None,
            match_folder: None,
            calibration_path: None,
            calibration: None,
            cut_ranges: Vec::new(),
            playback: Playback::new(),
            bridge: None,
            scoreboard_packages: discovery.packages,
            scoreboard_runtime: None,
            scoreboard_design_size: None,
            scoreboard_render_scale_last_apply: None,
            scoreboard_frame: None,
            scoreboard_frame_dirty: false,
            scoreboard_error,
            scoreboard_import: None,
            scoreboard_import_path: None,
            scoreboard_sync_anchor: None,
            scoreboard_replay_last_push: None,
            scoreboard_placement: reco_core::render::overlay::OverlayPlacement::default(),
            scoreboard_style: scoreboard_import::ScoreboardStyle::default(),
            scoreboard_derived_cut_ranges: Vec::new(),
            scoreboard_derived_start_secs: None,
            scoreboard_derived_end_secs: None,
            scoreboard_derived_output_path: None,
            recording_tx: None,
            recording_thread: None,
            recording_path: None,
            recording_frames: 0,
            cal_rx: None,
            sync_offset_job: None,
            pending_sync_offset_autosave: false,
            audio_envelope_left: Vec::new(),
            audio_envelope_right: Vec::new(),
            audio_window_frames: AUDIO_WAVEFORM_WINDOW_FRAMES,
            audio_envelope_center_secs: f64::NEG_INFINITY,
            audio_envelope_rx: None,
            audio_envelope_triggered_at: None,
            shared_gpu: None,
            #[cfg(feature = "automation")]
            autoload: AutoloadSpec::from_env(),
            #[cfg(feature = "automation")]
            auto_export_mode: false,
            #[cfg(feature = "automation")]
            auto_export_total: 0,
            #[cfg(feature = "automation")]
            auto_export_done: 0,
            #[cfg(feature = "automation")]
            auto_export_base: None,
            pose: PoseControl::new(PoseControlConfig {
                drag_deg_per_pixel: DRAG_DEG_PER_PIXEL,
                smoothing: POSE_SMOOTHING,
                fov_min_degrees: FOV_MIN,
                fov_max_degrees: FOV_MAX,
                // Pre-migration GUI: drag-right -> target_yaw +=,
                // i.e. PTZ-head convention. `invert_drag_x = true`
                // keeps that exact feel.
                invert_drag_x: true,
                rest_pose: ViewportPosition {
                    yaw: 0.0,
                    pitch: 0.0,
                    fov_degrees: Some(FOV_DEFAULT),
                },
                ..PoseControlConfig::default()
            }),
            pending_seek: None,
            last_render_at: None,
            preview_dirty: false,
            color_match_remeasure_pending: false,
            export_interrupted: Arc::new(AtomicBool::new(false)),
            calibration_interrupted: Arc::new(AtomicBool::new(false)),
            export_last_progress_at: Arc::new(Mutex::new(None)),
            export_thread: None,
            export_rx: None,
            cal_baseline: None,
            user_settings: {
                let mut s = crate::settings::GuiSettings::load();
                if s.telemetry_client_id.is_none() {
                    s.telemetry_client_id = Some(uuid::Uuid::new_v4().to_string());
                    s.save();
                }
                s
            },
            last_persisted_window_size: None,
            last_window_size_save_at: None,
            cal_baseline_left_params: None,
            cal_baseline_right_params: None,
            use_constrained_look: true,
            lens_preview_active: false,
            lens_preview_side: "left".into(),
            lens_correction_amount: 1.0,
            toasts: ToastManager::default(),
            telemetry: None,
            debug_window: None,
        }
    }

    fn is_exporting(&self) -> bool {
        self.export_thread.is_some()
    }

    fn configure_scoreboard(&mut self, enabled: bool, selected_index: usize) {
        self.scoreboard_runtime = None;
        self.scoreboard_design_size = None;
        self.scoreboard_frame = None;
        self.scoreboard_frame_dirty = false;
        if let Some(bridge) = self.bridge.as_mut() {
            bridge.clear_overlay();
        }
        self.preview_dirty = true;
        if !enabled {
            self.scoreboard_error.clear();
            return;
        }
        let Some(package) = self.scoreboard_packages.get(selected_index).cloned() else {
            self.scoreboard_error = "No valid scoreboard package is installed".into();
            return;
        };
        let design_size = (
            package.manifest.viewport.width,
            package.manifest.viewport.height,
        );
        match reco_scoreboard::ScoreboardRuntime::start(package, 30) {
            Ok(runtime) => {
                self.scoreboard_runtime = Some(runtime);
                self.scoreboard_design_size = Some(design_size);
                self.scoreboard_error.clear();
                // Apply whatever placement/viewport is already known
                // right away - without this a freshly (re)started
                // runtime renders at full design resolution until the
                // next unrelated placement change happens to touch it.
                self.apply_scoreboard_render_scale();
            }
            Err(error) => {
                self.scoreboard_error = error.to_string();
                log::error!("Cannot enable scoreboard: {error}");
            }
        }
    }

    /// Recompute and push the scoreboard's Chrome render scale from the
    /// current `scoreboard_placement` and the live preview's viewport
    /// size - see `reco_scoreboard::ScoreboardRuntime::set_render_scale`.
    /// A no-op when no scoreboard runtime or preview pipeline is active
    /// yet. Throttled (see `scoreboard_render_scale_last_apply`) - safe
    /// to call on every placement drag tick regardless.
    fn apply_scoreboard_render_scale(&mut self) {
        const MIN_INTERVAL: std::time::Duration = std::time::Duration::from_millis(150);
        if let Some(last) = self.scoreboard_render_scale_last_apply
            && last.elapsed() < MIN_INTERVAL
        {
            return;
        }
        let (Some(runtime), Some(design_size), Some(bridge)) = (
            self.scoreboard_runtime.as_ref(),
            self.scoreboard_design_size,
            self.bridge.as_ref(),
        ) else {
            return;
        };
        let render_scale = reco_core::render::overlay::contain_fit_render_scale(
            design_size,
            bridge.viewport_size(),
            self.scoreboard_placement,
        );
        if let Err(error) = runtime.set_render_scale(render_scale) {
            log::warn!("Scoreboard: could not apply render scale {render_scale:.3}: {error}");
        }
        self.scoreboard_render_scale_last_apply = Some(std::time::Instant::now());
    }

    /// Poll the independent HTML worker and upload only a changed RGBA frame.
    fn poll_scoreboard_overlay(&mut self) -> bool {
        let frame_result = self
            .scoreboard_runtime
            .as_mut()
            .map(OverlayFrameSource::try_frame);
        match frame_result {
            None | Some(Ok(None)) => {}
            Some(Ok(Some(frame))) => {
                self.scoreboard_frame = Some(frame);
                self.scoreboard_frame_dirty = true;
            }
            Some(Err(error)) => {
                self.scoreboard_error = error;
                log::error!("Disabling scoreboard: {}", self.scoreboard_error);
                self.scoreboard_runtime = None;
                self.scoreboard_frame = None;
                self.scoreboard_frame_dirty = false;
                if let Some(bridge) = self.bridge.as_mut() {
                    bridge.clear_overlay();
                }
                return true;
            }
        }

        if !self.scoreboard_frame_dirty {
            return false;
        }
        let (Some(frame), Some(bridge)) = (self.scoreboard_frame.as_ref(), self.bridge.as_mut())
        else {
            return false;
        };
        match bridge.set_overlay_frame(frame) {
            Ok(()) => {
                self.scoreboard_frame_dirty = false;
                true
            }
            Err(error) => {
                self.scoreboard_error = format!("Cannot upload scoreboard: {error}");
                log::error!("{}", self.scoreboard_error);
                self.scoreboard_runtime = None;
                self.scoreboard_frame = None;
                self.scoreboard_frame_dirty = false;
                bridge.clear_overlay();
                true
            }
        }
    }

    /// Recompute the replayed scoreboard state for the current preview
    /// position and push it to the running package, throttled to avoid a
    /// JS eval on every render tick (see `scoreboard_replay_last_push`).
    /// A no-op unless both a Match Logger export and a sync anchor are
    /// loaded - live-manual editing (no import loaded) is unaffected.
    fn push_scoreboard_replay(&mut self) {
        const MIN_INTERVAL: std::time::Duration = std::time::Duration::from_millis(150);
        let (Some(export), Some(anchor), Some(runtime)) = (
            self.scoreboard_import.as_ref(),
            self.scoreboard_sync_anchor.as_ref(),
            self.scoreboard_runtime.as_ref(),
        ) else {
            return;
        };
        if let Some(last) = self.scoreboard_replay_last_push
            && last.elapsed() < MIN_INTERVAL
        {
            return;
        }
        let fps = self.playback.fps();
        let video_seconds = if fps > 0.0 {
            self.playback.frame_index() as f64 / fps
        } else {
            0.0
        };
        let state = scoreboard_import::state_at(export, anchor, video_seconds);
        let state = scoreboard_import::apply_style(state, &self.scoreboard_style);
        if let Err(error) = runtime.update(&state) {
            self.scoreboard_error = format!("Cannot update scoreboard: {error}");
            log::error!("{}", self.scoreboard_error);
        }
        self.scoreboard_replay_last_push = Some(std::time::Instant::now());
    }

    fn reset_pipeline(&mut self) {
        self.bridge = None;
        self.scoreboard_frame_dirty = self.scoreboard_frame.is_some();
        self.playback = Playback::new();
        self.pose = PoseControl::new(PoseControlConfig {
            drag_deg_per_pixel: DRAG_DEG_PER_PIXEL,
            smoothing: POSE_SMOOTHING,
            fov_min_degrees: FOV_MIN,
            fov_max_degrees: FOV_MAX,
            invert_drag_x: true,
            rest_pose: ViewportPosition {
                yaw: 0.0,
                pitch: 0.0,
                fov_degrees: Some(FOV_DEFAULT),
            },
            ..PoseControlConfig::default()
        });
        self.pending_seek = None;
        self.last_render_at = None;
        self.preview_dirty = false;
        self.audio_envelope_left.clear();
        self.audio_envelope_right.clear();
        self.audio_envelope_center_secs = f64::NEG_INFINITY;
        self.audio_envelope_rx = None;
    }

    /// Build a PreviewBridge using the captured Slint GPU handles. Fails
    /// if the rendering notifier hasn't populated `shared_gpu` yet.
    fn build_bridge(
        &mut self,
        cal: &Calibration,
        input_w: u32,
        input_h: u32,
    ) -> Result<PreviewBridge, String> {
        let gpu = self
            .shared_gpu
            .as_ref()
            .ok_or("GPU not ready yet (Slint rendering not initialized)")?
            .clone();
        // Save baseline layout so Reset Calibration can restore it.
        self.cal_baseline = Some(cal.clone());
        PreviewBridge::new(
            gpu.device,
            gpu.queue,
            gpu.adapter_info,
            cal.clone(),
            input_w,
            input_h,
            PREVIEW_WIDTH_DEFAULT,
            PREVIEW_HEIGHT_DEFAULT,
        )
        .map_err(|e| format!("GPU init error: {e}"))
    }

    /// Apply an edited Topology to the renderer. `preview_dirty`
    /// triggers a re-render on the next timer tick.
    fn apply_layout(&mut self, layout: reco_core::calibration::Topology) {
        if let Some(cal) = self.calibration.as_mut() {
            cal.topology = layout.clone();
        }
        if let Some(bridge) = self.bridge.as_mut() {
            bridge.engine_mut().update_topology(layout);
            self.preview_dirty = true;
        }
        // Deliberately no `clamp_targets()` here: almost every topology
        // field shifts the no-black coverage boundary at least slightly,
        // so re-clamping on every calibration slider drag could pull the
        // camera pose (and everything on screen, including a seam-line
        // the user is watching) away from where they left it, even
        // though nothing they're actually panning/zooming changed. The
        // clamp still runs from the real pan/zoom/reset paths below,
        // which is where "Constrained look" is meant to intervene.
    }

    /// Apply edited framing (axis offset, tilt, roll) to the renderer.
    fn apply_framing(&mut self, framing: reco_core::calibration::Framing) {
        if let Some(cal) = self.calibration.as_mut() {
            cal.framing = framing.clone();
        }
        if let Some(bridge) = self.bridge.as_mut() {
            bridge.engine_mut().update_framing(framing);
            self.preview_dirty = true;
        }
        // See `apply_layout`'s comment - no auto-clamp on a calibration edit.
    }

    /// Persist the full `left_input` segment chain so a multi-segment
    /// selection survives an app restart (see `GuiSettings::last_left_segments`
    /// - `push_left`'s single-path MRU entry isn't enough to reconstruct one).
    fn persist_left_segments(&mut self) {
        let segments = self
            .left_input
            .as_ref()
            .map(reco_io::stitch_job::InputPath::all_paths)
            .unwrap_or_default();
        self.user_settings.set_last_left_segments(segments);
    }

    /// Same as [`Self::persist_left_segments`], for `right_input`.
    fn persist_right_segments(&mut self) {
        let segments = self
            .right_input
            .as_ref()
            .map(reco_io::stitch_job::InputPath::all_paths)
            .unwrap_or_default();
        self.user_settings.set_last_right_segments(segments);
    }

    /// Write the current (edited) calibration back to disk.
    fn save_calibration(&self) -> Result<(), String> {
        let (Some(cal), Some(path)) = (&self.calibration, &self.calibration_path) else {
            return Err("No calibration or path to save".into());
        };
        // `self.calibration` is the always-synced source of truth: every
        // mutation path (layout/framing sliders, lens sliders and pickers,
        // blend, lens correction, sync) writes it alongside the live
        // renderer, so it saves verbatim - no fold-ins from the pipeline
        // that could race or clobber each other.
        let json = serde_json::to_string_pretty(cal).map_err(|e| format!("serialize: {e}"))?;
        std::fs::write(path, json).map_err(|e| format!("write {}: {e}", path.display()))?;
        log::info!("Saved calibration to {}", path.display());
        Ok(())
    }

    /// Whether the calibration currently loaded is the one configured in
    /// preferences as the Default Calibration - i.e. saving now would
    /// overwrite the fallback future sessions rely on, not just this
    /// session's own file. `None == None` deliberately doesn't count (no
    /// default configured means nothing to protect).
    fn is_default_calibration(&self) -> bool {
        match (
            &self.calibration_path,
            &self.user_settings.default_calibration_path,
        ) {
            (Some(current), Some(default)) => current == default,
            _ => false,
        }
    }

    /// Restore Topology to the values loaded at init (or after auto-cal).
    fn reset_calibration(&mut self) {
        if let Some(base) = self.cal_baseline.clone() {
            self.apply_layout(base.topology);
            self.apply_framing(base.framing);
        }
    }

    /// Check if all three files are selected and try to initialize.
    fn try_init(&mut self) -> Result<bool, String> {
        // Open playback from the full InputPath (all concat segments) so the
        // timeline duration spans every file, not just the first. left_path/
        // right_path stay single (first segment) for lens/calibration.
        let (left, right, cal_path) =
            match (&self.left_input, &self.right_input, &self.calibration_path) {
                (Some(l), Some(r), Some(c)) => (l.clone(), r.clone(), c.clone()),
                _ => return Ok(false),
            };

        // Load calibration.
        let cal = Calibration::from_file(&cal_path)
            .map_err(|e| format!("Calibration load error: {e}"))?;

        // Open video source.
        let sync_offset = cal.sync_offset;
        self.playback
            .open(&left, &right, sync_offset)
            .map_err(|e| format!("Video open error: {e}"))?;

        let (input_w, input_h) = self
            .playback
            .input_dimensions()
            .ok_or("No input dimensions")?;

        let mut bridge = self.build_bridge(&cal, input_w, input_h)?;
        // A freshly built bridge starts with the compositor's own
        // hardcoded default placement (centered, largest that fits) -
        // sync in whatever placement is already known (app-level
        // restored settings, or a mid-session drag), regardless of
        // whether this calibration itself has a saved `scoreboard`
        // block. Without this, the scoreboard visibly jumps back to
        // that default every time the pipeline is rebuilt (app start,
        // after an export, ...) unless the calibration happens to carry
        // its own placement - see `init_with_calibration`'s copy of
        // this same fix for the other rebuild path.
        bridge.set_overlay_placement(self.scoreboard_placement);

        self.calibration = Some(cal);
        self.bridge = Some(bridge);
        // Viewport size may have changed with this rebuild - resync the
        // scoreboard's Chrome render scale to match (see
        // `apply_scoreboard_render_scale`'s own doc comment).
        self.apply_scoreboard_render_scale();
        Ok(true)
    }

    /// Initialize preview from a calibration result (no file needed).
    fn init_with_calibration(&mut self, cal: Calibration) -> Result<bool, String> {
        let (left, right) = match (&self.left_input, &self.right_input) {
            (Some(l), Some(r)) => (l.clone(), r.clone()),
            _ => return Err("Both video inputs required".into()),
        };

        let sync_offset = cal.sync_offset;
        self.playback
            .open(&left, &right, sync_offset)
            .map_err(|e| format!("Video open error: {e}"))?;

        let (input_w, input_h) = self
            .playback
            .input_dimensions()
            .ok_or("No input dimensions")?;

        let mut bridge = self.build_bridge(&cal, input_w, input_h)?;
        // See `try_init`'s copy of this same fix - this is the rebuild
        // path used after an export (and after auto-calibration), so
        // without it the scoreboard visibly jumps to the compositor's
        // default placement every time an export finishes.
        bridge.set_overlay_placement(self.scoreboard_placement);

        self.calibration = Some(cal);
        self.bridge = Some(bridge);
        // See `try_init`'s copy of this same fix.
        self.apply_scoreboard_render_scale();
        Ok(true)
    }

    /// Tear down the live pipeline so the preview stops rendering the
    /// stale source after a calibration failure or an in-place file
    /// swap. Keeps the user-picked paths on `AppState` so the user can
    /// fix and retry, but drops the bridge + playback + calibration.
    fn unload_pipeline(&mut self) {
        self.stop_recording();
        self.reset_pipeline();
        self.cal_baseline = None;
        self.cal_baseline_left_params = None;
        self.cal_baseline_right_params = None;
    }

    fn start_recording(&mut self, codec: &str, quality: &str) -> Result<PathBuf, String> {
        let bridge = self.bridge.as_ref().ok_or("No pipeline")?;
        let (w, h) = bridge.viewport_size();
        let rec_w = w & !3;
        let rec_h = h & !1;
        let fps = self.playback.fps();
        let fps_r = (fps.round() as i32, 1);

        let epoch = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        let folder = self
            .user_settings
            .recording_folder
            .as_ref()
            .filter(|p| p.is_dir())
            .cloned()
            .or_else(|| {
                self.left_path
                    .as_ref()
                    .and_then(|p| p.parent().map(|d| d.to_path_buf()))
            })
            .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));
        let folder = std::fs::canonicalize(&folder).unwrap_or(folder);
        let path = folder.join(format!("reco_recording_{epoch}.mp4"));

        let (encoder, enc_name) = reco_io::adapters::create_encoder(
            &path, rec_w, rec_h, fps_r, codec, quality, None, None, None,
        )
        .map_err(|e| format!("Failed to start recording: {e}"))?;

        log::info!(
            "Recording started: {} ({enc_name}, {codec}/{quality})",
            path.display()
        );

        // Spawn encoder thread with bounded channel (4 frames deep).
        // The UI thread sends NV12 data without blocking on FFmpeg.
        let (tx, rx) = std::sync::mpsc::sync_channel::<RecordingFrame>(4);
        let handle = std::thread::spawn(move || {
            let mut encoder: Box<dyn reco_core::encoder::Encoder + Send> = Box::new(encoder);
            while let Ok(frame) = rx.recv() {
                let _ = encoder.submit(reco_core::encoder::OutputFrame {
                    data: &frame.data,
                    width: frame.width,
                    height: frame.height,
                    format: reco_core::encoder::PixelFormat::Nv12,
                    pts_us: frame.pts_us,
                });
            }
            let _ = encoder.finish();
        });

        self.recording_tx = Some(tx);
        self.recording_thread = Some(handle);
        self.recording_path = Some(path.clone());
        self.recording_frames = 0;
        Ok(path)
    }

    fn stop_recording(&mut self) -> Option<(PathBuf, u64)> {
        // Drop the sender to signal the encoder thread to finish.
        self.recording_tx = None;
        if let Some(handle) = self.recording_thread.take() {
            let _ = handle.join();
        }
        let path = self.recording_path.take()?;
        let frames = self.recording_frames;
        self.recording_frames = 0;
        log::info!("Recording stopped: {frames} frames to {}", path.display());
        Some((path, frames))
    }

    fn is_recording(&self) -> bool {
        self.recording_tx.is_some()
    }

    /// Render the current frame. With zero-copy texture sharing, the
    /// same path works for both playback ticks and seek/step — no more
    /// sync vs async distinction.
    fn render_current(&mut self) -> Option<slint::Image> {
        let frame = self.playback.current_frame()?;

        let left = frame.left.as_planes();
        let right = frame.right.as_planes();

        // Lens preview mode: render single camera flat
        if self.lens_preview_active {
            let bridge = self.bridge.as_mut()?;
            let cal = bridge.engine().calibration();
            let (planes, params) = if self.lens_preview_side == "right" {
                (&right, cal.lenses[1].clone())
            } else {
                (&left, cal.lenses[0].clone())
            };
            return match bridge.render_lens_preview(planes, &params, self.lens_correction_amount) {
                Ok(img) => Some(img),
                Err(e) => {
                    log::error!("Lens preview error: {e}");
                    None
                }
            };
        }

        let pose = self
            .bridge
            .as_ref()?
            .engine()
            .orient_pose(self.pose.current_pose());

        let recording = self.is_recording();
        if recording {
            let bridge = self.bridge.as_mut().unwrap();
            let (w, h) = bridge.viewport_size();
            let fps = self.playback.fps();

            // NV12 readback for the encoder on every frame. This is the
            // only stitch render per frame - no separate display render.
            match bridge
                .engine_mut()
                .render_and_readback_nv12(&left, &right, pose)
            {
                Ok(Some(nv12)) => {
                    if let Some(tx) = self.recording_tx.as_ref() {
                        let pts = (self.recording_frames as f64 / fps * 1_000_000.0) as i64;
                        let _ = tx.try_send(RecordingFrame {
                            data: nv12.to_vec(),
                            width: w & !3,
                            height: h & !1,
                            pts_us: pts,
                        });
                    }
                    self.recording_frames += 1;
                }
                Ok(None) => {
                    self.recording_frames += 1;
                }
                Err(e) => log::error!("NV12 readback error: {e}"),
            }

            // Display preview at reduced rate (every 5th frame).
            // The display render is cheap compared to the NV12 readback
            // but we skip most frames to keep encoding smooth.
            if self.recording_frames.is_multiple_of(5) {
                let bridge = self.bridge.as_mut().unwrap();
                match bridge.render_frame(&left, &right, pose) {
                    Ok(img) => return Some(img),
                    Err(e) => log::error!("Preview render error: {e}"),
                }
            }
            None
        } else {
            match self.bridge.as_mut()?.render_frame(&left, &right, pose) {
                Ok(img) => Some(img),
                Err(e) => {
                    log::error!("Render error: {e}");
                    None
                }
            }
        }
    }

    /// Apply a pixel-space pan delta. Feeds `PoseControl::apply_drag`
    /// then runs the coverage clamp so the resulting target stays
    /// inside the no-black region.
    fn apply_pan(&mut self, dx_px: f32, dy_px: f32) {
        self.pose.apply_drag(dx_px, dy_px);
        self.clamp_targets();
        // Flag dirty so the playback timer requests redraws until the
        // smoothing lerp settles. Without this, when paused, the lerp
        // after the mouse is released is never run (timer sees nothing
        // to do and stops nudging Slint), so pan motion snaps/stalls.
        self.preview_dirty = true;
    }

    /// Apply a FOV delta (degrees). Clamps the target; tick handles smoothing.
    fn apply_zoom(&mut self, delta_deg: f32) {
        self.pose
            .apply_intent(ControlIntent::Pose(PoseIntent::DeltaFovDeg(delta_deg)));
        self.clamp_targets();
        self.preview_dirty = true;
    }

    /// Set FOV absolute (from the slider). Updates target; tick applies it.
    fn set_fov(&mut self, fov_deg: f32) {
        self.pose
            .apply_intent(ControlIntent::Pose(PoseIntent::SetFovDeg(fov_deg)));
        self.clamp_targets();
        self.preview_dirty = true;
    }

    /// Advance the PoseControl one smoothing step and push the
    /// resulting FOV back to the renderer pipeline. Returns `true`
    /// when the pose changed measurably (caller uses this to decide
    /// whether to re-render).
    fn smooth_camera(&mut self) -> bool {
        let before = self.pose.current_pose();
        self.pose.tick();
        if self.use_constrained_look
            && let Some(bridge) = self.bridge.as_ref()
        {
            let renderer = bridge.engine();
            let (vw, vh) = bridge.viewport_size();
            let aspect = vw as f32 / vh as f32;
            if let Some(coverage) = renderer.coverage() {
                self.pose.clamp_via_coverage(coverage, aspect);
            }
        }
        let after = self.pose.current_pose();

        let yaw_changed = (before.yaw - after.yaw).abs() > f32::EPSILON;
        let pitch_changed = (before.pitch - after.pitch).abs() > f32::EPSILON;
        let fov_changed = before.fov_degrees != after.fov_degrees;

        // FOV rides the pose into every render_frame call, so no cached
        // push is needed here - the render can never see a stale value.
        yaw_changed || pitch_changed || fov_changed
    }

    /// Set seam blend width. Reasonable range is 0.0 to 0.3.
    fn set_blend_width(&mut self, w: f32) {
        let w = w.clamp(0.0, 0.5);
        // Mirror into the source-of-truth calibration so topology slider
        // edits (which clone-and-reapply the whole Topology) and saves
        // cannot revert the blend to a stale value.
        if let Some(cal) = self.calibration.as_mut() {
            cal.topology.blend_width = w;
        }
        if let Some(bridge) = self.bridge.as_mut() {
            bridge.engine_mut().set_blend_width(w);
            self.preview_dirty = true;
        }
    }

    /// Set which camera fades over the other at the blend seam.
    fn set_blend_flip_direction(&mut self, flip: bool) {
        // Mirror into the source-of-truth calibration first - see
        // `set_blend_width`'s comment for why (same bug class: without
        // this, any other topology slider clones the stale pre-flip
        // value and silently reverts it).
        if let Some(cal) = self.calibration.as_mut() {
            cal.topology.blend_flip_direction = flip;
        }
        if let Some(bridge) = self.bridge.as_mut() {
            bridge
                .engine_mut()
                .pipeline_mut()
                .set_blend_flip_direction(flip);
            self.preview_dirty = true;
        }
    }

    /// Enable/disable the 2-band spatial seam blend.
    fn set_multiband_blend_enabled(&mut self, enabled: bool) {
        // See `set_blend_width`'s comment for why this must also update
        // the source-of-truth calibration, not just the live pipeline.
        if let Some(cal) = self.calibration.as_mut() {
            cal.topology.multiband_blend_enabled = enabled;
        }
        if let Some(bridge) = self.bridge.as_mut() {
            bridge
                .engine_mut()
                .pipeline_mut()
                .set_multiband_blend_enabled(enabled);
            self.preview_dirty = true;
        }
    }

    /// Show/hide the geometric seam-position debug line.
    fn set_show_seam_line(&mut self, show: bool) {
        if let Some(bridge) = self.bridge.as_mut() {
            bridge.engine_mut().pipeline_mut().set_show_seam_line(show);
            self.preview_dirty = true;
        }
    }

    /// Set the manual seam nudge to an absolute value (slider).
    fn set_seam_offset(&mut self, offset: f32) {
        if let Some(bridge) = self.bridge.as_mut() {
            bridge.engine_mut().pipeline_mut().set_seam_offset(offset);
            self.preview_dirty = true;
        }
        // Keep AppState's own calibration copy in sync - every other
        // calibration slider handler clones `self.calibration.topology`
        // as its starting point before editing its own field, so without
        // this the seam position would get silently reverted the next
        // time ANY other slider fires (the "seam jumps back" bug).
        if let Some(cal) = self.calibration.as_mut() {
            cal.topology.seam_offset = offset;
        }
    }

    /// Nudge the seam by a preview-width-normalized drag delta (dragging
    /// the preview itself while Show seam line is active). Flips sign
    /// under `blend_flip_direction` so a screen-space drag feels
    /// consistent either way - see `fisheye.wgsl`'s `seam_offset` comment
    /// for why the raw uniform's sign otherwise depends on which camera
    /// is fading. Returns the new absolute value so the caller can push it
    /// back to the "Seam position" slider.
    fn seam_drag(&mut self, dx_normalized: f32) -> Option<f32> {
        const SENSITIVITY: f32 = 0.6;
        let bridge = self.bridge.as_mut()?;
        let flip = bridge.engine().calibration().topology.blend_flip_direction;
        let sign = if flip { -1.0 } else { 1.0 };
        let current = bridge.engine().pipeline().seam_offset();
        let new_value = current + dx_normalized * SENSITIVITY * sign;
        bridge
            .engine_mut()
            .pipeline_mut()
            .set_seam_offset(new_value);
        self.preview_dirty = true;
        // Keep AppState's own calibration copy in sync - see
        // `set_seam_offset`'s comment for why this matters.
        if let Some(cal) = self.calibration.as_mut() {
            cal.topology.seam_offset = new_value;
        }
        Some(new_value)
    }

    /// Enable/disable automatic per-camera exposure/color matching.
    fn set_color_match_enabled(&mut self, enabled: bool) {
        // See `set_blend_width`'s comment - every `color_match_*` setter
        // below has the same bug class fixed here: without mirroring
        // into `self.calibration`, any OTHER calibration slider (which
        // clones `self.calibration.topology` as its starting point)
        // would silently revert this back to its stale pre-edit value.
        if let Some(cal) = self.calibration.as_mut() {
            cal.topology.color_match_enabled = enabled;
        }
        if let Some(bridge) = self.bridge.as_mut() {
            bridge
                .engine_mut()
                .pipeline_mut()
                .set_color_match_enabled(enabled);
            self.preview_dirty = true;
        }
    }

    /// Enable/disable automatic per-camera gamma fitting - see
    /// `StitchPipeline::set_color_match_auto_gamma`'s doc for why this
    /// exists (a static manual gamma tuned for one moment can visibly
    /// overshoot at another once lighting changes through a match).
    fn set_color_match_auto_gamma(&mut self, enabled: bool) {
        // See `set_color_match_enabled`'s comment on why this also has to
        // be mirrored into `self.calibration`.
        if let Some(cal) = self.calibration.as_mut() {
            cal.topology.color_match_auto_gamma = enabled;
        }
        if let Some(bridge) = self.bridge.as_mut() {
            bridge
                .engine_mut()
                .pipeline_mut()
                .set_color_match_auto_gamma(enabled);
            self.preview_dirty = true;
        }
    }

    /// Force an immediate color-match remeasure against the current frame,
    /// without waiting for the periodic interval (which only advances while
    /// rendering) or requiring a calibration change to trigger it.
    fn remeasure_color_match(&mut self) {
        if let Some(bridge) = self.bridge.as_mut() {
            bridge
                .engine_mut()
                .pipeline_mut()
                .force_color_match_remeasure();
            self.preview_dirty = true;
            // The actual measurement only completes on the next render
            // call - flag it so the timer tick can push a toast with the
            // real result once it's actually in, instead of reporting
            // something that doesn't exist yet.
            self.color_match_remeasure_pending = true;
            log::info!("Color-match remeasure forced by user");
        }
    }

    fn set_color_match_band_width(&mut self, w: f32) {
        if let Some(cal) = self.calibration.as_mut() {
            cal.topology.color_match_band_width = w;
        }
        if let Some(bridge) = self.bridge.as_mut() {
            bridge
                .engine_mut()
                .pipeline_mut()
                .set_color_match_band_width(w);
            self.preview_dirty = true;
        }
    }

    fn set_color_match_grid_cols(&mut self, cols: f32) {
        let cols = cols.round().max(1.0) as u32;
        if let Some(cal) = self.calibration.as_mut() {
            cal.topology.color_match_grid_cols = cols;
        }
        if let Some(bridge) = self.bridge.as_mut() {
            bridge
                .engine_mut()
                .pipeline_mut()
                .set_color_match_grid_cols(cols);
            self.preview_dirty = true;
        }
    }

    fn set_color_match_grid_rows(&mut self, rows: f32) {
        let rows = rows.round().max(1.0) as u32;
        if let Some(cal) = self.calibration.as_mut() {
            cal.topology.color_match_grid_rows = rows;
        }
        if let Some(bridge) = self.bridge.as_mut() {
            bridge
                .engine_mut()
                .pipeline_mut()
                .set_color_match_grid_rows(rows);
            self.preview_dirty = true;
        }
    }

    fn set_color_match_interval_frames(&mut self, frames: f32) {
        let frames = frames.round().max(1.0) as u32;
        if let Some(cal) = self.calibration.as_mut() {
            cal.topology.color_match_interval_frames = frames;
        }
        if let Some(bridge) = self.bridge.as_mut() {
            bridge
                .engine_mut()
                .pipeline_mut()
                .set_color_match_interval_frames(frames);
            self.preview_dirty = true;
        }
    }

    fn set_color_match_ema_alpha(&mut self, alpha: f32) {
        if let Some(cal) = self.calibration.as_mut() {
            cal.topology.color_match_ema_alpha = alpha;
        }
        if let Some(bridge) = self.bridge.as_mut() {
            bridge
                .engine_mut()
                .pipeline_mut()
                .set_color_match_ema_alpha(alpha);
            self.preview_dirty = true;
        }
    }

    fn set_color_match_max_y_offset(&mut self, v: f32) {
        if let Some(cal) = self.calibration.as_mut() {
            cal.topology.color_match_max_y_offset = v;
        }
        if let Some(bridge) = self.bridge.as_mut() {
            bridge
                .engine_mut()
                .pipeline_mut()
                .set_color_match_max_y_offset(v);
            self.preview_dirty = true;
        }
    }

    fn set_color_match_max_chroma_offset(&mut self, v: f32) {
        if let Some(cal) = self.calibration.as_mut() {
            cal.topology.color_match_max_chroma_offset = v;
        }
        if let Some(bridge) = self.bridge.as_mut() {
            bridge
                .engine_mut()
                .pipeline_mut()
                .set_color_match_max_chroma_offset(v);
            self.preview_dirty = true;
        }
    }

    /// Manual per-camera gamma, applied before the automatic match (see
    /// `reco_core::calibration::Topology::color_gamma_left`). Both sides
    /// are set together because the two sliders share one callback: a
    /// slider only ever knows its own value, and the pipeline setter
    /// takes the pair.
    fn set_color_gamma(&mut self, left: f32, right: f32) {
        if let Some(cal) = self.calibration.as_mut() {
            cal.topology.color_gamma_left = left;
            cal.topology.color_gamma_right = right;
        }
        if let Some(bridge) = self.bridge.as_mut() {
            bridge
                .engine_mut()
                .pipeline_mut()
                .set_color_gamma(left, right);
            self.preview_dirty = true;
        }
    }

    /// Restore all Auto Color Match tuning knobs to their engineering
    /// defaults - not the values loaded from the current calibration file,
    /// which is what `reset_calibration` does for the layout sliders.
    fn reset_color_match(&mut self) {
        use reco_core::calibration::{
            DEFAULT_COLOR_GAMMA, DEFAULT_COLOR_MATCH_AUTO_GAMMA, DEFAULT_COLOR_MATCH_BAND_WIDTH,
            DEFAULT_COLOR_MATCH_EMA_ALPHA, DEFAULT_COLOR_MATCH_ENABLED,
            DEFAULT_COLOR_MATCH_GRID_COLS, DEFAULT_COLOR_MATCH_GRID_ROWS,
            DEFAULT_COLOR_MATCH_INTERVAL_FRAMES, DEFAULT_COLOR_MATCH_MAX_CHROMA_OFFSET,
            DEFAULT_COLOR_MATCH_MAX_Y_OFFSET,
        };
        if let Some(bridge) = self.bridge.as_mut() {
            let pipeline = bridge.engine_mut().pipeline_mut();
            pipeline.set_color_match_enabled(DEFAULT_COLOR_MATCH_ENABLED);
            pipeline.set_color_match_band_width(DEFAULT_COLOR_MATCH_BAND_WIDTH);
            pipeline.set_color_match_grid_cols(DEFAULT_COLOR_MATCH_GRID_COLS);
            pipeline.set_color_match_grid_rows(DEFAULT_COLOR_MATCH_GRID_ROWS);
            pipeline.set_color_match_interval_frames(DEFAULT_COLOR_MATCH_INTERVAL_FRAMES);
            pipeline.set_color_match_ema_alpha(DEFAULT_COLOR_MATCH_EMA_ALPHA);
            pipeline.set_color_match_max_y_offset(DEFAULT_COLOR_MATCH_MAX_Y_OFFSET);
            pipeline.set_color_match_max_chroma_offset(DEFAULT_COLOR_MATCH_MAX_CHROMA_OFFSET);
            pipeline.set_color_gamma(DEFAULT_COLOR_GAMMA, DEFAULT_COLOR_GAMMA);
            pipeline.set_color_match_auto_gamma(DEFAULT_COLOR_MATCH_AUTO_GAMMA);
            self.preview_dirty = true;
        }
        if let Some(cal) = self.calibration.as_mut() {
            cal.topology.color_gamma_left = DEFAULT_COLOR_GAMMA;
            cal.topology.color_gamma_right = DEFAULT_COLOR_GAMMA;
            cal.topology.color_match_auto_gamma = DEFAULT_COLOR_MATCH_AUTO_GAMMA;
        }
    }

    fn set_rig_tilt(&mut self, deg: f32) {
        if let Some(cal) = self.calibration.as_mut() {
            cal.framing.tilt = (deg as f64).to_radians();
        }
        if let Some(bridge) = self.bridge.as_mut() {
            bridge.engine_mut().set_rig_tilt(deg.to_radians());
            self.preview_dirty = true;
        }
        // See `apply_layout`'s comment - no auto-clamp on a calibration edit.
    }

    fn set_sync_offset(&mut self, frames: i32) {
        let offset = frames as i64;
        let total = self.playback.total_frames().unwrap_or(u64::MAX) as i64;
        if offset.unsigned_abs() as i64 >= total {
            log::warn!("Sync offset {offset} exceeds video length ({total} frames), ignoring");
            return;
        }
        if let Some(cal) = self.calibration.as_mut() {
            cal.sync_offset = offset;
        }
        if let (Some(left), Some(right)) = (&self.left_input, &self.right_input) {
            let left = left.clone();
            let right = right.clone();
            let resume_frame = self.playback.frame_index();
            if let Err(e) = self.playback.open(&left, &right, offset) {
                log::error!("Failed to reopen playback with sync offset {offset}: {e}");
                return;
            }
            // `open()` always resets to frame 0 (it builds a fresh decode
            // pipeline to apply the new frame-skip alignment) - restore
            // the playhead so changing the sync offset doesn't jerk the
            // user back to the start of the clip.
            if let Some(total) = self.playback.total_frames().filter(|&t| t > 0) {
                let fraction = resume_frame.min(total - 1) as f32 / total as f32;
                if let Err(e) = self.playback.seek(fraction) {
                    log::error!("Failed to restore playhead after sync offset change: {e}");
                }
            }
            log::info!("Sync offset changed to {offset} frames");
            self.preview_dirty = true;
            // Force the audio-sync waveform to recompute against the new
            // offset instead of showing the stale pre-change envelope.
            self.audio_envelope_center_secs = f64::NEG_INFINITY;
        }
    }

    /// Update the audio-sync waveform's window width (frames), clamped to
    /// `AUDIO_WAVEFORM_WINDOW_FRAMES_RANGE`, and force the next tick's
    /// `maybe_recompute_audio_envelope` to recompute against it instead of
    /// showing the stale pre-change envelope.
    fn set_audio_window_frames(&mut self, frames: f32) {
        self.audio_window_frames = (frames as f64).clamp(
            AUDIO_WAVEFORM_WINDOW_FRAMES_RANGE.0,
            AUDIO_WAVEFORM_WINDOW_FRAMES_RANGE.1,
        );
        self.audio_envelope_center_secs = f64::NEG_INFINITY;
    }

    /// Poll for a completed audio-sync waveform envelope and, if the
    /// playhead has moved far enough since the last one, trigger a new
    /// background recompute. Called from the playback timer tick; cheap
    /// when idle (a few field reads), since the actual `ffmpeg`
    /// extraction only runs on a background thread when genuinely
    /// warranted.
    fn maybe_recompute_audio_envelope(&mut self, app_weak: &slint::Weak<RecoApp>) {
        if let Some(rx) = &self.audio_envelope_rx
            && let Ok((left, right)) = rx.try_recv()
        {
            self.audio_envelope_rx = None;
            self.audio_envelope_left = left.clone();
            self.audio_envelope_right = right.clone();
            if let Some(app) = app_weak.upgrade() {
                app.set_audio_envelope_left(slint::ModelRc::new(slint::VecModel::from(left)));
                app.set_audio_envelope_right(slint::ModelRc::new(slint::VecModel::from(right)));
            }
            return;
        }

        if self.audio_envelope_rx.is_some() {
            return; // job already in flight
        }

        let Some(app) = app_weak.upgrade() else {
            return;
        };
        if !app.get_audio_sync_expanded() {
            return;
        }
        let (Some(left_path), Some(right_path)) = (self.left_path.clone(), self.right_path.clone())
        else {
            return;
        };
        let fps = self.playback.fps();
        if fps <= 0.0 {
            return;
        }

        let window_secs = self.audio_window_frames / fps;
        let recenter_secs = AUDIO_WAVEFORM_RECENTER_FRAMES / fps;
        let center_secs = self.playback.frame_index() as f64 / fps;
        let moved_enough = (center_secs - self.audio_envelope_center_secs).abs() >= recenter_secs;
        let throttled = self
            .audio_envelope_triggered_at
            .is_some_and(|t| t.elapsed() < Duration::from_millis(AUDIO_WAVEFORM_THROTTLE_MS));
        if !moved_enough || throttled {
            return;
        }

        self.audio_envelope_center_secs = center_secs;
        self.audio_envelope_triggered_at = Some(Instant::now());

        // `playback.frame_index()` is a synced-timeline index: frame k of
        // the synced stream is raw left frame `k + max(-sync_offset, 0)`
        // and raw right frame `k + max(sync_offset, 0)` (see
        // `adapters::spawn_decode_pipeline_from_inputs`, which skips
        // frames from whichever camera started first). Extracting both
        // channels' audio windows at the same raw `center_secs` - as if
        // `sync_offset` were always 0 - would make the waveform blind to
        // the very thing it exists to verify: a correct offset should
        // pull the two traces' transients into alignment, and a wrong one
        // should show a residual shift.
        let sync_offset = self.calibration.as_ref().map_or(0, |c| c.sync_offset);
        let left_center_secs = center_secs + (-sync_offset).max(0) as f64 / fps;
        let right_center_secs = center_secs + sync_offset.max(0) as f64 / fps;

        let (tx, rx) = std::sync::mpsc::channel();
        self.audio_envelope_rx = Some(rx);
        std::thread::spawn(move || {
            let mut left = crate::waveform::extract_window_envelope(
                &left_path,
                left_center_secs,
                window_secs,
                AUDIO_WAVEFORM_BUCKETS,
            )
            .unwrap_or_default();
            let mut right = crate::waveform::extract_window_envelope(
                &right_path,
                right_center_secs,
                window_secs,
                AUDIO_WAVEFORM_BUCKETS,
            )
            .unwrap_or_default();
            crate::waveform::normalize_pair_to_peak(&mut left, &mut right);
            let _ = tx.send((left, right));
        });
    }

    /// Re-open the playback source from the current chained inputs without
    /// rebuilding the GPU pipeline. Used after a segment reorder: the
    /// calibration and bridge are unchanged, only the decode order differs,
    /// so the next render picks up the new order from the start.
    fn reopen_source(&mut self) {
        // Nothing consumes the source until a preview pipeline exists (e.g.
        // before calibration), so skip the costly decoder open. try_init opens
        // it in the current order once calibration is loaded.
        if self.bridge.is_none() {
            return;
        }
        let offset = self
            .calibration
            .as_ref()
            .map(|c| c.sync_offset)
            .unwrap_or(0);
        if let (Some(left), Some(right)) = (&self.left_input, &self.right_input) {
            let left = left.clone();
            let right = right.clone();
            if let Err(e) = self.playback.open(&left, &right, offset) {
                log::error!("Failed to reopen playback after reorder: {e}");
                return;
            }
            self.preview_dirty = true;
        }
    }

    fn set_rig_roll(&mut self, deg: f32) {
        if let Some(cal) = self.calibration.as_mut() {
            cal.framing.roll = (deg as f64).to_radians();
        }
        if let Some(bridge) = self.bridge.as_mut() {
            bridge.engine_mut().set_rig_roll(deg.to_radians());
            self.preview_dirty = true;
        }
        // See `apply_layout`'s comment - no auto-clamp on a calibration edit.
    }

    /// Reset yaw/pitch/fov targets to the rest pose. Routes through
    /// the translator so the same intent path works for both Slint
    /// callbacks and future remote transports.
    fn reset_view(&mut self) {
        self.pose
            .apply_intent(ControlIntent::Pose(PoseIntent::Reset));
    }

    /// Clamp the pose through the coverage boundary so pan input
    /// cannot set an unreachable goal. Delegates to
    /// `PoseControl::clamp_via_coverage`.
    fn clamp_targets(&mut self) {
        if !self.use_constrained_look {
            return;
        }
        let Some(bridge) = self.bridge.as_ref() else {
            return;
        };
        let renderer = bridge.engine();
        let (vw, vh) = bridge.viewport_size();
        let aspect = vw as f32 / vh as f32;
        if let Some(coverage) = renderer.coverage() {
            self.pose.clamp_via_coverage(coverage, aspect);
        }
    }

    /// Seek by a relative number of seconds (positive = forward).
    fn seek_relative(&mut self, secs: f32) -> Result<(), String> {
        let fps = self.playback.fps();
        let total = self.playback.total_frames().unwrap_or(0).max(1);
        if fps <= 0.0 || total == 0 {
            return Ok(());
        }
        let current = self.playback.frame_index() as i64;
        let delta_frames = (secs as f64 * fps) as i64;
        let target = (current + delta_frames).clamp(0, total as i64 - 1) as u64;
        let fraction = target as f32 / total as f32;
        self.playback.seek(fraction).map_err(|e| format!("{e}"))
    }
}

/// Extract just the filename from a path for display.
/// Seed the Slint lens-tune sliders and their display ranges from a
/// pair of baseline `Lens`. Called after auto-calibrate completes
/// and on Reset Lens. Ranges are chosen wide enough for meaningful
/// manual tuning (fx/fy: +/-15%, cx/cy: +/-10% of image dim) but tight
/// enough that the slider granularity is useful.
fn set_lens_sliders(
    app: &RecoApp,
    left: &reco_core::calibration::Lens,
    right: &reco_core::calibration::Lens,
) {
    // Ranges are computed from the left camera's baseline. In stereo
    // rigs the two lenses are typically matched models, so a single
    // range keeps the UI simpler. If the cameras ever differ materially
    // this can be revisited.
    let f_baseline = left.fx.max(left.fy);
    let fx_span = (f_baseline * 0.15).max(5.0);
    let w = left.width.max(1) as f64;
    let h = left.height.max(1) as f64;
    let cx_span = (w * 0.10).max(5.0);
    let cy_span = (h * 0.10).max(5.0);

    app.set_lens_fx_min((left.fx - fx_span) as f32);
    app.set_lens_fx_max((left.fx + fx_span) as f32);
    app.set_lens_fy_min((left.fy - fx_span) as f32);
    app.set_lens_fy_max((left.fy + fx_span) as f32);
    app.set_lens_cx_min((left.cx - cx_span) as f32);
    app.set_lens_cx_max((left.cx + cx_span) as f32);
    app.set_lens_cy_min((left.cy - cy_span) as f32);
    app.set_lens_cy_max((left.cy + cy_span) as f32);
    app.set_lens_k_range(0.3);

    app.set_lens_left_fx(left.fx as f32);
    app.set_lens_left_fy(left.fy as f32);
    app.set_lens_left_cx(left.cx as f32);
    app.set_lens_left_cy(left.cy as f32);
    app.set_lens_left_k1(left.distortion[0] as f32);
    app.set_lens_left_k2(left.distortion[1] as f32);
    app.set_lens_left_k3(left.distortion[2] as f32);
    app.set_lens_left_k4(left.distortion[3] as f32);

    app.set_lens_right_fx(right.fx as f32);
    app.set_lens_right_fy(right.fy as f32);
    app.set_lens_right_cx(right.cx as f32);
    app.set_lens_right_cy(right.cy as f32);
    app.set_lens_right_k1(right.distortion[0] as f32);
    app.set_lens_right_k2(right.distortion[1] as f32);
    app.set_lens_right_k3(right.distortion[2] as f32);
    app.set_lens_right_k4(right.distortion[3] as f32);
}

/// Human-readable description of how a lens profile was resolved.
fn profile_source_label(info: &LensProfileInfo) -> &'static str {
    match info.source {
        ProfileSource::AutoDetected => "Auto-detected",
        ProfileSource::Database => "Database match",
        ProfileSource::File(_) => "File",
        ProfileSource::Fallback => "Fallback",
    }
}

/// Populate the Slint lens-profile properties from calibration output.
///
/// Stamps the detected camera/lens/source for left and right, plus the
/// count of alternate profiles in the embedded database that match the
/// current video resolution (`in_w` x `in_h`). The candidate count lets
/// the user tell at a glance whether they could reasonably override the
/// auto-detected profile - zero means the picker has nothing new to offer.
fn set_lens_profile_props(
    app: &RecoApp,
    left: Option<LensProfileInfo>,
    right: Option<LensProfileInfo>,
    in_w: u32,
    in_h: u32,
) {
    if let Some(info) = &left {
        app.set_lens_left_camera(info.camera.clone().into());
        app.set_lens_left_lens(info.lens.clone().into());
        app.set_lens_left_source(profile_source_label(info).into());
    } else {
        app.set_lens_left_camera("Unknown".into());
        app.set_lens_left_lens("".into());
        app.set_lens_left_source("Not detected".into());
    }
    if let Some(info) = &right {
        app.set_lens_right_camera(info.camera.clone().into());
        app.set_lens_right_lens(info.lens.clone().into());
        app.set_lens_right_source(profile_source_label(info).into());
    } else {
        app.set_lens_right_camera("Unknown".into());
        app.set_lens_right_lens("".into());
        app.set_lens_right_source("Not detected".into());
    }

    // Count candidate profiles for the current resolution so the user
    // sees whether alternates exist. Loading the embedded database is
    // O(1) after the first call (static OnceCell inside reco-calibrate).
    let candidates = if in_w > 0 && in_h > 0 {
        let db = reco_calibrate::lens_database::LensDatabase::load_embedded();
        db.candidates(in_w, in_h).len() as i32
    } else {
        0
    };
    app.set_lens_candidates_count(candidates);
    app.set_lens_info_available(left.is_some() || right.is_some());
}

fn format_time(frame: u64, fps: f64) -> String {
    if fps <= 0.0 {
        return "00:00:00".into();
    }
    let total_secs = (frame as f64 / fps) as u64;
    let h = total_secs / 3600;
    let m = (total_secs % 3600) / 60;
    let s = total_secs % 60;
    format!("{h:02}:{m:02}:{s:02}")
}

fn sync_frame_display(app: &RecoApp, frame: u64, total: u64, fps: f64) {
    app.set_current_frame(frame as i32);
    app.set_total_frames(total as i32);
    app.set_current_time_text(format_time(frame, fps).into());
    app.set_total_time_text(format_time(total, fps).into());
}

fn display_name(path: &std::path::Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string())
}

/// Suggest an export output path: inside the match folder, named after
/// it, when the current selection came from "Select Match Folder" (see
/// `AppState::match_folder`); otherwise next to the left video file, as
/// before that feature existed. `None` only when neither is available
/// (no video loaded yet).
fn suggested_export_path(
    match_folder: Option<&std::path::Path>,
    left_path: Option<&std::path::Path>,
) -> Option<PathBuf> {
    if let Some(folder) = match_folder {
        let name = folder
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "match".into());
        return Some(folder.join(format!("{name}.mp4")));
    }
    let left_path = left_path?;
    let candidate = left_path.with_file_name(format!(
        "{}.mp4",
        left_path
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| "reco".into())
    ));
    // A source clip whose extension is already exactly (Windows
    // filenames are case-insensitive, so this includes e.g. `.MP4` -
    // the common DJI/most-camera case) `.mp4` would otherwise suggest
    // an output path that's the *same file* as the raw left camera
    // footage - starting that export would silently overwrite the
    // source. Fall back to a `_stitched` suffix (the old, always-safe
    // behavior) only in that case, so a source with any other
    // extension (`.mov`, `.MOV`, ...) still gets the plain name.
    let collides_with_source = candidate
        .file_name()
        .zip(left_path.file_name())
        .is_some_and(|(a, b)| a.eq_ignore_ascii_case(b));
    if collides_with_source {
        return Some(left_path.with_file_name(format!(
            "{}_stitched.mp4",
            left_path
                .file_stem()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_else(|| "reco".into())
        )));
    }
    Some(candidate)
}

/// Default calibration save location for a fresh Auto-Calibrate with no
/// location known yet (no match folder picked, no calibration ever
/// loaded or saved this session). Recordings follow the fixed
/// `<match>/Left/`, `<match>/Right/` convention (see the `match_folder`
/// module doc) - when the left video sits directly inside a folder
/// literally named "left" (case-insensitive), save one level up, in the
/// match folder that also holds Left/ and Right/, instead of inside
/// Left/ itself. Falls back to the left video's own folder when that
/// convention doesn't apply (e.g. two loose files with no Left/Right
/// structure).
fn suggested_calibration_path(left_path: Option<&std::path::Path>) -> Option<PathBuf> {
    let left = left_path?;
    let left_dir = left.parent()?;
    let base_dir = if left_dir
        .file_name()
        .is_some_and(|n| n.eq_ignore_ascii_case("left"))
    {
        left_dir.parent().unwrap_or(left_dir)
    } else {
        left_dir
    };
    let stem = left
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "reco".into());
    Some(base_dir.join(format!("{stem}_calibration.json")))
}

/// Build the `InputPath` for a camera slot from freshly picked file(s),
/// optionally appending to an already-selected chain (multi-segment
/// recordings, e.g. DJI 4GB splits, get picked a few files at a time).
/// `label` is only used for the segment-count log message ("Left"/"Right").
fn input_path_from_picks(
    paths: Vec<PathBuf>,
    existing: Option<&reco_io::stitch_job::InputPath>,
    label: &str,
) -> reco_io::stitch_job::InputPath {
    match existing {
        Some(existing) => {
            let mut all = existing.all_paths();
            all.extend(paths);
            log::info!("{label}: appended to {} total segments", all.len());
            reco_io::stitch_job::InputPath::Chained(all)
        }
        None => {
            if paths.len() == 1 {
                reco_io::stitch_job::InputPath::Single(paths.into_iter().next().unwrap())
            } else {
                log::info!(
                    "{label}: {} segments selected, chaining via concat demuxer",
                    paths.len()
                );
                reco_io::stitch_job::InputPath::Chained(paths)
            }
        }
    }
}

/// Convert an annotated AKAZE detection preview (see
/// `reco_calibrate::preview`) into a Slint image for display.
fn detection_preview_to_slint_image(
    preview: &reco_calibrate::preview::DetectionPreview,
) -> slint::Image {
    let mut buffer =
        slint::SharedPixelBuffer::<slint::Rgba8Pixel>::new(preview.width, preview.height);
    buffer.make_mut_bytes().copy_from_slice(&preview.rgba);
    slint::Image::from_rgba8(buffer)
}

/// Push the current MRU lists into the Slint properties that back the
/// Recent-files dialog. Called at startup and after every file pick.
fn sync_recent_paths(settings: &settings::GuiSettings, app: &RecoApp) {
    fn to_model(paths: &[std::path::PathBuf]) -> slint::ModelRc<slint::SharedString> {
        let v: Vec<slint::SharedString> = paths
            .iter()
            .map(|p| slint::SharedString::from(p.to_string_lossy().as_ref()))
            .collect();
        slint::ModelRc::new(slint::VecModel::from(v))
    }
    app.set_recent_left_paths(to_model(settings.recent_left.entries()));
    app.set_recent_right_paths(to_model(settings.recent_right.entries()));
    app.set_recent_calibration_paths(to_model(settings.recent_calibration.entries()));
}

/// Push `state.cut_ranges` (the authoritative Rust-side list) into the
/// Slint `cut-ranges` property. Called after every add/remove/update so
/// the scrubber bands and the numeric list both reflect committed state -
/// a drag in progress uses its own local preview and doesn't call this
/// until release (see `CutRangeItem`'s doc comment in main.slint).
fn sync_cut_ranges(state: &AppState, app: &RecoApp) {
    let items: Vec<CutRangeItem> = state
        .cut_ranges
        .iter()
        .map(|&(start, end)| CutRangeItem {
            start_secs: start as f32,
            end_secs: end as f32,
        })
        .collect();
    app.set_cut_ranges(slint::ModelRc::new(slint::VecModel::from(items)));
}

/// Recompute and re-apply the "Auto-cut kickoff lead-in + pauses" derived
/// cut ranges from the current Match Logger export + sync anchor,
/// replacing whatever this toggle previously added (see
/// `AppState::scoreboard_derived_cut_ranges`'s doc comment) without
/// touching any manually added/edited range.
///
/// Self-guarding: with both derive toggles off this only *removes* what
/// it previously added, which is exactly what every caller wants after
/// the export, the anchor, or a margin changed. Callers therefore do not
/// check the toggles themselves - they used to, in five places, and a
/// sixth condition would have had to be remembered in each of them the
/// moment a second derivation mode existed.
///
/// The two modes are mutually exclusive by construction: highlights
/// already excludes everything that is not a goal, so layering the
/// pause cuts underneath it could only ever subtract from a clip that
/// was deliberately chosen. Highlights wins if both are somehow set.
fn refresh_derived_cut_ranges(s: &mut AppState, app: &RecoApp) {
    let previous = std::mem::take(&mut s.scoreboard_derived_cut_ranges);
    if !previous.is_empty() {
        s.cut_ranges.retain(|r| !previous.contains(r));
    }
    // Same "only touch what we own" idea for export-start-secs (see
    // `scoreboard_derived_start_secs`'s doc comment) - only reset it if
    // it still holds exactly what we last suggested, so a manual edit
    // since then survives toggling this off.
    if let Some(previous_start) = s.scoreboard_derived_start_secs.take()
        && app.get_export_start_secs() == previous_start
    {
        app.set_export_start_secs(0.0);
    }
    // Same again for export-end-secs (see
    // `scoreboard_derived_end_secs`'s doc comment).
    if let Some(previous_end) = s.scoreboard_derived_end_secs.take()
        && app.get_export_end_secs() == previous_end
    {
        app.set_export_end_secs(0.0);
    }
    // And for the "_highlights" output filename.
    if let Some(previous_output) = s.scoreboard_derived_output_path.take()
        && app.get_export_output_path() == previous_output.as_str()
    {
        app.set_export_output_path(strip_highlights_suffix(&previous_output).into());
    }

    let (Some(export), Some(anchor)) = (
        s.scoreboard_import.as_ref(),
        s.scoreboard_sync_anchor.as_ref(),
    ) else {
        sync_cut_ranges(s, app);
        return;
    };

    if app.get_scoreboard_derive_highlights() {
        let lead = app.get_highlights_lead_secs().max(0.0) as f64;
        let trail = app.get_highlights_trail_secs().max(0.0) as f64;
        let windows = reco_io::cut_range::merge_windows(scoreboard_import::derived_goal_windows(
            export, anchor, lead, trail,
        ));
        // No goals logged means no reel. Leaving the export untouched is
        // the honest outcome - silently exporting the whole match under
        // a "_highlights" name would be worse than doing nothing.
        if let (Some(first), Some(last)) = (windows.first(), windows.last()) {
            let derived = reco_io::cut_range::gaps_between(&windows);
            s.cut_ranges.extend(derived.iter().copied());
            s.scoreboard_derived_cut_ranges = derived;

            // The reel's outer edges are start/end times, not cuts - same
            // reasoning as the pre-roll trim below.
            let start_secs = first.0 as f32;
            app.set_export_start_secs(start_secs);
            s.scoreboard_derived_start_secs = Some(start_secs);
            let end_secs = last.1 as f32;
            app.set_export_end_secs(end_secs);
            s.scoreboard_derived_end_secs = Some(end_secs);

            // Rename the output so a two-minute reel cannot land on top
            // of the full match export, which on this project is a
            // multi-gigabyte file that took an hour to produce. Done here
            // rather than at export time so the user can see and edit it.
            let output = app.get_export_output_path().to_string();
            if !output.is_empty() {
                let renamed = add_highlights_suffix(&output);
                app.set_export_output_path(renamed.clone().into());
                s.scoreboard_derived_output_path = Some(renamed);
            }
        }
        sync_cut_ranges(s, app);
        return;
    }

    if app.get_scoreboard_derive_cut_ranges() {
        // Margins are user-settable (the four NumEdits under the
        // auto-cut checkbox); they were a fixed 2.0 everywhere before.
        let cut_lead = app.get_autocut_cut_lead_secs().max(0.0) as f64;
        let cut_trail = app.get_autocut_cut_trail_secs().max(0.0) as f64;
        let kickoff_lead = app.get_autocut_kickoff_lead_secs().max(0.0) as f64;
        let match_end_trail = app.get_autocut_match_end_trail_secs().max(0.0) as f64;

        let derived = scoreboard_import::derived_cut_ranges(export, anchor, cut_lead, cut_trail);
        s.cut_ranges.extend(derived.iter().copied());
        s.scoreboard_derived_cut_ranges = derived;

        // Pre-roll trim is a --start-time seek, not a cut range - see
        // `scoreboard_import::derived_start_secs`'s doc comment for why
        // (also fixes a real bug: a cut range starting at the export's
        // own start wasn't actually being skipped).
        if let Some(start_secs) =
            scoreboard_import::derived_start_secs(export, anchor, kickoff_lead)
        {
            let start_secs = start_secs as f32;
            app.set_export_start_secs(start_secs);
            s.scoreboard_derived_start_secs = Some(start_secs);
        }
        // Post-match trim (the "signal einde wedstrijd" - a --end-time
        // seek, not a cut range, same reasoning as the pre-roll trim
        // above) - only when the log actually has a `match_end` event.
        if let Some(end_secs) = scoreboard_import::derived_end_secs(export, anchor, match_end_trail)
        {
            let end_secs = end_secs as f32;
            app.set_export_end_secs(end_secs);
            s.scoreboard_derived_end_secs = Some(end_secs);
        }
    }
    sync_cut_ranges(s, app);
}

/// Marker inserted into a highlights export's filename.
const HIGHLIGHTS_SUFFIX: &str = "_highlights";

/// `match.mp4` -> `match_highlights.mp4`, idempotent.
///
/// Operates on the string rather than a `PathBuf` because that is what
/// the UI property holds, and round-tripping through a path would
/// normalise separators the user typed.
fn add_highlights_suffix(output: &str) -> String {
    let (stem, extension) = match output.rfind('.') {
        // A dot in a directory name is not an extension.
        Some(dot) if !output[dot..].contains(['/', '\\']) => output.split_at(dot),
        _ => (output, ""),
    };
    if stem.ends_with(HIGHLIGHTS_SUFFIX) {
        return output.to_string();
    }
    format!("{stem}{HIGHLIGHTS_SUFFIX}{extension}")
}

/// Inverse of [`add_highlights_suffix`], for reverting a path this
/// suggested. Leaves anything it did not produce alone.
fn strip_highlights_suffix(output: &str) -> String {
    let (stem, extension) = match output.rfind('.') {
        Some(dot) if !output[dot..].contains(['/', '\\']) => output.split_at(dot),
        _ => (output, ""),
    };
    match stem.strip_suffix(HIGHLIGHTS_SUFFIX) {
        Some(base) => format!("{base}{extension}"),
        None => output.to_string(),
    }
}

/// Push the per-side segment filenames into the Slint left/right-segments
/// models so the Files panel shows what was imported.
fn sync_segments(state: &AppState, app: &RecoApp) {
    fn input_to_names(input: &Option<reco_io::stitch_job::InputPath>) -> Vec<slint::SharedString> {
        match input {
            Some(reco_io::stitch_job::InputPath::Single(p)) => {
                vec![display_name(p).into()]
            }
            Some(reco_io::stitch_job::InputPath::Chained(ps)) => {
                ps.iter().map(|p| display_name(p).into()).collect()
            }
            None => vec![],
        }
    }
    app.set_left_segments(slint::ModelRc::new(slint::VecModel::from(input_to_names(
        &state.left_input,
    ))));
    app.set_right_segments(slint::ModelRc::new(slint::VecModel::from(input_to_names(
        &state.right_input,
    ))));
}

fn sync_roi_points(state: &AppState, app: &RecoApp) {
    let (xs, ys) = if let Some(cal) = &state.calibration
        && let Some(roi) = &cal.field_roi
    {
        let is_right = state.lens_preview_side == "right";
        let side = if is_right { &roi.right } else { &roi.left };
        let lens = &cal.lenses[if is_right { 1 } else { 0 }];
        let display: Vec<[f64; 2]> = side
            .iter()
            .map(|p| raw_norm_to_rectified_norm(p[0], p[1], lens))
            .collect();
        let xs: Vec<f32> = display.iter().map(|p| p[0] as f32).collect();
        let ys: Vec<f32> = display.iter().map(|p| p[1] as f32).collect();
        (xs, ys)
    } else {
        (vec![], vec![])
    };
    let aspect = app.get_lens_frame_aspect();
    app.set_roi_path_commands(roi_path_commands(&xs, &ys, aspect).into());
    app.set_roi_points_x(slint::ModelRc::new(slint::VecModel::from(xs)));
    app.set_roi_points_y(slint::ModelRc::new(slint::VecModel::from(ys)));
}

/// SVG path `commands` string closing the polygon through `(xs, ys)`
/// (normalized `[0,1]`), for the ROI outline drawn alongside the vertex
/// dots. Empty/single-point polygons draw nothing (a line needs at least
/// two points).
///
/// `y` is divided by `aspect` (the camera's native width/height ratio) to
/// match the `Path` element's `viewbox-height: 1.0 / lens-frame-aspect`
/// in `main.slint` - the `Path` item always scales its viewbox into its
/// box uniformly (aspect-preserving), so a plain 0..1 y would land at a
/// different pixel than the vertex dots (which map y to `content-h`
/// independently of x/`content-w`). Giving the viewbox the box's own
/// aspect ratio and pre-scaling y here the same way makes the two match.
fn roi_path_commands(xs: &[f32], ys: &[f32], aspect: f32) -> String {
    if xs.len() < 2 {
        return String::new();
    }
    let mut s = String::new();
    for (i, (x, y)) in xs.iter().zip(ys).enumerate() {
        let y = y / aspect;
        if i == 0 {
            s.push_str(&format!("M {x} {y} "));
        } else {
            s.push_str(&format!("L {x} {y} "));
        }
    }
    s.push('Z');
    s
}

/// Convert a point in the ROI editor's displayed preview (normalized
/// `[0,1]`, rectified/undistorted - the lens-correction render always
/// shown while editing) into the raw distorted-frame normalized `[0,1]`
/// space `field_roi` is stored in and the AI detector consumes. Without
/// this, a boundary traced on the rectified preview gets saved as if it
/// were already in raw-frame coordinates - the two spaces are related by
/// the lens's own KB4 distortion (strongly non-linear, especially near
/// the frame edges), so the saved polygon silently drifts away from the
/// pitch it was drawn around and the detector rejects real in-bounds
/// detections. `lens` must be the calibration's lens for whichever
/// camera side is currently being edited. See
/// `reco_core::lens::undistorted_to_distorted`'s doc comment for the
/// underlying math (mirrors `fisheye.wgsl`'s fragment shader exactly).
fn rectified_norm_to_raw_norm(nx: f64, ny: f64, lens: &reco_core::calibration::Lens) -> [f64; 2] {
    let (w, h) = (lens.width, lens.height);
    let (raw_x, raw_y) =
        reco_core::lens::undistorted_to_distorted(nx * w as f64, ny * h as f64, w, h, lens);
    [
        (raw_x / w as f64).clamp(0.0, 1.0),
        (raw_y / h as f64).clamp(0.0, 1.0),
    ]
}

/// Inverse of [`rectified_norm_to_raw_norm`] - convert a stored
/// `field_roi` point (raw distorted-frame normalized) into the ROI
/// editor's displayed (rectified) preview normalized space, so the
/// overlay dots/outline and drag hit-testing land at their true
/// on-screen position instead of wherever the raw fraction happens to
/// fall in the visually very different rectified image. Falls back to
/// the raw point unchanged if Newton-Raphson doesn't converge (only
/// happens very close to the frame's extreme corners - see
/// `reco_core::lens::distorted_to_undistorted`'s doc comment) so a point
/// out there stays visible/grabbable near its true position instead of
/// vanishing.
fn raw_norm_to_rectified_norm(nx: f64, ny: f64, lens: &reco_core::calibration::Lens) -> [f64; 2] {
    let (w, h) = (lens.width, lens.height);
    match reco_core::lens::distorted_to_undistorted(nx * w as f64, ny * h as f64, w, h, lens) {
        Some((ux, uy)) => [
            (ux / w as f64).clamp(0.0, 1.0),
            (uy / h as f64).clamp(0.0, 1.0),
        ],
        None => [nx, ny],
    }
}

/// Hit-test radius (pixels, in the lens-preview content rect) for
/// grabbing an existing ROI point to drag or delete.
const ROI_HIT_RADIUS_PX: f32 = 10.0;

/// Nearest ROI point to `(lx, ly)` within [`ROI_HIT_RADIUS_PX`], if any.
/// `pts` are normalized `[0,1]`; `(cw, ch)` is the content rect size in
/// pixels the click coordinates are already relative to.
fn roi_hit_test(pts: &[[f64; 2]], lx: f32, ly: f32, cw: f32, ch: f32) -> Option<usize> {
    pts.iter()
        .enumerate()
        .map(|(i, p)| {
            let dx = p[0] as f32 * cw - lx;
            let dy = p[1] as f32 * ch - ly;
            (i, (dx * dx + dy * dy).sqrt())
        })
        .filter(|(_, d)| *d <= ROI_HIT_RADIUS_PX)
        .min_by(|a, b| a.1.total_cmp(&b.1))
        .map(|(i, _)| i)
}

/// Reconstruct Slint's `image-fit: contain` letterbox rect for the
/// panorama preview (outside lens-preview mode, where - unlike lens
/// preview - the displayed `Image` is sized to the box directly, so
/// there's no `preview-box.content-*`-equivalent property to read).
/// Used by the seam-line hit test; the goal-geometry editor does not need
/// this since it lives in the lens-preview window, not the panorama one.
fn panorama_letterbox_rect(box_w: f32, box_h: f32, output_aspect: f32) -> (f32, f32, f32, f32) {
    let box_aspect = box_w / box_h;
    let (content_w, content_h) = if box_aspect > output_aspect {
        (box_h * output_aspect, box_h)
    } else {
        (box_w, box_w / output_aspect)
    };
    let content_x = (box_w - content_w) / 2.0;
    let content_y = (box_h - content_h) / 2.0;
    (content_w, content_h, content_x, content_y)
}

/// Push the currently-selected goal's polygon (`state.lens_preview_side`,
/// shared with the ROI editor) to the Slint overlay - same raw-distorted-
/// to-rectified conversion as `sync_roi_points`, just reading
/// `cal.goal_geometry` instead of `cal.field_roi`.
fn sync_goal_points(state: &AppState, app: &RecoApp) {
    let (xs, ys) = if let Some(cal) = &state.calibration
        && let Some(goal) = &cal.goal_geometry
    {
        let is_right = state.lens_preview_side == "right";
        let side = if is_right { &goal.right } else { &goal.left };
        let lens = &cal.lenses[if is_right { 1 } else { 0 }];
        let display: Vec<[f64; 2]> = side
            .iter()
            .map(|p| raw_norm_to_rectified_norm(p[0], p[1], lens))
            .collect();
        let xs: Vec<f32> = display.iter().map(|p| p[0] as f32).collect();
        let ys: Vec<f32> = display.iter().map(|p| p[1] as f32).collect();
        (xs, ys)
    } else {
        (vec![], vec![])
    };
    let aspect = app.get_lens_frame_aspect();
    app.set_goal_path_commands(roi_path_commands(&xs, &ys, aspect).into());
    app.set_goal_points_x(slint::ModelRc::new(slint::VecModel::from(xs)));
    app.set_goal_points_y(slint::ModelRc::new(slint::VecModel::from(ys)));
}

/// Push whether `cal.autocam_pitch_limits` has each bound *set at all*
/// to the Slint status text/Clear button in the DETECTION ZONES card.
/// Cheap, calibration-only - independent of the live preview pose, so
/// this is safe to call on calibration load/change alone. The other
/// half (where the lines currently sit ON SCREEN, which depends on the
/// live camera pose) is [`sync_pitch_limit_overlay`], called every
/// vsync tick instead.
fn sync_pitch_limit_status(state: &AppState, app: &RecoApp) {
    let limits = state
        .calibration
        .as_ref()
        .and_then(|c| c.autocam_pitch_limits);
    app.set_has_pitch_limit_top_set(limits.is_some_and(|l| l.top_rad.is_some()));
    app.set_has_pitch_limit_bottom_set(limits.is_some_and(|l| l.bottom_rad.is_some()));
}

/// Everything needed to project between the stitched preview's screen
/// space and world-space pitch: the current camera basis/position, rig
/// tilt/roll, the pipeline's live FOV/aspect, and the current viewport
/// pose (world-space yaw/pitch `PoseControl` is tracking). `None` when
/// no calibration or no active preview bridge is available yet (e.g.
/// before files are loaded) - every pitch-limit callback treats that
/// as a no-op, matching `on_seam_hit_test`'s same early-return shape.
struct PitchLimitProjection {
    cam: reco_core::geometry::VirtualCamera,
    position: [f32; 3],
    fov_v_deg: f32,
    aspect: f32,
    rig_tilt: f32,
    rig_roll: f32,
    pose: ViewportPosition,
}

impl PitchLimitProjection {
    /// Bundle into the form `reco_core::geometry`'s screen<->world
    /// conversions actually take (see `ScreenProjection`'s own doc for
    /// why it exists as a separate type from this GUI-side struct: this
    /// one also carries `pose` before it's split into yaw/pitch here).
    fn as_screen_projection(&self) -> reco_core::geometry::ScreenProjection<'_> {
        reco_core::geometry::ScreenProjection {
            cam: &self.cam,
            position: self.position,
            world_yaw: self.pose.yaw,
            world_pitch: self.pose.pitch,
            fov_v_deg: self.fov_v_deg,
            aspect: self.aspect,
            rig_tilt: self.rig_tilt,
            rig_roll: self.rig_roll,
        }
    }
}

fn pitch_limit_projection(state: &AppState) -> Option<PitchLimitProjection> {
    let bridge = state.bridge.as_ref()?;
    let pipeline = bridge.engine().pipeline();
    let cal = pipeline.calibration();
    let lens_aspect = cal.lenses[0].width as f32 / cal.lenses[0].height as f32;
    let scene =
        reco_core::render::scene::SceneGeometry::new(&cal.topology, &cal.framing, lens_aspect);
    let cam = reco_core::geometry::VirtualCamera::new(&scene.camera_position);
    Some(PitchLimitProjection {
        cam,
        position: scene.camera_position,
        fov_v_deg: pipeline.viewport().fov_degrees,
        aspect: pipeline.viewport().aspect_ratio(),
        rig_tilt: cal.framing.tilt as f32,
        rig_roll: cal.framing.roll as f32,
        pose: state.pose.current_pose(),
    })
}

/// Re-derive where the two AI pitch-limit lines currently sit on the
/// stitched preview, given the live camera pose - called every vsync
/// tick while `show_pitch_limits` is on (the screen position moves as
/// the user pans/zooms, unlike ROI/GOAL's fixed lens-preview overlay).
/// A bound with no value, or one that currently projects behind the
/// camera (`project_world_to_screen` returns `None`), is simply left
/// not-visible for this frame - not an error, just off-screen right now.
/// Default screen-Y fraction (`[0,1]`, top-left origin) for a
/// pitch-limit line before it has ever been set. Without this, a fresh
/// calibration (`autocam_pitch_limits: None`) draws nothing at all when
/// "Show AI pitch limits" is turned on - there would be no line
/// anywhere on screen to grab, so the user could never create the
/// first value by dragging. Once dragged, the real world-space value
/// takes over (see [`pitch_limit_display_y`]).
const PITCH_LIMIT_DEFAULT_TOP_Y: f32 = 0.15;
/// Mirror of [`PITCH_LIMIT_DEFAULT_TOP_Y`] for the bottom line.
const PITCH_LIMIT_DEFAULT_BOTTOM_Y: f32 = 0.85;

/// Current on-screen Y fraction (`[0,1]`, top-left origin) for one
/// pitch-limit line - `index` 0 = top, 1 = bottom. `Some` whenever the
/// line should be drawn/grabbable right now: either a real stored value
/// that currently projects on-screen, or - when nothing is stored yet -
/// a fixed default position so the user always has something to drag
/// to create the first value (see the constants above). `None` only
/// for a *stored* value that's currently panned/zoomed out of view -
/// that case genuinely has nothing sensible to draw, unlike the unset
/// case.
fn pitch_limit_display_y(
    limits: Option<reco_core::calibration::AutocamPitchLimits>,
    proj: Option<&PitchLimitProjection>,
    index: i32,
) -> Option<f32> {
    let rad = limits.and_then(|l| if index == 0 { l.top_rad } else { l.bottom_rad });
    let Some(rad) = rad else {
        return Some(if index == 0 {
            PITCH_LIMIT_DEFAULT_TOP_Y
        } else {
            PITCH_LIMIT_DEFAULT_BOTTOM_Y
        });
    };
    let p = proj?;
    let (_, sy) =
        reco_core::geometry::project_world_to_screen(&p.as_screen_projection(), p.pose.yaw, rad)?;
    Some(((1.0 - sy) / 2.0).clamp(0.0, 1.0))
}

fn sync_pitch_limit_overlay(state: &AppState, app: &RecoApp) {
    let limits = state
        .calibration
        .as_ref()
        .and_then(|c| c.autocam_pitch_limits);
    let proj = pitch_limit_projection(state);

    let top = pitch_limit_display_y(limits, proj.as_ref(), 0);
    app.set_pitch_limit_top_visible(top.is_some());
    if let Some(y) = top {
        app.set_pitch_limit_top_screen_y(y);
    }
    let bottom = pitch_limit_display_y(limits, proj.as_ref(), 1);
    app.set_pitch_limit_bottom_visible(bottom.is_some());
    if let Some(y) = bottom {
        app.set_pitch_limit_bottom_screen_y(y);
    }
}

/// Hit-test radius (pixels) for grabbing the seam debug line - see
/// `reco_core::render::renderer::seam_line_screen_points` for how its
/// on-screen position is computed.
const SEAM_LINE_HIT_RADIUS_PX: f32 = 8.0;

/// Shortest distance from `(px, py)` to the line segment `(ax,ay)-(bx,by)`,
/// clamped to the segment itself (not the infinite line through it).
fn point_to_segment_distance(px: f32, py: f32, ax: f32, ay: f32, bx: f32, by: f32) -> f32 {
    let (dx, dy) = (bx - ax, by - ay);
    let len_sq = dx * dx + dy * dy;
    let t = if len_sq > 1e-6 {
        (((px - ax) * dx + (py - ay) * dy) / len_sq).clamp(0.0, 1.0)
    } else {
        0.0
    };
    let (cx, cy) = (ax + t * dx, ay + t * dy);
    ((px - cx).powi(2) + (py - cy).powi(2)).sqrt()
}

/// Index at which to insert a newly-clicked point into an existing ROI
/// polygon so the boundary stays a simple (non-self-crossing) loop
/// regardless of click order, instead of always appending at the end -
/// which draws a straight line from the polygon's last point to
/// wherever the new point landed, producing a crossing/star-shaped
/// outline unless points happen to be added walking the perimeter in
/// order (confirmed - this is exactly what a user saw when adding a
/// point after deleting others). Classic "cheapest insertion" heuristic:
/// try inserting after each existing point (including the closing edge
/// back to the first), and pick whichever adds the least extra
/// perimeter length.
fn roi_insert_index(pts: &[[f64; 2]], new: [f64; 2]) -> usize {
    if pts.len() < 2 {
        return pts.len();
    }
    let dist = |a: [f64; 2], b: [f64; 2]| ((a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2)).sqrt();
    (0..pts.len())
        .map(|i| {
            let a = pts[i];
            let b = pts[(i + 1) % pts.len()];
            let added_length = dist(a, new) + dist(new, b) - dist(a, b);
            (i + 1, added_length)
        })
        .min_by(|x, y| x.1.total_cmp(&y.1))
        .map(|(idx, _)| idx)
        .unwrap_or(pts.len())
}

/// Ring buffer backing the in-app Debug panel. Capped so long sessions
/// don't grow memory unbounded; separate from (and much shorter than)
/// the on-disk log file used for bug reports (see `log_file_path`).
static DEBUG_LOG_BUFFER: std::sync::LazyLock<Mutex<VecDeque<String>>> =
    std::sync::LazyLock::new(|| Mutex::new(VecDeque::new()));

const DEBUG_LOG_CAPACITY: usize = 500;

/// Snapshot of the current ring-buffer contents, newest line last, joined
/// for display in the Debug panel's scrollable text area.
fn debug_log_snapshot() -> String {
    DEBUG_LOG_BUFFER
        .lock()
        .unwrap()
        .iter()
        .cloned()
        .collect::<Vec<_>>()
        .join("\n")
}

/// `tracing_subscriber::fmt::layer()` writer that appends formatted lines
/// to `DEBUG_LOG_BUFFER` instead of a file/stream. A bare fn (not a
/// closure) so it satisfies the `MakeWriter` blanket impl for `Fn() -> W`.
fn debug_log_writer() -> DebugLogWriter {
    DebugLogWriter
}

struct DebugLogWriter;

impl std::io::Write for DebugLogWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let mut log = DEBUG_LOG_BUFFER.lock().unwrap();
        for line in String::from_utf8_lossy(buf).lines() {
            if log.len() >= DEBUG_LOG_CAPACITY {
                log.pop_front();
            }
            log.push_back(line.to_string());
        }
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// Mirrors every log/tracing event into the currently-active per-export
/// sidecar log file ([`reco_io::export_log`]), when one is open - a
/// no-op otherwise. Deliberately just a thin adapter: the actual
/// formatting/writing lives in `reco_io::export_log::record_event` so
/// `reco-io` doesn't need a `tracing-subscriber` dependency (see that
/// module's doc) - this `Layer` impl is the only part that has to live
/// here, next to the rest of this binary's own tracing setup.
struct ExportLogLayer;

impl<S: tracing::Subscriber> tracing_subscriber::Layer<S> for ExportLogLayer {
    fn on_event(
        &self,
        event: &tracing::Event<'_>,
        _ctx: tracing_subscriber::layer::Context<'_, S>,
    ) {
        reco_io::export_log::record_event(event);
    }
}

/// Install the standard tracing subscriber + log bridge.
///
/// Replaces the previous `env_logger::init()`. Bridges `log::*` calls
/// from reco-core / reco-io / reco-calibrate into tracing so user bug
/// reports arrive as one structured event stream instead of two
/// loggers writing to the same stderr.
///
/// M2 migration (deep-review-2026-04-18 decision 11).
fn init_tracing() {
    use tracing_subscriber::{EnvFilter, fmt, prelude::*};

    let _ = tracing_log::LogTracer::init();
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("info,ort::logging=warn"));

    // Feeds the in-app Debug panel (see `debug_log_snapshot`) - attached
    // to every branch below in addition to the file/stderr writer(s),
    // since release builds have no visible console
    // (`windows_subsystem = "windows"`). A generic fn rather than a
    // shared `let` binding: each `.with(debug_panel_layer())` call site
    // below layers onto a differently-typed subscriber stack (plain
    // fallback vs file-augmented), so a single shared value can't
    // monomorphize for both - only visible in release builds, since
    // debug builds compile out every branch but the fallback one.
    fn debug_panel_layer<S>() -> impl tracing_subscriber::Layer<S>
    where
        S: tracing::Subscriber + for<'a> tracing_subscriber::registry::LookupSpan<'a>,
    {
        fmt::layer()
            .with_target(false)
            .with_level(true)
            .with_ansi(false)
            .without_time()
            .with_writer(debug_log_writer)
    }

    // In release builds, write logs to a file so bug reports have context.
    // Debug builds just use stderr.
    #[cfg(not(debug_assertions))]
    if let Some(log_path) = log_file_path() {
        if let Some(parent) = log_path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        // Truncate if over 2 MB to prevent unbounded growth, otherwise append
        // so crash logs survive a restart.
        if log_path
            .metadata()
            .map(|m| m.len() > 2_000_000)
            .unwrap_or(false)
        {
            let _ = std::fs::remove_file(&log_path);
        }
        let file_result = std::fs::File::options()
            .create(true)
            .append(true)
            .open(&log_path);
        if let Err(ref e) = file_result {
            eprintln!(
                "Warning: could not open log file {}: {e}",
                log_path.display()
            );
        }
        if let Ok(file) = file_result {
            // Windows: file only (stderr is detached by windows_subsystem="windows").
            // Mac/Linux: file + stderr (user may launch from terminal).
            #[cfg(target_os = "windows")]
            {
                let _ = tracing_subscriber::registry()
                    .with(filter)
                    .with(
                        fmt::layer()
                            .with_target(true)
                            .with_level(true)
                            .with_ansi(false)
                            .with_writer(file),
                    )
                    .with(debug_panel_layer())
                    .with(ExportLogLayer)
                    .try_init();
                eprintln!("Log file: {}", log_path.display());
                return;
            }
            #[cfg(not(target_os = "windows"))]
            {
                let file = std::sync::Mutex::new(file);
                let _ = tracing_subscriber::registry()
                    .with(filter)
                    .with(
                        fmt::layer()
                            .with_target(true)
                            .with_level(true)
                            .with_ansi(false)
                            .with_writer(file),
                    )
                    .with(
                        fmt::layer()
                            .with_target(true)
                            .with_level(true)
                            .with_writer(std::io::stderr),
                    )
                    .with(debug_panel_layer())
                    .with(ExportLogLayer)
                    .try_init();
                eprintln!("Log file: {}", log_path.display());
                return;
            }
        }
    }

    let _ = tracing_subscriber::registry()
        .with(filter)
        .with(fmt::layer().with_target(true).with_level(true))
        .with(debug_panel_layer())
        .with(ExportLogLayer)
        .try_init();
}

/// Platform-appropriate log file path.
///
/// - Windows: next to executable (`reco-gui.log`)
/// - macOS: `~/Library/Logs/reco-gui.log`
/// - Linux: `~/.local/state/reco/reco-gui.log` (XDG_STATE_HOME)
fn log_file_path() -> Option<std::path::PathBuf> {
    #[cfg(target_os = "windows")]
    {
        std::env::current_exe()
            .ok()
            .and_then(|p| p.parent().map(|d| d.join("reco-gui.log")))
    }
    #[cfg(target_os = "macos")]
    {
        std::env::var("HOME")
            .ok()
            .map(|h| std::path::PathBuf::from(h).join("Library/Logs/reco-gui.log"))
    }
    #[cfg(target_os = "linux")]
    {
        std::env::var("XDG_STATE_HOME")
            .ok()
            .map(std::path::PathBuf::from)
            .or_else(|| {
                std::env::var("HOME")
                    .ok()
                    .map(|h| std::path::PathBuf::from(h).join(".local/state"))
            })
            .map(|d| d.join("reco/reco-gui.log"))
    }
}

/// Panic hook: emit panic location + payload as a `tracing::error!`
/// before the default hook runs, so a user-reported log file contains
/// the panic context alongside surrounding events.
fn install_panic_hook() {
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let location = info
            .location()
            .map(|l| format!("{}:{}:{}", l.file(), l.line(), l.column()))
            .unwrap_or_else(|| "<unknown>".into());
        let payload = if let Some(s) = info.payload().downcast_ref::<&str>() {
            (*s).to_string()
        } else if let Some(s) = info.payload().downcast_ref::<String>() {
            s.clone()
        } else {
            "<non-string panic payload>".into()
        };
        tracing::error!(
            target: "panic",
            location = %location,
            payload = %payload,
            "panic caught by tracing panic hook"
        );
        default_hook(info);
    }));
}

/// Snapshot the Export dialog's AI Tracking sliders into an
/// `AutocamDefaults`. Shared by calibration-level persistence
/// (`do_save_calibration`) and app-level persistence (the
/// `autocam-settings-changed` handler below) so the two never drift
/// out of sync with each other or with the slider list in `main.slint`.
fn snapshot_autocam_defaults(app: &RecoApp) -> reco_core::calibration::AutocamDefaults {
    reco_core::calibration::AutocamDefaults {
        tracking_mode: app.get_export_tracking_mode().to_string(),
        detection_interval: app.get_export_detection_interval() as u32,
        player_anchor_rad: app.get_export_player_anchor_rad(),
        ball_coast_secs: app.get_export_ball_coast_secs(),
        lookahead_secs: app.get_export_lookahead_secs() as f64,
        lookahead_reduced_bit_depth: app.get_export_lookahead_reduced_bit_depth(),
        preset: app.get_export_panner_preset().to_string(),
        framing: app.get_export_framing().to_string(),
        lock_pitch: app.get_export_lock_pitch(),
        cluster_mode: app.get_export_cluster_mode().to_string(),
        cluster_bandwidth_rad: app.get_export_cluster_bandwidth(),
        dead_zone_rad: app.get_export_dead_zone(),
        ball_weight: app.get_export_ball_weight(),
        ball_max_dist_from_cluster: app.get_export_ball_max_dist_from_cluster(),
        fov_tight: app.get_export_fov_tight(),
        fov_wide: app.get_export_fov_wide(),
        fov_default: app.get_export_fov_default(),
        fov_alpha: app.get_export_fov_alpha(),
        cluster_alpha: app.get_export_cluster_alpha(),
        confidence_threshold: app.get_export_confidence_threshold(),
    }
}

/// Apply a persisted `AutocamDefaults` onto the Export dialog's AI
/// Tracking sliders - the inverse of `snapshot_autocam_defaults`. Used
/// both for calibration-level restore (`try_init_and_update`) and
/// app-level restore (last-used settings, applied at startup before
/// any calibration is loaded).
fn apply_autocam_defaults(app: &RecoApp, ac: &reco_core::calibration::AutocamDefaults) {
    app.set_export_tracking_mode(ac.tracking_mode.clone().into());
    app.set_export_detection_interval(ac.detection_interval as i32);
    app.set_export_player_anchor_rad(ac.player_anchor_rad);
    app.set_export_ball_coast_secs(ac.ball_coast_secs);
    app.set_export_lookahead_secs(ac.lookahead_secs as f32);
    app.set_export_lookahead_reduced_bit_depth(ac.lookahead_reduced_bit_depth);
    app.set_export_panner_preset(ac.preset.clone().into());
    app.set_export_framing(ac.framing.clone().into());
    app.set_export_lock_pitch(ac.lock_pitch);
    app.set_export_cluster_mode(ac.cluster_mode.clone().into());
    app.set_export_cluster_bandwidth(ac.cluster_bandwidth_rad);
    app.set_export_dead_zone(ac.dead_zone_rad);
    app.set_export_ball_weight(ac.ball_weight);
    app.set_export_ball_max_dist_from_cluster(ac.ball_max_dist_from_cluster);
    app.set_export_fov_tight(ac.fov_tight);
    app.set_export_fov_wide(ac.fov_wide);
    app.set_export_fov_default(ac.fov_default);
    app.set_export_fov_alpha(ac.fov_alpha);
    app.set_export_cluster_alpha(ac.cluster_alpha);
    app.set_export_confidence_threshold(ac.confidence_threshold);
}

/// Curated banner-color presets shown in the SCOREBOARD card's ComboBox -
/// shared between `on_changed_scoreboard_banner_color` and
/// `apply_scoreboard_settings` so restoring a saved preset can't drift
/// from what picking it live actually produces.
fn scoreboard_banner_color_hex(preset_name: &str) -> Option<&'static str> {
    match preset_name {
        "Navy" => Some("#0b1a33"),
        "Black" => Some("#0a0a0a"),
        "Forest Green" => Some("#0e2e1a"),
        "Maroon" => Some("#33101a"),
        "Purple" => Some("#241333"),
        _ => None,
    }
}

/// Snapshot the current scoreboard state into a persistable
/// `ScoreboardSettings` - the scoreboard equivalent of
/// `snapshot_autocam_defaults`, see `do_save_calibration`. Unlike
/// autocam's pure-Slint mirror, some of this lives in `AppState` (the
/// loaded import's path, encoded logo paths), so this needs both.
fn snapshot_scoreboard_settings(
    app: &RecoApp,
    s: &AppState,
) -> reco_core::calibration::ScoreboardSettings {
    let package_id = s
        .scoreboard_packages
        .get(app.get_scoreboard_current_index().max(0) as usize)
        .map(|package| package.manifest.id.clone())
        .unwrap_or_default();
    reco_core::calibration::ScoreboardSettings {
        enabled: app.get_scoreboard_enabled(),
        package_id,
        match_logger_path: s.scoreboard_import_path.clone(),
        sync_event_ts_ms: s.scoreboard_sync_anchor.map(|anchor| anchor.event_ts_ms),
        sync_video_seconds: s
            .scoreboard_sync_anchor
            .map(|anchor| anchor.video_seconds)
            .unwrap_or(0.0),
        placement: s.scoreboard_placement,
        home_logo_path: s.scoreboard_style.home_logo_path.clone(),
        away_logo_path: s.scoreboard_style.away_logo_path.clone(),
        font_family: s.scoreboard_style.font_family.clone().unwrap_or_default(),
        logo_size_px: s.scoreboard_style.logo_size_px.unwrap_or(34.0),
        banner_color_name: app.get_scoreboard_banner_color_name().to_string(),
        derive_cut_ranges: app.get_scoreboard_derive_cut_ranges(),
        cut_lead_secs: app.get_autocut_cut_lead_secs(),
        cut_trail_secs: app.get_autocut_cut_trail_secs(),
        kickoff_lead_secs: app.get_autocut_kickoff_lead_secs(),
        match_end_trail_secs: app.get_autocut_match_end_trail_secs(),
        // The margins persist; the highlights toggle itself deliberately
        // does not - see `ScoreboardSettings::highlight_lead_secs`.
        highlight_lead_secs: app.get_highlights_lead_secs(),
        highlight_trail_secs: app.get_highlights_trail_secs(),
    }
}

/// Persist the scoreboard's current settings at app level (see
/// `GuiSettings::scoreboard_settings`'s doc comment) - call after any
/// change `snapshot_scoreboard_settings` would reflect. Takes `&mut
/// AppState` directly (not `Rc<RefCell<AppState>>`) so it composes
/// with a handler that already holds a `borrow_mut()`.
fn persist_scoreboard_settings(app: &RecoApp, s: &mut AppState) {
    let sb = snapshot_scoreboard_settings(app, s);
    s.user_settings.set_scoreboard_settings(sb);
}

/// Adopt a freshly parsed Match Logger export as the current one: the
/// `AppState` fields the replay reads, the SCOREBOARD card's summary and
/// sync labels, and the preview's redraw flag.
///
/// Shared by the explicit "Load Match Logger export..." button and the
/// implicit load "Select Match Folder" performs (see
/// `scoreboard_import::find_export_in_folder`), so the two can't drift
/// into leaving different amounts of state behind. Deliberately does
/// *not* refresh derived cut ranges or persist settings - both callers
/// do that themselves, after the rest of what each of them changed.
fn adopt_match_logger_export(
    app: &RecoApp,
    s: &mut AppState,
    path: PathBuf,
    export: scoreboard_import::MatchLoggerExport,
) {
    fn team_label(name: &str, fallback: &str) -> String {
        let trimmed = name.trim();
        if trimmed.is_empty() {
            fallback.to_string()
        } else {
            trimmed.to_string()
        }
    }
    let summary = format!(
        "{} vs {} - {} events",
        team_label(&export.home, "Home"),
        team_label(&export.away, "Away"),
        export.event_count()
    );
    let default_anchor = export
        .video_start_ms()
        .map(|event_ts_ms| scoreboard_import::SyncAnchor {
            event_ts_ms,
            video_seconds: 0.0,
        });

    s.scoreboard_import_path = Some(path);
    s.scoreboard_sync_anchor = default_anchor;
    s.scoreboard_import = Some(export);
    // Without this, the preview only redraws while playing/seeking (see
    // vsync_render_tick's gate), so the banner wouldn't pick up the
    // freshly loaded data until something unrelated happened to trigger
    // a redraw - read as a long, confusing delay after Load, even though
    // push_scoreboard_replay itself runs within ~150ms once a tick fires
    // at all.
    s.preview_dirty = true;

    app.set_scoreboard_match_summary(summary.into());
    app.set_scoreboard_sync_label(if default_anchor.is_some() {
        "Synced to video start - tap \"Set sync point\" if that's off".into()
    } else {
        slint::SharedString::from(
            "No video_start event in this export - tap \"Set sync point\" once scrubbed to the matching frame",
        )
    });
    app.set_scoreboard_error_text("".into());
}

/// Restore a persisted `ScoreboardSettings` onto the SCOREBOARD card and
/// `AppState` - the inverse of `snapshot_scoreboard_settings`. Re-reads
/// the Match Logger export and team logos from their saved paths rather
/// than trusting any embedded copy (there isn't one - see
/// `ScoreboardSettings`'s doc comment); a moved/deleted file is logged
/// and surfaced in `scoreboard_error_text`, not a reason to abort
/// restoring everything else.
fn apply_scoreboard_settings(
    state_ref: &Rc<RefCell<AppState>>,
    app: &RecoApp,
    settings: &reco_core::calibration::ScoreboardSettings,
) {
    let mut s = state_ref.borrow_mut();

    let package_index = s
        .scoreboard_packages
        .iter()
        .position(|package| package.manifest.id == settings.package_id);
    if let Some(index) = package_index {
        app.set_scoreboard_current_index(index as i32);
    }
    app.set_scoreboard_enabled(settings.enabled);
    if settings.enabled && package_index.is_some() {
        s.configure_scoreboard(
            app.get_scoreboard_enabled(),
            app.get_scoreboard_current_index().max(0) as usize,
        );
        app.set_scoreboard_editor_available(
            s.scoreboard_runtime
                .as_ref()
                .and_then(reco_scoreboard::ScoreboardRuntime::editor_url)
                .is_some(),
        );
        let network_editor_url = s
            .scoreboard_runtime
            .as_ref()
            .and_then(reco_scoreboard::ScoreboardRuntime::network_editor_url);
        app.set_scoreboard_share_available(network_editor_url.is_some());
        app.set_scoreboard_share_url(network_editor_url.unwrap_or_default().into());
    }

    if let Some(path) = settings.match_logger_path.as_ref() {
        match scoreboard_import::load(path) {
            Ok(export) => {
                let summary = format!(
                    "{} vs {} - {} events",
                    export.home,
                    export.away, // team_label's fallback doesn't matter for a restore - a valid saved export always has names
                    export.event_count()
                );
                s.scoreboard_import = Some(export);
                s.scoreboard_import_path = Some(path.clone());
                app.set_scoreboard_match_summary(summary.into());
            }
            Err(error) => {
                log::warn!(
                    "Cannot restore Match Logger export from {}: {error}",
                    path.display()
                );
                app.set_scoreboard_error_text(
                    format!("Saved Match Logger export not found: {}", path.display()).into(),
                );
            }
        }
    }
    // Restores the anchor exactly as saved (including a manual "Set sync
    // point" correction) rather than re-deriving the file's own
    // video_start default, which a prior manual correction may disagree
    // with.
    s.scoreboard_sync_anchor =
        settings
            .sync_event_ts_ms
            .map(|event_ts_ms| scoreboard_import::SyncAnchor {
                event_ts_ms,
                video_seconds: settings.sync_video_seconds,
            });
    if s.scoreboard_import.is_some() {
        app.set_scoreboard_sync_label(if s.scoreboard_sync_anchor.is_some() {
            format!(
                "Synced: video_start = {:.1}s into this video",
                settings.sync_video_seconds
            )
            .into()
        } else {
            "Not synced - tap \"Set sync point\"".into()
        });
    }

    s.scoreboard_placement = settings.placement;
    if let Some(bridge) = s.bridge.as_mut() {
        bridge.set_overlay_placement(settings.placement);
    }
    s.apply_scoreboard_render_scale();
    app.set_scoreboard_offset_x(settings.placement.offset.0);
    app.set_scoreboard_offset_y(settings.placement.offset.1);
    app.set_scoreboard_scale(settings.placement.scale);

    for (path, home) in [
        (settings.home_logo_path.as_ref(), true),
        (settings.away_logo_path.as_ref(), false),
    ] {
        let Some(path) = path else { continue };
        match scoreboard_import::image_data_uri(path) {
            Ok(data_uri) => {
                if home {
                    s.scoreboard_style.home_logo = Some(data_uri);
                    s.scoreboard_style.home_logo_path = Some(path.clone());
                    app.set_scoreboard_home_logo_path(path.to_string_lossy().into_owned().into());
                } else {
                    s.scoreboard_style.away_logo = Some(data_uri);
                    s.scoreboard_style.away_logo_path = Some(path.clone());
                    app.set_scoreboard_away_logo_path(path.to_string_lossy().into_owned().into());
                }
            }
            Err(error) => {
                log::warn!("Cannot restore team logo from {}: {error}", path.display());
            }
        }
    }

    s.scoreboard_style.font_family = if settings.font_family.is_empty() {
        None
    } else {
        Some(settings.font_family.clone())
    };
    app.set_scoreboard_font_family(settings.font_family.clone().into());

    s.scoreboard_style.logo_size_px = Some(settings.logo_size_px);
    app.set_scoreboard_logo_size(settings.logo_size_px);

    s.scoreboard_style.banner_color =
        scoreboard_banner_color_hex(&settings.banner_color_name).map(str::to_string);
    app.set_scoreboard_banner_color_name(settings.banner_color_name.clone().into());

    // Margins first: refresh_derived_cut_ranges below reads them straight
    // off the app properties, so restoring them after would derive this
    // session's first ranges with the wrong (default) values.
    app.set_autocut_cut_lead_secs(settings.cut_lead_secs);
    app.set_autocut_cut_trail_secs(settings.cut_trail_secs);
    app.set_autocut_kickoff_lead_secs(settings.kickoff_lead_secs);
    app.set_autocut_match_end_trail_secs(settings.match_end_trail_secs);
    app.set_highlights_lead_secs(settings.highlight_lead_secs);
    app.set_highlights_trail_secs(settings.highlight_trail_secs);

    app.set_scoreboard_derive_cut_ranges(settings.derive_cut_ranges);
    refresh_derived_cut_ranges(&mut s, app);

    s.preview_dirty = true;

    // `app.set_scoreboard_scale` above two-way-binds to a Slider with a
    // `changed` callback wired to `changed-scoreboard-placement`, which
    // fires synchronously and re-persists via `persist_scoreboard_settings`
    // - but at that point in this function the logo/font/logo-size/banner/
    // derive-cut-ranges fields set further down hadn't been applied yet,
    // so that mid-restore write saves a placement-correct-but-otherwise-
    // stale snapshot. Persisting again here, now that every field is
    // actually applied, overwrites that stale write with the real state
    // so the next restore reads back what was actually restored.
    persist_scoreboard_settings(app, &mut s);
}

fn main() -> anyhow::Result<()> {
    init_tracing();
    install_panic_hook();
    reco_io::init();

    // Select wgpu 28 as Slint's rendering backend. This MUST happen
    // before creating any window. femtovg-wgpu renders UI through
    // wgpu (DX12/Vulkan) instead of OpenGL, so it works on GPUs
    // that lack an OpenGL driver. downlevel_defaults() ensures iGPUs
    // and older hardware can satisfy the device limits.
    slint::BackendSelector::new()
        .require_wgpu_28({
            let mut config = slint::wgpu_28::WGPUConfiguration::default();
            if let slint::wgpu_28::WGPUConfiguration::Automatic(ref mut settings) = config {
                settings.device_required_limits = reco_core::wgpu::Limits::downlevel_defaults();
                settings.backends = reco_core::gpu::GpuContext::select_backends();
            }
            config
        })
        .select()?;

    let app = RecoApp::new()?;
    let state = Rc::new(RefCell::new(AppState::new()));

    // Initialize opt-in telemetry if the user enabled it.
    {
        let mut s = state.borrow_mut();
        if s.user_settings.telemetry_enabled {
            let cid = s
                .user_settings
                .telemetry_client_id
                .clone()
                .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
            log::info!("Telemetry enabled (client_id={}).", &cid[..8]);
            let client = telemetry_client::TelemetryClient::new(cid);
            client.app_open();
            s.telemetry = Some(client);
        } else {
            log::info!("Telemetry disabled (opt-in via Preferences).");
        }
    }

    let (ai_status, ai_available) = ai_capability_summary();
    log::info!("{ai_status}");
    app.set_ai_status(ai_status.clone().into());
    app.set_ai_available(ai_available);

    // Send context telemetry after AI probe.
    {
        let s = state.borrow();
        if let Some(ref t) = s.telemetry {
            let os = format!("{} {}", std::env::consts::OS, std::env::consts::ARCH);
            t.context("(pending GPU init)", &os, &ai_status);
        }
    }

    let version = format!(
        "v{}{}",
        env!("CARGO_PKG_VERSION"),
        option_env!("GIT_HASH")
            .filter(|h| !h.is_empty())
            .map(|h| format!(" ({h})"))
            .unwrap_or_default()
    );
    log::info!("Reco GUI {version}");
    app.set_version(version.into());

    {
        let s = state.borrow();
        app.set_dark_mode(s.user_settings.dark_mode);
        // App-level "last used" AI Tracking / panner settings, restored
        // before any video/calibration is loaded so the Export dialog
        // doesn't reset to hardcoded literals just because the user
        // hasn't explicitly clicked Save calibration this session. A
        // calibration with its own `autocam_defaults` still overrides
        // this once loaded (see the `RenderingSetup` branch below).
        if let Some(ac) = s.user_settings.autocam_defaults.as_ref() {
            apply_autocam_defaults(&app, ac);
            log::info!("Restored AI Tracking defaults from last session");
        }
        // "AI Tracking"/"Async Detect" checkboxes: always app-level,
        // never overridden by a calibration's own autocam_defaults (see
        // GuiSettings::autocam_enabled's doc comment) - restored here
        // unconditionally, not gated on autocam_defaults existing.
        app.set_export_autocam_enabled(s.user_settings.autocam_enabled);
        app.set_export_async_detect(s.user_settings.async_detect_enabled);

        // PAUZE transition: app-level for the same reason (see
        // `GuiSettings::pause_overlay_enabled`) - how a break should read
        // on screen is a house style, not a property of one match.
        app.set_export_pause_overlay_enabled(s.user_settings.pause_overlay_enabled);
        app.set_export_pause_overlay_fade_secs(s.user_settings.pause_overlay_fade_secs);
        app.set_export_pause_overlay_hold_secs(s.user_settings.pause_overlay_hold_secs);
    }

    // App-level SCOREBOARD card settings, same "restore before any
    // calibration" precedence as AI Tracking above - a calibration
    // with its own `scoreboard` field still overrides this once
    // loaded (the `RenderingSetup` branch below). Cloned out of its
    // own short borrow first: `apply_scoreboard_settings` takes the
    // `Rc<RefCell<AppState>>` directly and does its own `borrow_mut()`,
    // which would panic (already borrowed) inside the block above.
    let restored_scoreboard = state.borrow().user_settings.scoreboard_settings.clone();
    if let Some(sb) = restored_scoreboard.as_ref() {
        apply_scoreboard_settings(&state, &app, sb);
        log::info!("Restored SCOREBOARD settings from last session");
    }

    // Reopen the last-used left/right video and calibration file (if they
    // still exist on disk), so the app doesn't start from a blank slate
    // every launch. Only sets display fields and `AppState` paths here;
    // the actual pipeline init happens once the GPU is ready, in the
    // `RenderingSetup` branch below (same as picking files manually).
    {
        let mut s = state.borrow_mut();
        if let Some(input) = s.user_settings.restore_left_input() {
            let first = input.first_path().to_path_buf();
            let label = match &input {
                reco_io::stitch_job::InputPath::Single(p) => display_name(p),
                reco_io::stitch_job::InputPath::Chained(ps) => {
                    format!("{} ({} segments)", display_name(&ps[0]), ps.len())
                }
            };
            app.set_left_path(label.into());
            s.left_input = Some(input);
            s.left_path = Some(first);
        }
        if let Some(input) = s.user_settings.restore_right_input() {
            let first = input.first_path().to_path_buf();
            let label = match &input {
                reco_io::stitch_job::InputPath::Single(p) => display_name(p),
                reco_io::stitch_job::InputPath::Chained(ps) => {
                    format!("{} ({} segments)", display_name(&ps[0]), ps.len())
                }
            };
            app.set_right_path(label.into());
            s.right_input = Some(input);
            s.right_path = Some(first);
        }
        // Restore the match-folder export-suggestion link too (see
        // `GuiSettings::last_match_folder`'s doc comment) - without
        // this, a restart would still reopen the same left/right
        // videos but silently lose "suggest the export path inside
        // the match folder" and fall back to "next to the left video"
        // (i.e. inside its `Left/` subfolder) instead.
        if let Some(folder) = s.user_settings.last_match_folder.clone() {
            s.match_folder = Some(folder);
        }
        // Prefer the last calibration actually used in a session; only
        // fall back to the user's configured default when there's no
        // MRU entry (or it no longer exists on disk) - the default is a
        // safety net for "no calibration available", not a preference
        // over one the user picked themselves.
        if let Some(cal) = s
            .user_settings
            .last_calibration()
            .or_else(|| s.user_settings.default_calibration())
        {
            app.set_calibration_path(display_name(&cal).into());
            s.calibration_path = Some(cal);
        }
    }

    // Check for updates in the background.
    // Stores result in an Arc<Mutex> that the timer tick reads once.
    let update_result: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
    {
        let result = Arc::clone(&update_result);
        std::thread::spawn(move || {
            let current = env!("CARGO_PKG_VERSION");
            let resp = ureq::get(
                "https://api.github.com/repos/reco-project/video-stitcher/releases/latest",
            )
            .header("User-Agent", "reco-gui")
            .call();
            if let Ok(mut resp) = resp
                && let Ok(body) = resp.body_mut().read_to_string()
                && let Ok(json) = serde_json::from_str::<serde_json::Value>(&body)
                && let Some(tag) = json["tag_name"].as_str()
            {
                let latest = tag.trim_start_matches('v');
                let parse_ver = |s: &str| -> Option<Vec<u64>> {
                    s.split(&['.', '-'][..])
                        .take(3)
                        .map(|p| p.parse().ok())
                        .collect()
                };
                let is_newer = parse_ver(latest)
                    .zip(parse_ver(current))
                    .is_some_and(|(l, c)| l > c);
                if is_newer {
                    log::info!("Update available: {current} -> {latest}");
                    *result.lock().unwrap() = Some(tag.to_string());
                }
            }
        });
    }

    let mut codecs: Vec<slint::SharedString> = Vec::new();
    for (label, codec) in [
        ("h264", reco_io::ffmpeg::encoder::VideoCodec::H264),
        ("hevc", reco_io::ffmpeg::encoder::VideoCodec::Hevc),
        ("av1", reco_io::ffmpeg::encoder::VideoCodec::Av1),
    ] {
        if !reco_io::ffmpeg::encoder::available_encoders(codec).is_empty() {
            codecs.push(label.into());
        }
    }
    if codecs.is_empty() {
        log::warn!("No video encoders available - export will fail");
        codecs.push("h264".into());
    } else {
        log::info!(
            "Available codecs: {}",
            codecs
                .iter()
                .map(|c| c.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        );
    }
    app.set_available_codecs(slint::ModelRc::new(slint::VecModel::from(codecs)));

    // The selector is manifest-driven; no sport name is compiled into Rust.
    {
        let s = state.borrow();
        let names = s
            .scoreboard_packages
            .iter()
            .map(|package| package.manifest.name.clone().into())
            .collect::<Vec<slint::SharedString>>();
        app.set_available_scoreboards(slint::ModelRc::new(slint::VecModel::from(names)));
        app.set_scoreboard_error_text(s.scoreboard_error.clone().into());
    }

    // Seed recording and preview settings from persisted preferences.
    {
        let s = state.borrow();
        app.set_recording_codec(s.user_settings.recording_codec.clone().into());
        app.set_recording_quality(s.user_settings.recording_quality.clone().into());
        app.set_recording_folder(
            s.user_settings
                .recording_folder
                .as_ref()
                .map(|p| p.to_string_lossy().into_owned())
                .unwrap_or_default()
                .into(),
        );
        app.set_preview_aspect(s.user_settings.preview_aspect.clone().into());
    }

    // Seed the Recent-files dialog with the persisted MRU lists. If
    // the user never loaded anything before, these are empty and the
    // Recent button in the file bar stays disabled.
    sync_recent_paths(&state.borrow().user_settings, &app);

    // Restore last window size if the user resized before. Slint's
    // `set_size` takes a `PhysicalSize`; we stored logical dimensions
    // in settings but using them as physical is close enough at 1.0
    // scale (the common case) - if the user moves to a HiDPI display
    // the next resize will correct.
    // Window size restore disabled - Slint's preferred size (1280x820)
    // is a safe default across all displays. Users can maximize manually.

    // Capture Slint's wgpu device and queue on RenderingSetup. These
    // are reused by PreviewBridge so reco-core's stitch output lands
    // directly in Slint-owned textures with zero copies.
    let state_for_notifier = Rc::clone(&state);
    let app_weak_notifier = app.as_weak();
    app.window()
        .set_rendering_notifier(move |rendering_state, graphics_api| {
            match rendering_state {
                slint::RenderingState::RenderingSetup => {
                    let slint::GraphicsAPI::WGPU28 {
                        instance: _,
                        device,
                        queue,
                        ..
                    } = graphics_api
                    else {
                        log::warn!(
                            "Expected WGPU28 GraphicsAPI in rendering notifier, got something else"
                        );
                        return;
                    };

                    // Reconstruct adapter info by enumerating the instance. The
                    // notifier doesn't expose the adapter directly, but any adapter
                    // matching the device's backend will have the correct GPU name
                    // for logging — the device and queue are what actually matter
                    // for correctness.
                    let adapter_info = wgpu::AdapterInfo {
                        name: "Slint-shared wgpu 28 device".into(),
                        vendor: 0,
                        device: 0,
                        device_pci_bus_id: String::new(),
                        device_type: wgpu::DeviceType::Other,
                        driver: String::new(),
                        driver_info: String::new(),
                        backend: wgpu::Backend::Vulkan,
                        subgroup_min_size: 0,
                        subgroup_max_size: 0,
                        transient_saves_memory: false,
                    };

                    state_for_notifier.borrow_mut().shared_gpu = Some(SharedGpu {
                        device: device.clone(),
                        queue: queue.clone(),
                        adapter_info,
                    });
                    log::info!("Captured Slint wgpu 28 device/queue for zero-copy preview");

                    // If files were picked before the renderer was ready, the init
                    // path would have failed early. Retry now that we have the GPU.
                    if let Some(app) = app_weak_notifier.upgrade() {
                        try_init_and_update(&state_for_notifier, &app.as_weak());
                    }
                }
                slint::RenderingState::BeforeRendering => {
                    // Vsync-locked playback tick. Previously this ran off a
                    // 2 ms free-running timer, which put set_preview_frame
                    // calls at random phases relative to Slint's 60 Hz
                    // compositor. Small (~1 ms) submission jitter around
                    // the 33 ms video interval crossed vsync boundaries
                    // unpredictably, so individual frames displayed for
                    // 1, 2, or 3 vsync slots at random, producing visible
                    // judder perceived as ~25 fps. Driving from here
                    // phase-locks everything to the compositor.
                    vsync_render_tick(&state_for_notifier, &app_weak_notifier);
                }
                _ => {}
            }
        })?;

    // ── File picker callbacks ──

    let app_weak = app.as_weak();
    let state_ref = Rc::clone(&state);
    app.on_pick_left_video(move || {
        let dialog = rfd::FileDialog::new()
            .set_title("Select left camera video(s)")
            .add_filter(
                "Video",
                &["mp4", "MP4", "mov", "MOV", "avi", "AVI", "mkv", "MKV"],
            );
        let mut paths = dialog.pick_files().unwrap_or_default();
        if paths.is_empty() {
            return;
        }
        paths.sort();
        let input = {
            let s = state_ref.borrow();
            input_path_from_picks(paths, s.left_input.as_ref(), "Left")
        };
        let first = match &input {
            reco_io::stitch_job::InputPath::Single(p) => p.clone(),
            reco_io::stitch_job::InputPath::Chained(ps) => ps[0].clone(),
        };
        {
            let mut s = state_ref.borrow_mut();
            let changed = s.left_path.as_ref() != Some(&first);
            if changed && s.bridge.is_some() {
                s.unload_pipeline();
                if let Some(app) = app_weak.upgrade() {
                    app.set_files_loaded(false);
                    app.set_status_text("File changed - re-calibrate or load calibration".into());
                }
            }
            if let Some(app) = app_weak.upgrade() {
                let label = match &input {
                    reco_io::stitch_job::InputPath::Single(p) => display_name(p),
                    reco_io::stitch_job::InputPath::Chained(ps) => {
                        format!("{} ({} segments)", display_name(&ps[0]), ps.len())
                    }
                };
                app.set_left_path(label.into());
            }
            s.user_settings.push_left(first.clone());
            if let Some(app) = app_weak.upgrade() {
                sync_recent_paths(&s.user_settings, &app);
            }
            s.left_input = Some(input);
            s.left_path = Some(first);
            // A manual pick no longer necessarily matches this match
            // folder's `Left` subdir - drop the export-suggestion link
            // (both the live one and the persisted one, so a stale
            // folder doesn't come back on the next restart either).
            s.match_folder = None;
            s.user_settings.last_match_folder = None;
            s.persist_left_segments();
            drop(s);
            try_init_and_update(&state_ref, &app_weak);
        }
    });

    let app_weak = app.as_weak();
    let state_ref = Rc::clone(&state);
    app.on_pick_right_video(move || {
        let dialog = rfd::FileDialog::new()
            .set_title("Select right camera video(s)")
            .add_filter(
                "Video",
                &["mp4", "MP4", "mov", "MOV", "avi", "AVI", "mkv", "MKV"],
            );
        let mut paths = dialog.pick_files().unwrap_or_default();
        if paths.is_empty() {
            return;
        }
        paths.sort();
        let input = {
            let s = state_ref.borrow();
            input_path_from_picks(paths, s.right_input.as_ref(), "Right")
        };
        let first = match &input {
            reco_io::stitch_job::InputPath::Single(p) => p.clone(),
            reco_io::stitch_job::InputPath::Chained(ps) => ps[0].clone(),
        };
        {
            let mut s = state_ref.borrow_mut();
            let changed = s.right_path.as_ref() != Some(&first);
            if changed && s.bridge.is_some() {
                s.unload_pipeline();
                if let Some(app) = app_weak.upgrade() {
                    app.set_files_loaded(false);
                    app.set_status_text("File changed - re-calibrate or load calibration".into());
                }
            }
            if let Some(app) = app_weak.upgrade() {
                let label = match &input {
                    reco_io::stitch_job::InputPath::Single(p) => display_name(p),
                    reco_io::stitch_job::InputPath::Chained(ps) => {
                        format!("{} ({} segments)", display_name(&ps[0]), ps.len())
                    }
                };
                app.set_right_path(label.into());
            }
            s.user_settings.push_right(first.clone());
            if let Some(app) = app_weak.upgrade() {
                sync_recent_paths(&s.user_settings, &app);
            }
            s.right_input = Some(input);
            s.right_path = Some(first);
            // See the matching comment in `on_pick_left_video`.
            s.match_folder = None;
            s.user_settings.last_match_folder = None;
            s.persist_right_segments();
            drop(s);
            try_init_and_update(&state_ref, &app_weak);
        }
    });

    let app_weak = app.as_weak();
    let state_ref = Rc::clone(&state);
    app.on_pick_calibration(move || {
        let dialog = rfd::FileDialog::new()
            .set_title("Select calibration JSON")
            .add_filter("JSON", &["json", "JSON"]);
        if let Some(path) = dialog.pick_file() {
            let mut s = state_ref.borrow_mut();
            let changed = s.calibration_path.as_ref() != Some(&path);
            if changed && s.bridge.is_some() {
                s.unload_pipeline();
                if let Some(app) = app_weak.upgrade() {
                    app.set_files_loaded(false);
                    app.set_status_text("Calibration changed — reloading".into());
                }
            }
            if let Some(app) = app_weak.upgrade() {
                app.set_calibration_path(display_name(&path).into());
            }
            s.user_settings.push_calibration(path.clone());
            if let Some(app) = app_weak.upgrade() {
                sync_recent_paths(&s.user_settings, &app);
            }
            s.calibration_path = Some(path);
            drop(s);
            try_init_and_update(&state_ref, &app_weak);
        }
    });

    // Fills the left/right video and calibration pickers in one shot from
    // a match folder that follows the `<match>/Left/`, `<match>/Right/`
    // convention. Unlike the manual "+" pickers (which append to an
    // existing chain), this replaces the current selection outright - it
    // represents loading a whole match, not adding a segment to one.
    let app_weak = app.as_weak();
    let state_ref = Rc::clone(&state);
    app.on_pick_match_folder(move || {
        let dialog = rfd::FileDialog::new().set_title("Select match folder");
        let Some(folder) = dialog.pick_folder() else {
            return;
        };
        let scan = match match_folder::scan_match_folder(&folder) {
            Ok(scan) => scan,
            Err(e) => {
                log::warn!("Match folder pick failed: {e}");
                let mut s = state_ref.borrow_mut();
                if let Some(app) = app_weak.upgrade() {
                    s.toasts
                        .push(Severity::Error, "Match folder", e.to_string());
                    crate::toast::sync_to_ui(&s.toasts, &app);
                }
                return;
            }
        };

        let mut s = state_ref.borrow_mut();
        s.match_folder = Some(folder.clone());
        // Saved below via persist_left_segments/persist_right_segments
        // (both call UserSettings::save()) - no separate save() needed.
        s.user_settings.last_match_folder = Some(folder.clone());

        let left_input = input_path_from_picks(scan.left_videos, None, "Left");
        let right_input = input_path_from_picks(scan.right_videos, None, "Right");
        let left_first = left_input.first_path().to_path_buf();
        let right_first = right_input.first_path().to_path_buf();

        let files_changed = s.left_path.as_ref() != Some(&left_first)
            || s.right_path.as_ref() != Some(&right_first);
        if files_changed && s.bridge.is_some() {
            s.unload_pipeline();
            if let Some(app) = app_weak.upgrade() {
                app.set_files_loaded(false);
                app.set_status_text(
                    "Match folder changed - re-calibrate or load calibration".into(),
                );
            }
        }

        if let Some(app) = app_weak.upgrade() {
            let left_label = match &left_input {
                reco_io::stitch_job::InputPath::Single(p) => display_name(p),
                reco_io::stitch_job::InputPath::Chained(ps) => {
                    format!("{} ({} segments)", display_name(&ps[0]), ps.len())
                }
            };
            let right_label = match &right_input {
                reco_io::stitch_job::InputPath::Single(p) => display_name(p),
                reco_io::stitch_job::InputPath::Chained(ps) => {
                    format!("{} ({} segments)", display_name(&ps[0]), ps.len())
                }
            };
            app.set_left_path(left_label.into());
            app.set_right_path(right_label.into());
        }
        s.user_settings.push_left(left_first.clone());
        s.user_settings.push_right(right_first.clone());
        s.left_input = Some(left_input);
        s.right_input = Some(right_input);
        s.left_path = Some(left_first);
        s.right_path = Some(right_first);
        s.persist_left_segments();
        s.persist_right_segments();

        // Calibration: reuse an existing per-match file if this match was
        // opened before, else seed one by copying the user's configured
        // Default Calibration, else leave calibration unset - same as a
        // fresh three-file pick, the user calibrates manually.
        //
        // `freshly_created` tracks the copy-from-default case specifically
        // (not reuse, not "left unset") - only then does the sync-offset
        // prompt below make sense: a reused per-match file already has its
        // own, presumably-correct sync offset from when it was first set
        // up, and there's nothing to detect against with no calibration.
        let cal_path = scan.calibration_path;
        let mut freshly_created = false;
        if cal_path.exists() {
            log::info!(
                "Match folder: reusing existing calibration {}",
                cal_path.display()
            );
            s.calibration_path = Some(cal_path.clone());
            s.user_settings.push_calibration(cal_path);
        } else if let Some(default_cal) = s.user_settings.default_calibration_path.clone() {
            match std::fs::copy(&default_cal, &cal_path) {
                Ok(_) => {
                    log::info!(
                        "Match folder: seeded calibration from Default Calibration at {}",
                        cal_path.display()
                    );
                    s.calibration_path = Some(cal_path.clone());
                    s.user_settings.push_calibration(cal_path);
                    freshly_created = true;
                }
                Err(e) => {
                    log::warn!("Failed to copy Default Calibration into match folder: {e}");
                    s.calibration_path = None;
                    if let Some(app) = app_weak.upgrade() {
                        s.toasts.push(
                            Severity::Error,
                            "Default Calibration",
                            format!("Could not copy into match folder: {e}"),
                        );
                        crate::toast::sync_to_ui(&s.toasts, &app);
                    }
                }
            }
        } else {
            s.calibration_path = None;
        }

        if let Some(app) = app_weak.upgrade() {
            app.set_calibration_path(
                s.calibration_path
                    .as_ref()
                    .map(|p| display_name(p))
                    .unwrap_or_default()
                    .into(),
            );
            sync_recent_paths(&s.user_settings, &app);
        }

        // Match Logger export: the operator's export lands in the match
        // folder alongside the footage, so picking the folder loads it
        // too instead of making them repeat the pick in the SCOREBOARD
        // card. Any previously loaded export is dropped first - it
        // belongs to a different match, and leaving it attached would
        // replay the wrong score over this one's footage.
        if let Some(app) = app_weak.upgrade() {
            let had_previous = s.scoreboard_import.is_some();
            s.scoreboard_import = None;
            s.scoreboard_import_path = None;
            s.scoreboard_sync_anchor = None;
            app.set_scoreboard_match_summary("".into());
            app.set_scoreboard_sync_label("".into());

            match scoreboard_import::find_export_in_folder(&folder) {
                Some((path, export)) => {
                    let found = format!("{} vs {}", export.home.trim(), export.away.trim());
                    let file = display_name(&path);
                    adopt_match_logger_export(&app, &mut s, path, export);
                    s.toasts.push(
                        Severity::Info,
                        "Match Logger",
                        format!("Loaded {found} from {file}"),
                    );
                }
                None if had_previous => {
                    // Silence here would be worse than a toast: the
                    // scoreboard card just went blank, and the reason is
                    // not visible anywhere else.
                    s.toasts.push(
                        Severity::Warn,
                        "Match Logger",
                        "No export found in this match folder - the previous one was unloaded"
                            .to_string(),
                    );
                }
                None => log::info!(
                    "Match folder: no Match Logger export in {}",
                    folder.display()
                ),
            }
            crate::toast::sync_to_ui(&s.toasts, &app);
            // The derived ranges belong to whichever export is loaded
            // now, including "none at all" - same staleness argument as
            // in on_load_scoreboard_events.
            refresh_derived_cut_ranges(&mut s, &app);
            persist_scoreboard_settings(&app, &mut s);
        }

        drop(s);
        try_init_and_update(&state_ref, &app_weak);

        // Offer to detect+save the sync offset now, while the calibration
        // is still a fresh copy of the default - the previous camera
        // start-time gap it inherited almost certainly doesn't apply to
        // this match's recordings. Gated on files actually loading (not
        // just the copy succeeding) so the prompt doesn't show up over a
        // broken/empty preview.
        if freshly_created
            && let Some(app) = app_weak.upgrade()
            && app.get_files_loaded()
        {
            app.set_match_folder_sync_prompt_open(true);
        }
    });

    // ── Recent-files dialog callbacks ──
    //
    // Clicking an entry in the dialog is functionally equivalent to
    // picking that file via the native dialog: update the MRU (so it
    // moves to front), push to the Slint label property, and try to
    // initialize if all three slots are now filled.
    let app_weak = app.as_weak();
    let state_ref = Rc::clone(&state);
    app.on_load_recent_left(move |entry| {
        let path = PathBuf::from(entry.as_str());
        let mut s = state_ref.borrow_mut();
        let changed = s.left_path.as_ref() != Some(&path);
        if changed && s.bridge.is_some() {
            s.unload_pipeline();
            if let Some(app) = app_weak.upgrade() {
                app.set_files_loaded(false);
                app.set_status_text("File changed — re-calibrate or load calibration".into());
            }
        }
        if let Some(app) = app_weak.upgrade() {
            app.set_left_path(display_name(&path).into());
        }
        s.user_settings.push_left(path.clone());
        if let Some(app) = app_weak.upgrade() {
            sync_recent_paths(&s.user_settings, &app);
        }
        s.left_path = Some(path);
        drop(s);
        try_init_and_update(&state_ref, &app_weak);
    });

    let app_weak = app.as_weak();
    let state_ref = Rc::clone(&state);
    app.on_load_recent_right(move |entry| {
        let path = PathBuf::from(entry.as_str());
        let mut s = state_ref.borrow_mut();
        let changed = s.right_path.as_ref() != Some(&path);
        if changed && s.bridge.is_some() {
            s.unload_pipeline();
            if let Some(app) = app_weak.upgrade() {
                app.set_files_loaded(false);
                app.set_status_text("File changed — re-calibrate or load calibration".into());
            }
        }
        if let Some(app) = app_weak.upgrade() {
            app.set_right_path(display_name(&path).into());
        }
        s.user_settings.push_right(path.clone());
        if let Some(app) = app_weak.upgrade() {
            sync_recent_paths(&s.user_settings, &app);
        }
        s.right_path = Some(path);
        drop(s);
        try_init_and_update(&state_ref, &app_weak);
    });

    let app_weak = app.as_weak();
    let state_ref = Rc::clone(&state);
    app.on_load_recent_calibration(move |entry| {
        let path = PathBuf::from(entry.as_str());
        let mut s = state_ref.borrow_mut();
        let changed = s.calibration_path.as_ref() != Some(&path);
        if changed && s.bridge.is_some() {
            s.unload_pipeline();
            if let Some(app) = app_weak.upgrade() {
                app.set_files_loaded(false);
                app.set_status_text("Calibration changed — reloading".into());
            }
        }
        if let Some(app) = app_weak.upgrade() {
            app.set_calibration_path(display_name(&path).into());
        }
        s.user_settings.push_calibration(path.clone());
        if let Some(app) = app_weak.upgrade() {
            sync_recent_paths(&s.user_settings, &app);
        }
        s.calibration_path = Some(path);
        drop(s);
        try_init_and_update(&state_ref, &app_weak);
    });

    let app_weak = app.as_weak();
    let state_ref = Rc::clone(&state);
    app.on_clear_recent_files(move || {
        let mut s = state_ref.borrow_mut();
        s.user_settings.recent_left.clear();
        s.user_settings.recent_right.clear();
        s.user_settings.recent_calibration.clear();
        s.user_settings.save();
        if let Some(app) = app_weak.upgrade() {
            sync_recent_paths(&s.user_settings, &app);
        }
    });

    // ── File management callbacks (left panel) ──

    let app_weak = app.as_weak();
    let state_ref = Rc::clone(&state);
    app.on_clear_left(move || {
        let mut s = state_ref.borrow_mut();
        if s.bridge.is_some() {
            s.unload_pipeline();
        }
        s.left_path = None;
        s.left_input = None;
        s.persist_left_segments();
        if let Some(app) = app_weak.upgrade() {
            app.set_left_path("".into());
            app.set_files_loaded(false);
            app.set_status_text("Left video cleared. Calibration preserved.".into());
            sync_segments(&s, &app);
        }
    });

    let app_weak = app.as_weak();
    let state_ref = Rc::clone(&state);
    app.on_clear_right(move || {
        let mut s = state_ref.borrow_mut();
        if s.bridge.is_some() {
            s.unload_pipeline();
        }
        s.right_path = None;
        s.right_input = None;
        s.persist_right_segments();
        if let Some(app) = app_weak.upgrade() {
            app.set_right_path("".into());
            app.set_files_loaded(false);
            app.set_status_text("Right video cleared. Calibration preserved.".into());
            sync_segments(&s, &app);
        }
    });

    let app_weak = app.as_weak();
    let state_ref = Rc::clone(&state);
    app.on_clear_calibration(move || {
        let mut s = state_ref.borrow_mut();
        if s.bridge.is_some() {
            s.unload_pipeline();
        }
        s.calibration_path = None;
        s.calibration = None;
        if let Some(app) = app_weak.upgrade() {
            app.set_calibration_path("".into());
            app.set_files_loaded(false);
            app.set_status_text("Calibration cleared".into());
        }
    });

    // In-app field-ROI point editor (replaces the old browser-based
    // tool): click the lens preview to add a point, drag an existing one
    // to move it, right-click to delete it. All four callbacks read/
    // write `s.calibration.field_roi` for `s.lens_preview_side`; drag
    // moves are in-memory only (no disk write) and `roi_pointer_up`
    // flushes the save once per gesture, reusing the same persistence
    // tail `on_paste_roi` below uses.
    let state_ref = Rc::clone(&state);
    // Read-only: hit-tests an existing point without mutating anything.
    // The Slint side uses this on pointer-down to decide whether the
    // press landed on a point (start dragging it) or empty space
    // (tentatively a click-to-add, confirmed only on release if the
    // pointer never moved far enough to count as a pan instead - see
    // `roi_pointer_add`).
    app.on_roi_pointer_hit_test(move |lx, ly, cw, ch| {
        if cw <= 0.0 || ch <= 0.0 {
            return -1;
        }
        let s = state_ref.borrow();
        let is_right = s.lens_preview_side == "right";
        let Some(cal) = s.calibration.as_ref() else {
            return -1;
        };
        let Some(roi) = cal.field_roi.as_ref() else {
            return -1;
        };
        let pts = if is_right { &roi.right } else { &roi.left };
        let lens = &cal.lenses[if is_right { 1 } else { 0 }];
        let display: Vec<[f64; 2]> = pts
            .iter()
            .map(|p| raw_norm_to_rectified_norm(p[0], p[1], lens))
            .collect();
        roi_hit_test(&display, lx, ly, cw, ch)
            .map(|i| i as i32)
            .unwrap_or(-1)
    });

    let app_weak = app.as_weak();
    let state_ref = Rc::clone(&state);
    app.on_roi_pointer_add(move |lx, ly, cw, ch| {
        if cw <= 0.0 || ch <= 0.0 {
            return;
        }
        let mut s = state_ref.borrow_mut();
        let is_right = s.lens_preview_side == "right";
        let rectified = [
            (lx / cw).clamp(0.0, 1.0) as f64,
            (ly / ch).clamp(0.0, 1.0) as f64,
        ];
        if let Some(cal) = s.calibration.as_mut() {
            let lens = cal.lenses[if is_right { 1 } else { 0 }].clone();
            let norm = rectified_norm_to_raw_norm(rectified[0], rectified[1], &lens);
            let roi = cal.field_roi.get_or_insert_with(Default::default);
            let pts = if is_right {
                &mut roi.right
            } else {
                &mut roi.left
            };
            let idx = roi_insert_index(pts, norm);
            pts.insert(idx, norm);
        }
        // A confirmed click-to-add is a complete gesture on its own (no
        // separate `roi_pointer_up` follows it), so save right away.
        if let Err(e) = s.save_calibration() {
            log::error!("Failed to save calibration after ROI point add: {e}");
        }
        if let Some(app) = app_weak.upgrade() {
            app.set_has_roi(true);
            sync_roi_points(&s, &app);
        }
    });

    let app_weak = app.as_weak();
    let state_ref = Rc::clone(&state);
    app.on_roi_pointer_drag(move |index, lx, ly, cw, ch| {
        if cw <= 0.0 || ch <= 0.0 || index < 0 {
            return;
        }
        let mut s = state_ref.borrow_mut();
        let is_right = s.lens_preview_side == "right";
        let rectified = [
            (lx / cw).clamp(0.0, 1.0) as f64,
            (ly / ch).clamp(0.0, 1.0) as f64,
        ];
        if let Some(cal) = s.calibration.as_mut() {
            let lens = cal.lenses[if is_right { 1 } else { 0 }].clone();
            let norm = rectified_norm_to_raw_norm(rectified[0], rectified[1], &lens);
            if let Some(roi) = cal.field_roi.as_mut() {
                let pts = if is_right {
                    &mut roi.right
                } else {
                    &mut roi.left
                };
                if let Some(p) = pts.get_mut(index as usize) {
                    *p = norm;
                }
            }
        }
        if let Some(app) = app_weak.upgrade() {
            sync_roi_points(&s, &app);
        }
    });

    let state_ref = Rc::clone(&state);
    app.on_roi_pointer_up(move || {
        let s = state_ref.borrow();
        if let Err(e) = s.save_calibration() {
            log::error!("Failed to save calibration after ROI edit: {e}");
        }
    });

    let app_weak = app.as_weak();
    let state_ref = Rc::clone(&state);
    app.on_roi_pointer_delete(move |lx, ly, cw, ch| {
        if cw <= 0.0 || ch <= 0.0 {
            return;
        }
        let mut s = state_ref.borrow_mut();
        let is_right = s.lens_preview_side == "right";
        let Some(i) = s.calibration.as_ref().and_then(|cal| {
            let roi = cal.field_roi.as_ref()?;
            let pts = if is_right { &roi.right } else { &roi.left };
            let lens = &cal.lenses[if is_right { 1 } else { 0 }];
            let display: Vec<[f64; 2]> = pts
                .iter()
                .map(|p| raw_norm_to_rectified_norm(p[0], p[1], lens))
                .collect();
            roi_hit_test(&display, lx, ly, cw, ch)
        }) else {
            return;
        };
        if let Some(roi) = s.calibration.as_mut().and_then(|c| c.field_roi.as_mut()) {
            let pts = if is_right {
                &mut roi.right
            } else {
                &mut roi.left
            };
            pts.remove(i);
        }
        let has = s
            .calibration
            .as_ref()
            .and_then(|c| c.field_roi.as_ref())
            .is_some_and(|r| !r.left.is_empty() || !r.right.is_empty());
        if let Err(e) = s.save_calibration() {
            log::error!("Failed to save calibration after ROI point delete: {e}");
        }
        if let Some(app) = app_weak.upgrade() {
            app.set_has_roi(has);
            sync_roi_points(&s, &app);
        }
    });

    let app_weak = app.as_weak();
    let state_ref = Rc::clone(&state);
    app.on_paste_roi(move || {
        let mut s = state_ref.borrow_mut();
        // Try manual JSON input first, then clipboard
        let manual = app_weak
            .upgrade()
            .map(|a| {
                let t = a.get_roi_manual_json().to_string();
                a.set_roi_manual_json("".into());
                t
            })
            .unwrap_or_default();
        let clipboard_text = if !manual.trim().is_empty() {
            manual
        } else {
            match arboard::Clipboard::new().and_then(|mut cb| cb.get_text()) {
                Ok(t) => t,
                Err(e) => {
                    log::warn!("Clipboard read failed: {e}");
                    if let Some(app) = app_weak.upgrade() {
                        s.toasts.push(
                            crate::toast::Severity::Error,
                            "Paste ROI",
                            "Could not read clipboard. Paste JSON in the text field instead.",
                        );
                        crate::toast::sync_to_ui(&s.toasts, &app);
                    }
                    return;
                }
            }
        };

        let roi: reco_core::calibration::FieldRoi = match serde_json::from_str(&clipboard_text) {
            Ok(r) => r,
            Err(e) => {
                log::warn!("Clipboard is not valid ROI JSON: {e}");
                if let Some(app) = app_weak.upgrade() {
                    s.toasts.push(
                        crate::toast::Severity::Error,
                        "Paste ROI",
                        "Clipboard doesn't contain valid ROI JSON. Save ROI in the browser editor first.",
                    );
                    crate::toast::sync_to_ui(&s.toasts, &app);
                }
                return;
            }
        };

        let point_count = roi.left.len() + roi.right.len();
        if let Some(cal) = s.calibration.as_mut() {
            cal.field_roi = Some(roi);
        }
        if let Err(e) = s.save_calibration() {
            log::error!("Failed to save calibration with ROI: {e}");
        }

        if let Some(app) = app_weak.upgrade() {
            let has = point_count > 0;
            app.set_has_roi(has);
            sync_roi_points(&s, &app);
            s.toasts.push(
                crate::toast::Severity::Info,
                "ROI applied",
                format!("{point_count} points saved to calibration."),
            );
            crate::toast::sync_to_ui(&s.toasts, &app);
        }
    });

    let app_weak = app.as_weak();
    let state_ref = Rc::clone(&state);
    app.on_remove_left_segment(move |idx| {
        let mut s = state_ref.borrow_mut();
        let removed = match s.left_input {
            Some(reco_io::stitch_job::InputPath::Single(_)) if idx == 0 => {
                s.left_input = None;
                s.left_path = None;
                true
            }
            Some(reco_io::stitch_job::InputPath::Chained(ref mut paths)) => {
                let idx = idx as usize;
                if idx < paths.len() {
                    paths.remove(idx);
                    if paths.is_empty() {
                        s.left_input = None;
                        s.left_path = None;
                    } else if paths.len() == 1 {
                        let p = paths[0].clone();
                        s.left_path = Some(p.clone());
                        s.left_input = Some(reco_io::stitch_job::InputPath::Single(p));
                    } else {
                        s.left_path = Some(paths[0].clone());
                    }
                    true
                } else {
                    false
                }
            }
            _ => false,
        };
        if removed {
            s.persist_left_segments();
            if s.bridge.is_some() {
                s.unload_pipeline();
            }
            if let Some(app) = app_weak.upgrade() {
                let label = s
                    .left_input
                    .as_ref()
                    .map(|i| match i {
                        reco_io::stitch_job::InputPath::Single(p) => display_name(p),
                        reco_io::stitch_job::InputPath::Chained(ps) => {
                            format!("{} ({} segments)", display_name(&ps[0]), ps.len())
                        }
                    })
                    .unwrap_or_default();
                app.set_left_path(label.into());
                app.set_files_loaded(false);
                sync_segments(&s, &app);
            }
        }
    });

    let app_weak = app.as_weak();
    let state_ref = Rc::clone(&state);
    app.on_remove_right_segment(move |idx| {
        let mut s = state_ref.borrow_mut();
        let removed = match s.right_input {
            Some(reco_io::stitch_job::InputPath::Single(_)) if idx == 0 => {
                s.right_input = None;
                s.right_path = None;
                true
            }
            Some(reco_io::stitch_job::InputPath::Chained(ref mut paths)) => {
                let idx = idx as usize;
                if idx < paths.len() {
                    paths.remove(idx);
                    if paths.is_empty() {
                        s.right_input = None;
                        s.right_path = None;
                    } else if paths.len() == 1 {
                        let p = paths[0].clone();
                        s.right_path = Some(p.clone());
                        s.right_input = Some(reco_io::stitch_job::InputPath::Single(p));
                    } else {
                        s.right_path = Some(paths[0].clone());
                    }
                    true
                } else {
                    false
                }
            }
            _ => false,
        };
        if removed {
            s.persist_right_segments();
            if s.bridge.is_some() {
                s.unload_pipeline();
            }
            if let Some(app) = app_weak.upgrade() {
                let label = s
                    .right_input
                    .as_ref()
                    .map(|i| match i {
                        reco_io::stitch_job::InputPath::Single(p) => display_name(p),
                        reco_io::stitch_job::InputPath::Chained(ps) => {
                            format!("{} ({} segments)", display_name(&ps[0]), ps.len())
                        }
                    })
                    .unwrap_or_default();
                app.set_right_path(label.into());
                app.set_files_loaded(false);
                sync_segments(&s, &app);
            }
        }
    });

    // Cut ranges: exclude a sub-window from the export (e.g. a halftime
    // pause). "Add" drops a new 2s marker at the playhead (or centered in
    // the export window if the playhead sits outside it), clamped so it
    // starts non-negative and never extends past export-end/clip
    // duration - a degenerate zero-width range would otherwise be
    // possible right at the very end of the clip.
    let app_weak = app.as_weak();
    let state_ref = Rc::clone(&state);
    app.on_cut_range_add(move || {
        let mut s = state_ref.borrow_mut();
        let Some(app) = app_weak.upgrade() else {
            return;
        };
        let export_start = app.get_export_start_secs() as f64;
        let export_end = if app.get_export_end_secs() > 0.0 {
            app.get_export_end_secs() as f64
        } else {
            app.get_clip_duration_secs() as f64
        };
        let playhead_secs = if app.get_total_frames() > 0 {
            app.get_current_frame() as f64 / app.get_total_frames() as f64
                * app.get_clip_duration_secs() as f64
        } else {
            export_start
        };
        let default_width = 2.0_f64;
        let start = playhead_secs.clamp(export_start, (export_end - 0.2).max(export_start));
        let end = (start + default_width).min(export_end);
        if end - start < 0.2 {
            // Not enough room left in the export window for even a
            // minimal range - nothing sensible to add.
            return;
        }
        s.cut_ranges.push((start, end));
        sync_cut_ranges(&s, &app);
    });

    let app_weak = app.as_weak();
    let state_ref = Rc::clone(&state);
    app.on_cut_range_remove(move |idx| {
        let mut s = state_ref.borrow_mut();
        let idx = idx as usize;
        if idx < s.cut_ranges.len() {
            s.cut_ranges.remove(idx);
            if let Some(app) = app_weak.upgrade() {
                sync_cut_ranges(&s, &app);
            }
        }
    });

    // Commits a drag/edit's final (start, end) for one range. Clamps to
    // the clip bounds and a minimum 0.2s width; does NOT reject overlap
    // with a neighboring range here (StitchJob::run already validates
    // that loudly at export time via cut_range::validate_cut_ranges) -
    // live overlap-prevention while dragging two bands past each other
    // would need more UI design than a first pass warrants.
    let app_weak = app.as_weak();
    let state_ref = Rc::clone(&state);
    app.on_cut_range_update(move |idx, start, end| {
        let mut s = state_ref.borrow_mut();
        let idx = idx as usize;
        let Some(range) = s.cut_ranges.get_mut(idx) else {
            return;
        };
        let clip_duration = app_weak
            .upgrade()
            .map(|app| app.get_clip_duration_secs() as f64)
            .unwrap_or(f64::MAX);
        let start = (start as f64).max(0.0);
        let end = (end as f64).min(clip_duration);
        if end - start < 0.2 {
            // Reject a collapsed/inverted drag outright rather than
            // clamping to some arbitrary minimum the user didn't ask
            // for - the UI just snaps back to the last committed value.
        } else {
            *range = (start, end);
        }
        let committed = s.cut_ranges.get(idx).copied();
        if let (Some(app), Some((start, end))) = (app_weak.upgrade(), committed) {
            // Patch just this row's data in the EXISTING model rather than
            // sync_cut_ranges's full ModelRc replace: `edited(v)` fires on
            // every keystroke (see NumEdit's TextInput `edited` forwarding),
            // and replacing the whole model tears down and recreates every
            // row's widgets - including the TextInput the user is actively
            // typing into - dropping keyboard focus after each character.
            // `set_row_data` updates the row in place and keeps focus.
            // Found by the user: only the first typed digit registered,
            // every next one needed a re-click first.
            slint::Model::set_row_data(
                &app.get_cut_ranges(),
                idx,
                CutRangeItem {
                    start_secs: start as f32,
                    end_secs: end as f32,
                },
            );
        }
    });

    // Drag-to-reorder within a side. The segments are the same cameras in a
    // new temporal order, so the calibration still applies: reorder the
    // chained paths and rebuild the preview. The frame total is unchanged,
    // so the export trim is deliberately left as-is.
    let app_weak = app.as_weak();
    let state_ref = Rc::clone(&state);
    app.on_reorder_left_segment(move |from, to| {
        let mut s = state_ref.borrow_mut();
        let (from, to) = (from as usize, to as usize);
        let new_first = match s.left_input {
            Some(reco_io::stitch_job::InputPath::Chained(ref mut paths)) => {
                if from < paths.len() && to < paths.len() && from != to {
                    let p = paths.remove(from);
                    paths.insert(to, p);
                    Some(paths[0].clone())
                } else {
                    None
                }
            }
            _ => None,
        };
        let Some(first) = new_first else {
            return;
        };
        log::info!("Left: reordered segment {from} -> {to}");
        s.left_path = Some(first);
        s.persist_left_segments();
        if let Some(app) = app_weak.upgrade() {
            if let Some(reco_io::stitch_job::InputPath::Chained(ps)) = s.left_input.as_ref() {
                app.set_left_path(
                    format!("{} ({} segments)", display_name(&ps[0]), ps.len()).into(),
                );
            }
            sync_segments(&s, &app);
        }
        drop(s);
        // Re-open the preview source off the drop handler: the list reorders
        // and repaints immediately, then the (slow) 4K concat re-probe runs on
        // the next tick instead of freezing the UI on every drop.
        let reopen = Rc::clone(&state_ref);
        slint::Timer::single_shot(std::time::Duration::from_millis(40), move || {
            reopen.borrow_mut().reopen_source();
        });
    });

    let app_weak = app.as_weak();
    let state_ref = Rc::clone(&state);
    app.on_reorder_right_segment(move |from, to| {
        let mut s = state_ref.borrow_mut();
        let (from, to) = (from as usize, to as usize);
        let new_first = match s.right_input {
            Some(reco_io::stitch_job::InputPath::Chained(ref mut paths)) => {
                if from < paths.len() && to < paths.len() && from != to {
                    let p = paths.remove(from);
                    paths.insert(to, p);
                    Some(paths[0].clone())
                } else {
                    None
                }
            }
            _ => None,
        };
        let Some(first) = new_first else {
            return;
        };
        log::info!("Right: reordered segment {from} -> {to}");
        s.right_path = Some(first);
        s.persist_right_segments();
        if let Some(app) = app_weak.upgrade() {
            if let Some(reco_io::stitch_job::InputPath::Chained(ps)) = s.right_input.as_ref() {
                app.set_right_path(
                    format!("{} ({} segments)", display_name(&ps[0]), ps.len()).into(),
                );
            }
            sync_segments(&s, &app);
        }
        drop(s);
        // Re-open the preview source off the drop handler (see left handler).
        let reopen = Rc::clone(&state_ref);
        slint::Timer::single_shot(std::time::Duration::from_millis(40), move || {
            reopen.borrow_mut().reopen_source();
        });
    });

    // ── Preferences dialog callbacks ──
    //
    // Open prefills the prefs-* properties from user_settings; Save
    // reads them back and persists. Cancel just closes - no state
    // change needed because the Slint properties are scratch space
    // that gets re-seeded on next open.
    let app_weak = app.as_weak();
    let state_ref = Rc::clone(&state);
    app.on_open_prefs_dialog(move || {
        let Some(app) = app_weak.upgrade() else {
            return;
        };
        let s = state_ref.borrow();
        app.set_prefs_default_codec(s.user_settings.default_codec.clone().into());
        app.set_prefs_default_quality(s.user_settings.default_quality.clone().into());
        app.set_prefs_default_blend_width(s.user_settings.default_blend_width);
        app.set_prefs_ai_model_path(
            s.user_settings
                .ai_model_path
                .as_ref()
                .map(|p| p.to_string_lossy().into_owned())
                .unwrap_or_default()
                .into(),
        );
        app.set_prefs_default_calibration_path(
            s.user_settings
                .default_calibration_path
                .as_ref()
                .map(|p| p.to_string_lossy().into_owned())
                .unwrap_or_default()
                .into(),
        );
        app.set_recording_codec(s.user_settings.recording_codec.clone().into());
        app.set_recording_quality(s.user_settings.recording_quality.clone().into());
        app.set_recording_folder(
            s.user_settings
                .recording_folder
                .as_ref()
                .map(|p| p.to_string_lossy().into_owned())
                .unwrap_or_default()
                .into(),
        );
        app.set_prefs_telemetry_enabled(s.user_settings.telemetry_enabled);
        app.set_prefs_dark_mode(s.user_settings.dark_mode);
        app.set_prefs_dialog_open(true);
    });

    let app_weak = app.as_weak();
    let state_ref = Rc::clone(&state);
    app.on_save_prefs(move || {
        let Some(app) = app_weak.upgrade() else {
            return;
        };
        let mut s = state_ref.borrow_mut();
        s.user_settings.default_codec = app.get_prefs_default_codec().to_string();
        s.user_settings.default_quality = app.get_prefs_default_quality().to_string();
        s.user_settings.default_blend_width = app.get_prefs_default_blend_width();
        let model_path = app.get_prefs_ai_model_path().to_string();
        s.user_settings.ai_model_path = if model_path.is_empty() {
            None
        } else {
            Some(PathBuf::from(model_path))
        };
        let default_cal_path = app.get_prefs_default_calibration_path().to_string();
        s.user_settings.default_calibration_path = if default_cal_path.is_empty() {
            None
        } else {
            Some(PathBuf::from(default_cal_path))
        };
        s.user_settings.recording_codec = app.get_recording_codec().to_string();
        s.user_settings.recording_quality = app.get_recording_quality().to_string();
        let rec_folder = app.get_recording_folder().to_string();
        s.user_settings.recording_folder = if rec_folder.is_empty() {
            None
        } else {
            Some(PathBuf::from(rec_folder))
        };
        let dark = app.get_prefs_dark_mode();
        s.user_settings.dark_mode = dark;
        app.set_dark_mode(dark);

        let telemetry_wanted = app.get_prefs_telemetry_enabled();
        s.user_settings.telemetry_enabled = telemetry_wanted;
        if telemetry_wanted && s.telemetry.is_none() {
            let cid = s
                .user_settings
                .telemetry_client_id
                .clone()
                .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
            log::info!("Telemetry enabled by user (client_id={}).", &cid[..8]);
            let client = telemetry_client::TelemetryClient::new(cid);
            client.app_open();
            s.telemetry = Some(client);
        } else if !telemetry_wanted {
            if s.telemetry.is_some() {
                log::info!("Telemetry disabled by user.");
            }
            s.telemetry = None;
        }
        s.user_settings.save();
    });

    // Floating debug-log window: created lazily on first click, then
    // just shown/hidden after that so its position and size persist
    // across opens within the session. `on_close_requested` hides
    // rather than destroys the window when the user clicks the native
    // close button, matching that reuse.
    let state_ref = Rc::clone(&state);
    app.on_open_debug_window(move || {
        let mut s = state_ref.borrow_mut();
        if s.debug_window.is_none() {
            match DebugWindow::new() {
                Ok(dw) => {
                    dw.window()
                        .on_close_requested(|| slint::CloseRequestResponse::HideWindow);
                    s.debug_window = Some(dw);
                }
                Err(e) => {
                    log::warn!("Failed to create debug window: {e}");
                    return;
                }
            }
        }
        let dw = s.debug_window.as_ref().unwrap();
        dw.set_log_text(debug_log_snapshot().into());
        if let Err(e) = dw.show() {
            log::warn!("Failed to show debug window: {e}");
        }
    });

    app.on_open_website(|| {
        let _ = open::that("https://github.com/reco-project/video-stitcher");
    });

    app.on_open_forum(|| {
        let _ = open::that("https://forum.reco-project.org/");
    });

    // User-initiated now (toolbar button, see `update-available` in
    // main.slint) rather than an automatic browser-open the moment the
    // background version check finds a newer release.
    let app_weak = app.as_weak();
    app.on_open_update_page(move || {
        let Some(app) = app_weak.upgrade() else {
            return;
        };
        let tag = app.get_update_tag();
        let url = format!("https://github.com/reco-project/video-stitcher/releases/tag/{tag}");
        let _ = open::that(&url);
    });

    let app_weak = app.as_weak();
    app.on_pick_prefs_model(move || {
        let dialog = rfd::FileDialog::new()
            .set_title("Select YOLO ONNX model")
            .add_filter("ONNX", &["onnx"]);
        if let Some(path) = dialog.pick_file()
            && let Some(app) = app_weak.upgrade()
        {
            app.set_prefs_ai_model_path(path.to_string_lossy().to_string().into());
        }
    });

    let app_weak = app.as_weak();
    app.on_pick_prefs_default_calibration(move || {
        let dialog = rfd::FileDialog::new()
            .set_title("Select default calibration")
            .add_filter("Calibration JSON", &["json"]);
        if let Some(path) = dialog.pick_file()
            && let Some(app) = app_weak.upgrade()
        {
            app.set_prefs_default_calibration_path(path.to_string_lossy().to_string().into());
        }
    });

    let app_weak = app.as_weak();
    app.on_pick_recording_folder(move || {
        let dialog = rfd::FileDialog::new().set_title("Select default recording folder");
        if let Some(folder) = dialog.pick_folder()
            && let Some(app) = app_weak.upgrade()
        {
            app.set_recording_folder(folder.to_string_lossy().to_string().into());
        }
    });

    let state_ref = Rc::clone(&state);
    app.on_changed_preview_aspect(move |aspect| {
        let mut s = state_ref.borrow_mut();
        s.user_settings.preview_aspect = aspect.to_string();
        s.user_settings.save();
    });

    let app_weak = app.as_weak();
    let state_ref = Rc::clone(&state);
    app.on_changed_scoreboard(move || {
        let Some(app) = app_weak.upgrade() else {
            return;
        };
        let mut s = state_ref.borrow_mut();
        s.configure_scoreboard(
            app.get_scoreboard_enabled(),
            app.get_scoreboard_current_index().max(0) as usize,
        );
        app.set_scoreboard_error_text(s.scoreboard_error.clone().into());
        app.set_scoreboard_editor_available(
            s.scoreboard_runtime
                .as_ref()
                .and_then(reco_scoreboard::ScoreboardRuntime::editor_url)
                .is_some(),
        );
        let network_editor_url = s
            .scoreboard_runtime
            .as_ref()
            .and_then(reco_scoreboard::ScoreboardRuntime::network_editor_url);
        app.set_scoreboard_share_available(network_editor_url.is_some());
        app.set_scoreboard_share_url(network_editor_url.unwrap_or_default().into());
        if network_editor_url.is_none() {
            app.set_scoreboard_share_dialog_open(false);
        }
        persist_scoreboard_settings(&app, &mut s);
    });

    let app_weak = app.as_weak();
    let state_ref = Rc::clone(&state);
    app.on_edit_scoreboard(move || {
        let Some(app) = app_weak.upgrade() else {
            return;
        };
        let editor_url = state_ref
            .borrow()
            .scoreboard_runtime
            .as_ref()
            .and_then(reco_scoreboard::ScoreboardRuntime::editor_url)
            .map(str::to_owned);
        let Some(editor_url) = editor_url else {
            app.set_scoreboard_error_text("This scoreboard has no editor".into());
            return;
        };
        if let Err(error) = open::that(&editor_url) {
            let message = format!("Cannot open scoreboard editor: {error}");
            state_ref.borrow_mut().scoreboard_error.clone_from(&message);
            app.set_scoreboard_error_text(message.into());
        }
    });

    let app_weak = app.as_weak();
    app.on_copy_scoreboard_share(move || {
        let Some(app) = app_weak.upgrade() else {
            return;
        };
        let url = app.get_scoreboard_share_url().to_string();
        if url.is_empty() {
            return;
        }
        if let Err(error) =
            arboard::Clipboard::new().and_then(|mut clipboard| clipboard.set_text(url))
        {
            app.set_scoreboard_error_text(
                format!("Cannot copy scoreboard address: {error}").into(),
            );
        }
    });

    let app_weak = app.as_weak();
    let state_ref = Rc::clone(&state);
    app.on_load_scoreboard_events(move || {
        let Some(app) = app_weak.upgrade() else {
            return;
        };
        let dialog = rfd::FileDialog::new()
            .set_title("Load Match Logger export")
            .add_filter("Match Logger export", &["json"]);
        let Some(path) = dialog.pick_file() else {
            return;
        };
        match scoreboard_import::load(&path) {
            Ok(export) => {
                let mut s = state_ref.borrow_mut();
                adopt_match_logger_export(&app, &mut s, path, export);
                // Loading a different export while a toggle is already
                // on would otherwise leave the previous file's derived
                // ranges sitting there, silently stale.
                refresh_derived_cut_ranges(&mut s, &app);
                persist_scoreboard_settings(&app, &mut s);
            }
            Err(error) => {
                app.set_scoreboard_error_text(error.to_string().into());
            }
        }
    });

    let app_weak = app.as_weak();
    let state_ref = Rc::clone(&state);
    app.on_set_scoreboard_sync_point(move || {
        let Some(app) = app_weak.upgrade() else {
            return;
        };
        let mut s = state_ref.borrow_mut();
        let Some(export) = s.scoreboard_import.as_ref() else {
            return;
        };
        let Some(video_start_ms) = export.video_start_ms() else {
            app.set_scoreboard_error_text("This export has no video_start event to sync to".into());
            return;
        };
        let video_seconds = if s.playback.fps() > 0.0 {
            s.playback.frame_index() as f64 / s.playback.fps()
        } else {
            0.0
        };
        s.scoreboard_sync_anchor = Some(scoreboard_import::SyncAnchor {
            event_ts_ms: video_start_ms,
            video_seconds,
        });
        s.preview_dirty = true;
        app.set_scoreboard_sync_label(
            format!("Synced: video_start = {video_seconds:.1}s into this video").into(),
        );
        // Keep the derived cut ranges in step with a corrected sync point
        // instead of leaving them stale from the old anchor.
        refresh_derived_cut_ranges(&mut s, &app);
        persist_scoreboard_settings(&app, &mut s);
    });

    let app_weak = app.as_weak();
    let state_ref = Rc::clone(&state);
    app.on_toggled_scoreboard_derive_cut_ranges(move |enabled| {
        let Some(app) = app_weak.upgrade() else {
            return;
        };
        let mut s = state_ref.borrow_mut();
        if !enabled {
            let previous = std::mem::take(&mut s.scoreboard_derived_cut_ranges);
            if !previous.is_empty() {
                s.cut_ranges.retain(|r| !previous.contains(r));
            }
            sync_cut_ranges(&s, &app);
            persist_scoreboard_settings(&app, &mut s);
            return;
        }
        if s.scoreboard_import.is_none() || s.scoreboard_sync_anchor.is_none() {
            app.set_scoreboard_error_text(
                "Load a Match Logger export and set its sync point first".into(),
            );
            app.set_scoreboard_derive_cut_ranges(false);
            return;
        }
        // Mutually exclusive with highlights, from the other side.
        app.set_scoreboard_derive_highlights(false);
        refresh_derived_cut_ranges(&mut s, &app);
        persist_scoreboard_settings(&app, &mut s);
    });

    // The PAUZE checkbox or one of its two durations changed - persist
    // app-level so they survive a restart (they used to reset to
    // 3.0/4.0 every session).
    let app_weak = app.as_weak();
    let state_ref = Rc::clone(&state);
    app.on_changed_pause_overlay(move || {
        let Some(app) = app_weak.upgrade() else {
            return;
        };
        state_ref.borrow_mut().user_settings.set_pause_overlay(
            app.get_export_pause_overlay_enabled(),
            app.get_export_pause_overlay_fade_secs(),
            app.get_export_pause_overlay_hold_secs(),
        );
    });

    // One of the four auto-cut margin fields was edited. Re-derives in
    // place so the timeline shows the new boundaries immediately - the
    // whole point of the fields is judging the result against the
    // preview, which needs no re-toggling to see.
    let app_weak = app.as_weak();
    let state_ref = Rc::clone(&state);
    app.on_changed_autocut_margins(move || {
        let Some(app) = app_weak.upgrade() else {
            return;
        };
        let mut s = state_ref.borrow_mut();
        refresh_derived_cut_ranges(&mut s, &app);
        persist_scoreboard_settings(&app, &mut s);
    });

    // Same for the two highlight margins. Kept a separate callback from
    // the auto-cut one so each set of fields reads as belonging to its
    // own checkbox, even though both end in the same re-derive.
    let app_weak = app.as_weak();
    let state_ref = Rc::clone(&state);
    app.on_changed_highlights_margins(move || {
        let Some(app) = app_weak.upgrade() else {
            return;
        };
        let mut s = state_ref.borrow_mut();
        refresh_derived_cut_ranges(&mut s, &app);
        persist_scoreboard_settings(&app, &mut s);
    });

    let app_weak = app.as_weak();
    let state_ref = Rc::clone(&state);
    app.on_toggled_scoreboard_derive_highlights(move |enabled| {
        let Some(app) = app_weak.upgrade() else {
            return;
        };
        let mut s = state_ref.borrow_mut();
        if enabled {
            if s.scoreboard_import.is_none() || s.scoreboard_sync_anchor.is_none() {
                app.set_scoreboard_error_text(
                    "Load a Match Logger export and set its sync point first".into(),
                );
                app.set_scoreboard_derive_highlights(false);
                return;
            }
            // The two modes are mutually exclusive - see
            // `refresh_derived_cut_ranges`. Clearing the other checkbox
            // rather than silently ignoring it keeps the UI honest about
            // which one is in effect.
            app.set_scoreboard_derive_cut_ranges(false);
        }
        refresh_derived_cut_ranges(&mut s, &app);
        if enabled && s.scoreboard_derived_cut_ranges.is_empty() {
            // Nothing was derived, so nothing changed - the log has no
            // goals to build a reel from. Say so instead of leaving a
            // ticked box that did nothing.
            app.set_scoreboard_derive_highlights(false);
            app.set_scoreboard_error_text(
                "No goals in this Match Logger export - nothing to make highlights from".into(),
            );
        }
        persist_scoreboard_settings(&app, &mut s);
    });

    let app_weak = app.as_weak();
    let state_ref = Rc::clone(&state);
    app.on_changed_scoreboard_placement(move || {
        let Some(app) = app_weak.upgrade() else {
            return;
        };
        let placement = reco_core::render::overlay::OverlayPlacement {
            offset: (app.get_scoreboard_offset_x(), app.get_scoreboard_offset_y()),
            scale: app.get_scoreboard_scale(),
        };
        let mut s = state_ref.borrow_mut();
        s.scoreboard_placement = placement;
        if let Some(bridge) = s.bridge.as_mut() {
            bridge.set_overlay_placement(placement);
        }
        s.apply_scoreboard_render_scale();
        // Without this, the preview only redraws while playing/seeking
        // (see vsync_render_tick's gate) - a drag while paused updated the
        // compositor instantly but the screen wouldn't reflect it until
        // something unrelated happened to trigger a redraw, which read as
        // a huge, unusable lag. seam_drag (same kind of live-drag-a-render-
        // parameter interaction) sets this for the same reason.
        s.preview_dirty = true;
        persist_scoreboard_settings(&app, &mut s);
    });

    let app_weak = app.as_weak();
    let state_ref = Rc::clone(&state);
    app.on_pick_scoreboard_logo(move |team| {
        let Some(app) = app_weak.upgrade() else {
            return;
        };
        let dialog = rfd::FileDialog::new()
            .set_title("Choose a team logo")
            .add_filter("Image", &["png", "jpg", "jpeg", "gif", "webp", "svg"]);
        let Some(path) = dialog.pick_file() else {
            return;
        };
        match scoreboard_import::image_data_uri(&path) {
            Ok(data_uri) => {
                let mut s = state_ref.borrow_mut();
                let display_path = path.to_string_lossy().into_owned();
                if team == "home" {
                    s.scoreboard_style.home_logo = Some(data_uri);
                    s.scoreboard_style.home_logo_path = Some(path);
                    drop(s);
                    app.set_scoreboard_home_logo_path(display_path.into());
                } else {
                    s.scoreboard_style.away_logo = Some(data_uri);
                    s.scoreboard_style.away_logo_path = Some(path);
                    drop(s);
                    app.set_scoreboard_away_logo_path(display_path.into());
                }
                // See on_changed_scoreboard_placement's comment - the same
                // gate applies to picking up the re-rendered overlay
                // texture once push_scoreboard_replay pushes this change.
                let mut s = state_ref.borrow_mut();
                s.preview_dirty = true;
                persist_scoreboard_settings(&app, &mut s);
            }
            Err(message) => {
                app.set_scoreboard_error_text(message.into());
            }
        }
    });

    let app_weak = app.as_weak();
    let state_ref = Rc::clone(&state);
    app.on_changed_scoreboard_font(move |font| {
        let Some(app) = app_weak.upgrade() else {
            return;
        };
        let mut s = state_ref.borrow_mut();
        s.scoreboard_style.font_family = if font.is_empty() {
            None
        } else {
            Some(font.to_string())
        };
        s.preview_dirty = true;
        persist_scoreboard_settings(&app, &mut s);
    });

    let app_weak = app.as_weak();
    let state_ref = Rc::clone(&state);
    app.on_changed_scoreboard_logo_size(move |size| {
        let Some(app) = app_weak.upgrade() else {
            return;
        };
        let mut s = state_ref.borrow_mut();
        s.scoreboard_style.logo_size_px = Some(size);
        s.preview_dirty = true;
        persist_scoreboard_settings(&app, &mut s);
    });

    let app_weak = app.as_weak();
    let state_ref = Rc::clone(&state);
    app.on_changed_scoreboard_banner_color(move |preset_name| {
        let Some(app) = app_weak.upgrade() else {
            return;
        };
        // Curated presets, matching the ComboBox in main.slint - a full
        // color picker isn't otherwise used anywhere in reco-gui yet, so
        // this stays consistent with the Font dropdown right above it
        // rather than introducing a new kind of control for one field.
        let hex = match preset_name.as_str() {
            "Navy" => Some("#0b1a33"),
            "Black" => Some("#0a0a0a"),
            "Forest Green" => Some("#0e2e1a"),
            "Maroon" => Some("#33101a"),
            "Purple" => Some("#241333"),
            _ => None,
        };
        let mut s = state_ref.borrow_mut();
        s.scoreboard_style.banner_color = hex.map(str::to_string);
        s.preview_dirty = true;
        persist_scoreboard_settings(&app, &mut s);
    });

    // ── Auto-calibration callback ──

    let app_weak = app.as_weak();
    let state_ref = Rc::clone(&state);
    app.on_auto_calibrate(move || {
        let s = state_ref.borrow();
        let (left, right) = match (&s.left_path, &s.right_path) {
            (Some(l), Some(r)) => (l.clone(), r.clone()),
            _ => return,
        };
        let fps = s.playback.fps();
        // Fallback anchor: current playback position, in case the user
        // hasn't typed an explicit "Anchor frame".
        let current_time_secs = if fps > 0.0 {
            s.playback.frame_index() as f64 / fps
        } else {
            0.0
        };
        drop(s);

        let (
            use_imu_seeds,
            cal_frames,
            akaze_threshold,
            detect_y_min,
            detect_y_max,
            anchor_frame_text,
            sample_window,
            force_x_rx,
            force_z_rz,
            full_res_features,
        ) = app_weak
            .upgrade()
            .map(|a| {
                (
                    a.get_use_imu_seeds(),
                    a.get_calibration_frames().max(2) as usize,
                    a.get_cal_akaze_threshold() as f64,
                    a.get_cal_detect_y_min() as f64,
                    a.get_cal_detect_y_max() as f64,
                    a.get_cal_anchor_frame_text().to_string(),
                    a.get_cal_sample_window_secs() as f64,
                    a.get_force_x_rx(),
                    a.get_force_z_rz(),
                    a.get_cal_full_res_features(),
                )
            })
            .unwrap_or((
                false,
                4,
                0.0001,
                0.05,
                0.95,
                String::new(),
                0.0,
                false,
                false,
                false,
            ));

        // The anchor is a manually typed frame number when set (an exact,
        // reproducible point - "work around frame 1000" - rather than
        // wherever the timeline happens to be scrubbed to); an empty or
        // unparseable field falls back to the current playback position,
        // same as before this field existed.
        //
        // Deliberately computed from the GUI's own `fps`/current-frame
        // only, NOT from a total-duration bound here - see the comment
        // in the background thread below for why that bound has to come
        // from the single file `calibrate_videos` will actually read,
        // not from the (possibly multi-segment, much longer) preview
        // timeline this callback would otherwise reach for.
        let anchor_secs = anchor_frame_text
            .trim()
            .parse::<u64>()
            .ok()
            .filter(|_| fps > 0.0)
            .map(|frame| frame as f64 / fps)
            .unwrap_or(current_time_secs);

        if let Some(app) = app_weak.upgrade() {
            app.set_calibrating(true);
            app.set_calibration_step("Starting...".into());
            app.set_calibration_detail("".into());
            app.set_calibration_progress(0.0);
            app.set_calibration_log(slint::ModelRc::new(slint::VecModel::from(Vec::<
                slint::SharedString,
            >::new(
            ))));
            app.set_status_text("Auto-calibrating...".into());
        }

        let interrupted = {
            let s = state_ref.borrow();
            s.calibration_interrupted.store(false, Ordering::Relaxed);
            Arc::clone(&s.calibration_interrupted)
        };
        let (tx, rx) = std::sync::mpsc::channel();

        // Preserve the user's current lens (a picked profile or slider edits)
        // across recalibration. `self.calibration` is the source of truth -
        // the lens picker and sliders write it - so the optimizer is handed
        // the lens the user is actually looking at, not a stale fallback that
        // only `self.calibration` retained before the picker wrote it.
        let (existing_left_params, existing_right_params) = {
            let s = state_ref.borrow();
            match s.calibration.as_ref() {
                Some(cal) => {
                    log::info!(
                        "Re-calibrate: preserving current lens (left fx={:.1} cx={:.1}, \
                         right fx={:.1} cx={:.1})",
                        cal.lenses[0].fx,
                        cal.lenses[0].cx,
                        cal.lenses[1].fx,
                        cal.lenses[1].cx
                    );
                    (Some(cal.lenses[0].clone()), Some(cal.lenses[1].clone()))
                }
                None => (
                    s.cal_baseline_left_params.clone(),
                    s.cal_baseline_right_params.clone(),
                ),
            }
        };

        {
            let mut s = state_ref.borrow_mut();
            s.cal_rx = Some(rx);
        }

        // Run calibration on a background thread. Only Send types
        // (PathBuf, channel, AtomicBool, Weak) cross the boundary.
        let app_weak_bg = app_weak.clone();
        std::thread::spawn(move || {
            let app_weak_progress = app_weak_bg.clone();

            // The anchor/window math needs the true duration of the
            // single file `calibrate_videos` below will actually read -
            // NOT the GUI preview's playback duration, which spans every
            // segment of a multi-segment (chained) recording while
            // `calibrate_videos` only ever gets the *first* segment
            // (`left`/`right` here, see `try_init`'s "left_path/right_path
            // stay single" comment). Using the preview's much longer
            // total duration to bound `skip_end_secs` produced a
            // `skip_end_secs` far bigger than the actual file, which
            // `select_frame_indices` saturates to an empty usable range -
            // silently collapsing "6 frames" down to 1, sampled from
            // wherever that leaves it, not from the requested window at
            // all. Root-caused from a real failed run's log
            // (`extracting 1 frames` despite `cal_frames` frames
            // requested) - see FRICTION.md point 26's follow-up.
            let probed_duration_secs = match (
                reco_io::ffmpeg::calibration_io::probe_video(&left),
                reco_io::ffmpeg::calibration_io::probe_video(&right),
            ) {
                (Ok(l), Ok(r)) if l.fps > 0.0 => {
                    Some(l.total_frames.min(r.total_frames) as f64 / l.fps)
                }
                (l, r) => {
                    log::warn!(
                        "Auto-calibrate: could not probe true video duration for the \
                         anchor/sample-window bound (left: {l:?}, right: {r:?}); \
                         sampling from the anchor onward with no end bound"
                    );
                    None
                }
            };
            // Anchor beyond the actual single file's own duration means
            // the user scrubbed/typed a frame number past the end of the
            // first segment `calibrate_videos` reads - sampling would
            // start past the end of the file and find nothing. Clamp
            // with a loud warning rather than silently produce another
            // empty range.
            let anchor_secs = match probed_duration_secs {
                Some(d) if anchor_secs >= d => {
                    log::warn!(
                        "Auto-calibrate: anchor {anchor_secs:.1}s is past the first \
                         segment's own duration ({d:.1}s) - clamping. If this recording \
                         has multiple segments, only the first is ever sampled."
                    );
                    (d - 1.0).max(0.0)
                }
                _ => anchor_secs,
            };
            // "Sample window" (0 = off) centers calibration sampling on
            // the anchor: every sample frame stays within
            // `sample_window / 2` seconds either side of it, instead of
            // `select_frame_indices` spreading them across the rest of
            // the clip. Off (0) samples from the anchor onward through
            // the rest of the video, as Auto-Calibrate always did before
            // either field existed.
            let (skip_start, skip_end) = match probed_duration_secs {
                Some(d) if sample_window > 0.0 && d > 0.0 => {
                    let half = sample_window / 2.0;
                    let window_start = (anchor_secs - half).max(0.0);
                    let window_end = (anchor_secs + half).min(d);
                    (window_start, (d - window_end).max(0.0))
                }
                _ => (anchor_secs, 0.0),
            };

            // Bump frame-pair count above the reco-core default of 2.
            // More frames give the bundle adjustment more constraints
            // to settle on, which especially helps at 4K where AKAZE
            // feature matches are noisier per frame.
            log::info!(
                "Auto-calibrate: {cal_frames} frames, anchor={anchor_secs:.1}s, \
                 sample_window={sample_window:.0}s, skip=[{skip_start:.1}s, -{skip_end:.1}s], \
                 imu_seeds={use_imu_seeds}, force_x_rx={force_x_rx}, force_z_rz={force_z_rz}, \
                 akaze={akaze_threshold}, detect_y=[{detect_y_min:.2}, {detect_y_max:.2}], \
                 full_res_features={full_res_features}"
            );
            let mut config = reco_calibrate::CalibrationConfig {
                num_frames: cal_frames,
                skip_start_secs: skip_start,
                skip_end_secs: skip_end,
                use_imu_rotation_seeds: use_imu_seeds,
                ..Default::default()
            };
            config.akaze.threshold = akaze_threshold;
            config.akaze.detect_y_min = detect_y_min;
            config.akaze.detect_y_max = detect_y_max;
            config.akaze.detect_max_width = if full_res_features { 0 } else { 1920 };
            if force_x_rx {
                config.optimizer.enable_x_rx = true;
            }
            if force_z_rz {
                config.optimizer.enable_z_rz = true;
            }
            if existing_left_params.is_some() {
                log::info!("Re-calibrating with user-picked lens profiles");
            }
            // Accumulated across every progress tick, on this same
            // background thread - each tick sends the whole log so far
            // (not just the new line) since a Slint model property is
            // replaced wholesale, not appended to, from here.
            let mut log_lines: Vec<String> = Vec::new();
            let result = reco_calibrate::video::calibrate_videos(
                &left,
                &right,
                reco_calibrate::video::CalibrateVideosOptions {
                    config: Some(config),
                    left_params: existing_left_params,
                    right_params: existing_right_params,
                    ..Default::default()
                },
                &mut |progress| {
                    let step_name = format!("{:?}", progress.step);
                    let detail = progress.detail.clone();
                    let fraction = progress.fraction.unwrap_or(0.0).clamp(0.0, 1.0);
                    log_lines.push(if detail.is_empty() {
                        step_name.clone()
                    } else {
                        format!("{step_name}: {detail}")
                    });
                    let log_model: Vec<slint::SharedString> =
                        log_lines.iter().map(|l| l.as_str().into()).collect();
                    let weak = app_weak_progress.clone();
                    slint::invoke_from_event_loop(move || {
                        if let Some(app) = weak.upgrade() {
                            app.set_calibration_step(step_name.into());
                            app.set_calibration_detail(detail.clone().into());
                            app.set_calibration_progress(fraction);
                            app.set_calibration_log(slint::ModelRc::new(slint::VecModel::from(
                                log_model,
                            )));
                            app.set_status_text(format!("Calibrating: {detail}").into());
                        }
                    })
                    .ok();
                },
                &interrupted,
            );

            let cal_result: CalibrationResult = match result {
                Ok(r) => {
                    log::info!("Auto-calibration complete: {} matches", r.total_matches,);
                    Ok(CalibrationOutput {
                        calibration: r.calibration,
                        confidence: r.confidence,
                        total_matches: r.total_matches,
                        left_lens_profile: r.left_lens_profile,
                        right_lens_profile: r.right_lens_profile,
                        imu_diagnostics: r.imu_diagnostics,
                    })
                }
                Err(e) => Err(e),
            };
            tx.send(cal_result).ok();
        });
    });

    // Cancel button on the calibration progress popup. `calibrate_videos`
    // only checks this flag between steps (`check_interrupted` in
    // `reco_calibrate::video`), so a click can take up to one full
    // step's duration (e.g. a DJI telemetry parse) to actually stop.
    let state_ref = Rc::clone(&state);
    app.on_cancel_calibration(move || {
        state_ref
            .borrow()
            .calibration_interrupted
            .store(true, Ordering::Relaxed);
    });

    // Live AKAZE detection preview: fires on every AKAZE tuning-slider
    // change so "does this threshold find features?" is visible on the
    // current frame immediately, instead of only after a full
    // Auto-Calibrate run. See `detect_preview` module doc for why this
    // reuses one long-lived GPU context instead of Auto-Calibrate's
    // fresh-context-per-run approach.
    let app_weak_preview = app.as_weak();
    let detect_preview_worker = detect_preview::DetectPreviewWorker::spawn(move |preview| {
        let weak = app_weak_preview.clone();
        slint::invoke_from_event_loop(move || {
            if let Some(app) = weak.upgrade() {
                app.set_preview_frame(detection_preview_to_slint_image(&preview));
            }
        })
        .ok();
    });

    let app_weak = app.as_weak();
    let state_ref = Rc::clone(&state);
    app.on_detect_preview_tick(move || {
        let Some(app) = app_weak.upgrade() else {
            return;
        };
        let s = state_ref.borrow();
        let Some(frame) = s.playback.current_frame() else {
            return;
        };
        let Some((width, height)) = s.playback.input_dimensions() else {
            return;
        };
        // Needs known lens intrinsics to undistort with - falls back to
        // the baseline captured right after files load, same resolution
        // order as `on_auto_calibrate`.
        let (left_params, right_params) = match (
            s.calibration.as_ref(),
            s.cal_baseline_left_params.as_ref(),
            s.cal_baseline_right_params.as_ref(),
        ) {
            (Some(cal), _, _) => (cal.lenses[0].clone(), cal.lenses[1].clone()),
            (None, Some(l), Some(r)) => (l.clone(), r.clone()),
            _ => return,
        };

        let mut config = CalibrationConfig::default();
        config.akaze.threshold = app.get_cal_akaze_threshold() as f64;
        config.akaze.detect_y_min = app.get_cal_detect_y_min() as f64;
        config.akaze.detect_y_max = app.get_cal_detect_y_max() as f64;
        config.akaze.detect_max_width = if app.get_cal_full_res_features() {
            0
        } else {
            1920
        };

        let req = detect_preview::PreviewRequest {
            left_y: frame.left.y.clone(),
            left_u: frame.left.u.clone(),
            left_v: frame.left.v.clone(),
            right_y: frame.right.y.clone(),
            right_u: frame.right.u.clone(),
            right_v: frame.right.v.clone(),
            width,
            height,
            left_params,
            right_params,
            config,
        };
        drop(s);
        detect_preview_worker.request(req);
    });

    // ── Playback callbacks ──

    let app_weak = app.as_weak();
    let state_ref = Rc::clone(&state);
    app.on_toggle_playback(move || {
        let mut s = state_ref.borrow_mut();
        if s.is_exporting() {
            log::info!("Playback toggle ignored: export in progress");
            return;
        }
        let new_state = s.playback.toggle();
        if let Some(app) = app_weak.upgrade() {
            app.set_playing(new_state == PlayState::Playing);
        }
    });

    let app_weak = app.as_weak();
    let state_ref = Rc::clone(&state);
    app.on_pause_playback(move || {
        let mut s = state_ref.borrow_mut();
        if s.playback.state() == PlayState::Playing {
            s.playback.toggle();
        }
        if let Some(app) = app_weak.upgrade() {
            app.set_playing(false);
        }
    });

    let state_ref = Rc::clone(&state);
    app.on_changed_playback_speed(move |speed| {
        state_ref.borrow_mut().playback.set_speed(speed as f64);
    });

    let app_weak = app.as_weak();
    let state_ref = Rc::clone(&state);
    app.on_step_forward(move || {
        let mut s = state_ref.borrow_mut();
        if s.is_exporting() {
            return;
        }
        if s.playback.state() == PlayState::Playing {
            s.playback.toggle();
        }
        match s.playback.step_forward() {
            Ok(true) => {
                let img = s.render_current();
                let total = s.playback.total_frames().unwrap_or(0);
                let fps = s.playback.fps();
                if let (Some(app), Some(img)) = (app_weak.upgrade(), img) {
                    app.set_preview_frame(img);
                    sync_frame_display(&app, s.playback.frame_index(), total, fps);
                }
            }
            Ok(false) => {}
            Err(e) => log::error!("Step forward error: {e}"),
        }
        if let Some(app) = app_weak.upgrade() {
            app.set_playing(false);
        }
    });

    let app_weak = app.as_weak();
    let state_ref = Rc::clone(&state);
    app.on_step_backward(move || {
        let mut s = state_ref.borrow_mut();
        if s.is_exporting() {
            return;
        }
        if s.playback.state() == PlayState::Playing {
            s.playback.toggle();
        }
        // Step back = seek to current - 1.
        let target = s.playback.frame_index().saturating_sub(2);
        let total = s.playback.total_frames().unwrap_or(1).max(1);
        let fraction = target as f32 / total as f32;
        match s.playback.seek(fraction) {
            Ok(()) => {
                let img = s.render_current();
                let fps = s.playback.fps();
                if let (Some(app), Some(img)) = (app_weak.upgrade(), img) {
                    app.set_preview_frame(img);
                    sync_frame_display(&app, s.playback.frame_index(), total, fps);
                }
            }
            Err(e) => log::error!("Step backward error: {e}"),
        }
        if let Some(app) = app_weak.upgrade() {
            app.set_playing(false);
        }
    });

    let state_ref = Rc::clone(&state);
    app.on_seek(move |fraction| {
        let mut s = state_ref.borrow_mut();
        // Two problems the seek slider creates, both solved by debouncing:
        //
        // 1. Every Rust-driven frame advance sets `current-frame`,
        //    which updates the slider's bound `value`, which fires
        //    `changed(val)`, which calls us. A value delta of 0 is
        //    the echo — we drop it.
        //
        // 2. A user drag emits `changed` on every mouse pixel movement.
        //    Each seek reinits NVDEC (~50ms), and hundreds per second
        //    saturate the GPU. We defer the seek until the fraction
        //    has been stable for `SEEK_DEBOUNCE_MS`, which the timer
        //    tick monitors.
        let total = match s.playback.total_frames() {
            Some(t) if t > 0 => t,
            _ => return,
        };
        let target = ((fraction as f64) * total as f64) as u64;
        if target.abs_diff(s.playback.frame_index()) < 2 {
            // Echo from our own set_current_frame — ignore.
            return;
        }
        s.pending_seek = Some((fraction, Instant::now()));
    });

    // ── Camera / view control callbacks ──
    //
    // These handlers NEVER render synchronously. They only mutate
    // targets (or cheap per-renderer params like blend/rig tilt). The
    // 2ms timer tick reads targets, lerps current toward them, and
    // renders at a capped ~60Hz. This eliminates two problems at once:
    //   1. Per-pixel drag events no longer each trigger a GPU render,
    //      so the UI thread stays responsive to input
    //   2. Pan motion is visually continuous rather than the discrete
    //      jumps from raw input events

    let state_ref = Rc::clone(&state);
    app.on_pan(move |dx_px, dy_px| {
        state_ref.borrow_mut().apply_pan(dx_px, dy_px);
    });

    let state_ref = Rc::clone(&state);
    app.on_zoom(move |delta_deg| {
        state_ref.borrow_mut().apply_zoom(delta_deg);
    });

    let state_ref = Rc::clone(&state);
    app.on_reset_view(move || {
        state_ref.borrow_mut().reset_view();
    });

    let state_ref = Rc::clone(&state);
    let app_weak = app.as_weak();
    app.on_changed_blend_width(move |w| {
        state_ref.borrow_mut().set_blend_width(w);
        // Seam blend is persisted with the calibration, so a change is
        // unsaved work - surface the Save Calibration button.
        if let Some(app) = app_weak.upgrade() {
            app.set_cal_dirty(true);
        }
    });

    let state_ref = Rc::clone(&state);
    let app_weak = app.as_weak();
    app.on_changed_blend_flip_direction(move |flip| {
        state_ref.borrow_mut().set_blend_flip_direction(flip);
        if let Some(app) = app_weak.upgrade() {
            app.set_cal_dirty(true);
        }
    });

    let state_ref = Rc::clone(&state);
    app.on_changed_multiband_blend_enabled(move |enabled| {
        state_ref.borrow_mut().set_multiband_blend_enabled(enabled);
    });

    let state_ref = Rc::clone(&state);
    app.on_changed_show_seam_line(move |show| {
        state_ref.borrow_mut().set_show_seam_line(show);
    });

    let state_ref = Rc::clone(&state);
    let app_weak = app.as_weak();
    app.on_changed_seam_offset(move |offset| {
        state_ref.borrow_mut().set_seam_offset(offset);
        if let Some(app) = app_weak.upgrade() {
            app.set_cal_dirty(true);
        }
    });

    let state_ref = Rc::clone(&state);
    let app_weak = app.as_weak();
    app.on_seam_drag(move |dx_normalized| {
        let new_value = state_ref.borrow_mut().seam_drag(dx_normalized);
        if let (Some(app), Some(v)) = (app_weak.upgrade(), new_value) {
            app.set_seam_offset(v);
            app.set_cal_dirty(true);
        }
    });

    let state_ref = Rc::clone(&state);
    app.on_seam_hit_test(move |mouse_x, mouse_y, box_w, box_h| {
        if box_w <= 0.0 || box_h <= 0.0 {
            return false;
        }
        let s = state_ref.borrow();
        let Some(bridge) = s.bridge.as_ref() else {
            return false;
        };
        let pipeline = bridge.engine().pipeline();
        let output_aspect = pipeline.viewport().aspect_ratio();
        let pose = s.pose.current_pose();

        let (content_w, content_h, content_x, content_y) =
            panorama_letterbox_rect(box_w, box_h, output_aspect);
        let lx = mouse_x - content_x;
        let ly = mouse_y - content_y;
        if lx < 0.0 || lx > content_w || ly < 0.0 || ly > content_h {
            return false;
        }

        let Some((top, bottom)) = reco_core::render::renderer::seam_line_screen_points(
            pipeline.calibration(),
            pipeline.viewport(),
            pose.yaw,
            pose.pitch,
            output_aspect,
        ) else {
            return false;
        };
        let top_px = (top.0 * content_w, top.1 * content_h);
        let bottom_px = (bottom.0 * content_w, bottom.1 * content_h);
        let dist = point_to_segment_distance(lx, ly, top_px.0, top_px.1, bottom_px.0, bottom_px.1);
        dist <= SEAM_LINE_HIT_RADIUS_PX
    });

    // In-app goal-geometry point editor. Same interaction model AND same
    // raw-distorted-frame-normalized coordinate space as the field-ROI
    // editor (`on_roi_pointer_*` above) - the two now share one lens-
    // preview editing session (`zone-edit-mode`/`zone-edit-type` in
    // main.slint), just targeting `cal.goal_geometry` instead of
    // `cal.field_roi`.
    let state_ref = Rc::clone(&state);
    app.on_goal_pointer_hit_test(move |lx, ly, cw, ch| {
        if cw <= 0.0 || ch <= 0.0 {
            return -1;
        }
        let s = state_ref.borrow();
        let is_right = s.lens_preview_side == "right";
        let Some(cal) = s.calibration.as_ref() else {
            return -1;
        };
        let Some(goal) = cal.goal_geometry.as_ref() else {
            return -1;
        };
        let pts = if is_right { &goal.right } else { &goal.left };
        let lens = &cal.lenses[if is_right { 1 } else { 0 }];
        let display: Vec<[f64; 2]> = pts
            .iter()
            .map(|p| raw_norm_to_rectified_norm(p[0], p[1], lens))
            .collect();
        roi_hit_test(&display, lx, ly, cw, ch)
            .map(|i| i as i32)
            .unwrap_or(-1)
    });

    let app_weak = app.as_weak();
    let state_ref = Rc::clone(&state);
    app.on_goal_pointer_add(move |lx, ly, cw, ch| {
        if cw <= 0.0 || ch <= 0.0 {
            return;
        }
        let mut s = state_ref.borrow_mut();
        let is_right = s.lens_preview_side == "right";
        let rectified = [
            (lx / cw).clamp(0.0, 1.0) as f64,
            (ly / ch).clamp(0.0, 1.0) as f64,
        ];
        if let Some(cal) = s.calibration.as_mut() {
            let lens = cal.lenses[if is_right { 1 } else { 0 }].clone();
            let norm = rectified_norm_to_raw_norm(rectified[0], rectified[1], &lens);
            let goal = cal.goal_geometry.get_or_insert_with(Default::default);
            let pts = if is_right {
                &mut goal.right
            } else {
                &mut goal.left
            };
            let idx = roi_insert_index(pts, norm);
            pts.insert(idx, norm);
        }
        // A confirmed click-to-add is a complete gesture on its own (no
        // separate `goal_pointer_up` follows it), so save right away.
        if let Err(e) = s.save_calibration() {
            log::error!("Failed to save calibration after goal point add: {e}");
        }
        if let Some(app) = app_weak.upgrade() {
            app.set_has_goal_geometry(true);
            sync_goal_points(&s, &app);
        }
    });

    let app_weak = app.as_weak();
    let state_ref = Rc::clone(&state);
    app.on_goal_pointer_drag(move |index, lx, ly, cw, ch| {
        if cw <= 0.0 || ch <= 0.0 || index < 0 {
            return;
        }
        let mut s = state_ref.borrow_mut();
        let is_right = s.lens_preview_side == "right";
        let rectified = [
            (lx / cw).clamp(0.0, 1.0) as f64,
            (ly / ch).clamp(0.0, 1.0) as f64,
        ];
        if let Some(cal) = s.calibration.as_mut() {
            let lens = cal.lenses[if is_right { 1 } else { 0 }].clone();
            let norm = rectified_norm_to_raw_norm(rectified[0], rectified[1], &lens);
            if let Some(goal) = cal.goal_geometry.as_mut() {
                let pts = if is_right {
                    &mut goal.right
                } else {
                    &mut goal.left
                };
                if let Some(p) = pts.get_mut(index as usize) {
                    *p = norm;
                }
            }
        }
        if let Some(app) = app_weak.upgrade() {
            sync_goal_points(&s, &app);
        }
    });

    let state_ref = Rc::clone(&state);
    app.on_goal_pointer_up(move || {
        let s = state_ref.borrow();
        if let Err(e) = s.save_calibration() {
            log::error!("Failed to save calibration after goal edit: {e}");
        }
    });

    let app_weak = app.as_weak();
    let state_ref = Rc::clone(&state);
    app.on_goal_pointer_delete(move |lx, ly, cw, ch| {
        if cw <= 0.0 || ch <= 0.0 {
            return;
        }
        let mut s = state_ref.borrow_mut();
        let is_right = s.lens_preview_side == "right";
        let Some(i) = s.calibration.as_ref().and_then(|cal| {
            let goal = cal.goal_geometry.as_ref()?;
            let pts = if is_right { &goal.right } else { &goal.left };
            let lens = &cal.lenses[if is_right { 1 } else { 0 }];
            let display: Vec<[f64; 2]> = pts
                .iter()
                .map(|p| raw_norm_to_rectified_norm(p[0], p[1], lens))
                .collect();
            roi_hit_test(&display, lx, ly, cw, ch)
        }) else {
            return;
        };
        if let Some(goal) = s
            .calibration
            .as_mut()
            .and_then(|c| c.goal_geometry.as_mut())
        {
            let pts = if is_right {
                &mut goal.right
            } else {
                &mut goal.left
            };
            pts.remove(i);
        }
        let has = s
            .calibration
            .as_ref()
            .and_then(|c| c.goal_geometry.as_ref())
            .is_some_and(|g| !g.left.is_empty() || !g.right.is_empty());
        if let Err(e) = s.save_calibration() {
            log::error!("Failed to save calibration after goal point delete: {e}");
        }
        if let Some(app) = app_weak.upgrade() {
            app.set_has_goal_geometry(has);
            sync_goal_points(&s, &app);
        }
    });

    // Manual AI-tracking pitch safety margin: two horizontal lines on
    // the STITCHED preview - see `sync_pitch_limit_overlay`,
    // `pitch_limit_projection`, and `AutocamPitchLimits`'s doc comment.
    // Simpler than the ROI/GOAL point editors above (no add/delete,
    // always exactly two lines), but unlike them the screen position
    // depends on the live camera pose, not just calibration, so both
    // callbacks re-derive it fresh via `pitch_limit_projection` rather
    // than reading a cached Slint property.
    let state_ref = Rc::clone(&state);
    app.on_pitch_limit_pointer_hit_test(move |mouse_x, mouse_y, box_w, box_h| {
        if box_w <= 0.0 || box_h <= 0.0 {
            return -1;
        }
        let s = state_ref.borrow();
        let Some(bridge) = s.bridge.as_ref() else {
            return -1;
        };
        let output_aspect = bridge.engine().pipeline().viewport().aspect_ratio();
        let (content_w, content_h, content_x, content_y) =
            panorama_letterbox_rect(box_w, box_h, output_aspect);
        let lx = mouse_x - content_x;
        let ly = mouse_y - content_y;
        if lx < 0.0 || lx > content_w || ly < 0.0 || ly > content_h {
            return -1;
        }

        let limits = s.calibration.as_ref().and_then(|c| c.autocam_pitch_limits);
        let proj = pitch_limit_projection(&s);
        let candidates = [
            pitch_limit_display_y(limits, proj.as_ref(), 0).map(|y| (0, y * content_h)),
            pitch_limit_display_y(limits, proj.as_ref(), 1).map(|y| (1, y * content_h)),
        ];
        candidates
            .into_iter()
            .flatten()
            .filter(|&(_, y)| (y - ly).abs() <= SEAM_LINE_HIT_RADIUS_PX)
            .min_by(|(_, a), (_, b)| (a - ly).abs().total_cmp(&(b - ly).abs()))
            .map(|(idx, _)| idx)
            .unwrap_or(-1)
    });

    let app_weak = app.as_weak();
    let state_ref = Rc::clone(&state);
    app.on_pitch_limit_pointer_drag(move |index, mouse_x, mouse_y, box_w, box_h| {
        if box_w <= 0.0 || box_h <= 0.0 || (index != 0 && index != 1) {
            return;
        }
        let mut s = state_ref.borrow_mut();
        let Some(bridge) = s.bridge.as_ref() else {
            return;
        };
        let output_aspect = bridge.engine().pipeline().viewport().aspect_ratio();
        let (content_w, content_h, content_x, content_y) =
            panorama_letterbox_rect(box_w, box_h, output_aspect);
        let sx = (2.0 * (mouse_x - content_x) / content_w - 1.0).clamp(-1.0, 1.0);
        let sy = (1.0 - 2.0 * (mouse_y - content_y) / content_h).clamp(-1.0, 1.0);
        let Some(proj) = pitch_limit_projection(&s) else {
            return;
        };
        let target =
            reco_core::geometry::unproject_screen_to_world(&proj.as_screen_projection(), sx, sy);
        if let Some(cal) = s.calibration.as_mut() {
            let limits = cal
                .autocam_pitch_limits
                .get_or_insert_with(Default::default);
            if index == 0 {
                limits.top_rad = Some(target.pitch);
            } else {
                limits.bottom_rad = Some(target.pitch);
            }
        }
        if let Some(app) = app_weak.upgrade() {
            sync_pitch_limit_status(&s, &app);
            sync_pitch_limit_overlay(&s, &app);
        }
    });

    let app_weak = app.as_weak();
    let state_ref = Rc::clone(&state);
    app.on_changed_show_pitch_limits(move |enabled| {
        let mut s = state_ref.borrow_mut();
        if let Some(app) = app_weak.upgrade() {
            if enabled {
                sync_pitch_limit_overlay(&s, &app);
            }
            s.preview_dirty = true;
            app.window().request_redraw();
        }
    });

    let state_ref = Rc::clone(&state);
    app.on_pitch_limit_pointer_up(move || {
        let s = state_ref.borrow();
        if let Err(e) = s.save_calibration() {
            log::error!("Failed to save calibration after AI pitch limit edit: {e}");
        }
    });

    let app_weak = app.as_weak();
    let state_ref = Rc::clone(&state);
    app.on_pitch_limit_clear(move || {
        let mut s = state_ref.borrow_mut();
        if let Some(cal) = s.calibration.as_mut() {
            cal.autocam_pitch_limits = None;
        }
        if let Err(e) = s.save_calibration() {
            log::error!("Failed to save calibration after clearing the AI pitch limit: {e}");
        }
        if let Some(app) = app_weak.upgrade() {
            sync_pitch_limit_status(&s, &app);
            sync_pitch_limit_overlay(&s, &app);
        }
    });

    let state_ref = Rc::clone(&state);
    let app_weak = app.as_weak();
    app.on_changed_color_match_enabled(move |enabled| {
        state_ref.borrow_mut().set_color_match_enabled(enabled);
        if let Some(app) = app_weak.upgrade() {
            app.set_cal_dirty(true);
        }
    });

    let state_ref = Rc::clone(&state);
    let app_weak = app.as_weak();
    app.on_changed_color_gamma(move |left, right| {
        state_ref.borrow_mut().set_color_gamma(left, right);
        if let Some(app) = app_weak.upgrade() {
            app.set_cal_dirty(true);
        }
    });

    let state_ref = Rc::clone(&state);
    let app_weak = app.as_weak();
    app.on_changed_color_match_auto_gamma(move |enabled| {
        state_ref.borrow_mut().set_color_match_auto_gamma(enabled);
        if let Some(app) = app_weak.upgrade() {
            app.set_cal_dirty(true);
        }
    });

    let state_ref = Rc::clone(&state);
    let app_weak = app.as_weak();
    app.on_changed_color_match_band_width(move |w| {
        state_ref.borrow_mut().set_color_match_band_width(w);
        if let Some(app) = app_weak.upgrade() {
            app.set_cal_dirty(true);
        }
    });

    let state_ref = Rc::clone(&state);
    let app_weak = app.as_weak();
    app.on_changed_color_match_grid_cols(move |cols| {
        state_ref.borrow_mut().set_color_match_grid_cols(cols);
        if let Some(app) = app_weak.upgrade() {
            app.set_cal_dirty(true);
        }
    });

    let state_ref = Rc::clone(&state);
    let app_weak = app.as_weak();
    app.on_changed_color_match_grid_rows(move |rows| {
        state_ref.borrow_mut().set_color_match_grid_rows(rows);
        if let Some(app) = app_weak.upgrade() {
            app.set_cal_dirty(true);
        }
    });

    let state_ref = Rc::clone(&state);
    let app_weak = app.as_weak();
    app.on_changed_color_match_interval_frames(move |frames| {
        state_ref
            .borrow_mut()
            .set_color_match_interval_frames(frames);
        if let Some(app) = app_weak.upgrade() {
            app.set_cal_dirty(true);
        }
    });

    let state_ref = Rc::clone(&state);
    let app_weak = app.as_weak();
    app.on_changed_color_match_ema_alpha(move |alpha| {
        state_ref.borrow_mut().set_color_match_ema_alpha(alpha);
        if let Some(app) = app_weak.upgrade() {
            app.set_cal_dirty(true);
        }
    });

    let state_ref = Rc::clone(&state);
    let app_weak = app.as_weak();
    app.on_changed_color_match_max_y_offset(move |v| {
        state_ref.borrow_mut().set_color_match_max_y_offset(v);
        if let Some(app) = app_weak.upgrade() {
            app.set_cal_dirty(true);
        }
    });

    let state_ref = Rc::clone(&state);
    let app_weak = app.as_weak();
    app.on_changed_color_match_max_chroma_offset(move |v| {
        state_ref.borrow_mut().set_color_match_max_chroma_offset(v);
        if let Some(app) = app_weak.upgrade() {
            app.set_cal_dirty(true);
        }
    });

    let state_ref = Rc::clone(&state);
    app.on_remeasure_color_match(move || {
        state_ref.borrow_mut().remeasure_color_match();
    });

    let state_ref = Rc::clone(&state);
    let app_weak = app.as_weak();
    app.on_reset_color_match(move || {
        state_ref.borrow_mut().reset_color_match();
        if let Some(app) = app_weak.upgrade() {
            use reco_core::calibration::{
                DEFAULT_COLOR_GAMMA, DEFAULT_COLOR_MATCH_AUTO_GAMMA,
                DEFAULT_COLOR_MATCH_BAND_WIDTH, DEFAULT_COLOR_MATCH_EMA_ALPHA,
                DEFAULT_COLOR_MATCH_ENABLED, DEFAULT_COLOR_MATCH_GRID_COLS,
                DEFAULT_COLOR_MATCH_GRID_ROWS, DEFAULT_COLOR_MATCH_INTERVAL_FRAMES,
                DEFAULT_COLOR_MATCH_MAX_CHROMA_OFFSET, DEFAULT_COLOR_MATCH_MAX_Y_OFFSET,
            };
            app.set_color_match_enabled(DEFAULT_COLOR_MATCH_ENABLED);
            app.set_color_match_band_width(DEFAULT_COLOR_MATCH_BAND_WIDTH);
            app.set_color_match_grid_cols(DEFAULT_COLOR_MATCH_GRID_COLS as f32);
            app.set_color_match_grid_rows(DEFAULT_COLOR_MATCH_GRID_ROWS as f32);
            app.set_color_match_interval_frames(DEFAULT_COLOR_MATCH_INTERVAL_FRAMES as f32);
            app.set_color_match_ema_alpha(DEFAULT_COLOR_MATCH_EMA_ALPHA);
            app.set_color_match_max_y_offset(DEFAULT_COLOR_MATCH_MAX_Y_OFFSET);
            app.set_color_match_max_chroma_offset(DEFAULT_COLOR_MATCH_MAX_CHROMA_OFFSET);
            app.set_color_gamma_left(DEFAULT_COLOR_GAMMA);
            app.set_color_gamma_right(DEFAULT_COLOR_GAMMA);
            app.set_color_match_auto_gamma(DEFAULT_COLOR_MATCH_AUTO_GAMMA);
            app.set_cal_dirty(true);
        }
    });

    let state_ref = Rc::clone(&state);
    let app_weak = app.as_weak();
    app.on_changed_rig_tilt(move |deg| {
        state_ref.borrow_mut().set_rig_tilt(deg);
        if let Some(app) = app_weak.upgrade() {
            app.set_cal_dirty(true);
        }
    });

    let state_ref = Rc::clone(&state);
    let app_weak = app.as_weak();
    app.on_changed_rig_roll(move |deg| {
        state_ref.borrow_mut().set_rig_roll(deg);
        if let Some(app) = app_weak.upgrade() {
            app.set_cal_dirty(true);
        }
    });

    let state_ref = Rc::clone(&state);
    let app_weak = app.as_weak();
    app.on_changed_sync_offset(move |frames| {
        state_ref.borrow_mut().set_sync_offset(frames);
        if let Some(app) = app_weak.upgrade() {
            app.set_cal_dirty(true);
        }
    });

    let app_weak = app.as_weak();
    let state_ref = Rc::clone(&state);
    app.on_compute_sync_offset(move || {
        start_sync_offset_detection(&state_ref, &app_weak);
    });

    // "Yes" branch of the Select-Match-Folder sync-offset prompt: same
    // detection job as the manual button, but flagged to write straight
    // to the calibration file once it resolves (see
    // `AppState::pending_sync_offset_autosave`) instead of waiting for an
    // explicit Save - this calibration was only just created for this
    // match, so there's no risk of clobbering unrelated unsaved edits.
    let app_weak = app.as_weak();
    let state_ref = Rc::clone(&state);
    app.on_detect_match_folder_sync_offset(move || {
        state_ref.borrow_mut().pending_sync_offset_autosave = true;
        start_sync_offset_detection(&state_ref, &app_weak);
    });

    let state_ref = Rc::clone(&state);
    app.on_changed_audio_window_frames(move |frames| {
        state_ref.borrow_mut().set_audio_window_frames(frames);
    });

    let state_ref = Rc::clone(&state);
    app.on_changed_fov(move |deg| {
        state_ref.borrow_mut().set_fov(deg);
    });

    let app_weak = app.as_weak();
    let state_ref = Rc::clone(&state);
    app.on_seek_relative(move |secs| {
        let mut s = state_ref.borrow_mut();
        if let Err(e) = s.seek_relative(secs) {
            log::error!("Seek relative error: {e}");
            return;
        }
        let img = s.render_current();
        let total = s.playback.total_frames().unwrap_or(0);
        let fps = s.playback.fps();
        if let (Some(app), Some(img)) = (app_weak.upgrade(), img) {
            app.set_preview_frame(img);
            sync_frame_display(&app, s.playback.frame_index(), total, fps);
        }
    });

    // ── Live calibration editing callbacks ──
    //
    // Each slider writes the corresponding field on the Topology,
    // pushes the edited layout into the renderer, and flips cal-dirty
    // so the Save button becomes enabled.

    let app_weak = app.as_weak();
    let state_ref = Rc::clone(&state);
    app.on_changed_cal_intersect(move |v| {
        let mut s = state_ref.borrow_mut();
        let Some(mut layout) = s.calibration.as_ref().map(|c| c.topology.clone()) else {
            return;
        };
        layout.intersect = v as f64;
        s.apply_layout(layout);
        if let Some(app) = app_weak.upgrade() {
            app.set_cal_dirty(true);
        }
    });

    let app_weak = app.as_weak();
    let state_ref = Rc::clone(&state);
    app.on_changed_cal_camera_axis_offset(move |v| {
        let mut s = state_ref.borrow_mut();
        let Some(mut framing) = s.calibration.as_ref().map(|c| c.framing.clone()) else {
            return;
        };
        framing.axis_offset = v as f64;
        s.apply_framing(framing);
        if let Some(app) = app_weak.upgrade() {
            app.set_cal_dirty(true);
        }
    });

    let app_weak = app.as_weak();
    let state_ref = Rc::clone(&state);
    app.on_changed_cal_x_ty(move |v| {
        let mut s = state_ref.borrow_mut();
        let Some(mut layout) = s.calibration.as_ref().map(|c| c.topology.clone()) else {
            return;
        };
        layout.x_ty = v as f64;
        s.apply_layout(layout);
        if let Some(app) = app_weak.upgrade() {
            app.set_cal_dirty(true);
        }
    });

    let app_weak = app.as_weak();
    let state_ref = Rc::clone(&state);
    app.on_changed_cal_x_rx(move |v| {
        let mut s = state_ref.borrow_mut();
        let Some(mut layout) = s.calibration.as_ref().map(|c| c.topology.clone()) else {
            return;
        };
        layout.x_rx = v as f64;
        s.apply_layout(layout);
        if let Some(app) = app_weak.upgrade() {
            app.set_cal_dirty(true);
        }
    });

    let app_weak = app.as_weak();
    let state_ref = Rc::clone(&state);
    app.on_changed_cal_z_rz(move |v| {
        let mut s = state_ref.borrow_mut();
        let Some(mut layout) = s.calibration.as_ref().map(|c| c.topology.clone()) else {
            return;
        };
        layout.z_rz = v as f64;
        s.apply_layout(layout);
        if let Some(app) = app_weak.upgrade() {
            app.set_cal_dirty(true);
        }
    });

    let app_weak = app.as_weak();
    let state_ref = Rc::clone(&state);
    app.on_changed_cal_x_rz(move |v| {
        let mut s = state_ref.borrow_mut();
        let Some(mut layout) = s.calibration.as_ref().map(|c| c.topology.clone()) else {
            return;
        };
        layout.x_rz = v as f64;
        s.apply_layout(layout);
        if let Some(app) = app_weak.upgrade() {
            app.set_cal_dirty(true);
        }
    });

    let app_weak = app.as_weak();
    let state_ref = Rc::clone(&state);
    app.on_changed_cal_z_rx(move |v| {
        let mut s = state_ref.borrow_mut();
        let Some(mut layout) = s.calibration.as_ref().map(|c| c.topology.clone()) else {
            return;
        };
        layout.z_rx = v as f64;
        s.apply_layout(layout);
        if let Some(app) = app_weak.upgrade() {
            app.set_cal_dirty(true);
        }
    });

    let app_weak = app.as_weak();
    let state_ref = Rc::clone(&state);
    app.on_changed_cal_ground_tilt_x(move |v| {
        let mut s = state_ref.borrow_mut();
        let Some(mut layout) = s.calibration.as_ref().map(|c| c.topology.clone()) else {
            return;
        };
        layout.ground_tilt_x = v as f64;
        s.apply_layout(layout);
        if let Some(app) = app_weak.upgrade() {
            app.set_cal_dirty(true);
        }
    });

    let app_weak = app.as_weak();
    let state_ref = Rc::clone(&state);
    app.on_changed_cal_ground_tilt_z(move |v| {
        let mut s = state_ref.borrow_mut();
        let Some(mut layout) = s.calibration.as_ref().map(|c| c.topology.clone()) else {
            return;
        };
        layout.ground_tilt_z = v as f64;
        s.apply_layout(layout);
        if let Some(app) = app_weak.upgrade() {
            app.set_cal_dirty(true);
        }
    });

    let app_weak = app.as_weak();
    let state_ref = Rc::clone(&state);
    app.on_changed_cal_top_tilt_x(move |v| {
        let mut s = state_ref.borrow_mut();
        let Some(mut layout) = s.calibration.as_ref().map(|c| c.topology.clone()) else {
            return;
        };
        layout.top_tilt_x = v as f64;
        s.apply_layout(layout);
        if let Some(app) = app_weak.upgrade() {
            app.set_cal_dirty(true);
        }
    });

    let app_weak = app.as_weak();
    let state_ref = Rc::clone(&state);
    app.on_changed_cal_top_tilt_z(move |v| {
        let mut s = state_ref.borrow_mut();
        let Some(mut layout) = s.calibration.as_ref().map(|c| c.topology.clone()) else {
            return;
        };
        layout.top_tilt_z = v as f64;
        s.apply_layout(layout);
        if let Some(app) = app_weak.upgrade() {
            app.set_cal_dirty(true);
        }
    });

    let app_weak = app.as_weak();
    let state_ref = Rc::clone(&state);
    app.on_changed_cal_ground_tilt_band_width(move |v| {
        let mut s = state_ref.borrow_mut();
        let Some(mut layout) = s.calibration.as_ref().map(|c| c.topology.clone()) else {
            return;
        };
        layout.ground_tilt_band_width = v as f64;
        s.apply_layout(layout);
        if let Some(app) = app_weak.upgrade() {
            app.set_cal_dirty(true);
        }
    });

    let app_weak = app.as_weak();
    let state_ref = Rc::clone(&state);
    app.on_changed_cal_top_tilt_band_width(move |v| {
        let mut s = state_ref.borrow_mut();
        let Some(mut layout) = s.calibration.as_ref().map(|c| c.topology.clone()) else {
            return;
        };
        layout.top_tilt_band_width = v as f64;
        s.apply_layout(layout);
        if let Some(app) = app_weak.upgrade() {
            app.set_cal_dirty(true);
        }
    });

    // Shared by both `save-calibration` (after the default-calibration
    // check passes) and `confirm-save-calibration` (user already said
    // "yes, overwrite the default" in the warning modal).
    fn do_save_calibration(state_ref: &Rc<RefCell<AppState>>, app_weak: &slint::Weak<RecoApp>) {
        // Snapshot the Export dialog's AI Tracking sliders into the
        // calibration before serializing, so re-opening this calibration
        // restores them instead of resetting to hardcoded literals. Unlike
        // the topology/lens/blend sliders, these aren't part of the live
        // renderer, so they're only synced here rather than on every edit.
        if let Some(app) = app_weak.upgrade() {
            let ac = snapshot_autocam_defaults(&app);
            let sb = snapshot_scoreboard_settings(&app, &state_ref.borrow());
            let mut s = state_ref.borrow_mut();
            if let Some(cal) = s.calibration.as_mut() {
                cal.autocam_defaults = Some(ac);
                cal.scoreboard = Some(sb);
            }
        }
        let save_result = state_ref.borrow().save_calibration();
        match save_result {
            Err(e) => {
                log::error!("Save calibration: {e}");
                let mut s = state_ref.borrow_mut();
                if let Some(app) = app_weak.upgrade() {
                    app.set_status_text("Save failed".into());
                    s.toasts.push(Severity::Error, "Save failed", e);
                    crate::toast::sync_to_ui(&s.toasts, &app);
                }
            }
            Ok(()) => {
                let mut s = state_ref.borrow_mut();
                if let Some(app) = app_weak.upgrade() {
                    app.set_status_text("Calibration saved".into());
                    // Clear BOTH dirty flags: the Save button is shown on
                    // `cal-dirty || lens-dirty`, so leaving lens-dirty set
                    // kept the button visible after a successful save.
                    app.set_cal_dirty(false);
                    app.set_lens_dirty(false);
                    s.toasts.push(Severity::Info, "Calibration saved", "");
                    crate::toast::sync_to_ui(&s.toasts, &app);
                }
            }
        }
    }

    let app_weak = app.as_weak();
    let state_ref = Rc::clone(&state);
    app.on_save_calibration(move || {
        if state_ref.borrow().is_default_calibration() {
            if let Some(app) = app_weak.upgrade() {
                app.set_overwrite_default_cal_warning_open(true);
            }
            return;
        }
        do_save_calibration(&state_ref, &app_weak);
    });

    let app_weak = app.as_weak();
    let state_ref = Rc::clone(&state);
    app.on_confirm_save_calibration(move || {
        do_save_calibration(&state_ref, &app_weak);
    });

    let app_weak = app.as_weak();
    let state_ref = Rc::clone(&state);
    app.on_reset_calibration(move || {
        let mut s = state_ref.borrow_mut();
        s.reset_calibration();
        if let (Some(app), Some(layout)) = (app_weak.upgrade(), s.cal_baseline.as_ref()) {
            app.set_cal_intersect(layout.topology.intersect as f32);
            app.set_cal_camera_axis_offset(layout.framing.axis_offset as f32);
            app.set_cal_x_ty(layout.topology.x_ty as f32);
            app.set_cal_x_rx(layout.topology.x_rx as f32);
            app.set_cal_z_rz(layout.topology.z_rz as f32);
            app.set_cal_x_rz(layout.topology.x_rz as f32);
            app.set_cal_z_rx(layout.topology.z_rx as f32);
            app.set_cal_ground_tilt_x(layout.topology.ground_tilt_x as f32);
            app.set_cal_ground_tilt_z(layout.topology.ground_tilt_z as f32);
            app.set_cal_top_tilt_x(layout.topology.top_tilt_x as f32);
            app.set_cal_top_tilt_z(layout.topology.top_tilt_z as f32);
            app.set_cal_ground_tilt_band_width(layout.topology.ground_tilt_band_width as f32);
            app.set_cal_top_tilt_band_width(layout.topology.top_tilt_band_width as f32);
            // The reset also restored framing tilt/roll and the topology's
            // blend in the renderer - keep the View-panel sliders in sync.
            app.set_rig_tilt((layout.framing.tilt as f32).to_degrees());
            app.set_rig_roll((layout.framing.roll as f32).to_degrees());
            app.set_blend_width(layout.topology.blend_width);
            app.set_cal_dirty(false);
        }
    });

    // ── Live lens tuning callbacks ──
    //
    // Each slider emits `changed-lens-param` which asks Rust to read
    // the current fx/fy/cx/cy/k1-k4 from the UI properties for the
    // selected camera, build a `Lens`, and push it through
    // `update_camera_params`. Cheap per reco-core Batch F.

    let app_weak = app.as_weak();
    let state_ref = Rc::clone(&state);
    app.on_changed_lens_param(move || {
        let Some(app) = app_weak.upgrade() else {
            return;
        };
        let mut s = state_ref.borrow_mut();
        let selected = app.get_lens_selected_camera();
        // Slider edits replace only the intrinsics; everything else on the
        // lens (dims, correction strength) is preserved from the current
        // document lens - rebuilding via a constructor here used to reset
        // the user's lens-correction toggle to full.
        let Some((left_base, right_base)) = s
            .calibration
            .as_ref()
            .map(|c| (c.lenses[0].clone(), c.lenses[1].clone()))
            .or_else(|| {
                s.bridge.as_ref().map(|b| {
                    let c = b.engine().calibration();
                    (c.lenses[0].clone(), c.lenses[1].clone())
                })
            })
        else {
            return;
        };

        let (left_params, right_params) = match selected.as_str() {
            "right" => {
                let p = reco_core::calibration::Lens {
                    fx: app.get_lens_right_fx() as f64,
                    fy: app.get_lens_right_fy() as f64,
                    cx: app.get_lens_right_cx() as f64,
                    cy: app.get_lens_right_cy() as f64,
                    distortion: [
                        app.get_lens_right_k1() as f64,
                        app.get_lens_right_k2() as f64,
                        app.get_lens_right_k3() as f64,
                        app.get_lens_right_k4() as f64,
                    ],
                    ..right_base.clone()
                };
                (None, Some(p))
            }
            "both" => {
                // Mirror the Left sliders to both cameras. The Both tab
                // only shows the left sliders in the UI; the user's
                // intent is "apply these values to both lenses in
                // lockstep". We also push the mirrored values back into
                // the right-* Slint properties so when the user toggles
                // to Right later the sliders show what got applied.
                app.set_lens_right_fx(app.get_lens_left_fx());
                app.set_lens_right_fy(app.get_lens_left_fy());
                app.set_lens_right_cx(app.get_lens_left_cx());
                app.set_lens_right_cy(app.get_lens_left_cy());
                app.set_lens_right_k1(app.get_lens_left_k1());
                app.set_lens_right_k2(app.get_lens_left_k2());
                app.set_lens_right_k3(app.get_lens_left_k3());
                app.set_lens_right_k4(app.get_lens_left_k4());
                let left = reco_core::calibration::Lens {
                    fx: app.get_lens_left_fx() as f64,
                    fy: app.get_lens_left_fy() as f64,
                    cx: app.get_lens_left_cx() as f64,
                    cy: app.get_lens_left_cy() as f64,
                    distortion: [
                        app.get_lens_left_k1() as f64,
                        app.get_lens_left_k2() as f64,
                        app.get_lens_left_k3() as f64,
                        app.get_lens_left_k4() as f64,
                    ],
                    ..left_base.clone()
                };
                let right = reco_core::calibration::Lens {
                    fx: left.fx,
                    fy: left.fy,
                    cx: left.cx,
                    cy: left.cy,
                    distortion: left.distortion,
                    ..right_base.clone()
                };
                (Some(left), Some(right))
            }
            _ => {
                let p = reco_core::calibration::Lens {
                    fx: app.get_lens_left_fx() as f64,
                    fy: app.get_lens_left_fy() as f64,
                    cx: app.get_lens_left_cx() as f64,
                    cy: app.get_lens_left_cy() as f64,
                    distortion: [
                        app.get_lens_left_k1() as f64,
                        app.get_lens_left_k2() as f64,
                        app.get_lens_left_k3() as f64,
                        app.get_lens_left_k4() as f64,
                    ],
                    ..left_base.clone()
                };
                (Some(p), None)
            }
        };
        // Mirror slider edits into the source-of-truth calibration too, so
        // they survive a preview rebuild / recalibration / save (not just the
        // live renderer).
        if let Some(cal) = s.calibration.as_mut() {
            if let Some(l) = &left_params {
                cal.lenses[0] = l.clone();
            }
            if let Some(r) = &right_params {
                cal.lenses[1] = r.clone();
            }
        }
        if let Some(bridge) = s.bridge.as_mut() {
            bridge
                .engine_mut()
                .update_camera_params(left_params, right_params);
        }
        s.preview_dirty = true;
        app.set_lens_dirty(true);
    });

    let app_weak = app.as_weak();
    let state_ref = Rc::clone(&state);
    app.on_reset_lens(move || {
        let Some(app) = app_weak.upgrade() else {
            return;
        };
        let mut s = state_ref.borrow_mut();
        // Baseline lens params live on the calibration we snapshotted
        // when auto-calibrate completed (see cal_baseline_*_params).
        let (left_base, right_base) = (
            s.cal_baseline_left_params.clone(),
            s.cal_baseline_right_params.clone(),
        );
        if let (Some(left), Some(right)) = (left_base.as_ref(), right_base.as_ref()) {
            set_lens_sliders(&app, left, right);
            // Mirror into the source-of-truth calibration too, matching
            // every other lens mutation path.
            if let Some(cal) = s.calibration.as_mut() {
                cal.lenses[0] = left.clone();
                cal.lenses[1] = right.clone();
            }
            if let Some(bridge) = s.bridge.as_mut() {
                bridge
                    .engine_mut()
                    .update_camera_params(Some(left.clone()), Some(right.clone()));
            }
            s.preview_dirty = true;
            app.set_lens_dirty(false);
        }
    });

    // ── Lens picker callbacks ──

    let state_ref = Rc::clone(&state);
    let app_weak = app.as_weak();
    app.on_lens_search_changed(move |query| {
        let s = state_ref.borrow();
        let (in_w, in_h) = s.playback.input_dimensions().unwrap_or((0, 0));
        let db = reco_calibrate::lens_database::LensDatabase::embedded();
        let results = db.search(query.as_str(), in_w, in_h);
        let model: Vec<slint::SharedString> = results
            .iter()
            .map(|r| {
                slint::SharedString::from(format!(
                    "{} - {} - {}x{}",
                    r.camera, r.lens, r.width, r.height
                ))
            })
            .collect();
        if let Some(app) = app_weak.upgrade() {
            app.set_lens_search_results(slint::ModelRc::new(slint::VecModel::from(model)));
        }
    });

    let state_ref = Rc::clone(&state);
    let app_weak = app.as_weak();
    app.on_lens_pick(move |idx, side| {
        let app_ref = app_weak.upgrade();
        let query = app_ref
            .as_ref()
            .map(|a| a.get_lens_search_query().to_string())
            .unwrap_or_default();
        let mut s = state_ref.borrow_mut();
        let (in_w, in_h) = s.playback.input_dimensions().unwrap_or((0, 0));
        let db = reco_calibrate::lens_database::LensDatabase::embedded();
        let results = db.search(&query, in_w, in_h);
        let idx = idx as usize;
        if idx < results.len() {
            let summary = &results[idx];
            if let Some(params) = db.load_by_summary(summary) {
                let side_str = side.as_str();
                log::info!(
                    "Lens picker: applying {} - {} ({}x{}) to {side_str}",
                    summary.camera,
                    summary.lens,
                    summary.width,
                    summary.height
                );
                let scale_w = in_w as f64 / params.width as f64;
                let scale_h = in_h as f64 / params.height as f64;
                let mut scaled = reco_core::calibration::Lens::fisheye(
                    in_w,
                    in_h,
                    params.fx * scale_w,
                    params.fy * scale_h,
                    params.cx * scale_w,
                    params.cy * scale_h,
                    params.distortion,
                );
                // A profile supplies intrinsics; the correction strength is
                // the user's render knob and must survive the pick.
                scaled.correction = s.lens_correction_amount;
                let (apply_left, apply_right) = match side_str {
                    "left" => (Some(scaled.clone()), None),
                    "right" => (None, Some(scaled.clone())),
                    _ => (Some(scaled.clone()), Some(scaled.clone())),
                };
                // Write the source-of-truth calibration so the pick survives a
                // preview rebuild, recalibration, and save - not just the live
                // renderer (the bug: picker updated only the renderer).
                if let Some(cal) = s.calibration.as_mut() {
                    if side_str != "right" {
                        cal.lenses[0] = scaled.clone();
                    }
                    if side_str != "left" {
                        cal.lenses[1] = scaled.clone();
                    }
                }
                if let Some(bridge) = s.bridge.as_mut() {
                    bridge
                        .engine_mut()
                        .update_camera_params(apply_left, apply_right);
                }
                s.preview_dirty = true;
                if let Some(app) = app_ref.as_ref() {
                    if side_str != "right" {
                        app.set_lens_left_camera(summary.camera.clone().into());
                        app.set_lens_left_source("Picker".into());
                    }
                    if side_str != "left" {
                        app.set_lens_right_camera(summary.camera.clone().into());
                        app.set_lens_right_source("Picker".into());
                    }
                    set_lens_sliders(app, &scaled, &scaled);
                    app.set_lens_dirty(true);
                    app.set_lens_info_available(true);
                }
            }
        }
    });

    let app_weak = app.as_weak();
    let state_ref = Rc::clone(&state);
    app.on_lens_pick_file(move || {
        let dialog = rfd::FileDialog::new()
            .set_title("Load lens profile JSON")
            .add_filter("JSON", &["json"]);
        if let Some(path) = dialog.pick_file() {
            match reco_calibrate::lens_database::load_from_file(&path) {
                Ok(params) => {
                    let mut s = state_ref.borrow_mut();
                    let (in_w, in_h) = s.playback.input_dimensions().unwrap_or((0, 0));
                    let scale_w = if params.width > 0 {
                        in_w as f64 / params.width as f64
                    } else {
                        1.0
                    };
                    let scale_h = if params.height > 0 {
                        in_h as f64 / params.height as f64
                    } else {
                        1.0
                    };
                    let mut scaled = reco_core::calibration::Lens::fisheye(
                        in_w,
                        in_h,
                        params.fx * scale_w,
                        params.fy * scale_h,
                        params.cx * scale_w,
                        params.cy * scale_h,
                        params.distortion,
                    );
                    // Preserve the user's correction knob across the pick.
                    scaled.correction = s.lens_correction_amount;
                    if let Some(cal) = s.calibration.as_mut() {
                        cal.lenses[0] = scaled.clone();
                        cal.lenses[1] = scaled.clone();
                    }
                    if let Some(bridge) = s.bridge.as_mut() {
                        bridge
                            .engine_mut()
                            .update_camera_params(Some(scaled.clone()), Some(scaled.clone()));
                    }
                    s.preview_dirty = true;
                    if let Some(app) = app_weak.upgrade() {
                        set_lens_sliders(&app, &scaled, &scaled);
                        app.set_lens_dirty(true);
                        app.set_lens_picker_open(false);
                        let name = display_name(&path);
                        app.set_lens_left_camera(name.clone().into());
                        app.set_lens_left_source("File".into());
                        app.set_lens_right_camera(name.into());
                        app.set_lens_right_source("File".into());
                        app.set_lens_info_available(true);
                        s.toasts.push(
                            crate::toast::Severity::Info,
                            "Lens profile loaded",
                            path.display().to_string(),
                        );
                        crate::toast::sync_to_ui(&s.toasts, &app);
                    }
                }
                Err(e) => {
                    log::error!("Failed to load lens profile: {e}");
                    let mut s = state_ref.borrow_mut();
                    if let Some(app) = app_weak.upgrade() {
                        s.toasts.push(
                            crate::toast::Severity::Error,
                            "Failed to load lens profile",
                            e.to_string(),
                        );
                        crate::toast::sync_to_ui(&s.toasts, &app);
                    }
                }
            }
        }
    });

    // Slint's <=> binding updates the use-constrained-look property but
    // does not call back into Rust. Without this notify, AppState's
    // use_constrained_look stays at its initial value forever and the
    // UI checkbox is cosmetic.
    let app_weak = app.as_weak();
    let state_ref = Rc::clone(&state);
    app.on_changed_constrained_look(move || {
        let Some(app) = app_weak.upgrade() else {
            return;
        };
        let new_value = app.get_use_constrained_look();
        let mut s = state_ref.borrow_mut();
        s.use_constrained_look = new_value;
        // When re-enabling, apply the clamp to the current target so
        // the camera snaps back inside coverage instead of waiting for
        // the next pan/zoom input.
        if new_value {
            s.clamp_targets();
        }
        s.preview_dirty = true;
    });

    // ── Lens preview mode callbacks ──

    let state_ref = Rc::clone(&state);
    let app_weak = app.as_weak();
    app.on_changed_lens_preview(move || {
        let Some(app) = app_weak.upgrade() else {
            return;
        };
        let mut s = state_ref.borrow_mut();
        s.lens_preview_active = app.get_lens_preview_active();
        s.lens_preview_side = app.get_lens_preview_side().to_string();
        // Must run before `sync_roi_points`, which reads
        // `lens_frame_aspect` to scale the polygon - otherwise the very
        // first time this fires (e.g. the first "Edit ROI" click after
        // startup) it uses Slint's stale `1.0` default instead of the
        // real camera aspect, drawing a warped polygon that only
        // self-corrects after a later side-switch primes this value.
        if let Some((w, h)) = s.playback.input_dimensions() {
            app.set_lens_frame_aspect(w as f32 / h as f32);
        }
        sync_roi_points(&s, &app);
        sync_goal_points(&s, &app);
        s.preview_dirty = true;
    });

    let state_ref = Rc::clone(&state);
    let app_weak = app.as_weak();
    app.on_changed_lens_correction(move |amount| {
        let mut s = state_ref.borrow_mut();
        let clamped = if amount > 0.5 { 1.0 } else { 0.0 };
        s.lens_correction_amount = clamped;
        // Always mirror into the source-of-truth calibration; the
        // lens_preview_active guard below only gates the live pipeline
        // write (visual), never the document - otherwise a save during
        // lens preview persisted a stale correction.
        if let Some(cal) = s.calibration.as_mut() {
            for lens in &mut cal.lenses {
                lens.correction = clamped;
            }
        }
        if !s.lens_preview_active
            && let Some(bridge) = s.bridge.as_mut()
        {
            bridge.engine_mut().set_lens_correction_amount(clamped);
        }
        s.preview_dirty = true;
        if let Some(app) = app_weak.upgrade() {
            app.set_lens_correction_amount(clamped);
            // Lens correction is persisted with the calibration.
            app.set_cal_dirty(true);
        }
    });

    // ── Toast dismissal ──
    //
    // Slint's ToastStack fires `toast-dismissed(id)` when the user
    // clicks the × on a card. Rust removes the matching entry and
    // pushes the refreshed list back to Slint.
    let app_weak = app.as_weak();
    let state_ref = Rc::clone(&state);
    app.on_toast_dismissed(move |id| {
        let mut s = state_ref.borrow_mut();
        s.toasts.dismiss(id);
        if let Some(app) = app_weak.upgrade() {
            crate::toast::sync_to_ui(&s.toasts, &app);
        }
    });

    // ── Export dialog callbacks ──
    //
    // "Open" populates default values from current state (blend width
    // from preview, output path blank so user must pick one). "Start"
    // spawns a background thread running StitchJob; progress flows
    // back via invoke_from_event_loop so Slint properties stay on the
    // UI thread. "Cancel" flips the AtomicBool the job polls.

    let app_weak = app.as_weak();
    let state_ref = Rc::clone(&state);
    app.on_open_export_dialog(move || {
        let s = state_ref.borrow();
        if let Some(app) = app_weak.upgrade() {
            // Seed from persisted user defaults first (codec, quality,
            // model path) so the dialog reflects the user's last
            // choices across sessions...
            app.set_export_codec(s.user_settings.default_codec.clone().into());
            app.set_export_quality(s.user_settings.default_quality.clone().into());
            if let Some(model_path) = s.user_settings.ai_model_path.as_ref() {
                app.set_export_model_path(model_path.to_string_lossy().to_string().into());
            }
            let clip_secs = if s.playback.fps() > 0.0 {
                s.playback.total_frames().unwrap_or(0) as f32 / s.playback.fps() as f32
            } else {
                0.0
            };
            app.set_clip_duration_secs(clip_secs);
            if app.get_export_end_secs() == 0.0 {
                app.set_export_end_secs(clip_secs);
            }
            app.set_export_dialog_open(true);
        }
    });

    let app_weak = app.as_weak();
    app.on_pick_export_output(move || {
        let dialog = rfd::FileDialog::new()
            .set_title("Export stitched video to…")
            .add_filter("MP4", &["mp4"])
            .add_filter("MOV", &["mov"])
            .add_filter("MKV", &["mkv"]);
        if let Some(mut path) = dialog.save_file() {
            // Ensure an extension — ffmpeg picks muxer by extension.
            if path.extension().is_none() {
                path.set_extension("mp4");
            }
            if let Some(app) = app_weak.upgrade() {
                app.set_export_output_path(path.to_string_lossy().to_string().into());
            }
        }
    });

    let app_weak = app.as_weak();
    let state_ref = Rc::clone(&state);
    app.on_pick_export_model(move || {
        let dialog = rfd::FileDialog::new()
            .set_title("Select YOLO ONNX model")
            .add_filter("ONNX", &["onnx"]);
        if let Some(path) = dialog.pick_file()
            && let Some(app) = app_weak.upgrade()
        {
            app.set_export_model_path(path.to_string_lossy().to_string().into());
            // Remember across sessions so the user doesn't re-pick
            // the same ONNX every run. Save is best-effort.
            let mut s = state_ref.borrow_mut();
            s.user_settings.ai_model_path = Some(path);
            s.user_settings.save();
        }
    });

    // Fires on every AI Tracking / panner slider edit (see the
    // `changed export-xxx` handlers in main.slint). Persists immediately
    // to `GuiSettings` so the values survive an app restart even if the
    // user never explicitly clicks Save calibration - see
    // `GuiSettings::autocam_defaults`.
    let app_weak = app.as_weak();
    let state_ref = Rc::clone(&state);
    app.on_autocam_settings_changed(move || {
        if let Some(app) = app_weak.upgrade() {
            let ac = snapshot_autocam_defaults(&app);
            let mut s = state_ref.borrow_mut();
            // These are persisted app-level below, but a loaded
            // calibration's own `autocam_defaults` still wins on the
            // next load - so an edit made now is silently lost for this
            // match unless the calibration is saved too. Flag it dirty
            // so the Save Calibration button appears instead of leaving
            // the user to guess: changing e.g. FOV Tight otherwise
            // looked like "settings are not remembered" (app-level had
            // the new value, the calibration still had the old one, and
            // reloading restored the old one with no visible cue).
            //
            // Compared against the calibration's *stored* values rather
            // than set unconditionally: `apply_autocam_defaults` writes
            // these same properties when a calibration loads, which
            // fires this very callback - an unconditional flag would
            // mark every freshly-loaded calibration dirty before the
            // user touched anything.
            let differs_from_calibration = s
                .calibration
                .as_ref()
                .is_some_and(|c| c.autocam_defaults.as_ref() != Some(&ac));
            if differs_from_calibration {
                app.set_cal_dirty(true);
            }
            s.user_settings.set_autocam_defaults(ac);
            // "AI Tracking"/"Async Detect" checkboxes: app-level only,
            // deliberately not part of `AutocamDefaults` - see
            // `GuiSettings::autocam_enabled`'s doc comment.
            s.user_settings.set_ai_toggle_defaults(
                app.get_export_autocam_enabled(),
                app.get_export_async_detect(),
            );
        }
    });

    let app_weak = app.as_weak();
    app.on_apply_panner_preset(move |name| {
        #[cfg(feature = "autocam")]
        if let Some(app) = app_weak.upgrade() {
            use reco_autocam::panners::{ClusterMode, FieldPannerConfig, FramingMode};
            let cfg = FieldPannerConfig::from_preset_name(name.as_str()).unwrap_or_default();
            let framing = if cfg.framing == FramingMode::FrameAll {
                "frame_all"
            } else {
                "action"
            };
            let cluster = if cfg.cluster_mode == ClusterMode::TrimmedMean {
                "trimmed_mean"
            } else {
                "density"
            };
            app.set_export_framing(framing.into());
            app.set_export_cluster_mode(cluster.into());
            app.set_export_lock_pitch(cfg.lock_pitch);
            app.set_export_cluster_bandwidth(cfg.cluster_bandwidth_rad);
            app.set_export_dead_zone(cfg.dead_zone_rad);
            app.set_export_ball_weight(cfg.ball_weight);
            app.set_export_ball_max_dist_from_cluster(cfg.ball_max_dist_from_cluster);
            app.set_export_fov_tight(cfg.fov_tight);
            app.set_export_fov_wide(cfg.fov_wide);
            app.set_export_fov_default(cfg.fov_default);
            app.set_export_fov_alpha(cfg.fov_alpha);
            app.set_export_cluster_alpha(cfg.cluster_alpha);
        }
        #[cfg(not(feature = "autocam"))]
        let _ = (&app_weak, &name);
    });

    let app_weak = app.as_weak();
    let state_ref = Rc::clone(&state);
    app.on_start_export(move || {
        let Some(app) = app_weak.upgrade() else {
            return;
        };
        let output_str = app.get_export_output_path().to_string();
        if output_str.is_empty() {
            log::warn!("Export: no output path set, ignoring");
            return;
        }
        log::info!("Export requested: output={output_str}");
        let mut s = state_ref.borrow_mut();
        if s.export_thread.is_some() {
            log::warn!("Export already running, ignoring start request");
            return;
        }
        let output_path = PathBuf::from(&output_str);
        if let Some(parent) = output_path.parent() {
            if !parent.exists() {
                log::warn!("Export: output directory does not exist: {}", parent.display());
                app.set_export_error_text(
                    format!("Output directory does not exist: {}", parent.display()).into(),
                );
                return;
            }
            if parent
                .metadata()
                .is_ok_and(|m| m.permissions().readonly())
            {
                log::warn!(
                    "Export: output directory has read-only attribute: {} (may be normal on Windows)",
                    parent.display()
                );
            }
        }
        let (Some(left_path), Some(right_path), Some(cal)) = (
            s.left_path.clone(),
            s.right_path.clone(),
            s.calibration.clone(),
        ) else {
            log::error!("Cannot start export without left/right/calibration");
            return;
        };
        let left = s
            .left_input
            .clone()
            .unwrap_or(reco_io::stitch_job::InputPath::Single(left_path));
        let right = s
            .right_input
            .clone()
            .unwrap_or(reco_io::stitch_job::InputPath::Single(right_path));

        // Snapshot all export settings. Slint properties must not be
        // read from the worker thread — only the UI thread owns them.
        let output = PathBuf::from(output_str);
        let width = app.get_export_width() as u32;
        let height = app.get_export_height() as u32;
        let codec_str = app.get_export_codec().to_string();
        let quality_str = app.get_export_quality().to_string();
        let blend = app.get_blend_width();
        let blend_flip_direction = app.get_blend_flip_direction();
        let multiband_blend_enabled = app.get_multiband_blend_enabled();
        let seam_offset = app.get_seam_offset();
        let start_secs = app.get_export_start_secs();
        let end_secs = app.get_export_end_secs();
        // Ranges are already clamped/validated on every add/edit (see
        // on_cut_range_add/on_cut_range_update), so CutRange::new should
        // never actually reject one here - filter_map is defense in
        // depth, not the primary validation. StitchJob::run separately
        // rejects overlaps (there's no live overlap-prevention in the
        // UI - see on_cut_range_update's doc comment) with a clear error
        // surfaced through the normal export-failure path.
        let cut_ranges: Vec<reco_io::cut_range::CutRange> = s
            .cut_ranges
            .iter()
            .filter_map(|&(start, end)| reco_io::cut_range::CutRange::new(start, end).ok())
            .collect();
        log::info!(
            "Export range: start={start_secs:.1}s, end={end_secs:.1}s, {} cut range(s)",
            cut_ranges.len()
        );
        let pause_overlay = app.get_export_pause_overlay_enabled().then(|| {
            (
                app.get_export_pause_overlay_fade_secs(),
                app.get_export_pause_overlay_hold_secs(),
            )
        });
        let autocam = crate::export::AutocamUiConfig {
            enabled: app.get_export_autocam_enabled(),
            model_path: app.get_export_model_path().to_string(),
            tracking_mode: app.get_export_tracking_mode().to_string(),
            detection_interval: app.get_export_detection_interval() as u32,
            player_anchor_rad: app.get_export_player_anchor_rad(),
            ball_coast_secs: app.get_export_ball_coast_secs(),
            confidence_threshold: app.get_export_confidence_threshold(),
            lookahead_secs: app.get_export_lookahead_secs() as f64,
            lookahead_reduced_bit_depth: app.get_export_lookahead_reduced_bit_depth(),
            async_detect: app.get_export_async_detect(),
            preset: app.get_export_panner_preset().to_string(),
            framing: app.get_export_framing().to_string(),
            lock_pitch: app.get_export_lock_pitch(),
            cluster_mode: app.get_export_cluster_mode().to_string(),
            cluster_bandwidth_rad: app.get_export_cluster_bandwidth(),
            dead_zone_rad: app.get_export_dead_zone(),
            ball_weight: app.get_export_ball_weight(),
            ball_max_dist_from_cluster: app.get_export_ball_max_dist_from_cluster(),
            fov_tight: app.get_export_fov_tight(),
            fov_wide: app.get_export_fov_wide(),
            fov_default: app.get_export_fov_default(),
            fov_alpha: app.get_export_fov_alpha(),
            cluster_alpha: app.get_export_cluster_alpha(),
        };
        let replay_enabled = app.get_export_replay_enabled();
        let events_enabled = app.get_export_events_enabled();
        let events_path = if events_enabled {
            Some(output_path.with_extension("events.jsonl"))
        } else {
            None
        };
        let scoreboard_package = if app.get_scoreboard_enabled() {
            s.scoreboard_packages
                .get(app.get_scoreboard_current_index().max(0) as usize)
                .cloned()
        } else {
            None
        };
        let scoreboard_state = s
            .scoreboard_runtime
            .as_ref()
            .and_then(reco_scoreboard::ScoreboardRuntime::current_editor_state);
        // When a Match Logger export is loaded, it drives the scoreboard
        // through the whole export instead of the one frozen snapshot
        // above - see `crate::export::ScoreboardReplay`.
        let scoreboard_replay = match (
            s.scoreboard_import.as_ref(),
            s.scoreboard_sync_anchor.as_ref(),
        ) {
            (Some(export), Some(anchor)) => Some(crate::export::ScoreboardReplay {
                export: export.clone(),
                anchor: *anchor,
            }),
            _ => None,
        };
        let scoreboard_placement = s.scoreboard_placement;
        let scoreboard_style = s.scoreboard_style.clone();

        // Persist the user's codec / quality / blend choices as the
        // defaults for next session. Model path is saved in the
        // on_pick_export_model callback so it sticks even if the user
        // never actually hits Start. Save is best-effort.
        s.user_settings.default_codec = codec_str.clone();
        s.user_settings.default_quality = quality_str.clone();
        s.user_settings.default_blend_width = blend;
        s.user_settings.save();

        // Reset cancel flag, start a fresh channel for completion.
        s.export_interrupted.store(false, Ordering::Relaxed);
        let interrupted = Arc::clone(&s.export_interrupted);
        let (tx, rx) = std::sync::mpsc::channel();
        s.export_rx = Some(rx);

        // Don't seed the timestamp - wait for the first real progress update.
        // Seeding with now() would trigger "Finalizing" if the first frame
        // takes > 1.5s (common on slow GPUs or large videos).
        *s.export_last_progress_at.lock().unwrap() = None;
        let last_progress_at = Arc::clone(&s.export_last_progress_at);

        // Pause preview playback to avoid GPU contention with the
        // export pipeline. Preview rendering is also gated by
        // is_exporting() in vsync_render_tick.
        s.playback.pause();
        app.set_playing(false);

        // Release the preview pipeline so its VRAM is free for the export;
        // rebuilt on completion. run_export uses its own source.
        log::info!("Releasing preview GPU pipeline to free VRAM for export");
        s.scoreboard_runtime = None;
        s.reset_pipeline();

        app.set_export_error_text("".into());
        app.set_export_in_progress(true);
        app.set_export_progress(0.0);
        app.set_export_frames_done(0);
        app.set_export_frames_total(0);
        app.set_export_status_text("Initializing…".into());
        app.set_export_dialog_open(false);

        let app_weak_bg = app_weak.clone();
        let output_for_thread = output.clone();
        let handle = std::thread::spawn(move || {
            let outcome = crate::export::run_export(
                left,
                right,
                cal,
                output_for_thread,
                None, // stream URL (Phase 6 GUI wiring)
                replay_enabled,
                events_path,
                width,
                height,
                codec_str,
                quality_str,
                blend,
                blend_flip_direction,
                multiband_blend_enabled,
                seam_offset,
                start_secs,
                end_secs,
                cut_ranges,
                pause_overlay,
                autocam,
                scoreboard_package,
                scoreboard_state,
                scoreboard_replay,
                scoreboard_placement,
                scoreboard_style,
                app_weak_bg,
                &interrupted,
                last_progress_at,
            );
            let _ = tx.send(outcome);
        });
        s.export_thread = Some(handle);
    });

    let state_ref = Rc::clone(&state);
    app.on_cancel_export(move || {
        let s = state_ref.borrow();
        log::info!("Cancel requested");
        s.export_interrupted.store(true, Ordering::Relaxed);
    });

    let state_ref = Rc::clone(&state);
    let app_weak = app.as_weak();
    app.on_toggle_recording(move |codec, quality| {
        let mut s = state_ref.borrow_mut();
        if s.is_recording() {
            if let Some((path, frames)) = s.stop_recording()
                && let Some(app) = app_weak.upgrade()
            {
                app.set_recording(false);
                app.set_last_output_path(path.display().to_string().into());
                s.toasts.push_with_ttl(
                    crate::toast::Severity::Info,
                    "Recording saved",
                    format!("{frames} frames\n{}", path.display()),
                    Duration::from_secs(8),
                );
                crate::toast::sync_to_ui(&s.toasts, &app);
            }
        } else {
            match s.start_recording(codec.as_str(), quality.as_str()) {
                Ok(path) => {
                    if let Some(app) = app_weak.upgrade() {
                        app.set_recording(true);
                        s.toasts.push(
                            crate::toast::Severity::Info,
                            "Recording started",
                            path.display().to_string(),
                        );
                        crate::toast::sync_to_ui(&s.toasts, &app);
                    }
                }
                Err(e) => {
                    log::error!("Recording failed: {e}");
                    if let Some(app) = app_weak.upgrade() {
                        s.toasts
                            .push(crate::toast::Severity::Error, "Recording failed", e);
                        crate::toast::sync_to_ui(&s.toasts, &app);
                    }
                }
            }
        }
    });

    app.on_show_in_folder(move |path| {
        let path = PathBuf::from(path.as_str());
        let folder = path.parent().unwrap_or(&path);
        if let Err(e) = open::that(folder) {
            log::error!("Failed to open folder: {e}");
        }
    });

    let state_ref = Rc::clone(&state);
    let app_weak = app.as_weak();
    app.on_submit_bug_report(move |user_message, contact, include_logs| {
        let mut s = state_ref.borrow_mut();
        let msg = user_message.to_string();
        let contact_str = contact.to_string();

        let report = if include_logs {
            let sys_report = build_bug_report(&s, &app_weak);
            format!(
                "## User description\n{msg}\n\n## Contact\n{}\n\n{sys_report}",
                if contact_str.trim().is_empty() {
                    "(not provided)"
                } else {
                    contact_str.trim()
                }
            )
        } else {
            format!(
                "## User description\n{msg}\n\n## Contact\n{}",
                if contact_str.trim().is_empty() {
                    "(not provided)"
                } else {
                    contact_str.trim()
                }
            )
        };

        // Always try to send via telemetry (even without opt-in -
        // bug reports are an explicit user action, not passive tracking).
        let cid = s
            .user_settings
            .telemetry_client_id
            .clone()
            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
        let client = s.telemetry.get_or_insert_with(|| {
            log::info!("Creating one-shot telemetry client for bug report");
            telemetry_client::TelemetryClient::new(cid)
        });
        client.bug_report(&report);

        // Save contact for next time
        if !contact_str.trim().is_empty() {
            s.user_settings
                .telemetry_client_id
                .get_or_insert_with(|| uuid::Uuid::new_v4().to_string());
            s.user_settings.save();
        }

        let _ = arboard::Clipboard::new().and_then(|mut cb| cb.set_text(report));

        s.toasts.push(
            crate::toast::Severity::Info,
            "Report sent",
            "Thank you! Your report has been sent to the developer.",
        );

        if let Some(app) = app_weak.upgrade() {
            crate::toast::sync_to_ui(&s.toasts, &app);
        }
    });

    // ── Playback timer ──

    let app_weak = app.as_weak();
    let state_ref = Rc::clone(&state);
    let timer = slint::Timer::default();
    let update_check = Arc::clone(&update_result);
    timer.start(
        slint::TimerMode::Repeated,
        std::time::Duration::from_millis(TICK_INTERVAL_MS as u64),
        move || {
            // Dev/test preload (RECO_AUTOLOAD): fire once, after the GPU
            // device has been captured by the rendering notifier.
            #[cfg(feature = "automation")]
            {
                let ready = {
                    let s = state_ref.borrow();
                    s.autoload.is_some() && s.shared_gpu.is_some()
                };
                if ready {
                    let spec = state_ref.borrow_mut().autoload.take();
                    if let Some(spec) = spec {
                        run_autoload(&state_ref, &app_weak, spec);
                    }
                }
            }

            let mut s = state_ref.borrow_mut();

            // Independent of rendering: the audio-sync waveform must keep
            // computing even while paused, when nothing else in this timer
            // tick would otherwise be dirty.
            s.maybe_recompute_audio_envelope(&app_weak);

            // Check for update notification from the background thread.
            // Surfaced as a toolbar button (`update-available`/`update-tag`)
            // the user can click when they're ready, not an unasked
            // browser tab - see `open-update-page`'s handler below for
            // where the actual `open::that` call now lives.
            if let Ok(mut guard) = update_check.try_lock()
                && let Some(tag) = guard.take()
                && let Some(app) = app_weak.upgrade()
            {
                        s.toasts.push_with_ttl(
                            Severity::Info,
                            format!("Update available: {tag}"),
                            "Click \"Update available\" in the toolbar to open the download page.",
                            Duration::from_secs(30),
                        );
                        log::info!("Toast pushed for update {tag}");
                        crate::toast::sync_to_ui(&s.toasts, &app);
                        app.set_update_available(true);
                        app.set_update_tag(tag.into());
            }

            // Poll for calibration results from the background thread.
            if let Some(rx) = &s.cal_rx
                && let Ok(result) = rx.try_recv()
            {
                s.cal_rx = None;
                handle_calibration_result(result, &mut s, &app_weak);
                return;
            }

            // Poll for standalone sync-offset detection results.
            if let Some(rx) = &s.sync_offset_job
                && let Ok(result) = rx.try_recv()
            {
                s.sync_offset_job = None;
                if let Some(app) = app_weak.upgrade() {
                    app.set_detecting_sync(false);
                    match result {
                        Ok(r) => {
                            s.set_sync_offset(r.frames as i32);
                            app.set_sync_offset(r.frames as i32);
                            app.set_cal_dirty(true);
                            let mut status = format!(
                                "Sync offset detected: {} frames ({})",
                                r.frames, r.method
                            );
                            if s.pending_sync_offset_autosave {
                                s.pending_sync_offset_autosave = false;
                                match s.save_calibration() {
                                    Ok(()) => {
                                        app.set_cal_dirty(false);
                                        status.push_str(" - saved to calibration");
                                    }
                                    Err(e) => {
                                        log::error!(
                                            "Failed to auto-save detected sync offset: {e}"
                                        );
                                        status.push_str(" - failed to save, see log");
                                    }
                                }
                            }
                            app.set_status_text(status.into());
                        }
                        Err(e) => {
                            s.pending_sync_offset_autosave = false;
                            app.set_status_text(format!("Sync-offset detection failed: {e}").into());
                        }
                    }
                }
                return;
            }

            // Poll the export worker for completion.
            if let Some(rx) = &s.export_rx
                && let Ok(outcome) = rx.try_recv()
            {
                s.export_rx = None;
                if let Some(h) = s.export_thread.take() {
                    let _ = h.join();
                }
                // Rebuild the preview from the in-memory calibration, on any
                // outcome.
                if let Some(cal) = s.calibration.clone() {
                    match s.init_with_calibration(cal) {
                        Ok(_) => {
                            s.preview_dirty = true;
                            log::info!("Rebuilt live preview after export");
                        }
                        Err(e) => log::warn!("Failed to rebuild preview after export: {e}"),
                    }
                }
                if let Some(app) = app_weak.upgrade() {
                    s.configure_scoreboard(
                        app.get_scoreboard_enabled(),
                        app.get_scoreboard_current_index().max(0) as usize,
                    );
                    app.set_scoreboard_error_text(s.scoreboard_error.clone().into());
                    app.set_export_in_progress(false);
                    app.set_export_progress(0.0);
                    match outcome {
                        ExportOutcome::Ok(frames, path) => {
                            app.set_export_status_text("".into());
                            app.set_status_text(
                                format!("Export complete: {frames} frames -> {}", path.display(),)
                                    .into(),
                            );
                            app.set_last_output_path(path.display().to_string().into());
                            if let Some(ref t) = s.telemetry {
                                let fps = s.playback.fps();
                                let dur = if fps > 0.0 { frames as f64 / fps } else { 0.0 };
                                let codec = app.get_export_codec().to_string();
                                t.export_complete(frames, dur, &codec);
                            }
                            s.toasts.push(
                                Severity::Info,
                                "Export complete",
                                format!("{frames} frames to {}", path.display()),
                            );
                            crate::toast::sync_to_ui(&s.toasts, &app);

                            // Repeat-export driver: measure VRAM now that the
                            // preview has been rebuilt (the steady state between
                            // runs), then either trigger the next export or quit.
                            // A leak shows up as `used` that never returns to the
                            // baseline logged at startup. Deferred via single_shot
                            // so the next start_export runs after this borrow of
                            // `s` is released (start_export borrows it too).
                            #[cfg(feature = "automation")]
                            if s.auto_export_mode {
                                s.auto_export_done += 1;
                                let done = s.auto_export_done;
                                let total = s.auto_export_total;
                                let vram = autoexport_vram();
                                if done < total {
                                    log::info!(
                                        "RECO_AUTOEXPORT: run {done}/{total} complete, {vram}; starting next"
                                    );
                                    let base = s.auto_export_base.clone();
                                    let app_w = app_weak.clone();
                                    slint::Timer::single_shot(
                                        std::time::Duration::from_millis(800),
                                        move || {
                                            if let Some(app) = app_w.upgrade() {
                                                if let Some(base) = base {
                                                    let out = autoexport_run_path(&base, done + 1);
                                                    app.set_export_output_path(
                                                        out.display().to_string().into(),
                                                    );
                                                }
                                                app.invoke_start_export();
                                            }
                                        },
                                    );
                                } else {
                                    log::info!(
                                        "RECO_AUTOEXPORT: all {total} runs complete, final {vram}; quitting"
                                    );
                                    slint::Timer::single_shot(
                                        std::time::Duration::from_millis(1500),
                                        || {
                                            let _ = slint::quit_event_loop();
                                        },
                                    );
                                }
                            }
                        }
                        ExportOutcome::Cancelled => {
                            app.set_export_status_text("".into());
                            app.set_status_text("Export cancelled".into());
                            #[cfg(feature = "automation")]
                            if s.auto_export_mode {
                                log::warn!(
                                    "RECO_AUTOEXPORT: run cancelled, {}; quitting",
                                    autoexport_vram()
                                );
                                slint::Timer::single_shot(
                                    std::time::Duration::from_millis(1000),
                                    || {
                                        let _ = slint::quit_event_loop();
                                    },
                                );
                            }
                        }
                        ExportOutcome::Failed(err) => {
                            app.set_export_status_text("".into());
                            let (title, body) = match &err {
                                reco_io::stitch_job::StitchError::EmptyOutput { .. } => (
                                    "Export produced no video",
                                    "The selected codec may not be supported on this hardware. Try H.264 or HEVC.".to_string(),
                                ),
                                reco_io::stitch_job::StitchError::Gpu(e) => (
                                    "GPU error",
                                    format!("{e}"),
                                ),
                                reco_io::stitch_job::StitchError::Calibration(e) => (
                                    "Calibration error",
                                    e.clone(),
                                ),
                                reco_io::stitch_job::StitchError::Session(
                                    reco_core::session::types::SessionError::Config(detail),
                                ) => ("Export failed", detail.clone()),
                                other => (
                                    "Export failed",
                                    format!("{other}"),
                                ),
                            };
                            // Build the detailed status now (body is moved into
                            // the toast below) and apply it AFTER sync_to_ui,
                            // whose status-bar fallback would otherwise replace
                            // it with the generic toast title.
                            let status = format!("Export failed - {body}");
                            let msg = format!("{err}");
                            if let Some(ref t) = s.telemetry {
                                let codec = app.get_export_codec().to_string();
                                t.export_error(&msg, &codec);
                            }
                            s.toasts.push(Severity::Error, title, body);
                            crate::toast::sync_to_ui(&s.toasts, &app);
                            app.set_status_text(status.into());
                            #[cfg(feature = "automation")]
                            if s.auto_export_mode {
                                log::warn!(
                                    "RECO_AUTOEXPORT: run failed ({msg}), {}; quitting",
                                    autoexport_vram()
                                );
                                slint::Timer::single_shot(
                                    std::time::Duration::from_millis(1000),
                                    || {
                                        let _ = slint::quit_event_loop();
                                    },
                                );
                            }
                        }
                    }
                }
                return;
            }

            // Finalizing is now signaled explicitly by the export thread
            // via StitchJob::on_finalizing(), no timer heuristic needed.

            // Persist window size, debounced (Tier 3d).
            if let Some(app) = app_weak.upgrade() {
                let size = app.window().size();
                let cur = (size.width, size.height);
                let maximized = app.window().is_maximized();
                s.user_settings.window_maximized = maximized;
                let last = s.last_persisted_window_size.unwrap_or((0, 0));
                if cur != last {
                    s.last_window_size_save_at = Some(Instant::now());
                    s.last_persisted_window_size = Some(cur);
                    if !maximized {
                        s.user_settings.window_size = Some(cur);
                    }
                } else if let Some(since) = s.last_window_size_save_at
                    && since.elapsed() > Duration::from_secs(2)
                {
                    s.user_settings.save();
                    s.last_window_size_save_at = None;
                }
            }

            // Expire aged toasts (Tier 3a).
            if !s.toasts.is_empty()
                && s.toasts.expire(Instant::now())
                && let Some(app) = app_weak.upgrade()
            {
                crate::toast::sync_to_ui(&s.toasts, &app);
            }

            // Commit a debounced seek once the requested fraction has
            // stopped changing. During drag the fraction is refreshed
            // every pixel, so the elapsed check never passes. Only
            // after the user lets go does ~120ms pass without new
            // requests, triggering one seek instead of hundreds.
            if !s.is_exporting()
                && let Some((frac, requested_at)) = s.pending_seek
                && Instant::now().duration_since(requested_at)
                    >= Duration::from_millis(SEEK_DEBOUNCE_MS)
            {
                s.pending_seek = None;
                match s.playback.seek(frac) {
                    Ok(()) => {
                        let img = s.render_current();
                        let total = s.playback.total_frames().unwrap_or(0);
                        let fps = s.playback.fps();
                        if let (Some(app), Some(img)) = (app_weak.upgrade(), img) {
                            app.set_preview_frame(img);
                            sync_frame_display(&app, s.playback.frame_index(), total, fps);
                            s.last_render_at = Some(Instant::now());
                        }
                    }
                    Err(e) => log::error!("Seek error: {e}"),
                }
                return;
            }

            // Playback, camera-lerp, and rendering now happen in
            // `vsync_render_tick` (driven by Slint's BeforeRendering
            // notifier), so this timer only handles work that does
            // not need vsync alignment. When playback is active we
            // nudge Slint to keep redrawing so BeforeRendering fires
            // even if nothing marked the window dirty yet.
            if let Some(app) = app_weak.upgrade()
                && (app.get_playing() || s.pending_seek.is_some() || s.preview_dirty)
            {
                app.window().request_redraw();
            }
        },
    );

    // ── Debug log panel refresh timer ──
    //
    // Only pushes a snapshot while the floating debug window is open -
    // the ring buffer itself (`DEBUG_LOG_BUFFER`) keeps accumulating in
    // the background regardless via the tracing layer installed in
    // `init_tracing`.
    let debug_log_timer = slint::Timer::default();
    let state_ref = Rc::clone(&state);
    debug_log_timer.start(
        slint::TimerMode::Repeated,
        std::time::Duration::from_millis(500),
        move || {
            let s = state_ref.borrow();
            if let Some(dw) = s.debug_window.as_ref()
                && dw.window().is_visible()
            {
                dw.set_log_text(debug_log_snapshot().into());
            }
        },
    );

    // Auto-open the files panel when no files are loaded so the user
    // sees the first action they need to take.
    if !app.get_files_loaded() {
        app.set_files_panel_open(true);
    }

    // First-paint kick: the scrubber row (and, going by the same
    // symptom, potentially anything else in the transport bar) has been
    // observed invisible immediately after launch, only appearing once
    // some later, unrelated property change (e.g. "+ Add cut") forces a
    // redraw - see the `request_redraw` nudge a few lines above this
    // function for the same class of issue already known to affect this
    // renderer ("BeforeRendering fires even if nothing marked the window
    // dirty yet"). That nudge only runs while playing/seeking/dirty, so
    // a freshly-opened, fully idle window never gets it. One explicit
    // kick shortly after the event loop starts covers the idle-startup
    // case the same way.
    let app_weak = app.as_weak();
    slint::Timer::single_shot(std::time::Duration::from_millis(150), move || {
        if let Some(app) = app_weak.upgrade() {
            app.window().request_redraw();
        }
    });

    // Closing the main window must also close the floating debug window
    // (a separate top-level window - see `on_open_debug_window` above).
    // Without this, the event loop's "quit once no windows remain"
    // mechanism keeps the process alive with just the orphaned debug
    // window left on screen after the main window closes.
    let state_ref = Rc::clone(&state);
    app.window().on_close_requested(move || {
        if let Some(dw) = state_ref.borrow().debug_window.as_ref() {
            let _ = dw.hide();
        }
        slint::CloseRequestResponse::HideWindow
    });

    app.run()?;
    Ok(())
}

/// Vsync-aligned playback tick. Called from Slint's `BeforeRendering`
/// notifier so render submissions land at deterministic phase
/// relative to the compositor's 60 Hz cycle. Returns `true` when a
/// frame was submitted.
fn vsync_render_tick(state: &Rc<RefCell<AppState>>, app_weak: &slint::Weak<RecoApp>) -> bool {
    let mut s = state.borrow_mut();
    if s.is_exporting() {
        return false;
    }

    s.push_scoreboard_replay();
    if s.poll_scoreboard_overlay() {
        s.preview_dirty = true;
    }
    if let Some(app) = app_weak.upgrade() {
        app.set_scoreboard_error_text(s.scoreboard_error.clone().into());
        app.set_scoreboard_editor_available(
            s.scoreboard_runtime
                .as_ref()
                .and_then(reco_scoreboard::ScoreboardRuntime::editor_url)
                .is_some(),
        );
        let network_editor_url = s
            .scoreboard_runtime
            .as_ref()
            .and_then(reco_scoreboard::ScoreboardRuntime::network_editor_url);
        app.set_scoreboard_share_available(network_editor_url.is_some());
        app.set_scoreboard_share_url(network_editor_url.unwrap_or_default().into());
        if network_editor_url.is_none() {
            app.set_scoreboard_share_dialog_open(false);
        }
    }

    // Adaptive preview: resize render target to match the preview
    // container. Skipped during recording because the encoder was
    // initialized at a fixed resolution and the NV12 readback must
    // match. Also caps at 1920x1080 to prevent GPU starvation on
    // high-DPI displays.
    let mut viewport_resized = false;
    if !s.is_recording()
        && let Some(app) = app_weak.upgrade()
        && let Some(bridge) = s.bridge.as_mut()
    {
        let area_w = (app.get_preview_area_width().max(320.0) as u32).min(1920);
        let area_h = (app.get_preview_area_height().max(240.0) as u32).min(1080);
        let (cur_w, cur_h) = bridge.viewport_size();
        if area_w.abs_diff(cur_w) > 16 || area_h.abs_diff(cur_h) > 16 {
            bridge.resize(area_w, area_h);
            s.preview_dirty = true;
            viewport_resized = true;
        }
    }
    if viewport_resized {
        // The contain-fit ratio the scoreboard's render scale depends
        // on changes with the viewport - see
        // `apply_scoreboard_render_scale`'s own doc comment.
        s.apply_scoreboard_render_scale();
    }

    let camera_changed = s.smooth_camera();
    let video_advanced = match s.playback.tick() {
        Ok(advanced) => advanced,
        Err(e) => {
            log::error!("Playback tick error: {e}");
            if let Some(app) = app_weak.upgrade() {
                app.set_status_text(format!("Error: {e}").into());
            }
            false
        }
    };

    let was_dirty = s.preview_dirty;
    if !(camera_changed || video_advanced || was_dirty) {
        return false;
    }
    // Clear dirty only if the camera has fully converged on its target.
    // If the lerp still has work to do, keep dirty so the timer will
    // nudge Slint for another BeforeRendering on the next tick - that
    // is how paused panning stays smooth until the user lets go AND
    // the camera eases to rest.
    s.preview_dirty = camera_changed;

    let img = s.render_current();
    if let (Some(app), Some(img)) = (app_weak.upgrade(), img) {
        app.set_preview_frame(img);
        s.last_render_at = Some(Instant::now());
        if video_advanced {
            let fps = s.playback.fps();
            let total = s.playback.total_frames().unwrap_or(0);
            sync_frame_display(&app, s.playback.frame_index(), total, fps);
            if s.playback.state() == PlayState::Finished {
                app.set_playing(false);
                app.set_status_text("Playback finished".into());
            }
        }
        // Reflect camera state to the UI properties so sliders and
        // the reset button stay in sync with what the user is
        // actually seeing. FOV comes from PoseControl's current (the
        // renderer pipeline's fov is driven by `smooth_camera`).
        let current = s.pose.current_pose();
        app.set_yaw(current.yaw);
        app.set_pitch(current.pitch);
        if let Some(fov) = current.fov_degrees {
            app.set_fov(fov);
        }
        // The AI pitch-limit overlay's screen position depends on this
        // same live pose - only worth recomputing while it's actually
        // shown (`show_pitch_limits`), skipped otherwise so idle
        // playback doesn't pay for it every frame.
        if app.get_show_pitch_limits() {
            sync_pitch_limit_overlay(&s, &app);
        }
        if let Some(bridge) = s.bridge.as_ref() {
            let c = bridge.engine().pipeline().color_match_correction();
            app.set_color_match_status(
                format!(
                    "L: Y{:+.3} U{:+.3} V{:+.3}   R: Y{:+.3} U{:+.3} V{:+.3}",
                    c.left_offset[0],
                    c.left_offset[1],
                    c.left_offset[2],
                    c.right_offset[0],
                    c.right_offset[1],
                    c.right_offset[2],
                )
                .into(),
            );
            // This render is the first one after "Remeasure now" was
            // clicked (see `remeasure_color_match`), so `c` is the fresh
            // result - surface it as a toast, since the Debug panel's log
            // line is easy to miss and the status text alone is easy to
            // scroll past if the Color Mapping section isn't expanded.
            if s.color_match_remeasure_pending {
                s.color_match_remeasure_pending = false;
                s.toasts.push(
                    Severity::Info,
                    "Color match remeasured",
                    format!(
                        "L: Y{:+.3} U{:+.3} V{:+.3}   R: Y{:+.3} U{:+.3} V{:+.3}",
                        c.left_offset[0],
                        c.left_offset[1],
                        c.left_offset[2],
                        c.right_offset[0],
                        c.right_offset[1],
                        c.right_offset[2],
                    ),
                );
                crate::toast::sync_to_ui(&s.toasts, &app);
            }
        }
        return true;
    }
    false
}

/// GPU free/used VRAM (MB) via `nvidia-smi`, for repeat-export leak logging.
/// Returns a short descriptive string; never fails the run.
#[cfg(feature = "automation")]
fn autoexport_vram() -> String {
    std::process::Command::new("nvidia-smi")
        .args([
            "--query-gpu=memory.free,memory.used",
            "--format=csv,noheader,nounits",
        ])
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| format!("free/used MB = {}", s.trim()))
        .unwrap_or_else(|| "nvidia-smi unavailable".into())
}

/// Output path for repeat-export run `n`: `base_n.ext` (1-based), so successive
/// runs in one session do not overwrite each other.
#[cfg(feature = "automation")]
fn autoexport_run_path(base: &std::path::Path, n: u32) -> PathBuf {
    let stem = base
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("export");
    let ext = base.extension().and_then(|e| e.to_str()).unwrap_or("mp4");
    let name = format!("{stem}_{n}.{ext}");
    base.with_file_name(name)
}

/// Execute an [`AutoloadSpec`]: set inputs + calibration, build the preview,
/// and optionally start an export. Dev/test hook (RECO_AUTOLOAD) only.
#[cfg(feature = "automation")]
fn run_autoload(
    state: &Rc<RefCell<AppState>>,
    app_weak: &slint::Weak<RecoApp>,
    spec: AutoloadSpec,
) {
    log::info!(
        "RECO_AUTOLOAD: preloading {} left + {} right segment(s), cal {}",
        spec.left.len(),
        spec.right.len(),
        spec.cal.display()
    );
    let make_input = |paths: &[PathBuf]| {
        if paths.len() == 1 {
            reco_io::stitch_job::InputPath::Single(paths[0].clone())
        } else {
            reco_io::stitch_job::InputPath::Chained(paths.to_vec())
        }
    };
    {
        let mut s = state.borrow_mut();
        s.left_input = Some(make_input(&spec.left));
        s.left_path = Some(spec.left[0].clone());
        s.right_input = Some(make_input(&spec.right));
        s.right_path = Some(spec.right[0].clone());
        s.calibration_path = Some(spec.cal.clone());
        if let Some(app) = app_weak.upgrade() {
            app.set_left_path(display_name(&spec.left[0]).into());
            app.set_right_path(display_name(&spec.right[0]).into());
            app.set_calibration_path(display_name(&spec.cal).into());
        }
    }
    try_init_and_update(state, app_weak);
    if let Some(exp) = spec.export
        && let Some(app) = app_weak.upgrade()
    {
        // Repeat-export driver: run `exp.repeats` exports back-to-back in this
        // one session so a cross-export GPU leak shows up as VRAM that never
        // returns to baseline. The completion handler re-triggers the next run
        // and quits when done. `remaining` counts runs AFTER this first one.
        {
            let mut s = state.borrow_mut();
            s.auto_export_mode = true;
            s.auto_export_total = exp.repeats;
            s.auto_export_done = 0;
            s.auto_export_base = Some(exp.output.clone());
        }
        let first_out = autoexport_run_path(&exp.output, 1);
        app.set_export_output_path(first_out.display().to_string().into());
        if let Some(model) = &exp.model {
            app.set_export_autocam_enabled(true);
            app.set_export_model_path(model.display().to_string().into());
            app.set_export_tracking_mode("field".into());
            app.set_export_lookahead_secs(exp.lookahead_secs);
        }
        log::info!(
            "RECO_AUTOEXPORT: baseline {}; starting export 1/{} -> {} (lookahead {}s)",
            autoexport_vram(),
            exp.repeats,
            first_out.display(),
            exp.lookahead_secs
        );
        app.invoke_start_export();
    }
}

/// VRAM budget (bytes) available to the lookahead pool at export time.
///
/// Uses the same free-trusting estimate as the export pre-flight. Note the
/// slider samples `free` while the live preview is still resident, whereas the
/// export releases the preview first, so this figure is slightly conservative
/// (it counts the preview against the budget) - a safe direction: the slider
/// never shows green for a lookahead the export would reject. A test build can
/// override the budget via `RECO_VRAM_BUDGET_GB` to exercise the risk zones on a
/// large GPU.
fn budget_for_lookahead(free_vram: u64, total_vram: u64) -> usize {
    #[cfg(feature = "automation")]
    if let Ok(gb) = std::env::var("RECO_VRAM_BUDGET_GB")
        && let Ok(v) = gb.parse::<f64>()
    {
        return (v * 1e9) as usize;
    }
    // Same budget the export pre-flight uses, so the slider's risk zones match
    // what the engine will accept.
    reco_core::session::lookahead_budget_bytes(free_vram, total_vram)
}

/// Kick off a standalone sync-offset detection job (IMU, falling back to
/// audio) against the currently loaded left/right videos. Shared by the
/// manual "Detect Sync Offset" button and the "Select Match Folder"
/// sync-offset prompt - the two differ only in what happens once the
/// background job resolves (see `pending_sync_offset_autosave`).
fn start_sync_offset_detection(state: &Rc<RefCell<AppState>>, app_weak: &slint::Weak<RecoApp>) {
    let s = state.borrow();
    let (left, right) = match (&s.left_path, &s.right_path) {
        (Some(l), Some(r)) => (l.clone(), r.clone()),
        _ => return,
    };
    drop(s);

    let Some(app) = app_weak.upgrade() else {
        return;
    };
    app.set_detecting_sync(true);
    app.set_status_text("Detecting sync offset (IMU, falling back to audio)...".into());

    let rx = sync_offset::spawn_compute_sync_offset(left, right);
    state.borrow_mut().sync_offset_job = Some(rx);
}

fn try_init_and_update(state: &Rc<RefCell<AppState>>, app_weak: &slint::Weak<RecoApp>) {
    let mut s = state.borrow_mut();
    if let Some(app) = app_weak.upgrade() {
        sync_segments(&s, &app);
    }
    // Capture the pre-init clip length so we can distinguish an input
    // change (new load / appended segments) from a calibration-only
    // reload further down.
    let prev_total = s.playback.total_frames();
    match s.try_init() {
        Ok(true) => {
            let fps = s.playback.fps();
            let total = s.playback.total_frames().unwrap_or(0);

            s.clamp_targets();
            let clamped_fov = s.pose.current_fov_deg();
            if let Some(bridge) = s.bridge.as_mut() {
                bridge.engine_mut().set_fov(clamped_fov);
            }
            let img = s.render_current();
            // Seed calibration slider values from the baseline layout.
            let layout = s.cal_baseline.clone();

            let (in_w, in_h) = s.playback.input_dimensions().unwrap_or((0, 0));

            // Snapshot current lens params as the fine-tune baseline so
            // Reset Lens can restore them. For manual match.json loads
            // this comes from the loaded calibration directly.
            let lens_baseline = s.bridge.as_ref().map(|b| {
                let cal = b.engine().calibration();
                (cal.lenses[0].clone(), cal.lenses[1].clone())
            });
            if let Some((l, r)) = lens_baseline.as_ref() {
                s.cal_baseline_left_params = Some(l.clone());
                s.cal_baseline_right_params = Some(r.clone());
            }
            // Same for viewport-level settings (rig tilt, blend width).
            let rig_tilt_rad = s
                .bridge
                .as_ref()
                .map(|b| b.engine().calibration().framing.tilt as f32);
            let rig_roll_rad = s
                .bridge
                .as_ref()
                .map(|b| b.engine().calibration().framing.roll as f32);
            let blend_width = s
                .bridge
                .as_ref()
                .map(|b| b.engine().calibration().topology.blend_width);
            let blend_flip_direction = s
                .bridge
                .as_ref()
                .map(|b| b.engine().calibration().topology.blend_flip_direction);
            let seam_offset = s
                .bridge
                .as_ref()
                .map(|b| b.engine().calibration().topology.seam_offset);
            let multiband_blend_enabled = s
                .bridge
                .as_ref()
                .map(|b| b.engine().calibration().topology.multiband_blend_enabled);
            let color_match_enabled = s
                .bridge
                .as_ref()
                .map(|b| b.engine().calibration().topology.color_match_enabled);
            let color_match_band_width = s
                .bridge
                .as_ref()
                .map(|b| b.engine().calibration().topology.color_match_band_width);
            let color_match_grid_cols = s
                .bridge
                .as_ref()
                .map(|b| b.engine().calibration().topology.color_match_grid_cols);
            let color_match_grid_rows = s
                .bridge
                .as_ref()
                .map(|b| b.engine().calibration().topology.color_match_grid_rows);
            let color_match_interval_frames = s.bridge.as_ref().map(|b| {
                b.engine()
                    .calibration()
                    .topology
                    .color_match_interval_frames
            });
            let color_match_ema_alpha = s
                .bridge
                .as_ref()
                .map(|b| b.engine().calibration().topology.color_match_ema_alpha);
            let color_match_max_y_offset = s
                .bridge
                .as_ref()
                .map(|b| b.engine().calibration().topology.color_match_max_y_offset);
            let color_match_max_chroma_offset = s.bridge.as_ref().map(|b| {
                b.engine()
                    .calibration()
                    .topology
                    .color_match_max_chroma_offset
            });
            let color_gamma = s.bridge.as_ref().map(|b| {
                let t = &b.engine().calibration().topology;
                (t.color_gamma_left, t.color_gamma_right)
            });
            let color_match_auto_gamma = s
                .bridge
                .as_ref()
                .map(|b| b.engine().calibration().topology.color_match_auto_gamma);
            // Lens-correction strength came in via the loaded calibration and
            // the renderer was seeded with it at bridge creation; mirror it
            // into AppState so a later save re-persists the right value.
            let lens_correction = s.calibration.as_ref().map(|c| c.lenses[0].correction);
            if let Some(lc) = lens_correction {
                s.lens_correction_amount = lc;
            }

            if let Some(app) = app_weak.upgrade() {
                app.set_files_loaded(true);
                // Restore the Export dialog's AI Tracking sliders from the
                // calibration, if it was saved with defaults (see
                // `do_save_calibration`). Applied before the VRAM lookahead
                // check below so a restored lookahead value is still
                // subject to that same safety clamp.
                if let Some(ac) = s
                    .calibration
                    .as_ref()
                    .and_then(|c| c.autocam_defaults.as_ref())
                {
                    apply_autocam_defaults(&app, ac);
                    log::info!("Restored AI Tracking defaults from calibration");
                }
                // apply_scoreboard_settings borrows `state` itself, so `s`
                // has to be released for the duration of the call - it's
                // re-borrowed right after to keep the rest of this
                // function (VRAM checks etc. below) working unchanged.
                if let Some(mut sb) = s.calibration.as_ref().and_then(|c| c.scoreboard.clone()) {
                    // The calibration's own copy of the "preference"-style
                    // fields (placement, banner color, font, logo size,
                    // auto-cut) is whatever was true when THIS calibration
                    // was last saved - it goes stale the moment the user
                    // tweaks those app-wide from a different match, and
                    // applying it here would silently undo the app-level
                    // persistence added for exactly this reason (see
                    // `persist_scoreboard_settings`). Match-specific bits
                    // (Match Logger export, sync anchor, team logos,
                    // package/enabled) still come from the calibration.
                    // The app-level settings win for the rest whenever
                    // they exist.
                    if let Some(app_sb) = s.user_settings.scoreboard_settings.clone() {
                        sb.placement = app_sb.placement;
                        sb.banner_color_name = app_sb.banner_color_name;
                        sb.font_family = app_sb.font_family;
                        sb.logo_size_px = app_sb.logo_size_px;
                        sb.derive_cut_ranges = app_sb.derive_cut_ranges;
                        sb.cut_lead_secs = app_sb.cut_lead_secs;
                        sb.cut_trail_secs = app_sb.cut_trail_secs;
                        sb.kickoff_lead_secs = app_sb.kickoff_lead_secs;
                        sb.match_end_trail_secs = app_sb.match_end_trail_secs;
                    }
                    drop(s);
                    apply_scoreboard_settings(state, &app, &sb);
                    s = state.borrow_mut();
                    log::info!("Restored scoreboard settings from calibration");
                }
                // Lookahead VRAM risk thresholds for the export slider. The
                // pool stores source-resolution frames (re-rendered into the
                // export), so the ceiling scales with source resolution, not
                // output. Budget = total VRAM minus a reserve for
                // decode/stitch/encode/AI; the preview is freed during export,
                // so it is not counted here.
                match s
                    .bridge
                    .as_ref()
                    .and_then(|b| b.engine().gpu().available_vram())
                {
                    Some((free, total)) if total > 0 && in_w > 0 && in_h > 0 => {
                        let budget = budget_for_lookahead(free, total);
                        let fit = reco_core::session::lookahead_fit(in_w, in_h, 1, budget, fps);
                        app.set_lookahead_green_max(fit.safe_secs as f32);
                        app.set_lookahead_red_min(fit.max_secs as f32);
                        app.set_lookahead_risk_active(true);
                        log::info!(
                            "Lookahead VRAM fit: safe<={:.1}s tight<={:.1}s @ {in_w}x{in_h}, \
                             budget {:.1} GB",
                            fit.safe_secs,
                            fit.max_secs,
                            budget as f64 / 1e9,
                        );
                        // VRAM-aware default: if the current lookahead would
                        // not fit (red zone = guaranteed export failure), seed
                        // it to the comfortable value instead. This only
                        // rescues an unusable value; a lookahead that already
                        // fits is left as the user set it, so a deliberate
                        // in-budget choice is never overridden.
                        if app.get_export_lookahead_secs() as f64 > fit.max_secs {
                            let safe = fit.safe_secs as f32;
                            app.set_export_lookahead_secs(safe);
                            log::info!(
                                "Lookahead lowered to {safe:.1}s to fit VRAM \
                                 (chosen value exceeded the {:.1}s ceiling)",
                                fit.max_secs,
                            );
                        }
                    }
                    _ => app.set_lookahead_risk_active(false),
                }
                app.set_has_roi(
                    s.calibration
                        .as_ref()
                        .and_then(|c| c.field_roi.as_ref())
                        .is_some_and(|r| !r.left.is_empty() || !r.right.is_empty()),
                );
                app.set_has_goal_geometry(
                    s.calibration
                        .as_ref()
                        .and_then(|c| c.goal_geometry.as_ref())
                        .is_some_and(|g| !g.left.is_empty() || !g.right.is_empty()),
                );
                sync_goal_points(&s, &app);
                sync_pitch_limit_status(&s, &app);
                sync_frame_display(&app, s.playback.frame_index(), total, fps);
                app.set_fps(fps as f32);
                app.set_playback_speed(s.playback.speed() as f32);
                app.set_status_text(format!("Ready - {:.0} fps - {total} frames", fps).into());
                // The export trim defaults to the whole clip. When the input
                // length changes (new file, appended segments, or a shorter
                // clip) a previously seeded trim end would stick to the old
                // duration and silently truncate the export, so refresh it
                // here. A manual trim survives a calibration-only reload,
                // where the frame total is unchanged.
                if prev_total != Some(total) && fps > 0.0 {
                    let clip_secs = total as f32 / fps as f32;
                    app.set_clip_duration_secs(clip_secs);
                    app.set_export_start_secs(0.0);
                    app.set_export_end_secs(clip_secs);
                    log::info!(
                        "Export trim reset to full clip ({clip_secs:.1}s, {total} frames) after input change"
                    );
                }
                if let Some(img) = img {
                    app.set_preview_frame(img);
                }
                if let Some(layout) = layout {
                    app.set_cal_intersect(layout.topology.intersect as f32);
                    app.set_cal_camera_axis_offset(layout.framing.axis_offset as f32);
                    app.set_cal_x_ty(layout.topology.x_ty as f32);
                    app.set_cal_x_rx(layout.topology.x_rx as f32);
                    app.set_cal_z_rz(layout.topology.z_rz as f32);
                    app.set_cal_x_rz(layout.topology.x_rz as f32);
                    app.set_cal_z_rx(layout.topology.z_rx as f32);
                    app.set_cal_ground_tilt_x(layout.topology.ground_tilt_x as f32);
                    app.set_cal_ground_tilt_z(layout.topology.ground_tilt_z as f32);
                    app.set_cal_top_tilt_x(layout.topology.top_tilt_x as f32);
                    app.set_cal_top_tilt_z(layout.topology.top_tilt_z as f32);
                    app.set_cal_ground_tilt_band_width(
                        layout.topology.ground_tilt_band_width as f32,
                    );
                    app.set_cal_top_tilt_band_width(layout.topology.top_tilt_band_width as f32);
                    app.set_cal_dirty(false);
                }
                if let Some(rt) = rig_tilt_rad {
                    app.set_rig_tilt(rt.to_degrees());
                }
                if let Some(rr) = rig_roll_rad {
                    app.set_rig_roll(rr.to_degrees());
                }
                if let Some(bw) = blend_width {
                    app.set_blend_width(bw);
                }
                if let Some(flip) = blend_flip_direction {
                    app.set_blend_flip_direction(flip);
                }
                if let Some(so) = seam_offset {
                    app.set_seam_offset(so);
                }
                if let Some(mb) = multiband_blend_enabled {
                    app.set_multiband_blend_enabled(mb);
                }
                if let Some(v) = color_match_enabled {
                    app.set_color_match_enabled(v);
                }
                if let Some(v) = color_match_band_width {
                    app.set_color_match_band_width(v);
                }
                if let Some(v) = color_match_grid_cols {
                    app.set_color_match_grid_cols(v as f32);
                }
                if let Some(v) = color_match_grid_rows {
                    app.set_color_match_grid_rows(v as f32);
                }
                if let Some(v) = color_match_interval_frames {
                    app.set_color_match_interval_frames(v as f32);
                }
                if let Some(v) = color_match_ema_alpha {
                    app.set_color_match_ema_alpha(v);
                }
                if let Some(v) = color_match_max_y_offset {
                    app.set_color_match_max_y_offset(v);
                }
                if let Some(v) = color_match_max_chroma_offset {
                    app.set_color_match_max_chroma_offset(v);
                }
                if let Some((left, right)) = color_gamma {
                    app.set_color_gamma_left(left);
                    app.set_color_gamma_right(right);
                }
                if let Some(v) = color_match_auto_gamma {
                    app.set_color_match_auto_gamma(v);
                }
                if let Some(lc) = lens_correction {
                    app.set_lens_correction_amount(lc);
                }
                // Angular resolution of the source, for the zoom-range
                // preview's "upscale" readout: a KB4 lens maps r = fx *
                // theta, so fx is exactly the pixels per radian the sensor
                // resolves at centre.
                if let Some(cal) = s.calibration.as_ref() {
                    app.set_lens_px_per_rad(cal.lenses[0].fx as f32);
                }
                if let Some(cal) = s.calibration.as_ref() {
                    app.set_sync_offset(cal.sync_offset as i32);
                }
                app.set_fov(clamped_fov);
                // Manual calibration JSON does not embed lens-profile info,
                // so clear the display (hide the lens card) and just show
                // the candidates count for this resolution so the user
                // still knows how many database entries could match.
                set_lens_profile_props(&app, None, None, in_w, in_h);
                // Lens fine-tune sliders are seeded from the loaded
                // calibration's camera params either way; the Lens
                // fine-tune section is gated on `files-loaded` in Slint.
                if let Some((l, r)) = lens_baseline.as_ref() {
                    set_lens_sliders(&app, l, r);
                    app.set_lens_dirty(false);
                }
                // Seed export dialog output filename suggestion: inside
                // the match folder when one is active, else next to the
                // left video file (see `suggested_export_path`).
                if let Some(suggested) =
                    suggested_export_path(s.match_folder.as_deref(), s.left_path.as_deref())
                {
                    app.set_export_output_path(suggested.to_string_lossy().to_string().into());
                }

                if let Some(ref t) = s.telemetry {
                    let gpu = s
                        .bridge
                        .as_ref()
                        .map(|b| {
                            let g = b.engine().gpu();
                            format!("{} ({:?})", g.gpu_name(), g.backend_name())
                        })
                        .unwrap_or_else(|| "unknown".into());
                    let os = format!("{} {}", std::env::consts::OS, std::env::consts::ARCH);
                    let (ai_status, _) = ai_capability_summary();
                    t.context(&gpu, &os, &ai_status);
                    let (w, h) = s.playback.input_dimensions().unwrap_or((0, 0));
                    let fps = s.playback.fps();
                    let decoder = s
                        .bridge
                        .as_ref()
                        .map(|_| "D3D11VA/NVDEC/VT")
                        .unwrap_or("unknown");
                    let sync = s.calibration.as_ref().map(|c| c.sync_offset).unwrap_or(0);
                    t.source_info(w, h, fps, decoder, sync);
                }
            }
        }
        Ok(false) => {
            // Not all files selected yet - update status.
            let s_ref = &*s;
            let missing: Vec<&str> = [
                s_ref.left_path.is_none().then_some("left video"),
                s_ref.right_path.is_none().then_some("right video"),
                s_ref.calibration_path.is_none().then_some("calibration"),
            ]
            .into_iter()
            .flatten()
            .collect();

            if let Some(app) = app_weak.upgrade() {
                app.set_status_text(format!("Still need: {}", missing.join(", ")).into());
            }
        }
        Err(e) => {
            log::error!("Init error: {e}");
            if let Some(app) = app_weak.upgrade() {
                // Batch G emits "invalid input path (...): <reason>" for
                // validation failures. Classify so the toast carries a
                // reason-specific title; fall back to generic "Init
                // failed" otherwise.
                let (title, body) = classify_init_error(&e);
                app.set_status_text(title.clone().into());
                app.set_files_loaded(false);
                s.toasts.push(Severity::Error, title, body);
                crate::toast::sync_to_ui(&s.toasts, &app);
            }
        }
    }
}

/// Inspect an init error string and decide what to show the user.
///
/// Batch G's `SourceError::InvalidPath` display format is
/// `"invalid input path (path): reason"`. We substring-match to pick
/// a friendlier title; the full stringified error becomes the body.
fn classify_init_error(err: &str) -> (String, String) {
    if err.contains("invalid input path") {
        let title = if err.contains("file not found") {
            "File not found"
        } else if err.contains("permission denied") {
            "Permission denied"
        } else if err.contains("file is empty") {
            "Empty file"
        } else if err.contains("not a regular file") {
            "Not a video file"
        } else {
            "Invalid file"
        };
        (title.to_string(), err.to_string())
    } else {
        ("Init failed".to_string(), err.to_string())
    }
}

/// Handle a calibration result from the background thread.
fn handle_calibration_result(
    result: CalibrationResult,
    state: &mut AppState,
    app_weak: &slint::Weak<RecoApp>,
) {
    if let Some(app) = app_weak.upgrade() {
        app.set_calibrating(false);
    }

    match result {
        Ok(output) => {
            let confidence = output.confidence;
            let total_matches = output.total_matches;
            if let Some(ref t) = state.telemetry {
                t.calibration_complete(confidence, total_matches);
            }
            let left_profile = output.left_lens_profile.clone();
            let right_profile = output.right_lens_profile.clone();
            // Either camera resolving to a Fallback profile means no real
            // camera match was found - the user must know, because it is the
            // most common reason calibration looks wrong or fails outright.
            let used_fallback = [left_profile.as_ref(), right_profile.as_ref()]
                .into_iter()
                .flatten()
                .any(|p| matches!(p.source, ProfileSource::Fallback));
            // Surface whether the cameras' own recordings actually had
            // gyro/accelerometer/quaternion data to sync and orient from -
            // set unconditionally (not folded into a warning toast the
            // way `used_fallback` is below) because audio-only sync with
            // no rotation seed is the *normal* case on a DJI rig, not an
            // exceptional one - see `ImuDiagnostics::summary`.
            if let (Some(app), Some(diag)) = (app_weak.upgrade(), output.imu_diagnostics.as_ref()) {
                app.set_imu_status_text(diag.summary().into());
            }
            match state.init_with_calibration(output.calibration) {
                Ok(true) => {
                    let fps = state.playback.fps();
                    let total = state.playback.total_frames().unwrap_or(0);
                    state.reset_view();
                    state.clamp_targets();
                    let clamped_fov = state.pose.current_fov_deg();
                    if let Some(bridge) = state.bridge.as_mut() {
                        bridge.engine_mut().set_fov(clamped_fov);
                    }
                    let img = state.render_current();
                    let (in_w, in_h) = state.playback.input_dimensions().unwrap_or((0, 0));

                    // Snapshot camera intrinsics as the Lens fine-tune
                    // baseline so Reset Lens can restore them after
                    // manual edits.
                    let lens_baseline = state.bridge.as_ref().map(|b| {
                        let cal = b.engine().calibration();
                        (cal.lenses[0].clone(), cal.lenses[1].clone())
                    });
                    if let Some((l, r)) = lens_baseline.as_ref() {
                        state.cal_baseline_left_params = Some(l.clone());
                        state.cal_baseline_right_params = Some(r.clone());
                    }

                    // Grab the layout baseline so the Calibration sliders
                    // (intersect, camera-axis offset, x_ty) show the
                    // auto-calibrated values instead of 0. Without this
                    // the preview looks correct while the sliders read
                    // 0; clicking any of them snaps the layout to ~0
                    // and destroys the calibration.
                    let layout_baseline = state.cal_baseline.clone();
                    // Same idea for rig tilt and blend width: read the
                    // calibrated values off the viewport so the View
                    // panel sliders match what the preview actually shows.
                    let rig_tilt_rad = state
                        .bridge
                        .as_ref()
                        .map(|b| b.engine().calibration().framing.tilt as f32);
                    // rig_roll was previously omitted here (only the manual
                    // load restored it), so an auto-calibrated roll left the
                    // slider at 0 while the preview was corrected - touching
                    // it then snapped roll back to 0 and broke the cal.
                    let rig_roll_rad = state
                        .bridge
                        .as_ref()
                        .map(|b| b.engine().calibration().framing.roll as f32);
                    let blend_width = state
                        .bridge
                        .as_ref()
                        .map(|b| b.engine().calibration().topology.blend_width);
                    let blend_flip_direction = state
                        .bridge
                        .as_ref()
                        .map(|b| b.engine().calibration().topology.blend_flip_direction);
                    let seam_offset = state
                        .bridge
                        .as_ref()
                        .map(|b| b.engine().calibration().topology.seam_offset);
                    let multiband_blend_enabled = state
                        .bridge
                        .as_ref()
                        .map(|b| b.engine().calibration().topology.multiband_blend_enabled);
                    let color_match_enabled = state
                        .bridge
                        .as_ref()
                        .map(|b| b.engine().calibration().topology.color_match_enabled);
                    let color_match_band_width = state
                        .bridge
                        .as_ref()
                        .map(|b| b.engine().calibration().topology.color_match_band_width);
                    let color_match_grid_cols = state
                        .bridge
                        .as_ref()
                        .map(|b| b.engine().calibration().topology.color_match_grid_cols);
                    let color_match_grid_rows = state
                        .bridge
                        .as_ref()
                        .map(|b| b.engine().calibration().topology.color_match_grid_rows);
                    let color_match_interval_frames = state.bridge.as_ref().map(|b| {
                        b.engine()
                            .calibration()
                            .topology
                            .color_match_interval_frames
                    });
                    let color_match_ema_alpha = state
                        .bridge
                        .as_ref()
                        .map(|b| b.engine().calibration().topology.color_match_ema_alpha);
                    let color_match_max_y_offset = state
                        .bridge
                        .as_ref()
                        .map(|b| b.engine().calibration().topology.color_match_max_y_offset);
                    let color_match_max_chroma_offset = state.bridge.as_ref().map(|b| {
                        b.engine()
                            .calibration()
                            .topology
                            .color_match_max_chroma_offset
                    });
                    let color_gamma = state.bridge.as_ref().map(|b| {
                        let t = &b.engine().calibration().topology;
                        (t.color_gamma_left, t.color_gamma_right)
                    });
                    let color_match_auto_gamma = state
                        .bridge
                        .as_ref()
                        .map(|b| b.engine().calibration().topology.color_match_auto_gamma);
                    let lens_correction =
                        state.calibration.as_ref().map(|c| c.lenses[0].correction);
                    if let Some(lc) = lens_correction {
                        state.lens_correction_amount = lc;
                    }

                    // Auto-save calibration so it appears in Recent and
                    // can be reloaded. Keep an already-known location
                    // (the match folder's calibration path set by
                    // "Select Match Folder", or wherever a calibration
                    // was already loaded from) rather than deriving a
                    // fresh one and clobbering it - this used to always
                    // save next to the left video regardless, which put
                    // a new calibration inside .../Left/ instead of the
                    // match folder that also holds Left/ and Right/.
                    let cal_path = state
                        .calibration_path
                        .clone()
                        .or_else(|| suggested_calibration_path(state.left_path.as_deref()));
                    if let Some(cal_path) = cal_path
                        && let Some(cal) = state.calibration.as_ref()
                    {
                        match serde_json::to_string_pretty(cal) {
                            Ok(json) => match std::fs::write(&cal_path, json) {
                                Ok(()) => {
                                    log::info!("Auto-saved calibration to {}", cal_path.display());
                                    state.calibration_path = Some(cal_path.clone());
                                    state.user_settings.push_calibration(cal_path.clone());
                                }
                                Err(e) => {
                                    log::warn!("Failed to auto-save calibration: {e}");
                                }
                            },
                            Err(e) => {
                                log::warn!("Failed to serialize calibration: {e}");
                            }
                        }
                    }

                    if let Some(app) = app_weak.upgrade() {
                        let cal_label = state
                            .calibration_path
                            .as_ref()
                            .map(|p| display_name(p))
                            .unwrap_or_else(|| "(auto-calibrated)".into());
                        app.set_files_loaded(true);
                        app.set_has_roi(
                            state
                                .calibration
                                .as_ref()
                                .and_then(|c| c.field_roi.as_ref())
                                .is_some_and(|r| !r.left.is_empty() || !r.right.is_empty()),
                        );
                        app.set_has_goal_geometry(
                            state
                                .calibration
                                .as_ref()
                                .and_then(|c| c.goal_geometry.as_ref())
                                .is_some_and(|g| !g.left.is_empty() || !g.right.is_empty()),
                        );
                        sync_goal_points(state, &app);
                        sync_pitch_limit_status(state, &app);
                        app.set_calibration_path(cal_label.into());
                        sync_recent_paths(&state.user_settings, &app);
                        sync_frame_display(&app, state.playback.frame_index(), total, fps);
                        app.set_fps(fps as f32);
                        app.set_playback_speed(state.playback.speed() as f32);
                        app.set_status_text(
                            format!("Auto-calibrated - {:.0} fps - {total} frames", fps,).into(),
                        );
                        if let Some(img) = img {
                            app.set_preview_frame(img);
                        }
                        if let Some(layout) = layout_baseline.as_ref() {
                            app.set_cal_intersect(layout.topology.intersect as f32);
                            app.set_cal_camera_axis_offset(layout.framing.axis_offset as f32);
                            app.set_cal_x_ty(layout.topology.x_ty as f32);
                            app.set_cal_x_rx(layout.topology.x_rx as f32);
                            app.set_cal_z_rz(layout.topology.z_rz as f32);
                            app.set_cal_x_rz(layout.topology.x_rz as f32);
                            app.set_cal_z_rx(layout.topology.z_rx as f32);
                            app.set_cal_ground_tilt_x(layout.topology.ground_tilt_x as f32);
                            app.set_cal_ground_tilt_z(layout.topology.ground_tilt_z as f32);
                            app.set_cal_top_tilt_x(layout.topology.top_tilt_x as f32);
                            app.set_cal_top_tilt_z(layout.topology.top_tilt_z as f32);
                            app.set_cal_ground_tilt_band_width(
                                layout.topology.ground_tilt_band_width as f32,
                            );
                            app.set_cal_top_tilt_band_width(
                                layout.topology.top_tilt_band_width as f32,
                            );
                            app.set_cal_dirty(false);
                        }
                        if let Some(rt) = rig_tilt_rad {
                            app.set_rig_tilt(rt.to_degrees());
                        }
                        if let Some(rr) = rig_roll_rad {
                            app.set_rig_roll(rr.to_degrees());
                        }
                        if let Some(bw) = blend_width {
                            app.set_blend_width(bw);
                        }
                        if let Some(flip) = blend_flip_direction {
                            app.set_blend_flip_direction(flip);
                        }
                        if let Some(so) = seam_offset {
                            app.set_seam_offset(so);
                        }
                        if let Some(mb) = multiband_blend_enabled {
                            app.set_multiband_blend_enabled(mb);
                        }
                        if let Some(v) = color_match_enabled {
                            app.set_color_match_enabled(v);
                        }
                        if let Some(v) = color_match_band_width {
                            app.set_color_match_band_width(v);
                        }
                        if let Some(v) = color_match_grid_cols {
                            app.set_color_match_grid_cols(v as f32);
                        }
                        if let Some(v) = color_match_grid_rows {
                            app.set_color_match_grid_rows(v as f32);
                        }
                        if let Some(v) = color_match_interval_frames {
                            app.set_color_match_interval_frames(v as f32);
                        }
                        if let Some(v) = color_match_ema_alpha {
                            app.set_color_match_ema_alpha(v);
                        }
                        if let Some(v) = color_match_max_y_offset {
                            app.set_color_match_max_y_offset(v);
                        }
                        if let Some(v) = color_match_max_chroma_offset {
                            app.set_color_match_max_chroma_offset(v);
                        }
                        if let Some((left, right)) = color_gamma {
                            app.set_color_gamma_left(left);
                            app.set_color_gamma_right(right);
                        }
                        if let Some(v) = color_match_auto_gamma {
                            app.set_color_match_auto_gamma(v);
                        }
                        if let Some(lc) = lens_correction {
                            app.set_lens_correction_amount(lc);
                        }
                        // Same angular-resolution readout as above.
                        if let Some(cal) = state.calibration.as_ref() {
                            app.set_lens_px_per_rad(cal.lenses[0].fx as f32);
                        }
                        if let Some(cal) = state.calibration.as_ref() {
                            app.set_sync_offset(cal.sync_offset as i32);
                        }
                        app.set_fov(clamped_fov);
                        set_lens_profile_props(&app, left_profile, right_profile, in_w, in_h);
                        if let Some((l, r)) = lens_baseline.as_ref() {
                            set_lens_sliders(&app, l, r);
                            app.set_lens_dirty(false);
                        }

                        if confidence < 0.5 {
                            log::warn!(
                                "Low calibration confidence ({:.0}%, {total_matches} matches). \
                                 Stitch quality may be poor.",
                                confidence * 100.0
                            );
                            state.toasts.push(
                                Severity::Warn,
                                "Low calibration confidence",
                                format!(
                                    "{:.0}% confidence ({total_matches} matches). \
                                     Try recording with more camera overlap.",
                                    confidence * 100.0
                                ),
                            );
                            crate::toast::sync_to_ui(&state.toasts, &app);
                        }
                        if used_fallback {
                            log::warn!(
                                "calibration used a GENERIC fallback lens profile (no \
                                 camera match at {in_w}x{in_h}); stitch quality may be \
                                 poor and the optimizer can fail to converge"
                            );
                            state.toasts.push(
                                Severity::Warn,
                                "Using a generic lens profile",
                                format!(
                                    "No lens profile matched your camera at {in_w}x{in_h}, \
                                     so a generic one was used. If the stitch looks wrong, \
                                     load a lens profile or adjust the Lens sliders."
                                ),
                            );
                            crate::toast::sync_to_ui(&state.toasts, &app);
                        }
                    }
                }
                Ok(false) => {}
                Err(e) => {
                    log::error!("Post-calibration init: {e}");
                    if let Some(app) = app_weak.upgrade() {
                        app.set_status_text("Post-calibration init failed".into());
                        state.toasts.push(Severity::Error, "Init failed", e.clone());
                        crate::toast::sync_to_ui(&state.toasts, &app);
                    }
                }
            }
        }
        Err(e) => {
            // Cancel (the progress popup's Cancel button) surfaces as a
            // plain error like any other failure - but unlike a real
            // one, the user's already-loaded session is still good and
            // shouldn't be torn down for a deliberate stop.
            if matches!(e, reco_calibrate::video::CalibrateVideosError::Cancelled) {
                log::info!("Auto-calibration cancelled");
                if let Some(app) = app_weak.upgrade() {
                    app.set_status_text("Calibration cancelled".into());
                }
                return;
            }
            log::error!("Auto-calibration failed: {e}");
            if let Some(ref t) = state.telemetry {
                t.calibration_error(&e.to_string());
            }
            // Critical: unload the live pipeline so the preview stops
            // rendering whatever it was showing before. Otherwise the
            // preview keeps playing the OLD right/left video while the
            // state thinks the new paths are active - and export would
            // read the new paths and produce garbage. Flipping
            // `files-loaded=false` forces the user to re-pick or
            // re-calibrate from a clean state.
            state.unload_pipeline();
            if let Some(app) = app_weak.upgrade() {
                app.set_files_loaded(false);
                app.set_status_text("Calibration failed".into());
                // A failed attempt has no IMU diagnostics of its own;
                // clear rather than leave a stale summary from whatever
                // the last successful run was.
                app.set_imu_status_text("".into());
                // Toast wants a display-ready message; stringify at
                // the UI boundary (not across the mpsc channel).
                state
                    .toasts
                    .push(Severity::Error, "Auto-calibration failed", e.to_string());
                crate::toast::sync_to_ui(&state.toasts, &app);
            }
        }
    }
}

#[cfg(test)]
mod roi_polygon_tests {
    use super::roi_insert_index;

    #[test]
    fn first_two_points_append() {
        let pts: Vec<[f64; 2]> = vec![];
        assert_eq!(roi_insert_index(&pts, [0.1, 0.1]), 0);
        let pts = vec![[0.1, 0.1]];
        assert_eq!(roi_insert_index(&pts, [0.9, 0.9]), 1);
    }

    #[test]
    fn inserts_along_the_nearest_edge_not_always_at_the_end() {
        // A square, corners in perimeter order.
        let pts = vec![[0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0]];
        // Midpoint of the top edge (0,0)-(1,0) belongs between indices 0 and 1.
        assert_eq!(roi_insert_index(&pts, [0.5, 0.0]), 1);
        // Midpoint of the closing edge (0,1)-(0,0) belongs after the last point.
        assert_eq!(roi_insert_index(&pts, [0.0, 0.5]), 4);
    }

    #[test]
    fn out_of_order_clicks_still_close_a_simple_polygon() {
        // Same square, but as a user would build it while deleting and
        // re-adding: two opposite corners first, then the remaining two.
        // Naive append-at-end would bowtie; cheapest-insertion should not.
        let mut pts = vec![[0.0, 0.0], [1.0, 1.0]];
        let idx = roi_insert_index(&pts, [1.0, 0.0]);
        pts.insert(idx, [1.0, 0.0]);
        let idx = roi_insert_index(&pts, [0.0, 1.0]);
        pts.insert(idx, [0.0, 1.0]);
        assert_eq!(pts, vec![[0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0]]);
    }
}

#[cfg(test)]
mod suggested_export_path_tests {
    use super::suggested_export_path;
    use std::path::{Path, PathBuf};

    #[test]
    fn prefers_match_folder_named_after_it() {
        let folder = Path::new("D:/Matches/TeamA - TeamB 2026-08-22");
        let left = Path::new("D:/Matches/TeamA - TeamB 2026-08-22/Left/DJI_0001.mp4");
        assert_eq!(
            suggested_export_path(Some(folder), Some(left)),
            Some(PathBuf::from(
                "D:/Matches/TeamA - TeamB 2026-08-22/TeamA - TeamB 2026-08-22.mp4"
            ))
        );
    }

    #[test]
    fn falls_back_to_left_video_without_a_match_folder() {
        // A source extension that isn't already exactly `.mp4` (`.MOV`,
        // as an iPhone/GoPro source might use) so the suggested `.mp4`
        // output genuinely differs from it - see the collision test
        // below for the (very common, e.g. any already-`.mp4`/`.MP4`
        // source) case where it doesn't.
        let left = Path::new("D:/Recordings/DJI_0001.MOV");
        assert_eq!(
            suggested_export_path(None, Some(left)),
            Some(PathBuf::from("D:/Recordings/DJI_0001.mp4"))
        );
    }

    #[test]
    fn falls_back_to_stitched_suffix_when_the_plain_name_would_overwrite_the_source() {
        // Windows filenames are case-insensitive - `DJI_0001.MP4` (the
        // real, uppercase-extension form DJI cameras actually use) and
        // a suggested `DJI_0001.mp4` output are the *same file* there,
        // so the plain name must not be suggested in this case.
        let left = Path::new("D:/Recordings/DJI_0001.MP4");
        assert_eq!(
            suggested_export_path(None, Some(left)),
            Some(PathBuf::from("D:/Recordings/DJI_0001_stitched.mp4"))
        );
    }

    #[test]
    fn none_when_nothing_loaded_yet() {
        assert_eq!(suggested_export_path(None, None), None);
    }
}

#[cfg(test)]
mod highlights_suffix_tests {
    use super::{add_highlights_suffix, strip_highlights_suffix};

    #[test]
    fn suffix_goes_before_the_extension() {
        assert_eq!(
            add_highlights_suffix(r"D:\Matches\TeamA - TeamB.mp4"),
            r"D:\Matches\TeamA - TeamB_highlights.mp4"
        );
        assert_eq!(
            add_highlights_suffix("/home/someone/match.mkv"),
            "/home/someone/match_highlights.mkv"
        );
    }

    /// Toggling the checkbox twice must not stack suffixes - the
    /// derived path is recomputed from whatever is in the field, which
    /// after one toggle already carries it.
    #[test]
    fn adding_the_suffix_twice_changes_nothing() {
        let once = add_highlights_suffix("match.mp4");
        assert_eq!(add_highlights_suffix(&once), once);
    }

    /// A dot in a directory name is not an extension - inserting there
    /// would produce a path in a folder that does not exist.
    #[test]
    fn a_dot_in_a_folder_name_is_not_an_extension() {
        assert_eq!(
            add_highlights_suffix(r"D:\v1.2\match"),
            r"D:\v1.2\match_highlights"
        );
        assert_eq!(
            add_highlights_suffix("/srv/2026.08/match"),
            "/srv/2026.08/match_highlights"
        );
    }

    #[test]
    fn stripping_reverses_adding_and_leaves_anything_else_alone() {
        let path = r"D:\Matches\TeamA - TeamB.mp4";
        assert_eq!(strip_highlights_suffix(&add_highlights_suffix(path)), path);
        // Never produced by add_highlights_suffix, so never touched.
        assert_eq!(strip_highlights_suffix(path), path);
        assert_eq!(
            strip_highlights_suffix("my_highlights_reel.mp4"),
            "my_highlights_reel.mp4"
        );
    }
}

#[cfg(test)]
mod suggested_calibration_path_tests {
    use super::suggested_calibration_path;
    use std::path::{Path, PathBuf};

    #[test]
    fn saves_one_level_up_from_a_left_folder() {
        // The fixed <match>/Left/, <match>/Right/ layout (`match_folder`
        // module doc) - this is the case a bare Auto-Calibrate (no
        // "Select Match Folder" pick, no calibration ever loaded) used
        // to get wrong, saving inside Left/ instead of the match folder.
        let left = Path::new("D:/Matches/TeamA - TeamB 2026-08-22/Left/DJI_0001.mp4");
        assert_eq!(
            suggested_calibration_path(Some(left)),
            Some(PathBuf::from(
                "D:/Matches/TeamA - TeamB 2026-08-22/DJI_0001_calibration.json"
            ))
        );
    }

    #[test]
    fn case_insensitive_left_folder_name() {
        let left = Path::new("D:/Matches/TeamA - TeamB/LEFT/DJI_0001.mp4");
        assert_eq!(
            suggested_calibration_path(Some(left)),
            Some(PathBuf::from(
                "D:/Matches/TeamA - TeamB/DJI_0001_calibration.json"
            ))
        );
    }

    #[test]
    fn falls_back_to_left_videos_own_folder_without_the_left_right_layout() {
        let left = Path::new("D:/Recordings/DJI_0001.mp4");
        assert_eq!(
            suggested_calibration_path(Some(left)),
            Some(PathBuf::from("D:/Recordings/DJI_0001_calibration.json"))
        );
    }

    #[test]
    fn none_when_nothing_loaded_yet() {
        assert_eq!(suggested_calibration_path(None), None);
    }
}
