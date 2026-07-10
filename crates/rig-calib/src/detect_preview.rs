//! Live AKAZE detection preview.
//!
//! Runs feature detection on the *current* displayed frame with whatever
//! AKAZE tuning sliders are set to right now, so adjusting threshold /
//! detect-Y-band / full-res in the "Auto-calibrate tuning" panel shows an
//! immediate visual result instead of only taking effect on the next full
//! Auto-Calibrate run.
//!
//! Unlike [`crate::calibration::spawn_auto_calibrate`], which spins up a
//! fresh `GpuContext` per run, this worker creates its GPU context and
//! per-resolution `GpuUndistort` pipelines once and reuses them for the
//! life of the app - it fires on every slider tweak and has to stay cheap.
//!
//! A single background thread pulls from a one-slot "latest request wins"
//! mailbox: submitting a new request while the previous one is still being
//! processed silently replaces it, so a fast slider drag collapses into
//! just the final value instead of queueing every intermediate frame.

use std::sync::{Arc, Condvar, Mutex};

use reco_calibrate::CalibrationConfig;
use reco_calibrate::preview::DetectionPreview;
use reco_core::calibration::CameraParams;
use reco_core::gpu::GpuContext;
use reco_core::lens::undistort::GpuUndistort;

/// One AKAZE detection-preview request: the currently displayed YUV frame
/// pair plus the lens params and AKAZE settings to detect with.
pub struct PreviewRequest {
    pub left_y: Vec<u8>,
    pub left_u: Vec<u8>,
    pub left_v: Vec<u8>,
    pub right_y: Vec<u8>,
    pub right_u: Vec<u8>,
    pub right_v: Vec<u8>,
    pub width: u32,
    pub height: u32,
    pub left_params: CameraParams,
    pub right_params: CameraParams,
    pub config: CalibrationConfig,
}

struct Mailbox {
    request: Option<PreviewRequest>,
    shutdown: bool,
}

/// Handle to the background detection-preview worker. Dropping it stops
/// the worker thread.
pub struct DetectPreviewWorker {
    state: Arc<(Mutex<Mailbox>, Condvar)>,
}

impl DetectPreviewWorker {
    /// Spawn the worker thread. `on_result` is called from the background
    /// thread with the rendered preview for the most recent request - it
    /// must marshal onto the Slint event loop itself before touching UI
    /// state (same convention as `calibration::spawn_auto_calibrate`).
    pub fn spawn(on_result: impl Fn(DetectionPreview) + Send + 'static) -> Self {
        let state = Arc::new((
            Mutex::new(Mailbox {
                request: None,
                shutdown: false,
            }),
            Condvar::new(),
        ));
        let state_bg = Arc::clone(&state);

        std::thread::spawn(move || {
            let gpu = match GpuContext::new_blocking() {
                Ok(gpu) => gpu,
                Err(e) => {
                    log::error!("AKAZE preview worker: GPU init failed: {e}");
                    return;
                }
            };

            // Cached per-resolution undistort pipelines - rebuilt only
            // when the loaded footage's resolution changes.
            let mut left_undistort: Option<(u32, u32, GpuUndistort)> = None;
            let mut right_undistort: Option<(u32, u32, GpuUndistort)> = None;

            loop {
                let req = {
                    let (lock, cvar) = &*state_bg;
                    let mut mailbox = lock.lock().unwrap();
                    while mailbox.request.is_none() && !mailbox.shutdown {
                        mailbox = cvar.wait(mailbox).unwrap();
                    }
                    if mailbox.shutdown {
                        return;
                    }
                    mailbox.request.take().unwrap()
                };

                let left = ensure_undistort(&gpu, &mut left_undistort, req.width, req.height);
                let left_rgba = left.undistort(
                    &gpu,
                    &req.left_y,
                    &req.left_u,
                    &req.left_v,
                    &req.left_params,
                );
                let right = ensure_undistort(&gpu, &mut right_undistort, req.width, req.height);
                let right_rgba = right.undistort(
                    &gpu,
                    &req.right_y,
                    &req.right_u,
                    &req.right_v,
                    &req.right_params,
                );

                let preview = reco_calibrate::preview_akaze_detection(
                    &left_rgba,
                    req.width,
                    req.height,
                    &right_rgba,
                    req.width,
                    req.height,
                    &req.config,
                );
                on_result(preview);
            }
        });

        Self { state }
    }

    /// Submit a new preview request, replacing any request still waiting
    /// to be picked up by the worker.
    pub fn request(&self, req: PreviewRequest) {
        let (lock, cvar) = &*self.state;
        let mut mailbox = lock.lock().unwrap();
        mailbox.request = Some(req);
        cvar.notify_one();
    }
}

impl Drop for DetectPreviewWorker {
    fn drop(&mut self) {
        let (lock, cvar) = &*self.state;
        let mut mailbox = lock.lock().unwrap();
        mailbox.shutdown = true;
        cvar.notify_one();
    }
}

fn ensure_undistort<'a>(
    gpu: &GpuContext,
    cached: &'a mut Option<(u32, u32, GpuUndistort)>,
    width: u32,
    height: u32,
) -> &'a GpuUndistort {
    let needs_rebuild = !matches!(cached, Some((w, h, _)) if *w == width && *h == height);
    if needs_rebuild {
        let aspect = width as f32 / height as f32;
        *cached = Some((width, height, GpuUndistort::new(gpu, width, height, aspect)));
    }
    &cached.as_ref().unwrap().2
}
