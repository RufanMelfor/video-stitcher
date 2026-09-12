//! Raw-camera AI debug export: decode Left/Right source video directly
//! (no stitching, no panorama, no virtual camera), run the AI detector
//! on each raw camera frame exactly as a real tracking run would, draw
//! its bounding boxes on top, and encode each camera feed to its own
//! output video.
//!
//! # Why two separate files, not one side-by-side video
//!
//! An earlier version of this composited both cameras into one
//! side-by-side frame. Two full raw camera frames (e.g. 3840px each)
//! side by side exceeds H.264's hard 4096px width limit (a codec-level
//! ceiling, not a GPU capability gap) - every hardware encoder
//! (NVENC/QSV/AMF) refused to open, silently falling back through each
//! one to slow software encoding, which is what actually surfaced the
//! bug: a `7680x2880` export took over 5 minutes just probing encoders
//! before the first frame. Two separate native-resolution outputs sidesteps
//! the limit entirely and lets the fast hardware path work normally.
//!
//! # Why this exists, and why it looks nothing like `StitchJob`
//!
//! An earlier version of this diagnostic (`reco_core::render::
//! ai_debug_overlay`, removed) burned AI markers into the *stitched,
//! panned/zoomed* export - a projection of what the detector saw,
//! computed by re-running the detector's raw camera-space output
//! through panorama-space mapping and then the virtual camera's
//! yaw/pitch/FOV/tilt-correction chain. That re-projection is exactly
//! where two real, shipped bugs lived (a wrong hardcoded COCO ball
//! class id, then a double rig-tilt-correction in the screen
//! projection) - bugs that are structurally impossible here, because
//! this module never projects anything. It draws each detection's box
//! directly on the same raw camera pixels the detector ran inference
//! on, in the detector's own normalized camera-space coordinates
//! (`Detection::center_x/center_y/width/height`) - "exactly what the
//! model saw", matching how Label Studio (or
//! `reco-io/examples/dump_detection_frames.rs`, this module's single-
//! frame ancestor) shows bounding boxes on raw training frames.
//!
//! Because nothing here needs a virtual camera, panorama projection, or
//! GPU stitching, this module doesn't build on [`crate::StitchJob`] or
//! `reco_core::session::StitchSession` at all - it's a much smaller,
//! standalone frame-accurate decode -> detect -> draw -> encode loop
//! (run twice, once per camera), sharing only
//! [`crate::ffmpeg::decoder::VideoDecoder`] with the real stitch path
//! for decoding. Less shared surface with the real render pipeline
//! means less risk of a diagnostic-only code path ever contaminating a
//! real export.
//!
//! Encoding is the one exception: the per-frame loop submits to a
//! [`reco_core::async_encode::AsyncEncodeThread`] wrapping
//! [`crate::adapters::FfmpegFileEncoder`] - the exact same pipelined
//! encoder [`crate::StitchJob`] uses (`StitchSession::set_encoder`),
//! reused here rather than reinvented, since it already solves the
//! same problem: not blocking the frame loop on ffmpeg's encode call.
//! See "Pipelining" below.
//!
//! # Pipelining
//!
//! Each frame's work is decode (CPU) -> detect (GPU-preprocessed or
//! CPU) -> draw boxes (CPU) -> encode (CPU/GPU depending on the active
//! hardware encoder). Only the encode stage is offloaded to a
//! background thread ([`AsyncEncodeThread`]) - decode/detect/draw stay
//! serial on the calling thread. This was a deliberate, narrower scope
//! than fully overlapping every stage (e.g. also prefetching the next
//! frame's decode during the current frame's GPU/encode work): it's
//! the smallest change that removes the encoder's blocking wait from
//! the hot loop, reusing an already-proven building block instead of
//! adding new concurrency machinery to a tool that's meant to stay
//! simple. Diagnosed via live `nvidia-smi` sampling during a real
//! export: GPU utilization sat at 20-46%, not saturated, because the
//! loop was fully serial end-to-end - encoding (`VideoEncoder::
//! write_yuv420p_planes`, blocking on ffmpeg's `send_frame`/
//! `receive_packets`) was one of the stages with nothing to overlap
//! it. If this remains a bottleneck after this change, decode
//! prefetch is the next candidate - not attempted here.
//!
//! # Detector
//!
//! This module does not construct a detector itself - `reco-io` has no
//! dependency on `reco-autocam`/`reco-detect` (those depend on
//! `reco-core`, not the other way around). Callers (reco-cli, reco-gui)
//! build a `Box<dyn UnifiedDetector>` the same way `setup_autocam`
//! does internally and pass it to [`run`]. `UnifiedDetector::detect`
//! runs on `DetectorFrame::Cpu` by default, which every decoded
//! [`reco_core::source::YuvFrame`] already satisfies with no
//! preprocessing.
//!
//! # GPU preprocessing (optional)
//!
//! Passing a [`reco_core::gpu::GpuContext`] to [`run`] switches the
//! per-frame detector call from `DetectorFrame::Cpu` to
//! `DetectorFrame::WgpuNv12`. This module never constructs the
//! `WgpuPreprocessingDetector` wrapper itself (same "reco-io stays
//! detector-agnostic" boundary as above - that wrapper lives in
//! `reco-autocam`, which `reco-io` does not depend on); the caller is
//! expected to have already wrapped its `CpuYoloDetector` with one
//! before calling [`run`], exactly as `setup_autocam` does. What this
//! module DOES own is the part that has to live in the decode loop
//! regardless of which crate builds the detector: converting each
//! decoded `YuvFrame`'s planar YUV420P chroma (separate U/V planes) to
//! interleaved NV12 (a single UV plane, [`WgpuPreprocessor`]'s only
//! accepted chroma layout - see [`yuv420p_to_interleaved_uv`]) and
//! uploading Y/UV into a small per-camera wgpu texture pair reused
//! across frames.
//!
//! GPU preprocessing is a per-call opportunistic upgrade over the CPU
//! path - the CPU resize this replaces (~300ms/frame at large model
//! input sizes) was found to be the actual bottleneck behind "zero GPU
//! utilization" reports; the ORT inference call itself already used
//! whatever hardware EP was available either way. A missing/`None` GPU
//! context degrades cleanly to the original all-CPU behavior (same
//! `DetectorFrame::Cpu` path, same `UnifiedDetector` that would run
//! either way), matching `setup_autocam`'s own cascading-fallback
//! pattern - GPU preprocessing is never required to get correct boxes,
//! only faster ones.
//!
//! `WgpuPreprocessor` itself lives in `reco-detect` (`reco-io` doesn't
//! depend on that crate - see above), so it isn't linked from this doc
//! comment.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};

use reco_core::async_encode::AsyncEncodeThread;
use reco_core::detect::detector::{
    ChromaFormat, DetectorError, DetectorFrame, RawFrame, UnifiedDetector,
};
use reco_core::encoder::EncodeError;
use reco_core::geometry::CameraId;
use reco_core::gpu::GpuContext;
use reco_core::source::YuvFrame;
use reco_core::wgpu;
use thiserror::Error;

use crate::adapters::FfmpegFileEncoder;
use crate::ffmpeg::decoder::{DecodeError, VideoDecoder};
use crate::ffmpeg::encoder::EncoderConfig;
use crate::stitch_job::InputPath;

/// Errors from [`run`].
#[derive(Debug, Error)]
pub enum RawCameraDebugError {
    /// Decoding either source video failed.
    #[error("decode: {0}")]
    Decode(#[from] DecodeError),
    /// Encoding the composite output failed.
    #[error("encode: {0}")]
    Encode(#[from] EncodeError),
    /// `cut_ranges` itself was invalid (overlapping/malformed) - see
    /// `crate::cut_range::validate_cut_ranges`.
    #[error("cut_ranges: {0}")]
    CutRange(String),
    /// The GPU decode path failed *after* it had already started
    /// successfully (a staging copy or plane readback error mid-run).
    /// Failures while *opening* the GPU path are not errors - they log
    /// and fall back to CPU decode instead.
    #[error("gpu decode: {0}")]
    GpuDecode(String),
}

/// Bounding-box color convention, shared with
/// `reco-io/examples/dump_detection_frames.rs` so a user cross-checking
/// this export against that tool's PNG dumps sees the same colors for
/// the same classes: person=blue, ball=red, referee=green, other=yellow.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct BoxColor {
    r: u8,
    g: u8,
    b: u8,
}

const COLOR_PERSON: BoxColor = BoxColor {
    r: 64,
    g: 128,
    b: 255,
};
const COLOR_BALL: BoxColor = BoxColor {
    r: 255,
    g: 32,
    b: 32,
};
const COLOR_REFEREE: BoxColor = BoxColor {
    r: 32,
    g: 220,
    b: 32,
};
const COLOR_OTHER: BoxColor = BoxColor {
    r: 255,
    g: 220,
    b: 32,
};
const COLOR_ROI: BoxColor = BoxColor {
    r: 255,
    g: 230,
    b: 0,
};

fn class_color(class_id: u16, ball_class_id: Option<u16>) -> BoxColor {
    match class_id {
        0 => COLOR_PERSON,
        id if Some(id) == ball_class_id => COLOR_BALL,
        2 => COLOR_REFEREE,
        _ => COLOR_OTHER,
    }
}

/// Which camera(s) a run processes - see [`RawCameraDebugConfig::cameras`].
/// Not persisted (same "never a saved setting" constraint as the rest
/// of this config) - deliberately re-chosen every run so a diagnostic
/// pass never silently skips a camera because of a stale prior choice.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CameraSelection {
    Left,
    Right,
    Both,
}

impl CameraSelection {
    fn includes(self, camera: CameraId) -> bool {
        matches!(
            (self, camera),
            (CameraSelection::Both, _)
                | (CameraSelection::Left, CameraId::Left)
                | (CameraSelection::Right, CameraId::Right)
        )
    }
}

/// Configuration for [`run`]. Every field is required (no calibration-
/// persisted defaults - this is a one-shot diagnostic run, never a
/// saved setting, mirroring the removed stitched overlay's own "never
/// persisted" design constraint).
pub struct RawCameraDebugConfig {
    pub left: InputPath,
    pub right: InputPath,
    /// Output path for the Left camera's boxed video. Unused (never
    /// opened/written) when [`cameras`](Self::cameras) excludes Left.
    pub output_left: PathBuf,
    /// Output path for the Right camera's boxed video. Unused (never
    /// opened/written) when [`cameras`](Self::cameras) excludes Right.
    pub output_right: PathBuf,
    /// Which camera(s) to actually process. A user diagnosing one
    /// side's tracking shouldn't have to wait through (and re-encode)
    /// the other camera's full pass just to get the file they wanted.
    pub cameras: CameraSelection,
    /// Skip this many seconds of both sources before drawing/encoding
    /// starts. Converted to a frame count using the left source's own
    /// decoded fps (matches `StitchJob::start_time`'s convention:
    /// `(start_secs * fps).round()`) - resolved inside [`run`] itself
    /// rather than by the caller, so it only needs one decoder open.
    pub start_secs: f64,
    /// Stop drawing/encoding at this source timestamp (seconds), same
    /// semantics as `StitchJob::end_time`. `None` runs to the source's
    /// end (or `max_frames`, if set). Resolved the same way
    /// `start_secs` is: fed straight into `crate::cut_range::
    /// keep_windows` as its end bound, so an end time and cut ranges
    /// compose exactly like they do for a real export.
    pub end_secs: Option<f64>,
    /// Extra frames to skip on the right source only, to correct a
    /// sync offset between the two cameras (same sign convention as
    /// `StitchJob::sync_offset`: positive skips right frames).
    pub sync_offset_right: i64,
    /// Stop after this many output frames. `None` runs to the shorter
    /// source's end.
    pub max_frames: Option<u64>,
    /// Time ranges to exclude from the export (e.g. a halftime pause),
    /// same semantics as `StitchJob::cut_ranges` - see
    /// `crate::cut_range`'s module doc. Skipped between windows via a
    /// real seek (`VideoDecoder::seek_to_secs`), not frame-accurate
    /// sequential decode-and-discard like the rest of this module's
    /// frame stepping: a cut range is typically minutes long (a match's
    /// halftime), and decoding through one just to throw the frames away
    /// would make this diagnostic impractically slow for no benefit -
    /// nothing is drawn or encoded from inside a cut range, so the
    /// small seek-accuracy loss at its boundary is invisible in the
    /// output. Empty runs the whole `[start_secs, end)` span uncut.
    pub cut_ranges: Vec<crate::cut_range::CutRange>,
    /// Run detection only every Nth decoded frame, reusing the previous
    /// detection's boxes on the frames in between (`1` = detect on every
    /// frame, the original behavior and the most faithful diagnostic).
    ///
    /// Detection is by far the dominant per-frame cost here - unlike a
    /// real export, this tool detects on TWO full-resolution raw camera
    /// feeds (e.g. 3840x2880 each) rather than one downscaled stitched
    /// output, and originally did so on every single frame. A real
    /// export's own `detection_interval` (default 3, commonly 10) exists
    /// for exactly this reason; matching it here trades diagnostic
    /// temporal resolution for throughput, which is a reasonable trade
    /// when the question being answered is "does the AI lose the ball"
    /// rather than "what did frame 2277 exactly look like".
    ///
    /// Boxes drawn on a skipped frame are the last real detection's,
    /// held until the next detection frame - so a ball that moves
    /// between detections shows a box lagging slightly behind it. At
    /// small intervals (2-3) that lag is barely visible; at 10+ it is
    /// obvious on fast motion, and a box can appear to "stick" where the
    /// ball no longer is. That is a display artifact of the interval,
    /// NOT the tracker losing the ball - do not diagnose ball-loss from
    /// a run with a large interval. `0` is treated as `1`.
    pub detection_interval: u32,
    /// The attached model's real ball class id (see
    /// `reco_autocam::setup_autocam`'s `resolved_ball_class_id`
    /// doc comment for why this can't be a fixed COCO constant).
    /// `None` draws every non-person, non-referee class in the
    /// "other" (yellow) color.
    pub ball_class_id: Option<u16>,
    /// Detector confidence floor `[0,1]` - a raw detection below this
    /// score is not drawn. `None` draws everything the detector itself
    /// returned (most detectors already apply their own threshold
    /// internally, so this is usually a no-op filter).
    pub confidence_threshold: Option<f32>,
    /// Field ROI polygons (already densified through rectified space -
    /// see `FieldRoi::densified`), normalized `[0,1]` raw-camera
    /// coordinates, drawn as a yellow outline when present. `None`
    /// (or an empty polygon) draws nothing.
    pub field_roi_left: Option<Vec<[f64; 2]>>,
    pub field_roi_right: Option<Vec<[f64; 2]>>,
    /// Output encoder configuration (codec/quality/container). The
    /// resolution is fixed by the two decoded source frames
    /// side-by-side, so `EncoderConfig` here only controls how those
    /// pixels are compressed.
    pub encoder: EncoderConfig,
    /// When set, each decoded frame is uploaded to a wgpu NV12 texture
    /// pair and handed to `detector` as `DetectorFrame::WgpuNv12`
    /// instead of `DetectorFrame::Cpu` - see this module's "GPU
    /// preprocessing" doc section. The caller is expected to have
    /// already wrapped its detector with `WgpuPreprocessingDetector`
    /// bound to this same device/queue (this module has no dependency
    /// on that wrapper type - see this module's doc comment). `None`
    /// runs the original all-CPU path unchanged; this is also the
    /// automatic fallback if GPU upload ever fails mid-run.
    pub gpu: Option<GpuContext>,
}

/// One output frame's box count, reported via `on_progress` for a
/// status line ("N boxes drawn this frame"). [`run`] processes Left
/// fully, then Right (see its own doc comment for why) - `camera`
/// says which pass this progress update belongs to, and `frames_done`
/// counts frames within that pass, NOT a combined total across both;
/// a caller wanting one running total should sum a completed Left
/// pass's final `frames_done` with the current Right pass's.
#[derive(Debug, Clone, Copy)]
pub struct RawCameraDebugProgress {
    pub camera: CameraId,
    pub frames_done: u64,
    pub detections: usize,
}

/// Run the raw-camera AI debug export: decode, detect, draw, encode -
/// see this module's doc comment for the full design rationale.
///
/// **Left is processed fully - every frame decoded, detected, drawn,
/// and encoded to a finished file - before Right starts.** Not
/// interleaved frame-by-frame like a stitched export legitimately
/// needs to be (there, both cameras feed one synchronized panorama
/// frame). Here the two output files are entirely independent, and a
/// user reviewing this diagnostic only ever watches one video at a
/// time anyway - so finishing Left completely means there's something
/// watchable as soon as possible, instead of making the user wait for
/// the combined runtime of both before either file is usable. Same
/// total work either way; this only changes the order it completes in.
///
/// `detector` runs on both cameras' frames in turn, exactly as a real
/// tracking run's detector would (no shared state between the two
/// calls is assumed or required - a detector that DOES keep per-call
/// state, like a stateful tracker rather than a stateless detector,
/// would need one instance per camera; every in-tree `UnifiedDetector`
/// is stateless per call).
///
/// Detection runs every `config.detection_interval` frames (`1` =
/// every frame, the most faithful diagnostic). Frames in between reuse
/// the previous detection's boxes - see
/// [`RawCameraDebugConfig::detection_interval`] for the trade-off and
/// its effect on how the output should be read.
pub fn run(
    config: RawCameraDebugConfig,
    mut detector: Box<dyn UnifiedDetector>,
    interrupted: &AtomicBool,
    mut on_progress: impl FnMut(RawCameraDebugProgress),
) -> Result<u64, RawCameraDebugError> {
    let left_frames = if config.cameras.includes(CameraId::Left) {
        run_one_camera(
            CameraId::Left,
            &config.left,
            &config.output_left,
            config.field_roi_left.as_deref(),
            0, // Left's own decode never needs a sync-offset skip
            &config,
            detector.as_mut(),
            interrupted,
            &mut on_progress,
        )?
    } else {
        0
    };
    if interrupted.load(Ordering::Relaxed) {
        return Ok(left_frames);
    }
    let right_frames = if config.cameras.includes(CameraId::Right) {
        run_one_camera(
            CameraId::Right,
            &config.right,
            &config.output_right,
            config.field_roi_right.as_deref(),
            config.sync_offset_right,
            &config,
            detector.as_mut(),
            interrupted,
            &mut on_progress,
        )?
    } else {
        0
    };

    Ok(left_frames + right_frames)
}

/// One camera's full decode -> detect -> draw -> encode pass, sharing
/// `detector` (and the cut-range/start-time settings on `config`)
/// with whichever pass runs next - see [`run`]'s doc comment for why
/// this runs twice sequentially rather than once, interleaved.
/// `sync_offset_frames` shifts this camera's own start/seek points
/// (Left always passes `0`; Right passes `config.sync_offset_right` -
/// same sign convention as `StitchJob::sync_offset`).
#[allow(clippy::too_many_arguments)]
fn run_one_camera(
    camera: CameraId,
    input: &InputPath,
    output: &std::path::Path,
    field_roi: Option<&[[f64; 2]]>,
    sync_offset_frames: i64,
    config: &RawCameraDebugConfig,
    detector: &mut dyn UnifiedDetector,
    interrupted: &AtomicBool,
    on_progress: &mut impl FnMut(RawCameraDebugProgress),
) -> Result<u64, RawCameraDebugError> {
    // Prefer hardware decode when a GPU context is available: a raw
    // camera feed here is full-resolution (e.g. 3840x2880 HEVC) and
    // software-decoding it is this tool's dominant per-frame cost once
    // detection is thinned by `detection_interval`. Falls back to CPU
    // decode for every reason the GPU path can be unavailable - see
    // `D3d11FrameSource::try_new`.
    #[cfg(target_os = "windows")]
    let gpu_source = config
        .gpu
        .as_ref()
        .and_then(|gpu| D3d11FrameSource::try_new(input, gpu, camera));
    #[cfg(not(target_os = "windows"))]
    let gpu_source: Option<std::convert::Infallible> = None;

    let mut dec = match gpu_source {
        #[cfg(target_os = "windows")]
        Some(s) => FrameSource::D3d11(Box::new(s)),
        #[cfg(not(target_os = "windows"))]
        Some(_) => unreachable!("no GPU decode path off Windows"),
        None => FrameSource::Cpu(Box::new(CpuFrameSource {
            decoder: VideoDecoder::open_input(input)?,
            current: None,
        })),
    };

    let fps = dec.fps();
    let start_frame = if config.start_secs > 0.0 {
        (config.start_secs * fps).round() as u64
    } else {
        0
    };

    // Skip to the requested start, sequentially - frame-accurate by
    // construction (see `dump_detection_frames.rs`'s identical
    // convention: a lossy `-ss` seek lands on the nearest keyframe, not
    // the exact frame, which matters for a fast-moving ball).
    let skip = (start_frame as i64 + sync_offset_frames).max(0);
    for _ in 0..skip {
        if !dec.skip_one_frame()? {
            log::warn!("raw-camera debug: {camera} source ended during start-frame skip");
            return Ok(0);
        }
    }

    // Cut ranges resolve to the keep-windows they imply - same
    // `[start, end)` sequence `StitchJob` would decode, minus the
    // excluded spans. An empty `cut_ranges` degenerates to exactly one
    // window covering `[start_secs, end)`, so this loop's shape below
    // is a strict superset of (and byte-for-byte equivalent to, in that
    // case) the old uncut behavior.
    let sorted_cuts = crate::cut_range::validate_cut_ranges(config.cut_ranges.clone())
        .map_err(RawCameraDebugError::CutRange)?;
    let keep_windows =
        crate::cut_range::keep_windows(config.start_secs, config.end_secs, &sorted_cuts);
    if !sorted_cuts.is_empty() {
        let summary: Vec<String> = keep_windows
            .iter()
            .map(|(s, e)| match e {
                Some(e) => format!("{s:.2}-{e:.2}s"),
                None => format!("{s:.2}s-end"),
            })
            .collect();
        log::info!(
            "raw-camera debug: {camera}: {} cut range(s) excluded, {} keep window(s): {}",
            sorted_cuts.len(),
            keep_windows.len(),
            summary.join(", "),
        );
    }

    let mut encoder: Option<AsyncEncodeThread> = None;
    let mut frames_done = 0u64;

    // GPU NV12 upload texture pool, opened lazily on this camera's
    // first decoded frame (Left and Right never share one - see
    // `NvUploadPool`'s doc comment). `None` for either the whole run
    // (no `config.gpu`) or this camera specifically (its pool failed
    // to build) - both degrade to the CPU path.
    let mut gpu_pool: Option<NvUploadPool> = None;
    let mut gpu_upload_warned = false;

    // Reused NV12 scratch buffer for the encode submit below (Y plane
    // followed by interleaved UV, `AsyncEncodeThread::submit`'s only
    // accepted layout - see `FfmpegFileEncoder`). Independent of
    // `gpu_pool`'s own `uv_scratch`: this one is needed unconditionally
    // (encoding always goes through NV12 now, GPU preprocessing or not),
    // that one only exists when a GPU context is active. Resized (not
    // reallocated) if resolution ever changes mid-run.
    let mut nv12_scratch: Vec<u8> = Vec::new();
    let mut frame_index_for_pts = 0u64;

    // Last real detection's boxes, redrawn on frames that skip
    // detection (see `RawCameraDebugConfig::detection_interval`).
    // Starts empty, so frames before the first detection draw no
    // boxes rather than stale ones from the other camera's pass.
    let mut last_dets: Vec<DrawDet> = Vec::new();
    let detect_every = config.detection_interval.max(1) as u64;
    if detect_every > 1 {
        log::info!(
            "raw-camera debug: {camera}: detecting every {detect_every} frames \
             (boxes on skipped frames are the previous detection's)"
        );
    }

    'windows: for (window_index, (win_start, win_end)) in keep_windows.iter().enumerate() {
        if window_index > 0 {
            // Window 0's start was already reached by the start_secs
            // skip above; every later window needs an actual seek past
            // its preceding cut range. Real seek, not frame-accurate
            // discard - see `RawCameraDebugConfig::cut_ranges`'s doc
            // comment for why.
            log::info!(
                "raw-camera debug: {camera}: seeking to {win_start:.2}s for the next keep \
                 window (skipping a cut range)"
            );
            dec.seek_to_secs(*win_start + sync_offset_frames as f64 / fps)?;
        }
        let window_end_frame = win_end.map(|e| ((e - win_start) * fps).round().max(0.0) as u64);

        let mut window_frame = 0u64;
        loop {
            if interrupted.load(Ordering::Relaxed) {
                log::info!("raw-camera debug: {camera}: interrupted after {frames_done} frame(s)");
                break 'windows;
            }
            if let Some(max) = config.max_frames
                && frames_done >= max
            {
                break 'windows;
            }
            if let Some(end_frame) = window_end_frame
                && window_frame >= end_frame
            {
                break; // this window's own end reached - move to the next one
            }
            let Some(frame) = dec.next_frame()? else {
                break 'windows; // source ended - nothing more to do for this camera
            };
            window_frame += 1;

            // `frames_done` counts this camera's own emitted frames and
            // only ever increases, so it drives the interval directly:
            // frame 0 always detects, then every Nth after it. Using it
            // (rather than a per-window counter) keeps the detection
            // cadence unbroken across a cut-range seek.
            if frames_done.is_multiple_of(detect_every) {
                last_dets = detect_one_gpu_or_cpu(
                    detector,
                    camera,
                    frame,
                    config.confidence_threshold,
                    config.gpu.as_ref(),
                    &mut gpu_pool,
                    &mut gpu_upload_warned,
                );
            }
            let detections_this_frame = last_dets.len();

            if let Some(roi) = field_roi {
                draw_polygon(frame, roi, COLOR_ROI);
            }
            for d in &last_dets {
                draw_detection(frame, d, config.ball_class_id);
            }

            let enc = match &mut encoder {
                Some(e) => e,
                None => {
                    let file_encoder = FfmpegFileEncoder::new(
                        output,
                        frame.width,
                        frame.height,
                        (fps.round() as i32, 1),
                        &config.encoder,
                    )?;
                    log::info!(
                        "raw-camera debug: {camera} {}x{} -> {} ({})",
                        frame.width,
                        frame.height,
                        output.display(),
                        file_encoder.encoder_name(),
                    );
                    // buffer_count=2: same in-flight depth `StitchJob`
                    // uses for its own primary encoder
                    // (`StitchSession::set_encoder`'s call site) - lets
                    // the calling thread stay one frame ahead of the
                    // encode thread without unbounded queueing.
                    encoder = Some(AsyncEncodeThread::new(
                        Box::new(file_encoder),
                        frame.width,
                        frame.height,
                        2,
                    ));
                    encoder.as_mut().expect("just inserted")
                }
            };

            let nv12_len = frame.y.len() + frame.u.len() + frame.v.len();
            nv12_scratch.resize(nv12_len, 0);
            let (y_dst, uv_dst) = nv12_scratch.split_at_mut(frame.y.len());
            y_dst.copy_from_slice(&frame.y);
            yuv420p_to_interleaved_uv(&frame.u, &frame.v, uv_dst);
            // pts_us is advisory here (this diagnostic tool has no
            // B-frames/reordering to drive with real timestamps) - a
            // monotonically increasing per-frame counter in
            // frame-duration units is enough for `AsyncEncodeThread`'s
            // API contract without threading the decoder's own PTS
            // through this loop.
            let pts_us = ((frame_index_for_pts as f64) * 1_000_000.0 / fps).round() as i64;
            frame_index_for_pts += 1;
            enc.submit(&nv12_scratch, pts_us)?;

            frames_done += 1;
            on_progress(RawCameraDebugProgress {
                camera,
                frames_done,
                detections: detections_this_frame,
            });
        }
    }

    if let Some(mut enc) = encoder {
        enc.finish()?;
    } else {
        log::warn!("raw-camera debug: {camera}: no frames decoded, no output written");
    }

    Ok(frames_done)
}

/// Per-camera frame source: either plain CPU decode, or D3D11VA
/// hardware decode with the decoded NV12 planes copied back to the CPU.
///
/// # Why the GPU variant reads back instead of staying on the GPU
///
/// This tool draws its boxes on the CPU, straight onto the decoded YUV
/// planes, so that every pixel that is not part of a box stays exactly
/// as the detector saw it. Keeping the frame on the GPU would mean
/// drawing with a shader into an RGBA target and converting back, which
/// puts the whole image through an NV12 -> RGB -> NV12 roundtrip - a
/// lossy chroma conversion applied to the entire frame for the sake of
/// a few thin rectangles. See
/// [`reco_core::gpu::nv12_readback`]'s module doc.
///
/// What the GPU path does buy is the decode itself: a raw camera feed
/// here is full-resolution (e.g. 3840x2880 HEVC) and decoding two of
/// them on the CPU is the dominant per-frame cost once detection is
/// thinned out by [`RawCameraDebugConfig::detection_interval`].
enum FrameSource {
    /// Software decode - always available, the fallback for everything.
    /// The `Option<YuvFrame>` parks each decoded frame so `next_frame`
    /// can hand back a `&mut` with the same signature the GPU variant
    /// (which reuses one scratch frame) needs.
    Cpu(Box<CpuFrameSource>),
    /// D3D11VA hardware decode + per-plane readback into `scratch`.
    #[cfg(target_os = "windows")]
    D3d11(Box<D3d11FrameSource>),
}

/// State for the software decode path - boxed inside [`FrameSource`]
/// so neither variant dominates the enum's size.
struct CpuFrameSource {
    decoder: VideoDecoder,
    current: Option<YuvFrame>,
}

/// State for the D3D11VA decode path - boxed inside [`FrameSource`]
/// because it is much larger than the CPU variant.
#[cfg(target_os = "windows")]
struct D3d11FrameSource {
    decoder: VideoDecoder,
    staging: reco_core::interop::d3d11::D3d11StagingPool,
    readback: Option<reco_core::gpu::nv12_readback::Nv12Readback>,
    gpu: GpuContext,
    /// Reused YUV420P frame handed back to the caller each time, so the
    /// rest of the loop is identical to the CPU path.
    scratch: YuvFrame,
}

impl FrameSource {
    /// The source's frame rate, or `30.0` if it reports none.
    fn fps(&self) -> f64 {
        let fps = match self {
            Self::Cpu(s) => s.decoder.fps(),
            #[cfg(target_os = "windows")]
            Self::D3d11(s) => s.decoder.fps(),
        };
        if fps > 0.0 { fps } else { 30.0 }
    }

    fn seek_to_secs(&mut self, secs: f64) -> Result<(), DecodeError> {
        match self {
            Self::Cpu(s) => s.decoder.seek_to_secs(secs),
            #[cfg(target_os = "windows")]
            Self::D3d11(s) => s.decoder.seek_to_secs(secs),
        }
    }

    /// Decode and discard one frame, without the readback/conversion
    /// work `next_frame` does - used for the start-time skip, where the
    /// pixels are thrown away anyway. `Ok(false)` means the source ended.
    fn skip_one_frame(&mut self) -> Result<bool, RawCameraDebugError> {
        match self {
            Self::Cpu(s) => Ok(s.decoder.next_frame()?.is_some()),
            #[cfg(target_os = "windows")]
            Self::D3d11(s) => Ok(s.decoder.next_frame_d3d11()?.is_some()),
        }
    }

    /// Decode one frame as YUV420P planes, whichever way this source
    /// decodes. `Ok(None)` means the source ended.
    fn next_frame(&mut self) -> Result<Option<&mut YuvFrame>, RawCameraDebugError> {
        match self {
            Self::Cpu(s) => {
                s.current = s.decoder.next_frame()?;
                Ok(s.current.as_mut())
            }
            #[cfg(target_os = "windows")]
            Self::D3d11(s) => s.next_frame(),
        }
    }
}

#[cfg(target_os = "windows")]
impl D3d11FrameSource {
    /// Try to open `input` for D3D11VA hardware decode.
    ///
    /// Returns `None` (with a log line) for every reason this path can
    /// be unavailable rather than failing the export - no hardware
    /// device, a non-DX12 wgpu backend, a codec the GPU can't decode.
    /// The caller falls back to CPU decode, which always works.
    ///
    /// **Disabled as of 2026-09-11** (always returns `None`, falling
    /// back to CPU decode): confirmed via direct diagnostic logging
    /// inside [`Self::next_frame`] that [`Nv12Readback::read`] returns
    /// all-zero Y/UV planes on every single frame, not just the first -
    /// only the CPU-drawn ROI/box overlay is ever real content in the
    /// output. Root cause NOT found despite extensive investigation
    /// into wgpu-core's texture init-tracking
    /// (`TextureUses::UNINITIALIZED` / `TextureClearMode::None` on a
    /// `create_texture_from_hal`-imported texture, and whether
    /// `copy_texture_to_buffer`'s `NeedsInitializedMemory` check
    /// silently fails for it) - two lines of reasoning contradicted
    /// each other (a `ClearError` should panic via wgpu's default
    /// "errors are fatal" handler, but the process never panicked
    /// across 287 real frames). See
    /// [[project_d3d11_gpu_decode_empty_frames_bug]] in memory for the
    /// full investigation trail and next steps. The rest of this
    /// module (`FrameSource`, `D3d11FrameSource`, `Nv12Readback`) is
    /// left in place, unused via this early return, rather than
    /// deleted - the code itself was never shown to be structurally
    /// wrong, only its result.
    fn try_new(input: &InputPath, gpu: &GpuContext, camera: CameraId) -> Option<Self> {
        let _ = (input, gpu, camera);
        log::info!(
            "raw-camera debug: {camera}: D3D11VA GPU decode disabled (known bug - \
             produces empty frames, see project_d3d11_gpu_decode_empty_frames_bug memory), \
             using CPU decode"
        );
        None
    }

    /// The real implementation `try_new` disables above - kept intact
    /// (not deleted) so a future session can re-enable it (remove the
    /// short-circuit above, restore this as `try_new`) once the bug is
    /// fixed, without reconstructing this from scratch. Never called
    /// while the bug is open; `#[allow(dead_code)]` because clippy
    /// otherwise flags an unused private fn.
    #[allow(dead_code)]
    fn try_new_impl(input: &InputPath, gpu: &GpuContext, camera: CameraId) -> Option<Self> {
        if !gpu.is_dx12() {
            log::info!(
                "raw-camera debug: {camera}: wgpu backend is not DX12, using CPU decode \
                 (D3D11VA interop needs DX12)"
            );
            return None;
        }
        let device = crate::ffmpeg::decoder::create_shared_hw_device()?;
        if device.backend() != crate::ffmpeg::decoder::DecodeBackend::D3d11va {
            log::info!(
                "raw-camera debug: {camera}: shared hw device is {:?}, not D3D11VA - using \
                 CPU decode",
                device.backend()
            );
            return None;
        }
        let decoder = match VideoDecoder::open_input_with_shared_device(input, &device) {
            Ok(d) => d,
            Err(e) => {
                log::warn!(
                    "raw-camera debug: {camera}: D3D11VA decoder open failed ({e}), \
                     using CPU decode"
                );
                return None;
            }
        };
        if decoder.backend() != crate::ffmpeg::decoder::DecodeBackend::D3d11va {
            log::info!(
                "raw-camera debug: {camera}: decoder fell back to {:?}, using CPU decode",
                decoder.backend()
            );
            return None;
        }

        // Dimensions aren't known until the first decoded frame, so the
        // staging pool and readback are built lazily in `next_frame`.
        log::info!("raw-camera debug: {camera}: D3D11VA hardware decode enabled");
        Some(Self {
            decoder,
            // One slot: this loop fully consumes each frame (stage ->
            // read back -> draw -> encode) before decoding the next, so
            // there is never more than one staged frame in flight.
            staging: reco_core::interop::d3d11::D3d11StagingPool::new(
                gpu,
                0,
                0,
                1,
                false,
                reco_core::render::renderer::GpuPixelFormat::Nv12,
            )
            .ok()?,
            readback: None,
            gpu: gpu.clone(),
            scratch: YuvFrame {
                y: Vec::new(),
                u: Vec::new(),
                v: Vec::new(),
                width: 0,
                height: 0,
                timestamp_us: 0,
            },
        })
    }

    /// Decode one frame on the GPU, copy its NV12 planes back, and
    /// de-interleave the chroma into this source's reusable YUV420P
    /// scratch frame.
    fn next_frame(&mut self) -> Result<Option<&mut YuvFrame>, RawCameraDebugError> {
        let Some(frame) = self.decoder.next_frame_d3d11()? else {
            return Ok(None);
        };
        let (w, h) = (frame.width, frame.height);

        // (Re)build the size-dependent resources on the first frame, or
        // if the source ever changes resolution mid-stream.
        if self.readback.as_ref().map(|r| r.frame_size()) != Some((w, h)) {
            self.staging = reco_core::interop::d3d11::D3d11StagingPool::new(
                &self.gpu,
                w,
                h,
                1,
                false,
                reco_core::render::renderer::GpuPixelFormat::Nv12,
            )
            .map_err(|e| RawCameraDebugError::GpuDecode(e.to_string()))?;
            self.readback = Some(
                reco_core::gpu::nv12_readback::Nv12Readback::new(&self.gpu, w, h)
                    .map_err(|e| RawCameraDebugError::GpuDecode(e.to_string()))?,
            );
            self.scratch.width = w;
            self.scratch.height = h;
            self.scratch.y = vec![0u8; (w * h) as usize];
            self.scratch.u = vec![0u8; ((w / 2) * (h / 2)) as usize];
            self.scratch.v = vec![0u8; ((w / 2) * (h / 2)) as usize];
        }

        // SAFETY: `frame.texture` is a valid ID3D11Texture2D* owned by
        // `frame`, which outlives this call.
        unsafe {
            self.staging
                .stage_frame(frame.texture, frame.array_slice, 0)
                .map_err(|e| RawCameraDebugError::GpuDecode(e.to_string()))?;
        }

        let readback = self.readback.as_mut().expect("built above");
        let plane_source = self.staging.plane_source(0);
        let (y, uv) = readback
            .read(&self.gpu, plane_source.texture)
            .map_err(|e| RawCameraDebugError::GpuDecode(e.to_string()))?;

        self.scratch.y.copy_from_slice(y);
        interleaved_uv_to_yuv420p(uv, &mut self.scratch.u, &mut self.scratch.v);
        self.scratch.timestamp_us = frame.timestamp_us;
        Ok(Some(&mut self.scratch))
    }
}

/// One drawable detection, normalized-camera-space (`[0,1]`), the same
/// shape `reco_core::detect::detector::Detection` already carries -
/// re-declared locally only so callers that filter/relabel before
/// drawing don't need to round-trip through `Detection` itself.
struct DrawDet {
    class_id: u16,
    confidence: f32,
    center_x: f32,
    center_y: f32,
    width: f32,
    height: f32,
}

fn detect_one(
    detector: &mut dyn UnifiedDetector,
    camera: CameraId,
    frame: &YuvFrame,
    confidence_threshold: Option<f32>,
) -> Vec<DrawDet> {
    let raw = RawFrame {
        y: &frame.y,
        chroma: ChromaFormat::Yuv420p {
            u: &frame.u,
            v: &frame.v,
        },
        width: frame.width,
        height: frame.height,
    };
    run_detect(
        detector,
        camera,
        &DetectorFrame::Cpu(raw),
        confidence_threshold,
        "CPU YUV420P",
    )
}

/// Detect on one camera's frame, using GPU NV12 preprocessing when
/// `gpu` is available and this camera's upload pool builds/uploads
/// successfully, falling back to the plain CPU path (same as
/// [`detect_one`]) otherwise - see this module's "GPU preprocessing"
/// doc section. `pool` is this camera's own lazily-built texture pair
/// (Left and Right never share one - see [`NvUploadPool`]'s doc
/// comment); `warned_once` suppresses repeat fallback log spam across
/// the whole run once GPU upload has failed for either camera.
#[allow(clippy::too_many_arguments)]
fn detect_one_gpu_or_cpu(
    detector: &mut dyn UnifiedDetector,
    camera: CameraId,
    frame: &YuvFrame,
    confidence_threshold: Option<f32>,
    gpu: Option<&GpuContext>,
    pool: &mut Option<NvUploadPool>,
    warned_once: &mut bool,
) -> Vec<DrawDet> {
    let Some(gpu) = gpu else {
        return detect_one(detector, camera, frame, confidence_threshold);
    };

    let pool = match pool {
        Some(p) if p.width == frame.width && p.height == frame.height => p,
        _ => match NvUploadPool::new(gpu, frame.width, frame.height) {
            Ok(p) => pool.insert(p),
            Err(e) => {
                if !*warned_once {
                    log::warn!(
                        "raw-camera debug: GPU NV12 texture pool failed on {camera} \
                         ({e}), falling back to CPU preprocessing for this camera"
                    );
                    *warned_once = true;
                }
                return detect_one(detector, camera, frame, confidence_threshold);
            }
        },
    };

    pool.upload(gpu, frame);
    let y_view = pool.y_view();
    let uv_view = pool.uv_view();
    let gpu_frame = DetectorFrame::WgpuNv12 {
        y_view: &y_view,
        uv_view: &uv_view,
        width: frame.width,
        height: frame.height,
        rotation: 0,
    };
    run_detect(
        detector,
        camera,
        &gpu_frame,
        confidence_threshold,
        "wgpu NV12",
    )
}

/// Shared detect-call + confidence-filter + `DrawDet` mapping, used by
/// both the CPU and GPU frame-construction paths above so the fail-soft
/// logging and coordinate mapping only exist once.
fn run_detect(
    detector: &mut dyn UnifiedDetector,
    camera: CameraId,
    frame: &DetectorFrame<'_>,
    confidence_threshold: Option<f32>,
    kind_for_log: &str,
) -> Vec<DrawDet> {
    match detector.detect(camera, frame) {
        Ok(dets) => dets
            .into_iter()
            .filter(|d| confidence_threshold.is_none_or(|t| d.confidence >= t))
            .map(|d| DrawDet {
                class_id: d.class_id,
                confidence: d.confidence,
                center_x: d.center_x,
                center_y: d.center_y,
                width: d.width,
                height: d.height,
            })
            .collect(),
        Err(DetectorError::UnsupportedFrameKind) => {
            // Logged once per occurrence rather than panicking: a
            // misconfigured caller could hand this module a detector
            // that doesn't accept the frame kind we just built (e.g. a
            // CPU-only backend with a GPU context configured, or vice
            // versa) - fail soft (no boxes that frame) so the rest of
            // the diagnostic export still finishes and the log makes
            // the misconfiguration obvious.
            log::warn!("raw-camera debug: detector does not accept {kind_for_log} frames");
            Vec::new()
        }
        Err(e) => {
            log::warn!("raw-camera debug: detection failed on {camera}: {e}");
            Vec::new()
        }
    }
}

/// Per-camera wgpu NV12 upload textures, reused across frames.
///
/// `WgpuPreprocessor`/`DetectorFrame::WgpuNv12` require genuinely
/// interleaved NV12 chroma (one `Rg8Unorm` UV plane), not the planar
/// YUV420P (separate U/V planes) `VideoDecoder::next_frame()` produces.
/// Confirmed by reading `WgpuPreprocessor`'s WGSL shader, which samples
/// `uv_tex` once per texel and takes both channels (`.rg`) from that
/// single sample. [`yuv420p_to_interleaved_uv`] does the (cheap,
/// microseconds, not the ~300ms/frame resize this whole path exists to
/// avoid) CPU-side interleave before each upload.
///
/// One pool per camera, not shared: `WgpuPreprocessor::preprocess` is
/// fully synchronous/blocking per call (see its own doc comment on the
/// submission-index wait), so nothing here needs double-buffering or
/// in-flight tracking - but Left and Right can be different
/// resolutions (independent source cameras), so their textures can't
/// be the same size and must be separate pools regardless.
struct NvUploadPool {
    y_texture: wgpu::Texture,
    uv_texture: wgpu::Texture,
    width: u32,
    height: u32,
    /// Reused across frames - a fresh `Vec` per call was a measurable
    /// per-frame allocation at 3840x2880 chroma resolution.
    uv_scratch: Vec<u8>,
}

impl NvUploadPool {
    fn new(gpu: &GpuContext, width: u32, height: u32) -> Result<Self, String> {
        if width == 0 || height == 0 {
            return Err(format!("zero-sized frame ({width}x{height})"));
        }
        let device = gpu.device();
        let usage = wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST;
        let y_texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("raw_camera_debug_nv12_y"),
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::R8Unorm,
            usage,
            view_formats: &[],
        });
        let (uv_w, uv_h) = (width.div_ceil(2), height.div_ceil(2));
        let uv_texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("raw_camera_debug_nv12_uv"),
            size: wgpu::Extent3d {
                width: uv_w,
                height: uv_h,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rg8Unorm,
            usage,
            view_formats: &[],
        });
        Ok(Self {
            y_texture,
            uv_texture,
            width,
            height,
            uv_scratch: vec![0u8; (uv_w * uv_h * 2) as usize],
        })
    }

    fn y_view(&self) -> wgpu::TextureView {
        self.y_texture
            .create_view(&wgpu::TextureViewDescriptor::default())
    }

    fn uv_view(&self) -> wgpu::TextureView {
        self.uv_texture
            .create_view(&wgpu::TextureViewDescriptor::default())
    }

    /// Upload one decoded frame's Y plane as-is and its U/V planes
    /// interleaved into NV12, overwriting this pool's textures in
    /// place (no new allocation - `queue.write_texture` on an
    /// already-sized texture).
    fn upload(&mut self, gpu: &GpuContext, frame: &YuvFrame) {
        let (uv_w, uv_h) = (self.width.div_ceil(2), self.height.div_ceil(2));
        yuv420p_to_interleaved_uv(&frame.u, &frame.v, &mut self.uv_scratch);

        gpu.queue().write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &self.y_texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            &frame.y,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(self.width),
                rows_per_image: Some(self.height),
            },
            wgpu::Extent3d {
                width: self.width,
                height: self.height,
                depth_or_array_layers: 1,
            },
        );
        gpu.queue().write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &self.uv_texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            &self.uv_scratch,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(uv_w * 2),
                rows_per_image: Some(uv_h),
            },
            wgpu::Extent3d {
                width: uv_w,
                height: uv_h,
                depth_or_array_layers: 1,
            },
        );
    }
}

/// Interleave planar YUV420P chroma (`u`, `v`: separate half-resolution
/// planes) into NV12 chroma (`out`: one half-resolution plane, `u,v`
/// pairs per texel) - see [`NvUploadPool`]'s doc comment for why this
/// conversion is needed at all. `out` must already be sized
/// `u.len() + v.len()` bytes (2 bytes per chroma sample); callers reuse
/// the same buffer across frames rather than reallocating.
///
/// `u`/`v` can be shorter than the pool's nominal chroma plane size on
/// an odd-dimensioned source frame (the decoder still rounds chroma
/// planes down); any tail `out` bytes beyond `2 * min(u.len(), v.len())`
/// are left at their previous value rather than indexed out of bounds.
/// De-interleave NV12 chroma (`uv`: one half-resolution plane, `u,v`
/// pairs per texel) back into planar YUV420P (`u`, `v`: separate
/// half-resolution planes) - the exact inverse of
/// [`yuv420p_to_interleaved_uv`].
///
/// Needed because the D3D11VA decode path produces NV12 (what the
/// hardware decoder and the GPU detector both want) while this module's
/// drawing and encoding code works on planar YUV420P. Any tail bytes
/// beyond the shortest of the three buffers are left untouched rather
/// than indexed out of bounds, matching the forward function.
fn interleaved_uv_to_yuv420p(uv: &[u8], u: &mut [u8], v: &mut [u8]) {
    let n = u.len().min(v.len()).min(uv.len() / 2);
    for i in 0..n {
        u[i] = uv[2 * i];
        v[i] = uv[2 * i + 1];
    }
}

fn yuv420p_to_interleaved_uv(u: &[u8], v: &[u8], out: &mut [u8]) {
    let n = u.len().min(v.len()).min(out.len() / 2);
    for i in 0..n {
        out[2 * i] = u[i];
        out[2 * i + 1] = v[i];
    }
}

/// Convert an sRGB-ish `BoxColor` to BT.601 full-range YUV (matches
/// `dump_detection_frames.rs`'s own YUV<->RGB convention, so the two
/// tools' colors read the same by eye even though this module draws
/// directly on YUV planes instead of an intermediate RGBA buffer).
fn color_to_yuv(c: BoxColor) -> (u8, u8, u8) {
    let (r, g, b) = (c.r as f32, c.g as f32, c.b as f32);
    let y = 0.299 * r + 0.587 * g + 0.114 * b;
    let u = -0.168736 * r - 0.331264 * g + 0.5 * b + 128.0;
    let v = 0.5 * r - 0.418688 * g - 0.081312 * b + 128.0;
    (
        y.clamp(0.0, 255.0) as u8,
        u.clamp(0.0, 255.0) as u8,
        v.clamp(0.0, 255.0) as u8,
    )
}

/// Draw one detection's bounding box directly on a decoded YUV420P
/// frame: a thick rectangle outline, color-coded by class (see
/// [`class_color`]). Coordinates are normalized camera-space `[0,1]`,
/// the same convention `Detection::center_x/center_y/width/height`
/// document - multiply by the frame's own pixel dimensions to place
/// them, exactly as `dump_detection_frames.rs`'s `draw_detection` does
/// against its RGBA buffer.
fn draw_detection(frame: &mut YuvFrame, d: &DrawDet, ball_class_id: Option<u16>) {
    let (w, h) = (frame.width as f32, frame.height as f32);
    let (cx, cy) = (d.center_x * w, d.center_y * h);
    let (bw, bh) = (d.width * w, d.height * h);
    let x0 = (cx - bw / 2.0).max(0.0) as i32;
    let y0 = (cy - bh / 2.0).max(0.0) as i32;
    let x1 = (cx + bw / 2.0).min(w - 1.0) as i32;
    let y1 = (cy + bh / 2.0).min(h - 1.0) as i32;
    let color = class_color(d.class_id, ball_class_id);

    // A tiny box (small/distant ball) can be a few pixels - invisible
    // as a 1px outline. A thick outline stays findable at a glance,
    // matching `dump_detection_frames.rs::draw_detection`'s reasoning.
    let thickness = 3i32.min(((bw.min(bh) as i32) / 2).max(1));
    for t in 0..thickness {
        draw_rect_outline_yuv(frame, x0 - t, y0 - t, x1 + t, y1 + t, color);
    }
    let _ = d.confidence; // logged by the caller, not drawn as text here
}

fn draw_rect_outline_yuv(
    frame: &mut YuvFrame,
    x0: i32,
    y0: i32,
    x1: i32,
    y1: i32,
    color: BoxColor,
) {
    draw_line_yuv(frame, x0, y0, x1, y0, color);
    draw_line_yuv(frame, x0, y1, x1, y1, color);
    draw_line_yuv(frame, x0, y0, x0, y1, color);
    draw_line_yuv(frame, x1, y0, x1, y1, color);
}

/// Draw the field ROI polygon as a closed, thick outline (yellow) -
/// same role as `dump_detection_frames.rs::draw_polygon`: a ball the
/// model found but that never shows up in a real export's
/// `detections_raw` (`RoiFilteredDetector` drops it before the tracker
/// ever sees it) is exactly what an ROI that's too tight looks like
/// against this outline.
fn draw_polygon(frame: &mut YuvFrame, points: &[[f64; 2]], color: BoxColor) {
    if points.len() < 2 {
        return;
    }
    let (w, h) = (frame.width as f32, frame.height as f32);
    let px = |p: &[f64; 2]| ((p[0] as f32 * w) as i32, (p[1] as f32 * h) as i32);
    let thickness = 2i32;
    for i in 0..points.len() {
        let (x0, y0) = px(&points[i]);
        let (x1, y1) = px(&points[(i + 1) % points.len()]);
        for t in -thickness..=thickness {
            draw_line_yuv(frame, x0, y0 + t, x1, y1 + t, color);
            draw_line_yuv(frame, x0 + t, y0, x1 + t, y1, color);
        }
    }
}

/// Plot one pixel's Y (and, on even chroma-sample columns/rows, U/V)
/// to `color`. YUV420P chroma is shared by a 2x2 luma block, so a
/// single-pixel-wide line only gets continuous color coverage on even
/// coordinates - acceptable for a thick (3px+) diagnostic outline
/// where every other line in the stack already covers the gap.
fn put_pixel_yuv(frame: &mut YuvFrame, x: i32, y: i32, color: BoxColor) {
    let (w, h) = (frame.width as i32, frame.height as i32);
    if x < 0 || y < 0 || x >= w || y >= h {
        return;
    }
    let (yv, uv, vv) = color_to_yuv(color);
    let (x, y) = (x as usize, y as usize);
    let wu = frame.width as usize;
    frame.y[y * wu + x] = yv;
    let cw = wu / 2;
    let (cx, cy) = (x / 2, y / 2);
    let idx = cy * cw + cx;
    if let Some(u) = frame.u.get_mut(idx) {
        *u = uv;
    }
    if let Some(v) = frame.v.get_mut(idx) {
        *v = vv;
    }
}

/// Simple axis-aligned line (all callers draw horizontal/vertical
/// segments only) - mirrors `dump_detection_frames.rs::draw_line`'s
/// Bresenham-ish approach, adapted to write YUV planes instead of an
/// RGBA image.
fn draw_line_yuv(frame: &mut YuvFrame, x0: i32, y0: i32, x1: i32, y1: i32, color: BoxColor) {
    let steps = (x1 - x0).abs().max((y1 - y0).abs()).max(1);
    for i in 0..=steps {
        let x = x0 + (x1 - x0) * i / steps;
        let y = y0 + (y1 - y0) * i / steps;
        put_pixel_yuv(frame, x, y, color);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn solid(w: u32, h: u32, y: u8) -> YuvFrame {
        YuvFrame {
            y: vec![y; (w * h) as usize],
            u: vec![128; ((w / 2) * (h / 2)) as usize],
            v: vec![128; ((w / 2) * (h / 2)) as usize],
            width: w,
            height: h,
            timestamp_us: 0,
        }
    }

    #[test]
    fn class_color_matches_dump_detection_frames_convention() {
        assert_eq!(class_color(0, Some(1)), COLOR_PERSON);
        assert_eq!(class_color(1, Some(1)), COLOR_BALL);
        assert_eq!(class_color(2, Some(1)), COLOR_REFEREE);
        assert_eq!(class_color(3, Some(1)), COLOR_OTHER);
    }

    #[test]
    fn class_color_falls_back_to_other_without_resolved_ball_id() {
        // class_id 1 isn't specially colored unless the caller resolved
        // the model's real ball class id - avoids assuming COCO's
        // ordering (see this module's doc comment).
        assert_eq!(class_color(1, None), COLOR_OTHER);
    }

    #[test]
    fn camera_selection_both_includes_either_camera() {
        assert!(CameraSelection::Both.includes(CameraId::Left));
        assert!(CameraSelection::Both.includes(CameraId::Right));
    }

    #[test]
    fn camera_selection_left_excludes_right() {
        assert!(CameraSelection::Left.includes(CameraId::Left));
        assert!(!CameraSelection::Left.includes(CameraId::Right));
    }

    #[test]
    fn camera_selection_right_excludes_left() {
        assert!(CameraSelection::Right.includes(CameraId::Right));
        assert!(!CameraSelection::Right.includes(CameraId::Left));
    }

    #[test]
    fn draw_detection_paints_visible_box_pixels() {
        let mut frame = solid(64, 64, 16);
        let d = DrawDet {
            class_id: 1,
            confidence: 0.9,
            center_x: 0.5,
            center_y: 0.5,
            width: 0.25,
            height: 0.25,
        };
        draw_detection(&mut frame, &d, Some(1));
        // Ball color's Y should differ from the flat background fill
        // somewhere in the frame (the box outline was actually drawn).
        assert!(frame.y.iter().any(|&y| y != 16));
    }

    #[test]
    fn draw_polygon_paints_outline_without_panicking_on_two_points() {
        let mut frame = solid(64, 64, 16);
        draw_polygon(&mut frame, &[[0.1, 0.1], [0.9, 0.9]], COLOR_ROI);
        assert!(frame.y.iter().any(|&y| y != 16));
    }

    #[test]
    fn draw_polygon_is_a_no_op_for_fewer_than_two_points() {
        let mut frame = solid(64, 64, 16);
        draw_polygon(&mut frame, &[[0.5, 0.5]], COLOR_ROI);
        assert!(frame.y.iter().all(|&y| y == 16));
    }

    #[test]
    fn put_pixel_yuv_ignores_out_of_bounds_coordinates() {
        let mut frame = solid(4, 4, 16);
        // Must not panic or index out of range.
        put_pixel_yuv(&mut frame, -1, -1, COLOR_BALL);
        put_pixel_yuv(&mut frame, 100, 100, COLOR_BALL);
        assert!(frame.y.iter().all(|&y| y == 16));
    }

    /// The interval gate itself: `frames_done` is the pre-increment
    /// count of frames already emitted for this camera, so frame 0
    /// always detects and every Nth after it does too. Guards the two
    /// things that would silently break the diagnostic: a `0` interval
    /// (must not divide by zero / must behave as "every frame"), and
    /// frame 0 never being skipped (a run whose very first frame had no
    /// detection would draw nothing at all until frame N).
    #[test]
    fn detection_interval_gate_selects_the_right_frames() {
        let detects = |interval: u32, frames: u64| -> Vec<u64> {
            let every = interval.max(1) as u64;
            (0..frames).filter(|f| f.is_multiple_of(every)).collect()
        };
        assert_eq!(detects(1, 6), vec![0, 1, 2, 3, 4, 5], "1 = every frame");
        assert_eq!(detects(3, 10), vec![0, 3, 6, 9]);
        assert_eq!(detects(10, 25), vec![0, 10, 20]);
        // 0 must be treated as 1, not panic on a modulo-by-zero.
        assert_eq!(detects(0, 4), vec![0, 1, 2, 3], "0 degrades to every frame");
        // Frame 0 always detects, whatever the interval.
        for interval in [1u32, 2, 3, 5, 10, 30] {
            assert_eq!(detects(interval, 1), vec![0], "interval {interval}");
        }
    }

    #[test]
    fn yuv420p_to_interleaved_uv_pairs_u_and_v_samples() {
        let u = vec![10, 20, 30];
        let v = vec![110, 120, 130];
        let mut out = vec![0u8; 6];
        yuv420p_to_interleaved_uv(&u, &v, &mut out);
        assert_eq!(out, vec![10, 110, 20, 120, 30, 130]);
    }

    /// The GPU decode path converts NV12 -> YUV420P on the way back from
    /// the hardware decoder, so this must be an exact inverse of the
    /// upload-side interleave or every GPU-decoded frame's colors shift.
    #[test]
    fn interleaved_uv_to_yuv420p_is_the_inverse_of_the_interleave() {
        let u_in = vec![10u8, 20, 30, 40];
        let v_in = vec![110u8, 120, 130, 140];
        let mut interleaved = vec![0u8; 8];
        yuv420p_to_interleaved_uv(&u_in, &v_in, &mut interleaved);

        let mut u_out = vec![0u8; 4];
        let mut v_out = vec![0u8; 4];
        interleaved_uv_to_yuv420p(&interleaved, &mut u_out, &mut v_out);
        assert_eq!(u_out, u_in);
        assert_eq!(v_out, v_in);
    }

    #[test]
    fn interleaved_uv_to_yuv420p_stops_at_the_shortest_buffer() {
        // Mismatched sizes must not panic or index out of range - same
        // fail-soft contract as the forward direction.
        let uv = vec![1u8, 9, 2, 8];
        let mut u_out = vec![0xAAu8; 4];
        let mut v_out = vec![0xBBu8; 4];
        interleaved_uv_to_yuv420p(&uv, &mut u_out, &mut v_out);
        assert_eq!(u_out, vec![1, 2, 0xAA, 0xAA]);
        assert_eq!(v_out, vec![9, 8, 0xBB, 0xBB]);
    }

    #[test]
    fn interleaved_uv_to_yuv420p_handles_empty_input() {
        let mut u_out: Vec<u8> = Vec::new();
        let mut v_out: Vec<u8> = Vec::new();
        interleaved_uv_to_yuv420p(&[], &mut u_out, &mut v_out);
        assert!(u_out.is_empty() && v_out.is_empty());
    }

    #[test]
    fn yuv420p_to_interleaved_uv_handles_empty_planes() {
        let mut out = vec![0u8; 0];
        // Must not panic on empty input (e.g. a degenerate 0-height frame).
        yuv420p_to_interleaved_uv(&[], &[], &mut out);
        assert!(out.is_empty());
    }

    #[test]
    fn yuv420p_to_interleaved_uv_stops_at_the_shortest_input() {
        // Mismatched plane lengths (shouldn't happen from a real
        // decoder, but must not panic/index out of range) - only the
        // overlapping prefix is written, matching this function's own
        // doc comment.
        let u = vec![1, 2, 3];
        let v = vec![9, 8];
        let mut out = vec![0xAAu8; 6];
        yuv420p_to_interleaved_uv(&u, &v, &mut out);
        assert_eq!(out, vec![1, 9, 2, 8, 0xAA, 0xAA]);
    }

    #[test]
    fn nv_upload_pool_rejects_zero_sized_frames() {
        // No real GPU needed for this branch - `NvUploadPool::new`
        // validates dimensions before touching the device.
        let Ok(gpu) = GpuContext::new_blocking() else {
            eprintln!("Skipping GPU test: no adapter available");
            return;
        };
        assert!(NvUploadPool::new(&gpu, 0, 480).is_err());
        assert!(NvUploadPool::new(&gpu, 640, 0).is_err());
    }
}
