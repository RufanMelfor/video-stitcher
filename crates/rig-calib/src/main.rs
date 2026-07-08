#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

//! RIG CALIB - standalone lens + rig calibration tool.
//!
//! Loads left/right camera footage, runs (or loads) a stereo
//! calibration, and exposes live sliders for rig geometry (tilt, roll,
//! sync offset, seam blend, plane intersect/axis-offset) and per-camera
//! lens distortion, with a GPU-rendered preview so changes are visible
//! immediately. This is a stripped-down sibling of `reco-gui` - no
//! export, telemetry, preferences, or ROI/autocam features, just
//! calibration.
//!
//! ## Architecture
//!
//! Slint and reco-core share a single wgpu 28 device, exactly as in
//! `reco-gui`: `main()` selects the wgpu 28 backend, a
//! `set_rendering_notifier` callback captures Slint's device/queue on
//! `RenderingSetup`, and those handles feed `GpuContext::from_device_queue`
//! so reco-core renders stitched frames directly into Slint-owned
//! textures with no CPU readback (see `preview::PreviewBridge`).

mod calibration;
mod playback;
mod preview;
mod settings;
mod status;
mod waveform;

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;
use std::time::{Duration, Instant};

use clap::Parser;
use reco_calibrate::{LensProfileInfo, ProfileSource};
use reco_control::pose_control::{PoseControl, PoseControlConfig};
use reco_control::{ControlIntent, IntentTranslator, PoseIntent};
use reco_core::calibration::{CameraParams, MatchCalibration, PlaneLayout};
use reco_core::detect::director::ViewportPosition;
use reco_core::wgpu;
use reco_io::stitch_job::InputPath;

use crate::calibration::{AutoCalibrateHandle, AutoCalibrateParams};
use crate::playback::{PlayState, Playback};
use crate::preview::PreviewBridge;
use crate::settings::RigCalibSettings;
use crate::status::{Severity, StatusLine};

slint::include_modules!();

/// wgpu handles captured from Slint's rendering notifier. Populated once
/// on `RenderingSetup`; used to build `PreviewBridge` when files load.
#[derive(Clone)]
struct SharedGpu {
    device: wgpu::Device,
    queue: wgpu::Queue,
    adapter_info: wgpu::AdapterInfo,
}

/// Default preview viewport dimensions (before the first adaptive
/// resize reads the actual preview container size from Slint).
const PREVIEW_WIDTH_DEFAULT: u32 = 1920;
const PREVIEW_HEIGHT_DEFAULT: u32 = 1080;

/// FOV clamp range (degrees), matching CLI preview / reco-gui.
const FOV_MIN: f32 = 20.0;
const FOV_MAX: f32 = 150.0;
const FOV_DEFAULT: f32 = 75.0;

/// Mouse drag sensitivity (deg/px). Matches reco-gui's PTZ-head feel.
const DRAG_DEG_PER_PIXEL: f32 = 0.287;

/// Exponential smoothing factor for pan/zoom easing. Matches reco-gui.
const POSE_SMOOTHING: f32 = 0.25;

/// Free-fly camera movement speed (scene units per second).
const FLY_SPEED: f32 = 0.6;
/// Free-fly vertical (E/C) movement speed - kept separate from FLY_SPEED
/// since up/down needs finer control than horizontal/forward movement.
const FLY_VERTICAL_SPEED: f32 = 0.2;
/// Free-fly speed multiplier while Shift is held.
const FLY_BOOST: f32 = 4.0;

/// How long the seek-slider fraction must stay stable before the seek
/// actually executes. Each seek reinits the decoder (~50ms); without
/// debouncing a drag would saturate it with hundreds of reinits.
const SEEK_DEBOUNCE_MS: u64 = 120;

/// Width of the audio-sync waveform window, in frames of video at the
/// source's own fps - converted to seconds of audio at recompute time
/// since fps is only known once a file is loaded. Frame-based (not a
/// fixed duration) because the whole point is spotting a frame-scale
/// `sync_offset` error as a shifted transient; a multi-second window
/// dilutes that shift into a barely-visible fraction of the display.
const AUDIO_WAVEFORM_WINDOW_FRAMES: f64 = 10.0;
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

/// Optionally preload left/right videos and a calibration file.
#[derive(Parser)]
#[command(name = "rig-calib", about = "Lens + rig calibration tool")]
struct Args {
    /// Left camera video.
    #[arg(long)]
    left: Option<PathBuf>,
    /// Right camera video.
    #[arg(long)]
    right: Option<PathBuf>,
    /// Calibration JSON to load (skips auto-calibrate).
    #[arg(long)]
    calibration: Option<PathBuf>,
}

/// Application state shared between Slint callbacks.
struct AppState {
    left_path: Option<PathBuf>,
    right_path: Option<PathBuf>,
    calibration_path: Option<PathBuf>,
    calibration: Option<MatchCalibration>,
    playback: Playback,
    bridge: Option<PreviewBridge>,
    shared_gpu: Option<SharedGpu>,
    pose: PoseControl,
    /// Pending debounced seek: (fraction, time the request was made).
    pending_seek: Option<(f32, Instant)>,
    /// Set by control changes that don't go through the camera-smoothing
    /// path but still need a re-render. Cleared once the tick renders.
    preview_dirty: bool,
    /// Original PlaneLayout - what auto-calibrate (or the loaded file)
    /// produced. Live calibration sliders edit relative to this so
    /// Reset restores it.
    cal_baseline_layout: Option<PlaneLayout>,
    /// Baseline camera intrinsics for Reset Lens.
    cal_baseline_left_params: Option<CameraParams>,
    cal_baseline_right_params: Option<CameraParams>,
    use_constrained_look: bool,
    lens_preview_active: bool,
    lens_preview_side: String,
    lens_correction_amount: f32,
    auto_calibrate: Option<AutoCalibrateHandle>,
    /// Free-fly mode (F key): WASD/E/C translate the virtual camera through
    /// the 3D scene; mouse-drag still looks around. Debug navigation aid for
    /// inspecting stitch geometry - not part of the normal calibration flow.
    fly_mode: bool,
    /// Held free-fly movement keys (w/a/s/d/e/c, lowercased).
    keys_down: std::collections::HashSet<char>,
    /// Shift state at the last movement-key event (speed boost).
    fly_shift: bool,
    /// Last free-fly integration tick, for frame-rate-independent movement.
    last_move_time: Instant,
    /// Persisted recent-files settings (last left/right video, last
    /// calibration file) so the app reopens with them pre-filled.
    settings: RigCalibSettings,
    /// Left/right audio-sync waveform envelopes currently displayed,
    /// downsampled around `audio_envelope_center_secs`. Empty until the
    /// first recompute (see [`AppState::maybe_recompute_audio_envelope`]).
    audio_envelope_left: Vec<f32>,
    audio_envelope_right: Vec<f32>,
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
}

fn pose_config() -> PoseControlConfig {
    PoseControlConfig {
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
    }
}

impl AppState {
    fn new() -> Self {
        Self {
            left_path: None,
            right_path: None,
            calibration_path: None,
            calibration: None,
            playback: Playback::new(),
            bridge: None,
            shared_gpu: None,
            pose: PoseControl::new(pose_config()),
            pending_seek: None,
            preview_dirty: false,
            cal_baseline_layout: None,
            cal_baseline_left_params: None,
            cal_baseline_right_params: None,
            use_constrained_look: true,
            lens_preview_active: false,
            lens_preview_side: "left".into(),
            lens_correction_amount: 1.0,
            auto_calibrate: None,
            fly_mode: false,
            keys_down: std::collections::HashSet::new(),
            fly_shift: false,
            last_move_time: Instant::now(),
            settings: RigCalibSettings::load(),
            audio_envelope_left: Vec::new(),
            audio_envelope_right: Vec::new(),
            audio_envelope_center_secs: f64::NEG_INFINITY,
            audio_envelope_rx: None,
            audio_envelope_triggered_at: None,
        }
    }

    fn reset_pipeline(&mut self) {
        self.bridge = None;
        self.playback = Playback::new();
        self.pose = PoseControl::new(pose_config());
        self.pending_seek = None;
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
        cal: &MatchCalibration,
        input_w: u32,
        input_h: u32,
    ) -> Result<PreviewBridge, String> {
        let gpu = self
            .shared_gpu
            .as_ref()
            .ok_or("GPU not ready yet (Slint rendering not initialized)")?
            .clone();
        self.cal_baseline_layout = Some(cal.layout.clone());
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

    /// Apply an edited PlaneLayout to the renderer.
    fn apply_layout(&mut self, layout: PlaneLayout) {
        if let Some(cal) = self.calibration.as_mut() {
            cal.layout = layout.clone();
        }
        if let Some(bridge) = self.bridge.as_mut() {
            bridge.renderer_mut().update_layout(layout);
            self.preview_dirty = true;
        }
        self.clamp_targets();
    }

    /// Write the current (edited) calibration back to disk. Falls back
    /// to `<left>_calibration.json` if no calibration file was ever
    /// loaded (e.g. straight after auto-calibrate).
    fn save_calibration(&mut self) -> Result<PathBuf, String> {
        let Some(cal) = self.calibration.clone() else {
            return Err("No calibration to save".into());
        };
        let path = match self.calibration_path.clone() {
            Some(p) => p,
            None => {
                let left = self
                    .left_path
                    .as_ref()
                    .ok_or("No left video to derive a calibration path from")?;
                left.with_file_name(format!(
                    "{}_calibration.json",
                    left.file_stem()
                        .map(|s| s.to_string_lossy().into_owned())
                        .unwrap_or_else(|| "video".into())
                ))
            }
        };
        let mut out = cal;
        out.lens_correction_amount = self.lens_correction_amount;
        if let Some(bridge) = self.bridge.as_ref() {
            let pipeline = bridge.renderer().pipeline();
            out.left = pipeline.calibration().left.clone();
            out.right = pipeline.calibration().right.clone();
            out.blend_width = pipeline.viewport().blend_width;
        }
        let json = serde_json::to_string_pretty(&out).map_err(|e| format!("serialize: {e}"))?;
        std::fs::write(&path, json).map_err(|e| format!("write {}: {e}", path.display()))?;
        self.calibration_path = Some(path.clone());
        self.settings.push_calibration(path.clone());
        log::info!("Saved calibration to {}", path.display());
        Ok(path)
    }

    /// Restore PlaneLayout to the values loaded at init (or after auto-cal).
    fn reset_calibration(&mut self) {
        if let Some(layout) = self.cal_baseline_layout.clone() {
            self.apply_layout(layout);
        }
    }

    /// Try to open playback + build the preview pipeline from
    /// `left_path`/`right_path`/`calibration_path`. Returns `Ok(false)`
    /// if not all three are set yet.
    fn try_init(&mut self) -> Result<bool, String> {
        let (left, right, cal_path) =
            match (&self.left_path, &self.right_path, &self.calibration_path) {
                (Some(l), Some(r), Some(c)) => (l.clone(), r.clone(), c.clone()),
                _ => return Ok(false),
            };

        let cal = MatchCalibration::from_file(&cal_path)
            .map_err(|e| format!("Calibration load error: {e}"))?;

        let sync_offset = cal.sync_offset;
        self.playback
            .open(
                &InputPath::Single(left),
                &InputPath::Single(right),
                sync_offset,
            )
            .map_err(|e| format!("Video open error: {e}"))?;

        let (input_w, input_h) = self
            .playback
            .input_dimensions()
            .ok_or("No input dimensions")?;

        let bridge = self.build_bridge(&cal, input_w, input_h)?;

        self.calibration = Some(cal);
        self.bridge = Some(bridge);
        Ok(true)
    }

    /// Initialize preview from a calibration result (no file needed).
    fn init_with_calibration(&mut self, cal: MatchCalibration) -> Result<bool, String> {
        let (left, right) = match (&self.left_path, &self.right_path) {
            (Some(l), Some(r)) => (l.clone(), r.clone()),
            _ => return Err("Both video inputs required".into()),
        };

        let sync_offset = cal.sync_offset;
        self.playback
            .open(
                &InputPath::Single(left),
                &InputPath::Single(right),
                sync_offset,
            )
            .map_err(|e| format!("Video open error: {e}"))?;

        let (input_w, input_h) = self
            .playback
            .input_dimensions()
            .ok_or("No input dimensions")?;

        let bridge = self.build_bridge(&cal, input_w, input_h)?;

        self.calibration = Some(cal);
        self.bridge = Some(bridge);
        Ok(true)
    }

    /// Tear down the live pipeline so the preview stops rendering the
    /// stale source after a calibration failure or a file swap.
    fn unload_pipeline(&mut self) {
        self.reset_pipeline();
        self.cal_baseline_layout = None;
        self.cal_baseline_left_params = None;
        self.cal_baseline_right_params = None;
    }

    /// Clear the loaded/derived calibration so a completely fresh
    /// auto-calibrate can be run. Left/right videos stay loaded.
    fn clear_calibration(&mut self) {
        self.calibration_path = None;
        self.calibration = None;
        self.unload_pipeline();
    }

    /// Render the current frame (or the flat lens-preview view).
    fn render_current(&mut self) -> Option<slint::Image> {
        let frame = self.playback.current_frame()?;
        let left = frame.left.as_planes();
        let right = frame.right.as_planes();

        if self.lens_preview_active {
            let bridge = self.bridge.as_mut()?;
            let cal = bridge.renderer().pipeline().calibration();
            let (planes, params) = if self.lens_preview_side == "right" {
                (&right, cal.right.clone())
            } else {
                (&left, cal.left.clone())
            };
            return match bridge.render_lens_preview(planes, &params, self.lens_correction_amount) {
                Ok(img) => Some(img),
                Err(e) => {
                    log::error!("Lens preview error: {e}");
                    None
                }
            };
        }

        let bridge = self.bridge.as_ref()?;
        let rig_tilt = bridge.renderer().pipeline().viewport().rig_tilt;
        let pose = self.pose.render_pose(rig_tilt);
        match bridge.render_frame(&left, &right, pose.yaw, pose.pitch) {
            Ok(img) => Some(img),
            Err(e) => {
                log::error!("Render error: {e}");
                None
            }
        }
    }

    fn apply_pan(&mut self, dx_px: f32, dy_px: f32) {
        self.pose.apply_drag(dx_px, dy_px);
        self.clamp_targets();
        self.preview_dirty = true;
    }

    fn apply_zoom(&mut self, delta_deg: f32) {
        IntentTranslator::new(&mut self.pose)
            .dispatch(ControlIntent::Pose(PoseIntent::DeltaFovDeg(delta_deg)));
        self.clamp_targets();
        self.preview_dirty = true;
    }

    /// Toggle free-fly camera mode (F key or the "Fly Camera" button).
    /// Clears any held movement keys. Returns the new state so the caller
    /// can mirror it onto the Slint `fly-mode` property (button label).
    fn toggle_fly(&mut self) -> bool {
        self.fly_mode = !self.fly_mode;
        self.keys_down.clear();
        self.last_move_time = Instant::now();
        log::info!(
            "Fly mode {} - WASD move, E/C up/down, Shift boost, drag to look, F to exit",
            if self.fly_mode { "ON" } else { "OFF" }
        );
        self.fly_mode
    }

    /// Track a held free-fly movement key (w/a/s/d/e/c); records Shift for boost.
    fn set_fly_key(&mut self, text: &str, pressed: bool, shift: bool) {
        self.fly_shift = shift;
        let Some(c) = text.chars().next().map(|c| c.to_ascii_lowercase()) else {
            return;
        };
        if !matches!(c, 'w' | 'a' | 's' | 'd' | 'e' | 'c') {
            return;
        }
        if pressed {
            self.keys_down.insert(c);
        } else {
            self.keys_down.remove(&c);
        }
    }

    /// True while fly mode is active with movement keys held.
    fn fly_active(&self) -> bool {
        self.fly_mode && !self.keys_down.is_empty()
    }

    /// Integrate held movement keys into the virtual camera position, frame-rate
    /// independent via dt. Movement follows where the camera looks (yaw/pitch).
    fn apply_fly(&mut self) {
        if !self.fly_active() {
            self.last_move_time = Instant::now();
            return;
        }
        let dt = self.last_move_time.elapsed().as_secs_f32().min(0.05);
        self.last_move_time = Instant::now();

        let mut mv = [0.0_f32; 3]; // [right, up, forward]
        if self.keys_down.contains(&'d') {
            mv[0] += 1.0;
        }
        if self.keys_down.contains(&'a') {
            mv[0] -= 1.0;
        }
        if self.keys_down.contains(&'e') {
            mv[1] += 1.0;
        }
        if self.keys_down.contains(&'c') {
            mv[1] -= 1.0;
        }
        if self.keys_down.contains(&'w') {
            mv[2] += 1.0;
        }
        if self.keys_down.contains(&'s') {
            mv[2] -= 1.0;
        }
        if mv == [0.0; 3] {
            return;
        }

        let Some(rig_tilt) = self
            .bridge
            .as_ref()
            .map(|b| b.renderer().pipeline().viewport().rig_tilt)
        else {
            return;
        };
        let render = self.pose.render_pose(rig_tilt);
        let boost = if self.fly_shift { FLY_BOOST } else { 1.0 };
        let step = FLY_SPEED * boost * dt;
        let vstep = FLY_VERTICAL_SPEED * boost * dt;
        if let Some(bridge) = self.bridge.as_mut() {
            bridge.renderer_mut().pipeline_mut().fly_camera(
                [mv[0] * step, mv[1] * vstep, mv[2] * step],
                render.yaw,
                render.pitch,
            );
            // The coverage boundary used for no-black-edge pan/tilt
            // clamping is otherwise only refreshed on calibration/layout
            // changes, so without this it stays frozen at the camera's
            // starting position and fly mode's movement barely changes
            // what you can look at.
            bridge.renderer_mut().refresh_coverage();
            self.preview_dirty = true;
        }
    }

    fn smooth_camera(&mut self) -> bool {
        let before = self.pose.current_pose();
        self.pose.tick();
        if self.use_constrained_look
            && let Some(bridge) = self.bridge.as_ref()
        {
            let renderer = bridge.renderer();
            let (vw, vh) = bridge.viewport_size();
            let aspect = vw as f32 / vh as f32;
            let rig_tilt = renderer.pipeline().viewport().rig_tilt;
            self.pose
                .clamp_via_coverage(renderer.coverage(), aspect, rig_tilt);
        }
        let after = self.pose.current_pose();

        let yaw_changed = (before.yaw - after.yaw).abs() > f32::EPSILON;
        let pitch_changed = (before.pitch - after.pitch).abs() > f32::EPSILON;
        let fov_changed = before.fov_degrees != after.fov_degrees;

        if fov_changed
            && let Some(fov) = after.fov_degrees
            && let Some(bridge) = self.bridge.as_mut()
        {
            bridge.renderer_mut().pipeline_mut().set_fov(fov);
        }

        yaw_changed || pitch_changed || fov_changed
    }

    fn set_blend_width(&mut self, w: f32) {
        if let Some(bridge) = self.bridge.as_mut() {
            bridge.renderer_mut().set_blend_width(w.clamp(0.0, 0.5));
            self.preview_dirty = true;
        }
    }

    fn set_rig_tilt(&mut self, deg: f32) {
        if let Some(cal) = self.calibration.as_mut() {
            cal.rig_tilt = (deg as f64).to_radians();
        }
        if let Some(bridge) = self.bridge.as_mut() {
            bridge.renderer_mut().set_rig_tilt(deg.to_radians());
            self.preview_dirty = true;
        }
        self.clamp_targets();
    }

    fn set_rig_roll(&mut self, deg: f32) {
        if let Some(cal) = self.calibration.as_mut() {
            cal.rig_roll = (deg as f64).to_radians();
        }
        if let Some(bridge) = self.bridge.as_mut() {
            bridge.renderer_mut().set_rig_roll(deg.to_radians());
            self.preview_dirty = true;
        }
        self.clamp_targets();
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
        if let (Some(left), Some(right)) = (&self.left_path, &self.right_path) {
            let left = InputPath::Single(left.clone());
            let right = InputPath::Single(right.clone());
            if let Err(e) = self.playback.open(&left, &right, offset) {
                log::error!("Failed to reopen playback with sync offset {offset}: {e}");
                return;
            }
            log::info!("Sync offset changed to {offset} frames");
            self.preview_dirty = true;
            // Force the audio-sync waveform to recompute against the new
            // offset instead of showing the stale pre-change envelope.
            self.audio_envelope_center_secs = f64::NEG_INFINITY;
        }
    }

    /// Poll for a completed audio-sync waveform envelope and, if the
    /// playhead has moved far enough since the last one, trigger a new
    /// background recompute. Called from the vsync render tick; cheap
    /// when idle (a few field reads), since the actual `ffmpeg`
    /// extraction only runs on a background thread when genuinely
    /// warranted.
    fn maybe_recompute_audio_envelope(&mut self, app_weak: &slint::Weak<CalibrateApp>) {
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

        let window_secs = AUDIO_WAVEFORM_WINDOW_FRAMES / fps;
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

        let (tx, rx) = std::sync::mpsc::channel();
        self.audio_envelope_rx = Some(rx);
        std::thread::spawn(move || {
            let left = crate::waveform::extract_window_envelope(
                &left_path,
                center_secs,
                window_secs,
                AUDIO_WAVEFORM_BUCKETS,
            )
            .unwrap_or_default();
            let right = crate::waveform::extract_window_envelope(
                &right_path,
                center_secs,
                window_secs,
                AUDIO_WAVEFORM_BUCKETS,
            )
            .unwrap_or_default();
            let _ = tx.send((left, right));
        });
    }

    fn reset_view(&mut self) {
        IntentTranslator::new(&mut self.pose).dispatch(ControlIntent::Pose(PoseIntent::Reset));
    }

    fn clamp_targets(&mut self) {
        if !self.use_constrained_look {
            return;
        }
        let Some(bridge) = self.bridge.as_ref() else {
            return;
        };
        let renderer = bridge.renderer();
        let (vw, vh) = bridge.viewport_size();
        let aspect = vw as f32 / vh as f32;
        let rig_tilt = renderer.pipeline().viewport().rig_tilt;
        self.pose
            .clamp_via_coverage(renderer.coverage(), aspect, rig_tilt);
    }

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

/// Seed the Slint lens-tune sliders and their display ranges from a
/// pair of baseline `CameraParams`. Ranges: fx/fy +/-15%, cx/cy +/-10%
/// of image dimension - wide enough for meaningful manual tuning,
/// tight enough that slider granularity stays useful.
fn set_lens_sliders(app: &CalibrateApp, left: &CameraParams, right: &CameraParams) {
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
    app.set_lens_left_k1(left.d[0] as f32);
    app.set_lens_left_k2(left.d[1] as f32);
    app.set_lens_left_k3(left.d[2] as f32);
    app.set_lens_left_k4(left.d[3] as f32);

    app.set_lens_right_fx(right.fx as f32);
    app.set_lens_right_fy(right.fy as f32);
    app.set_lens_right_cx(right.cx as f32);
    app.set_lens_right_cy(right.cy as f32);
    app.set_lens_right_k1(right.d[0] as f32);
    app.set_lens_right_k2(right.d[1] as f32);
    app.set_lens_right_k3(right.d[2] as f32);
    app.set_lens_right_k4(right.d[3] as f32);
}

fn profile_source_label(info: &LensProfileInfo) -> &'static str {
    match info.source {
        ProfileSource::AutoDetected => "Auto-detected",
        ProfileSource::Database => "Database match",
        ProfileSource::File(_) => "File",
        ProfileSource::Fallback => "Fallback",
    }
}

/// Populate the Slint lens-profile properties from calibration output.
fn set_lens_profile_props(
    app: &CalibrateApp,
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
        return "0:00.000".into();
    }
    let total_ms = (frame as f64 / fps * 1000.0) as u64;
    let m = total_ms / 60_000;
    let s = (total_ms / 1000) % 60;
    let ms = total_ms % 1000;
    format!("{m}:{s:02}.{ms:03}")
}

fn sync_frame_display(app: &CalibrateApp, frame: u64, total: u64, fps: f64) {
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

fn set_status(app: &CalibrateApp, status: StatusLine) {
    app.set_status_text(status.text.into());
    app.set_status_is_error(status.severity == Severity::Error);
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

/// Directory logs are written to: a `logs/` folder next to the executable
/// (stable regardless of the process's current working directory, unlike
/// relying on CWD).
fn log_dir() -> std::path::PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|p| p.to_path_buf()))
        .unwrap_or_else(|| std::path::PathBuf::from("."))
        .join("logs")
}

/// Sets up logging to both stdout and a per-launch log file (`logs/rig-
/// calib-<unix-timestamp>.log` next to the exe), so a full session -
/// including every Auto-Calibrate run - can be handed over for debugging
/// without needing to capture the terminal. Returns the file-writer guard,
/// which must be kept alive for the process lifetime (dropping it stops
/// the background flush thread and can lose buffered log lines).
fn init_tracing() -> Option<tracing_appender::non_blocking::WorkerGuard> {
    use tracing_subscriber::{EnvFilter, fmt, prelude::*};

    let _ = tracing_log::LogTracer::init();
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));

    let dir = log_dir();
    match std::fs::create_dir_all(&dir) {
        Ok(()) => {
            let unix_secs = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0);
            let filename = format!("rig-calib-{unix_secs}.log");
            let file_appender = tracing_appender::rolling::never(&dir, &filename);
            let (non_blocking, guard) = tracing_appender::non_blocking(file_appender);

            let _ = tracing_subscriber::registry()
                .with(filter)
                .with(fmt::layer().with_target(true).with_level(true))
                .with(
                    fmt::layer()
                        .with_target(true)
                        .with_level(true)
                        .with_ansi(false)
                        .with_writer(non_blocking),
                )
                .try_init();
            println!("Logging to {}", dir.join(&filename).display());
            Some(guard)
        }
        Err(e) => {
            eprintln!(
                "Could not create log directory {}: {e} (logging to stdout only)",
                dir.display()
            );
            let _ = tracing_subscriber::registry()
                .with(filter)
                .with(fmt::layer().with_target(true).with_level(true))
                .try_init();
            None
        }
    }
}

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
        tracing::error!(target: "panic", location = %location, payload = %payload, "panic");
        default_hook(info);
    }));
}

fn try_init_and_update(state: &Rc<RefCell<AppState>>, app_weak: &slint::Weak<CalibrateApp>) {
    let mut s = state.borrow_mut();
    match s.try_init() {
        Ok(true) => {
            let fps = s.playback.fps();
            let total = s.playback.total_frames().unwrap_or(0);

            s.clamp_targets();
            let clamped_fov = s.pose.current_fov_deg();
            if let Some(bridge) = s.bridge.as_mut() {
                bridge.renderer_mut().pipeline_mut().set_fov(clamped_fov);
            }
            let img = s.render_current();
            let layout = s.cal_baseline_layout.clone();
            let (in_w, in_h) = s.playback.input_dimensions().unwrap_or((0, 0));

            let lens_baseline = s.bridge.as_ref().map(|b| {
                let cal = b.renderer().pipeline().calibration();
                (cal.left.clone(), cal.right.clone())
            });
            if let Some((l, r)) = lens_baseline.as_ref() {
                s.cal_baseline_left_params = Some(l.clone());
                s.cal_baseline_right_params = Some(r.clone());
            }
            let rig_tilt_rad = s
                .bridge
                .as_ref()
                .map(|b| b.renderer().pipeline().viewport().rig_tilt);
            let rig_roll_rad = s
                .bridge
                .as_ref()
                .map(|b| b.renderer().pipeline().viewport().rig_roll);
            let blend_width = s
                .bridge
                .as_ref()
                .map(|b| b.renderer().pipeline().viewport().blend_width);
            let lens_correction = s.calibration.as_ref().map(|c| c.lens_correction_amount);
            if let Some(lc) = lens_correction {
                s.lens_correction_amount = lc;
            }

            if let Some(app) = app_weak.upgrade() {
                app.set_files_loaded(true);
                sync_frame_display(&app, s.playback.frame_index(), total, fps);
                app.set_fps(fps as f32);
                // Fresh `Playback` always resets to 1.0x - reflect that in the
                // UI too, so a custom speed set on a previous clip doesn't
                // linger as a stale display after loading a new one.
                app.set_playback_speed(s.playback.speed() as f32);
                set_status(
                    &app,
                    StatusLine::info(format!("Ready - {fps:.0} fps - {total} frames")),
                );
                if let Some(img) = img {
                    app.set_preview_frame(img);
                }
                if let Some(layout) = layout {
                    app.set_cal_intersect(layout.intersect as f32);
                    app.set_cal_camera_axis_offset(layout.camera_axis_offset as f32);
                    app.set_cal_x_ty(layout.x_ty as f32);
                    app.set_cal_ground_tilt_x(layout.ground_tilt_x as f32);
                    app.set_cal_ground_tilt_z(layout.ground_tilt_z as f32);
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
                if let Some(lc) = lens_correction {
                    app.set_lens_correction_amount(lc);
                }
                if let Some(cal) = s.calibration.as_ref() {
                    app.set_sync_offset(cal.sync_offset as i32);
                }
                app.set_fov(clamped_fov);
                set_lens_profile_props(&app, None, None, in_w, in_h);
                if let Some((l, r)) = lens_baseline.as_ref() {
                    set_lens_sliders(&app, l, r);
                    app.set_lens_dirty(false);
                }
            }
        }
        Ok(false) => {
            let missing: Vec<&str> = [
                s.left_path.is_none().then_some("left video"),
                s.right_path.is_none().then_some("right video"),
                s.calibration_path.is_none().then_some("calibration"),
            ]
            .into_iter()
            .flatten()
            .collect();
            if let Some(app) = app_weak.upgrade() {
                set_status(
                    &app,
                    StatusLine::info(format!("Still need: {}", missing.join(", "))),
                );
            }
        }
        Err(e) => {
            log::error!("Init error: {e}");
            if let Some(app) = app_weak.upgrade() {
                app.set_files_loaded(false);
                set_status(&app, StatusLine::error(e));
            }
        }
    }
}

fn handle_calibration_result(
    result: Result<reco_calibrate::CalibrationResult, reco_calibrate::video::CalibrateVideosError>,
    state: &mut AppState,
    app_weak: &slint::Weak<CalibrateApp>,
) {
    if let Some(app) = app_weak.upgrade() {
        app.set_calibrating(false);
        app.set_calibration_progress(-1.0);
    }

    match result {
        Ok(output) => {
            let confidence = output.confidence;
            let total_matches = output.total_matches;
            let left_profile = output.left_lens_profile.clone();
            let right_profile = output.right_lens_profile.clone();
            let used_fallback = [left_profile.as_ref(), right_profile.as_ref()]
                .into_iter()
                .flatten()
                .any(|p| matches!(p.source, ProfileSource::Fallback));

            match state.init_with_calibration(output.calibration) {
                Ok(true) => {
                    let fps = state.playback.fps();
                    let total = state.playback.total_frames().unwrap_or(0);
                    state.reset_view();
                    state.clamp_targets();
                    let clamped_fov = state.pose.current_fov_deg();
                    if let Some(bridge) = state.bridge.as_mut() {
                        bridge.renderer_mut().pipeline_mut().set_fov(clamped_fov);
                    }
                    let img = state.render_current();
                    let (in_w, in_h) = state.playback.input_dimensions().unwrap_or((0, 0));

                    let lens_baseline = state.bridge.as_ref().map(|b| {
                        let cal = b.renderer().pipeline().calibration();
                        (cal.left.clone(), cal.right.clone())
                    });
                    if let Some((l, r)) = lens_baseline.as_ref() {
                        state.cal_baseline_left_params = Some(l.clone());
                        state.cal_baseline_right_params = Some(r.clone());
                    }
                    let layout_baseline = state.cal_baseline_layout.clone();
                    let rig_tilt_rad = state
                        .bridge
                        .as_ref()
                        .map(|b| b.renderer().pipeline().viewport().rig_tilt);
                    let rig_roll_rad = state
                        .bridge
                        .as_ref()
                        .map(|b| b.renderer().pipeline().viewport().rig_roll);
                    let blend_width = state
                        .bridge
                        .as_ref()
                        .map(|b| b.renderer().pipeline().viewport().blend_width);
                    let lens_correction =
                        state.calibration.as_ref().map(|c| c.lens_correction_amount);
                    if let Some(lc) = lens_correction {
                        state.lens_correction_amount = lc;
                    }

                    // Auto-save next to the left video so Save Calibration
                    // has somewhere to write immediately.
                    if let Err(e) = state.save_calibration() {
                        log::warn!("Auto-save after calibration failed: {e}");
                    }

                    if let Some(app) = app_weak.upgrade() {
                        let cal_label = state
                            .calibration_path
                            .as_ref()
                            .map(|p| display_name(p))
                            .unwrap_or_else(|| "(auto-calibrated)".into());
                        app.set_files_loaded(true);
                        app.set_calibration_path(cal_label.into());
                        sync_frame_display(&app, state.playback.frame_index(), total, fps);
                        app.set_fps(fps as f32);
                        set_status(
                            &app,
                            StatusLine::info(format!(
                                "Auto-calibrated - {fps:.0} fps - {total} frames"
                            )),
                        );
                        if let Some(img) = img {
                            app.set_preview_frame(img);
                        }
                        if let Some(layout) = layout_baseline.as_ref() {
                            app.set_cal_intersect(layout.intersect as f32);
                            app.set_cal_camera_axis_offset(layout.camera_axis_offset as f32);
                            app.set_cal_x_ty(layout.x_ty as f32);
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
                        if let Some(lc) = lens_correction {
                            app.set_lens_correction_amount(lc);
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
                                "Low calibration confidence ({:.0}%, {total_matches} matches)",
                                confidence * 100.0
                            );
                            set_status(
                                &app,
                                StatusLine::warn(format!(
                                    "Low confidence ({:.0}%, {total_matches} matches) - try more camera overlap",
                                    confidence * 100.0
                                )),
                            );
                        } else if used_fallback {
                            set_status(
                                &app,
                                StatusLine::warn(format!(
                                    "No lens profile matched at {in_w}x{in_h}; using a generic one - load a profile or adjust Lens sliders"
                                )),
                            );
                        }
                    }
                }
                Ok(false) => {}
                Err(e) => {
                    log::error!("Post-calibration init: {e}");
                    if let Some(app) = app_weak.upgrade() {
                        set_status(
                            &app,
                            StatusLine::error(format!("Post-calibration init failed: {e}")),
                        );
                    }
                }
            }
        }
        Err(e) => {
            log::error!("Auto-calibration failed: {e}");
            state.unload_pipeline();
            if let Some(app) = app_weak.upgrade() {
                app.set_files_loaded(false);
                set_status(&app, StatusLine::error(format!("Calibration failed: {e}")));
            }
        }
    }
}

/// Vsync-aligned playback + camera-smoothing tick, driven from Slint's
/// `BeforeRendering` notifier so frame submissions land at a
/// deterministic phase relative to the compositor's cycle.
fn vsync_render_tick(state: &Rc<RefCell<AppState>>, app_weak: &slint::Weak<CalibrateApp>) {
    let mut s = state.borrow_mut();

    if let Some(app) = app_weak.upgrade()
        && let Some(bridge) = s.bridge.as_mut()
    {
        let area_w = (app.get_preview_area_width().max(320.0) as u32).min(1920);
        let area_h = (app.get_preview_area_height().max(240.0) as u32).min(1080);
        let (cur_w, cur_h) = bridge.viewport_size();
        if area_w.abs_diff(cur_w) > 16 || area_h.abs_diff(cur_h) > 16 {
            bridge.resize(area_w, area_h);
            s.preview_dirty = true;
        }
    }

    s.apply_fly();
    let camera_changed = s.smooth_camera();
    let video_advanced = match s.playback.tick() {
        Ok(advanced) => advanced,
        Err(e) => {
            log::error!("Playback tick error: {e}");
            if let Some(app) = app_weak.upgrade() {
                set_status(&app, StatusLine::error(format!("{e}")));
            }
            false
        }
    };

    let was_dirty = s.preview_dirty;
    if camera_changed || video_advanced || was_dirty {
        s.preview_dirty = camera_changed;
        let img = s.render_current();
        if let (Some(app), Some(img)) = (app_weak.upgrade(), img) {
            app.set_preview_frame(img);
            if video_advanced {
                let fps = s.playback.fps();
                let total = s.playback.total_frames().unwrap_or(0);
                sync_frame_display(&app, s.playback.frame_index(), total, fps);
                if s.playback.state() == PlayState::Finished {
                    app.set_playing(false);
                    set_status(&app, StatusLine::info("Playback finished"));
                }
            }
            let current = s.pose.current_pose();
            app.set_yaw(current.yaw);
            app.set_pitch(current.pitch);
            if let Some(fov) = current.fov_degrees {
                app.set_fov(fov);
            }
        }
    }
}

fn main() -> anyhow::Result<()> {
    let _log_guard = init_tracing();
    install_panic_hook();
    reco_io::init();
    let args = Args::parse();

    slint::BackendSelector::new()
        .require_wgpu_28({
            let mut config = slint::wgpu_28::WGPUConfiguration::default();
            if let slint::wgpu_28::WGPUConfiguration::Automatic(ref mut settings) = config {
                settings.device_required_limits = reco_core::wgpu::Limits::downlevel_defaults();
            }
            config
        })
        .select()?;

    let app = CalibrateApp::new()?;
    let state = Rc::new(RefCell::new(AppState::new()));

    let version = format!(
        "v{}{}",
        env!("CARGO_PKG_VERSION"),
        option_env!("GIT_HASH")
            .filter(|h| !h.is_empty())
            .map(|h| format!(" ({h})"))
            .unwrap_or_default()
    );
    app.set_version(version.into());

    // Preload CLI-provided paths, falling back to the last-used ones
    // persisted from a previous session (if that file/video still exists).
    // Explicit CLI paths are pushed to settings so they become the new
    // "last used" too; paths recovered from settings are already there.
    {
        let mut s = state.borrow_mut();
        if let Some(p) = args.left.as_ref() {
            s.settings.push_left(p.clone());
        }
        if let Some(p) = args.right.as_ref() {
            s.settings.push_right(p.clone());
        }
        if let Some(p) = args.calibration.as_ref() {
            s.settings.push_calibration(p.clone());
        }
        let left = args.left.or_else(|| s.settings.last_left());
        let right = args.right.or_else(|| s.settings.last_right());
        let cal = args.calibration.or_else(|| s.settings.last_calibration());
        if let Some(left) = left {
            app.set_left_path(display_name(&left).into());
            s.left_path = Some(left);
        }
        if let Some(right) = right {
            app.set_right_path(display_name(&right).into());
            s.right_path = Some(right);
        }
        if let Some(cal) = cal {
            app.set_calibration_path(display_name(&cal).into());
            s.calibration_path = Some(cal);
        }
    }

    // Capture Slint's wgpu device and queue on RenderingSetup, mirroring
    // reco-gui: reco-core's stitch output lands directly in Slint-owned
    // textures with zero copies.
    let state_for_notifier = Rc::clone(&state);
    let app_weak_notifier = app.as_weak();
    app.window().set_rendering_notifier(
        move |rendering_state, graphics_api| match rendering_state {
            slint::RenderingState::RenderingSetup => {
                let slint::GraphicsAPI::WGPU28 {
                    instance: _,
                    device,
                    queue,
                    ..
                } = graphics_api
                else {
                    log::warn!("Expected WGPU28 GraphicsAPI in rendering notifier");
                    return;
                };
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
                if let Some(app) = app_weak_notifier.upgrade() {
                    try_init_and_update(&state_for_notifier, &app.as_weak());
                }
            }
            slint::RenderingState::BeforeRendering => {
                vsync_render_tick(&state_for_notifier, &app_weak_notifier);
            }
            _ => {}
        },
    )?;

    // ── File picker callbacks ──

    let app_weak = app.as_weak();
    let state_ref = Rc::clone(&state);
    app.on_pick_left_video(move || {
        let dialog = rfd::FileDialog::new()
            .set_title("Select left camera video")
            .add_filter(
                "Video",
                &["mp4", "MP4", "mov", "MOV", "avi", "AVI", "mkv", "MKV"],
            );
        let Some(path) = dialog.pick_file() else {
            return;
        };
        let mut s = state_ref.borrow_mut();
        let changed = s.left_path.as_ref() != Some(&path);
        if changed && s.bridge.is_some() {
            s.unload_pipeline();
            if let Some(app) = app_weak.upgrade() {
                app.set_files_loaded(false);
            }
        }
        if let Some(app) = app_weak.upgrade() {
            app.set_left_path(display_name(&path).into());
        }
        s.left_path = Some(path.clone());
        s.settings.push_left(path);
        drop(s);
        try_init_and_update(&state_ref, &app_weak);
    });

    let app_weak = app.as_weak();
    let state_ref = Rc::clone(&state);
    app.on_pick_right_video(move || {
        let dialog = rfd::FileDialog::new()
            .set_title("Select right camera video")
            .add_filter(
                "Video",
                &["mp4", "MP4", "mov", "MOV", "avi", "AVI", "mkv", "MKV"],
            );
        let Some(path) = dialog.pick_file() else {
            return;
        };
        let mut s = state_ref.borrow_mut();
        let changed = s.right_path.as_ref() != Some(&path);
        if changed && s.bridge.is_some() {
            s.unload_pipeline();
            if let Some(app) = app_weak.upgrade() {
                app.set_files_loaded(false);
            }
        }
        if let Some(app) = app_weak.upgrade() {
            app.set_right_path(display_name(&path).into());
        }
        s.right_path = Some(path.clone());
        s.settings.push_right(path);
        drop(s);
        try_init_and_update(&state_ref, &app_weak);
    });

    let app_weak = app.as_weak();
    let state_ref = Rc::clone(&state);
    app.on_pick_calibration(move || {
        let dialog = rfd::FileDialog::new()
            .set_title("Select calibration JSON")
            .add_filter("JSON", &["json", "JSON"]);
        let Some(path) = dialog.pick_file() else {
            return;
        };
        let mut s = state_ref.borrow_mut();
        let changed = s.calibration_path.as_ref() != Some(&path);
        if changed && s.bridge.is_some() {
            s.unload_pipeline();
            if let Some(app) = app_weak.upgrade() {
                app.set_files_loaded(false);
            }
        }
        if let Some(app) = app_weak.upgrade() {
            app.set_calibration_path(display_name(&path).into());
        }
        s.calibration_path = Some(path.clone());
        s.settings.push_calibration(path);
        drop(s);
        try_init_and_update(&state_ref, &app_weak);
    });

    let app_weak = app.as_weak();
    let state_ref = Rc::clone(&state);
    app.on_clear_calibration(move || {
        let mut s = state_ref.borrow_mut();
        s.clear_calibration();
        if let Some(app) = app_weak.upgrade() {
            app.set_calibration_path("".into());
            app.set_files_loaded(false);
            app.set_cal_dirty(false);
            app.set_lens_dirty(false);
            set_status(
                &app,
                StatusLine::info("Calibration cleared - run Auto-Calibrate or load a file"),
            );
        }
    });

    // ── Auto-calibrate ──

    let app_weak = app.as_weak();
    let state_ref = Rc::clone(&state);
    app.on_auto_calibrate(move || {
        let s = state_ref.borrow();
        let (left, right) = match (&s.left_path, &s.right_path) {
            (Some(l), Some(r)) => (l.clone(), r.clone()),
            _ => return,
        };
        let current_time_secs = if s.playback.fps() > 0.0 {
            s.playback.frame_index() as f64 / s.playback.fps()
        } else {
            0.0
        };
        let (existing_left_params, existing_right_params) = match s.calibration.as_ref() {
            Some(cal) => (Some(cal.left.clone()), Some(cal.right.clone())),
            None => (
                s.cal_baseline_left_params.clone(),
                s.cal_baseline_right_params.clone(),
            ),
        };
        drop(s);

        let Some(app) = app_weak.upgrade() else {
            return;
        };
        let params = AutoCalibrateParams {
            left,
            right,
            start_secs: current_time_secs,
            num_frames: (app.get_calibration_frames().max(2)) as usize,
            akaze_threshold: app.get_cal_akaze_threshold() as f64,
            detect_y_min: app.get_cal_detect_y_min() as f64,
            detect_y_max: app.get_cal_detect_y_max() as f64,
            skip_end_secs: app.get_cal_skip_end_secs() as f64,
            use_imu_seeds: app.get_use_imu_seeds(),
            force_x_rx: app.get_force_x_rx(),
            force_z_rz: app.get_force_z_rz(),
            detect_max_width: if app.get_cal_full_res_features() {
                0
            } else {
                1920
            },
            existing_left_params,
            existing_right_params,
        };

        app.set_calibrating(true);
        app.set_calibration_step("Starting...".into());
        app.set_calibration_progress(-1.0);
        set_status(&app, StatusLine::info("Auto-calibrating..."));

        let app_weak_progress = app_weak.clone();
        let handle =
            calibration::spawn_auto_calibrate(params, move |step, detail, fraction, preview| {
                let weak = app_weak_progress.clone();
                slint::invoke_from_event_loop(move || {
                    if let Some(app) = weak.upgrade() {
                        app.set_calibration_step(step.into());
                        app.set_calibration_progress(fraction.unwrap_or(-1.0));
                        set_status(&app, StatusLine::info(format!("Calibrating: {detail}")));
                        if let Some(preview) = preview {
                            app.set_preview_frame(detection_preview_to_slint_image(&preview));
                        }
                    }
                })
                .ok();
            });
        state_ref.borrow_mut().auto_calibrate = Some(handle);
    });

    let app_weak = app.as_weak();
    let state_ref = Rc::clone(&state);
    app.on_cancel_calibrate(move || {
        let s = state_ref.borrow();
        if let Some(handle) = s.auto_calibrate.as_ref() {
            handle
                .interrupted
                .store(true, std::sync::atomic::Ordering::Relaxed);
        }
        drop(s);
        if let Some(app) = app_weak.upgrade() {
            set_status(&app, StatusLine::info("Cancelling..."));
        }
    });

    // ── Playback callbacks ──

    let app_weak = app.as_weak();
    let state_ref = Rc::clone(&state);
    app.on_toggle_playback(move || {
        let mut s = state_ref.borrow_mut();
        let new_state = s.playback.toggle();
        if let Some(app) = app_weak.upgrade() {
            app.set_playing(new_state == PlayState::Playing);
        }
    });

    let app_weak = app.as_weak();
    let state_ref = Rc::clone(&state);
    app.on_step_forward(move || {
        let mut s = state_ref.borrow_mut();
        if s.playback.state() == PlayState::Playing {
            s.playback.toggle();
        }
        match s.playback.step_forward() {
            Ok(true) => {
                let img = s.render_current();
                if let (Some(app), Some(img)) = (app_weak.upgrade(), img) {
                    app.set_preview_frame(img);
                    app.set_current_frame(s.playback.frame_index() as i32);
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
        if s.playback.state() == PlayState::Playing {
            s.playback.toggle();
        }
        let target = s.playback.frame_index().saturating_sub(2);
        let total = s.playback.total_frames().unwrap_or(1).max(1);
        let fraction = target as f32 / total as f32;
        match s.playback.seek(fraction) {
            Ok(()) => {
                let img = s.render_current();
                if let (Some(app), Some(img)) = (app_weak.upgrade(), img) {
                    app.set_preview_frame(img);
                    app.set_current_frame(s.playback.frame_index() as i32);
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
        let total = match s.playback.total_frames() {
            Some(t) if t > 0 => t,
            _ => return,
        };
        let target = ((fraction as f64) * total as f64) as u64;
        if target.abs_diff(s.playback.frame_index()) < 2 {
            return;
        }
        s.pending_seek = Some((fraction, Instant::now()));
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
        if let (Some(app), Some(img)) = (app_weak.upgrade(), img) {
            app.set_preview_frame(img);
            app.set_current_frame(s.playback.frame_index() as i32);
        }
    });

    let state_ref = Rc::clone(&state);
    app.on_changed_playback_speed(move |speed| {
        state_ref.borrow_mut().playback.set_speed(speed as f64);
    });

    // ── Camera / view control callbacks ──

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
    app.on_fly_toggle(move || {
        let new_state = state_ref.borrow_mut().toggle_fly();
        if let Some(app) = app_weak.upgrade() {
            app.set_fly_mode(new_state);
        }
    });

    let state_ref = Rc::clone(&state);
    app.on_fly_key(move |text, pressed, shift| {
        state_ref
            .borrow_mut()
            .set_fly_key(text.as_str(), pressed, shift);
    });

    // ── Rig calibration callbacks ──

    let state_ref = Rc::clone(&state);
    let app_weak = app.as_weak();
    app.on_changed_blend_width(move |w| {
        state_ref.borrow_mut().set_blend_width(w);
        if let Some(app) = app_weak.upgrade() {
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
    app.on_changed_cal_intersect(move |v| {
        let mut s = state_ref.borrow_mut();
        let Some(mut layout) = s.calibration.as_ref().map(|c| c.layout.clone()) else {
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
        let Some(mut layout) = s.calibration.as_ref().map(|c| c.layout.clone()) else {
            return;
        };
        layout.camera_axis_offset = v as f64;
        s.apply_layout(layout);
        if let Some(app) = app_weak.upgrade() {
            app.set_cal_dirty(true);
        }
    });

    let app_weak = app.as_weak();
    let state_ref = Rc::clone(&state);
    app.on_changed_cal_x_ty(move |v| {
        let mut s = state_ref.borrow_mut();
        let Some(mut layout) = s.calibration.as_ref().map(|c| c.layout.clone()) else {
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
    app.on_changed_cal_ground_tilt_x(move |v| {
        let mut s = state_ref.borrow_mut();
        let Some(mut layout) = s.calibration.as_ref().map(|c| c.layout.clone()) else {
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
        let Some(mut layout) = s.calibration.as_ref().map(|c| c.layout.clone()) else {
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
    app.on_save_calibration(move || {
        let mut s = state_ref.borrow_mut();
        match s.save_calibration() {
            Err(e) => {
                log::error!("Save calibration: {e}");
                if let Some(app) = app_weak.upgrade() {
                    set_status(&app, StatusLine::error(format!("Save failed: {e}")));
                }
            }
            Ok(path) => {
                if let Some(app) = app_weak.upgrade() {
                    app.set_calibration_path(display_name(&path).into());
                    set_status(&app, StatusLine::info("Calibration saved"));
                    app.set_cal_dirty(false);
                    app.set_lens_dirty(false);
                }
            }
        }
    });

    let app_weak = app.as_weak();
    let state_ref = Rc::clone(&state);
    app.on_reset_calibration(move || {
        let mut s = state_ref.borrow_mut();
        s.reset_calibration();
        if let (Some(app), Some(layout)) = (app_weak.upgrade(), s.cal_baseline_layout.as_ref()) {
            app.set_cal_intersect(layout.intersect as f32);
            app.set_cal_camera_axis_offset(layout.camera_axis_offset as f32);
            app.set_cal_x_ty(layout.x_ty as f32);
            app.set_cal_dirty(false);
        }
    });

    // ── Lens tuning callbacks ──

    let app_weak = app.as_weak();
    let state_ref = Rc::clone(&state);
    app.on_changed_lens_param(move || {
        let Some(app) = app_weak.upgrade() else {
            return;
        };
        let mut s = state_ref.borrow_mut();
        let selected = app.get_lens_selected_camera();
        let (left_wh, right_wh) = s
            .bridge
            .as_ref()
            .map(|b| {
                let c = b.renderer().pipeline().calibration();
                (
                    (c.left.width, c.left.height),
                    (c.right.width, c.right.height),
                )
            })
            .unwrap_or(((0, 0), (0, 0)));

        let (left_params, right_params) = match selected.as_str() {
            "right" => {
                let p = CameraParams {
                    fx: app.get_lens_right_fx() as f64,
                    fy: app.get_lens_right_fy() as f64,
                    cx: app.get_lens_right_cx() as f64,
                    cy: app.get_lens_right_cy() as f64,
                    d: [
                        app.get_lens_right_k1() as f64,
                        app.get_lens_right_k2() as f64,
                        app.get_lens_right_k3() as f64,
                        app.get_lens_right_k4() as f64,
                    ],
                    width: right_wh.0,
                    height: right_wh.1,
                };
                (None, Some(p))
            }
            "both" => {
                app.set_lens_right_fx(app.get_lens_left_fx());
                app.set_lens_right_fy(app.get_lens_left_fy());
                app.set_lens_right_cx(app.get_lens_left_cx());
                app.set_lens_right_cy(app.get_lens_left_cy());
                app.set_lens_right_k1(app.get_lens_left_k1());
                app.set_lens_right_k2(app.get_lens_left_k2());
                app.set_lens_right_k3(app.get_lens_left_k3());
                app.set_lens_right_k4(app.get_lens_left_k4());
                let left = CameraParams {
                    fx: app.get_lens_left_fx() as f64,
                    fy: app.get_lens_left_fy() as f64,
                    cx: app.get_lens_left_cx() as f64,
                    cy: app.get_lens_left_cy() as f64,
                    d: [
                        app.get_lens_left_k1() as f64,
                        app.get_lens_left_k2() as f64,
                        app.get_lens_left_k3() as f64,
                        app.get_lens_left_k4() as f64,
                    ],
                    width: left_wh.0,
                    height: left_wh.1,
                };
                let right = CameraParams {
                    width: right_wh.0,
                    height: right_wh.1,
                    ..left.clone()
                };
                (Some(left), Some(right))
            }
            _ => {
                let p = CameraParams {
                    fx: app.get_lens_left_fx() as f64,
                    fy: app.get_lens_left_fy() as f64,
                    cx: app.get_lens_left_cx() as f64,
                    cy: app.get_lens_left_cy() as f64,
                    d: [
                        app.get_lens_left_k1() as f64,
                        app.get_lens_left_k2() as f64,
                        app.get_lens_left_k3() as f64,
                        app.get_lens_left_k4() as f64,
                    ],
                    width: left_wh.0,
                    height: left_wh.1,
                };
                (Some(p), None)
            }
        };

        if let Some(cal) = s.calibration.as_mut() {
            if let Some(l) = &left_params {
                cal.left = l.clone();
            }
            if let Some(r) = &right_params {
                cal.right = r.clone();
            }
        }
        if let Some(bridge) = s.bridge.as_mut() {
            bridge
                .renderer_mut()
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
        let (left_base, right_base) = (
            s.cal_baseline_left_params.clone(),
            s.cal_baseline_right_params.clone(),
        );
        if let (Some(left), Some(right)) = (left_base.as_ref(), right_base.as_ref()) {
            set_lens_sliders(&app, left, right);
            if let Some(bridge) = s.bridge.as_mut() {
                bridge
                    .renderer_mut()
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
        if idx >= results.len() {
            return;
        }
        let summary = &results[idx];
        let Some(params) = db.load_by_summary(summary) else {
            return;
        };
        let side_str = side.as_str();
        let scale_w = in_w as f64 / params.width as f64;
        let scale_h = in_h as f64 / params.height as f64;
        let scaled = CameraParams {
            width: in_w,
            height: in_h,
            fx: params.fx * scale_w,
            fy: params.fy * scale_h,
            cx: params.cx * scale_w,
            cy: params.cy * scale_h,
            d: params.d,
        };
        let (apply_left, apply_right) = match side_str {
            "left" => (Some(scaled.clone()), None),
            "right" => (None, Some(scaled.clone())),
            _ => (Some(scaled.clone()), Some(scaled.clone())),
        };
        if let Some(cal) = s.calibration.as_mut() {
            if side_str != "right" {
                cal.left = scaled.clone();
            }
            if side_str != "left" {
                cal.right = scaled.clone();
            }
        }
        if let Some(bridge) = s.bridge.as_mut() {
            bridge
                .renderer_mut()
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
    });

    let app_weak = app.as_weak();
    let state_ref = Rc::clone(&state);
    app.on_lens_pick_file(move || {
        let dialog = rfd::FileDialog::new()
            .set_title("Load lens profile JSON")
            .add_filter("JSON", &["json"]);
        let Some(path) = dialog.pick_file() else {
            return;
        };
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
                let scaled = CameraParams {
                    width: in_w,
                    height: in_h,
                    fx: params.fx * scale_w,
                    fy: params.fy * scale_h,
                    cx: params.cx * scale_w,
                    cy: params.cy * scale_h,
                    d: params.d,
                };
                if let Some(cal) = s.calibration.as_mut() {
                    cal.left = scaled.clone();
                    cal.right = scaled.clone();
                }
                if let Some(bridge) = s.bridge.as_mut() {
                    bridge
                        .renderer_mut()
                        .update_camera_params(Some(scaled.clone()), Some(scaled.clone()));
                }
                s.preview_dirty = true;
                if let Some(app) = app_weak.upgrade() {
                    set_lens_sliders(&app, &scaled, &scaled);
                    app.set_lens_dirty(true);
                    let name = display_name(&path);
                    app.set_lens_left_camera(name.clone().into());
                    app.set_lens_left_source("File".into());
                    app.set_lens_right_camera(name.into());
                    app.set_lens_right_source("File".into());
                    app.set_lens_info_available(true);
                    set_status(
                        &app,
                        StatusLine::info(format!("Lens profile loaded: {}", path.display())),
                    );
                }
            }
            Err(e) => {
                log::error!("Failed to load lens profile: {e}");
                if let Some(app) = app_weak.upgrade() {
                    set_status(
                        &app,
                        StatusLine::error(format!("Failed to load lens profile: {e}")),
                    );
                }
            }
        }
    });

    let app_weak = app.as_weak();
    let state_ref = Rc::clone(&state);
    app.on_changed_constrained_look(move || {
        let Some(app) = app_weak.upgrade() else {
            return;
        };
        let new_value = app.get_use_constrained_look();
        let mut s = state_ref.borrow_mut();
        s.use_constrained_look = new_value;
        if new_value {
            s.clamp_targets();
        }
        s.preview_dirty = true;
    });

    let state_ref = Rc::clone(&state);
    let app_weak = app.as_weak();
    app.on_changed_lens_preview(move || {
        let Some(app) = app_weak.upgrade() else {
            return;
        };
        let mut s = state_ref.borrow_mut();
        s.lens_preview_active = app.get_lens_preview_active();
        s.lens_preview_side = app.get_lens_preview_side().to_string();
        s.preview_dirty = true;
    });

    let state_ref = Rc::clone(&state);
    let app_weak = app.as_weak();
    app.on_changed_lens_correction(move |amount| {
        let mut s = state_ref.borrow_mut();
        let clamped = if amount > 0.5 { 1.0 } else { 0.0 };
        s.lens_correction_amount = clamped;
        if !s.lens_preview_active
            && let Some(bridge) = s.bridge.as_mut()
        {
            bridge
                .renderer_mut()
                .pipeline_mut()
                .set_lens_correction_amount(clamped);
        }
        s.preview_dirty = true;
        if let Some(app) = app_weak.upgrade() {
            app.set_lens_correction_amount(clamped);
            app.set_cal_dirty(true);
        }
    });

    // ── Timer: debounced seek + calibration-result poll ──

    let app_weak = app.as_weak();
    let state_ref = Rc::clone(&state);
    let timer = slint::Timer::default();
    timer.start(
        slint::TimerMode::Repeated,
        std::time::Duration::from_millis(2),
        move || {
            let mut s = state_ref.borrow_mut();

            // Independent of rendering: the audio-sync waveform must keep
            // computing even while paused, when `vsync_render_tick` (which
            // only fires on an actual redraw) may never run because
            // nothing else is playing/seeking/dirty.
            s.maybe_recompute_audio_envelope(&app_weak);

            if s.auto_calibrate.is_some() {
                let done = {
                    let handle = s.auto_calibrate.as_ref().unwrap();
                    handle.rx.try_recv().ok()
                };
                if let Some(result) = done {
                    s.auto_calibrate = None;
                    handle_calibration_result(result, &mut s, &app_weak);
                    return;
                }
            }

            if let Some((frac, requested_at)) = s.pending_seek
                && Instant::now().duration_since(requested_at)
                    >= Duration::from_millis(SEEK_DEBOUNCE_MS)
            {
                s.pending_seek = None;
                match s.playback.seek(frac) {
                    Ok(()) => {
                        let img = s.render_current();
                        if let (Some(app), Some(img)) = (app_weak.upgrade(), img) {
                            app.set_preview_frame(img);
                            app.set_current_frame(s.playback.frame_index() as i32);
                        }
                    }
                    Err(e) => log::error!("Seek error: {e}"),
                }
                return;
            }

            if let Some(app) = app_weak.upgrade()
                && (app.get_playing()
                    || s.pending_seek.is_some()
                    || s.preview_dirty
                    || s.fly_active())
            {
                app.window().request_redraw();
            }
        },
    );

    app.run()?;
    Ok(())
}

#[cfg(test)]
mod format_time_tests {
    use super::format_time;

    #[test]
    fn zero_fps_returns_zero() {
        assert_eq!(format_time(123, 0.0), "0:00.000");
    }

    #[test]
    fn formats_minutes_seconds_milliseconds() {
        // 30fps, frame 1985 -> 66.1666...s -> 1:06.166
        assert_eq!(format_time(1985, 30.0), "1:06.166");
    }

    #[test]
    fn zero_frame_is_zero() {
        assert_eq!(format_time(0, 30.0), "0:00.000");
    }
}
