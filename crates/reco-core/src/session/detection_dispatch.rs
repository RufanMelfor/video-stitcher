//! Detection entry points for [`StitchSession`].
//!
//! Each `detect_and_update_director_*` variant wraps its residency's
//! handles into per-camera [`DetectorFrame`]s and hands them to the
//! engine ([`StitchCore::run_detection_frames`]), which owns the
//! detector, the schedule, panorama mapping, and the tracker/panner
//! chain. The session-side job is purely "wrap residency handles into
//! frames"; that machinery relocates onto the GPU executor with the
//! 9B-ii residency migration.

use super::StitchSession;
use crate::core::StitchCore;
use crate::detect::detector::DetectorFrame;
use crate::geometry::CameraId;
use crate::session::types::SessionError;
use crate::source::StereoFrame;

/// Per-camera CPU frames for a CPU-resident stereo pair; `None` for
/// GPU-resident variants (their arms build residency-specific frames).
fn cpu_frames<'a>(
    frame: &'a StereoFrame,
    width: u32,
    height: u32,
) -> Option<[(CameraId, DetectorFrame<'a>); 2]> {
    use crate::detect::detector::{ChromaFormat, RawFrame};
    match frame {
        StereoFrame::Yuv420p(pair) => Some([
            (
                CameraId::Left,
                DetectorFrame::Cpu(RawFrame {
                    y: &pair.left.y,
                    chroma: ChromaFormat::Yuv420p {
                        u: &pair.left.u,
                        v: &pair.left.v,
                    },
                    width,
                    height,
                }),
            ),
            (
                CameraId::Right,
                DetectorFrame::Cpu(RawFrame {
                    y: &pair.right.y,
                    chroma: ChromaFormat::Yuv420p {
                        u: &pair.right.u,
                        v: &pair.right.v,
                    },
                    width,
                    height,
                }),
            ),
        ]),
        StereoFrame::Nv12(pair) => Some([
            (
                CameraId::Left,
                DetectorFrame::Cpu(RawFrame {
                    y: &pair.left.y,
                    chroma: ChromaFormat::Nv12 { uv: &pair.left.uv },
                    width,
                    height,
                }),
            ),
            (
                CameraId::Right,
                DetectorFrame::Cpu(RawFrame {
                    y: &pair.right.y,
                    chroma: ChromaFormat::Nv12 { uv: &pair.right.uv },
                    width,
                    height,
                }),
            ),
        ]),
        _ => None,
    }
}

/// Per-camera CUDA NV12 frames from the shared-texture slot pointers.
///
/// Only called from the Linux CUDA zero-copy path
/// (`detect_and_update_director_gpu`, `detect_and_track_only`'s
/// `StereoFrame::GpuResident` arm) - Windows uses the D3D11VA/wgpu path
/// (`wgpu_nv12_frames`) instead, so this must stay Linux-only or it's
/// dead code there.
#[cfg(target_os = "linux")]
pub(super) fn cuda_nv12_frames(
    left_buf: &crate::interop::zero_copy::GpuBufInfo,
    right_buf: &crate::interop::zero_copy::GpuBufInfo,
    left_slot: u8,
    right_slot: u8,
    left_rotation: i32,
    right_rotation: i32,
) -> [(CameraId, DetectorFrame<'static>); 2] {
    use crate::detect::detector::GpuNv12Frame;
    let ls = left_slot as usize;
    let rs = right_slot as usize;
    let is_10bit = left_buf.pixel_format == crate::render::renderer::GpuPixelFormat::P010;
    [
        (
            CameraId::Left,
            DetectorFrame::Cuda(GpuNv12Frame {
                y_ptr: left_buf.y_ptr[ls],
                uv_ptr: left_buf.uv_ptr[ls],
                y_pitch: left_buf.y_pitch[ls],
                uv_pitch: left_buf.uv_pitch[ls],
                width: left_buf.width,
                height: left_buf.height,
                rotation: left_rotation,
                is_10bit,
            }),
        ),
        (
            CameraId::Right,
            DetectorFrame::Cuda(GpuNv12Frame {
                y_ptr: right_buf.y_ptr[rs],
                uv_ptr: right_buf.uv_ptr[rs],
                y_pitch: right_buf.y_pitch[rs],
                uv_pitch: right_buf.uv_pitch[rs],
                width: right_buf.width,
                height: right_buf.height,
                rotation: right_rotation,
                is_10bit,
            }),
        ),
    ]
}

/// Per-camera wgpu NV12 texture-view frames (shared decode textures /
/// D3D11 staging views).
#[cfg(any(target_os = "linux", target_os = "windows"))]
#[allow(clippy::too_many_arguments)]
pub(super) fn wgpu_nv12_frames<'a>(
    left_y: &'a wgpu::TextureView,
    left_uv: &'a wgpu::TextureView,
    right_y: &'a wgpu::TextureView,
    right_uv: &'a wgpu::TextureView,
    width: u32,
    height: u32,
    left_rotation: i32,
    right_rotation: i32,
) -> [(CameraId, DetectorFrame<'a>); 2] {
    [
        (
            CameraId::Left,
            DetectorFrame::WgpuNv12 {
                y_view: left_y,
                uv_view: left_uv,
                width,
                height,
                rotation: left_rotation,
            },
        ),
        (
            CameraId::Right,
            DetectorFrame::WgpuNv12 {
                y_view: right_y,
                uv_view: right_uv,
                width,
                height,
                rotation: right_rotation,
            },
        ),
    ]
}

/// Per-camera Metal CVPixelBuffer frames (backends import natively).
#[cfg(any(target_os = "macos", target_os = "ios"))]
pub(super) fn metal_frames(
    left_cvpb: crate::interop::metal::CVPixelBufferRef,
    right_cvpb: crate::interop::metal::CVPixelBufferRef,
    width: u32,
    height: u32,
) -> [(CameraId, DetectorFrame<'static>); 2] {
    [
        (
            CameraId::Left,
            DetectorFrame::Metal {
                cv_pixel_buffer: left_cvpb,
                width,
                height,
            },
        ),
        (
            CameraId::Right,
            DetectorFrame::Metal {
                cv_pixel_buffer: right_cvpb,
                width,
                height,
            },
        ),
    ]
}

impl StitchSession {
    /// Shared detection skeleton: gate by the engine's schedule, let
    /// the closure feed the engine's detector, then drive the
    /// tracker/panner chain.
    ///
    /// Every `detect_and_update_director_*` variant is a one-liner
    /// wrapper that passes a closure here. Adding a new detection
    /// backend means writing one closure, not copying 15 lines.
    fn detect_and_update_director_with(
        &mut self,
        elapsed: std::time::Duration,
        detect_fn: impl FnOnce(&mut StitchCore),
    ) -> Result<(), SessionError> {
        let due = self.core.detection_due(self.frame_count);
        if due {
            detect_fn(&mut self.core);
        }
        self.fire_sink_and_update_director(elapsed, due)
    }

    /// Run detection + trackers only (no panner). For the lookahead
    /// produce phase where we want the WorldState but don't want to
    /// advance the panner.
    ///
    /// Returns [`PendingWorldState::Pending`] instead of resolving the
    /// tracker immediately when [`enable_async_detect`](Self::enable_async_detect)
    /// is active and the detector actually deferred its inference call
    /// (see [`Self::run_detection_frames_maybe_async`]) - the caller
    /// resolves it later, right before the frame is consumed, via
    /// [`Self::resolve_pending_world_state`].
    pub(crate) fn detect_and_track_only(
        &mut self,
        frame: &StereoFrame,
        elapsed: std::time::Duration,
        produce_index: u64,
    ) -> Result<super::frame_buffer::PendingWorldState, SessionError> {
        use super::frame_buffer::PendingWorldState;

        let mut went_async = false;
        if self.core.detection_due(produce_index) {
            match frame {
                #[cfg(target_os = "linux")]
                StereoFrame::GpuResident {
                    left_slot,
                    right_slot,
                } => {
                    if self.core.detector_needs_cuda_frames() {
                        if let Some((ref left_buf, ref right_buf)) = self.gpu_buf_info {
                            crate::profile_scope!("gpu_detect_total");
                            let frames = cuda_nv12_frames(
                                left_buf,
                                right_buf,
                                *left_slot,
                                *right_slot,
                                self.left_rotation,
                                self.right_rotation,
                            );
                            went_async = Self::run_detection_frames_maybe_async(
                                &mut self.core,
                                self.async_detect.as_ref(),
                                &mut self.pending_finishers,
                                &frames,
                                produce_index,
                            );
                        }
                    } else if let Some(ref views) = self.gpu_shared_views {
                        crate::profile_scope!("detect_wgpu_nv12");
                        let ls = *left_slot as usize;
                        let rs = *right_slot as usize;
                        let (w, h) = self.core.source_info();
                        let frames = wgpu_nv12_frames(
                            &views[ls * 2],
                            &views[ls * 2 + 1],
                            &views[4 + rs * 2],
                            &views[4 + rs * 2 + 1],
                            w,
                            h,
                            self.left_rotation,
                            self.right_rotation,
                        );
                        went_async = Self::run_detection_frames_maybe_async(
                            &mut self.core,
                            self.async_detect.as_ref(),
                            &mut self.pending_finishers,
                            &frames,
                            produce_index,
                        );
                    }
                }
                #[cfg(target_os = "windows")]
                StereoFrame::D3d11Resident { .. } => {
                    if let Some(ref pool) = self.d3d11_staging_pool {
                        crate::profile_scope!("detect_wgpu_nv12");
                        let left_slot = (produce_index as usize * 2) % pool.n_slots();
                        let right_slot = (produce_index as usize * 2 + 1) % pool.n_slots();
                        let (w, h) = self.core.source_info();
                        let frames = wgpu_nv12_frames(
                            pool.y_view(left_slot),
                            pool.uv_view(left_slot),
                            pool.y_view(right_slot),
                            pool.uv_view(right_slot),
                            w,
                            h,
                            self.left_rotation,
                            self.right_rotation,
                        );
                        went_async = Self::run_detection_frames_maybe_async(
                            &mut self.core,
                            self.async_detect.as_ref(),
                            &mut self.pending_finishers,
                            &frames,
                            produce_index,
                        );
                    }
                }
                #[cfg(any(target_os = "macos", target_os = "ios"))]
                StereoFrame::MetalResident { left, right } => {
                    let frames =
                        metal_frames(left.as_ptr(), right.as_ptr(), left.width(), left.height());
                    self.core.run_detection_frames(&frames);
                }
                #[cfg(target_os = "linux")]
                StereoFrame::NvmmResident { left, right } => {
                    crate::profile_scope!("detect_preletterboxed_total");
                    if let Some(frames) = self.nvmm_detector_frames(left, right) {
                        self.core.run_detection_frames(&frames);
                    }
                }
                _ => {
                    let (w, h) = self.core.source_info();
                    if let Some(frames) = cpu_frames(frame, w, h) {
                        self.core.run_detection_frames(&frames);
                    }
                }
            }
        }

        if went_async {
            return Ok(PendingWorldState::Pending(produce_index));
        }
        Ok(PendingWorldState::Ready(
            self.core
                .track_only(produce_index, elapsed.as_secs_f64() * 1000.0),
        ))
    }

    /// Try to run `frames` through the detector's [`DetectSplit`]-aware
    /// entry point instead of the always-synchronous
    /// [`StitchCore::run_detection_frames`]. Returns `true` when the
    /// call was deferred to `self.async_detect` (caller must resolve
    /// it later via [`Self::resolve_pending_world_state`] before the
    /// tracker can see this produce index's detections), `false` when
    /// it resolved synchronously and `self.core.last_detections` is
    /// already up to date exactly like [`StitchCore::run_detection_frames`]
    /// would have left it.
    ///
    /// Falls back to the plain synchronous path whenever async detect
    /// isn't enabled at all - zero behavior change unless a caller
    /// opted in via [`Self::enable_async_detect`].
    ///
    /// Takes explicit disjoint borrows (`core`/`async_detect`/
    /// `pending_finishers`) instead of `&mut self` so callers already
    /// holding a borrow of some *other* session field (e.g. the D3D11
    /// staging pool, whose texture views `frames` borrows from) can
    /// still call this - a plain `&mut self` method here would conflict
    /// with that unrelated borrow even though the two never touch the
    /// same fields.
    fn run_detection_frames_maybe_async(
        core: &mut crate::core::StitchCore,
        async_detect: Option<&crate::async_detect::AsyncDetectThread>,
        pending_finishers: &mut std::collections::HashMap<u64, crate::detect::detector::FinishFn>,
        frames: &[(CameraId, DetectorFrame<'_>); 2],
        produce_index: u64,
    ) -> bool {
        use crate::detect::detector::{DetectSplit, DetectorError, FinishFn};

        let Some(async_detect) = async_detect else {
            core.run_detection_frames(frames);
            return false;
        };
        let Some(ref mut detector) = core.detector else {
            return false;
        };

        let mut pending_jobs = Vec::new();
        let mut done_dets = Vec::new();
        let mut finish_fn: Option<FinishFn> = None;
        for (camera, frame) in frames.iter() {
            match detector.detect_split(*camera, frame) {
                Ok(DetectSplit::Done(dets)) => done_dets.extend(dets),
                Ok(DetectSplit::Pending { job, finish }) => {
                    pending_jobs.push(job);
                    finish_fn = Some(finish);
                }
                Err(DetectorError::UnsupportedFrameKind) => log::debug!(
                    "StitchSession detector '{}' does not support this frame residency ({camera:?})",
                    detector.name()
                ),
                Err(e) => log::warn!(
                    "StitchSession detector '{}' {camera:?}: {e}",
                    detector.name()
                ),
            }
        }

        let Some(mut finish) = finish_fn else {
            // Every camera resolved synchronously (e.g. a frame kind
            // this detector doesn't add async support for) - same
            // outcome as the plain synchronous path.
            core.set_detections_from_raw(done_dets);
            return false;
        };
        if !done_dets.is_empty() {
            // Rare mixed case (one camera split, the other didn't) -
            // fold the already-available detections into what the
            // composed finish ultimately produces so nothing is lost.
            let extra = done_dets;
            let prior = finish;
            finish = Box::new(move |mut dets| {
                dets.extend(extra);
                prior(dets)
            });
        }
        async_detect.submit(produce_index, pending_jobs);
        pending_finishers.insert(produce_index, finish);
        true
    }

    /// Resolve a produce index's still-[`Pending`](super::frame_buffer::PendingWorldState::Pending)
    /// detection: block for the async worker's result (FIFO submission
    /// order guarantees it's this exact index - see
    /// [`crate::async_detect`]'s ordering guarantee), apply the
    /// `finish` closure stashed by
    /// [`Self::run_detection_frames_maybe_async`], map to panorama
    /// coordinates, feed the tracker, and return the resulting
    /// `WorldState` + panorama-mapped detections. Called from
    /// `run_loop::run_panner_once` right before a frame is consumed -
    /// never while it's still just sitting in the lookahead buffer.
    pub(crate) fn resolve_pending_world_state(
        &mut self,
        produce_index: u64,
        timestamp_ms: f64,
    ) -> (
        crate::detect::tracker::WorldState,
        Vec<crate::detect::director::MappedDetection>,
    ) {
        let raw = match self.async_detect.as_ref().and_then(|a| a.recv()) {
            Some(result) => {
                debug_assert_eq!(
                    result.produce_index, produce_index,
                    "async detect result out of order - FIFO guarantee violated"
                );
                result.detections
            }
            None => Vec::new(), // worker unavailable/died - degrade to no detections
        };
        let finished = match self.pending_finishers.remove(&produce_index) {
            Some(finish) => finish(raw),
            None => raw,
        };
        self.core.set_detections_from_raw(finished);
        let world_state = self.core.track_only(produce_index, timestamp_ms);
        (world_state, self.core.last_detections().to_vec())
    }

    /// Allocate the NVMM detection surfaces for the Jetson zero-copy path.
    ///
    /// Call once before [`run`](Self::run) when feeding a
    /// `StereoFrame::NvmmResident` source (mirrors
    /// [`setup_gpu_source`](Self::setup_gpu_source) for the desktop GPU
    /// path). `model_size` is the detector's square input dimension (e.g.
    /// 1280); `src_width`/`src_height` are the capture resolution, used to
    /// compute the letterbox geometry. Without this the NVMM detection arm
    /// no-ops (the director still advances, just without detections).
    #[cfg(target_os = "linux")]
    pub fn setup_nvmm_detection(
        &mut self,
        model_size: u32,
        src_width: u32,
        src_height: u32,
    ) -> Result<(), SessionError> {
        let left =
            crate::nvbuf_transform::NvBufDetectionSurface::new(model_size, src_width, src_height)
                .map_err(|e| SessionError::ZeroCopy(format!("NVMM left detection surface: {e}")))?;
        let right =
            crate::nvbuf_transform::NvBufDetectionSurface::new(model_size, src_width, src_height)
                .map_err(|e| SessionError::ZeroCopy(format!("NVMM right detection surface: {e}")))?;
        self.nvmm_det_left = Some(left);
        self.nvmm_det_right = Some(right);
        log::info!(
            "NVMM detection surfaces ready: {model_size}x{model_size} (src {src_width}x{src_height})"
        );
        Ok(())
    }

    /// Letterbox a stereo NVMM frame into the pre-allocated CUDA
    /// detection surfaces (set up by
    /// [`setup_nvmm_detection`](Self::setup_nvmm_detection)) and wrap
    /// the results as per-camera
    /// [`DetectorFrame::CudaRgbaLetterboxed`]. Returns `None` when the
    /// surfaces are not set up or a transform fails (logged). Shared by
    /// the buffered produce arm and the immediate-render detect arm.
    #[cfg(target_os = "linux")]
    pub(crate) fn nvmm_detector_frames(
        &mut self,
        left: &crate::source::NvmmPlaneInfo,
        right: &crate::source::NvmmPlaneInfo,
    ) -> Option<[(CameraId, DetectorFrame<'static>); 2]> {
        let (Some(det_left), Some(det_right)) =
            (self.nvmm_det_left.as_mut(), self.nvmm_det_right.as_mut())
        else {
            return None;
        };
        unsafe {
            if let Err(e) = det_left.transform_from_nvmm(left.surface_ptr) {
                log::warn!("NVMM left detection transform failed: {e}");
                return None;
            }
            if let Err(e) = det_right.transform_from_nvmm(right.surface_ptr) {
                log::warn!("NVMM right detection transform failed: {e}");
                return None;
            }
        }
        Some([
            (
                CameraId::Left,
                DetectorFrame::CudaRgbaLetterboxed {
                    ptr: det_left.data_ptr,
                    src_width: left.width,
                    src_height: left.height,
                },
            ),
            (
                CameraId::Right,
                DetectorFrame::CudaRgbaLetterboxed {
                    ptr: det_right.data_ptr,
                    src_width: right.width,
                    src_height: right.height,
                },
            ),
        ])
    }

    /// Run detection on a CPU-resident stereo frame (YUV420P / NV12).
    pub fn detect_and_update_director(
        &mut self,
        frame: &StereoFrame,
        elapsed: std::time::Duration,
    ) -> Result<(), SessionError> {
        let (w, h) = self.core.source_info();
        self.detect_and_update_director_with(elapsed, |core| {
            if let Some(frames) = cpu_frames(frame, w, h) {
                core.run_detection_frames(&frames);
            }
        })
    }

    /// Whether detection should run on the current frame.
    pub fn detection_should_run(&self) -> bool {
        self.core.detection_due(self.frame_count)
    }

    /// Run detection on CPU-resident RGBA frames.
    pub fn detect_and_update_director_rgba(
        &mut self,
        left_rgba: &[u8],
        right_rgba: &[u8],
        width: u32,
        height: u32,
        elapsed: std::time::Duration,
    ) -> Result<(), SessionError> {
        self.detect_and_update_director_with(elapsed, |core| {
            core.run_detection_frames(&[
                (
                    CameraId::Left,
                    DetectorFrame::Rgba {
                        data: left_rgba,
                        width,
                        height,
                    },
                ),
                (
                    CameraId::Right,
                    DetectorFrame::Rgba {
                        data: right_rgba,
                        width,
                        height,
                    },
                ),
            ])
        })
    }

    /// Run detection on CUDA-resident RGBA frames (Bayer zero-copy).
    #[cfg(any(target_os = "linux", target_os = "windows"))]
    #[allow(clippy::too_many_arguments)]
    pub fn detect_and_update_director_cuda_rgba(
        &mut self,
        left_ptr: crate::interop::cuda::CUdeviceptr,
        left_pitch: usize,
        right_ptr: crate::interop::cuda::CUdeviceptr,
        right_pitch: usize,
        width: u32,
        height: u32,
        elapsed: std::time::Duration,
    ) -> Result<(), SessionError> {
        self.detect_and_update_director_with(elapsed, |core| {
            core.run_detection_frames(&[
                (
                    CameraId::Left,
                    DetectorFrame::CudaRgba {
                        ptr: left_ptr,
                        pitch: left_pitch,
                        width,
                        height,
                    },
                ),
                (
                    CameraId::Right,
                    DetectorFrame::CudaRgba {
                        ptr: right_ptr,
                        pitch: right_pitch,
                        width,
                        height,
                    },
                ),
            ])
        })
    }

    /// Detect on pre-letterboxed CUDA RGBA (NvBufSurfTransform output).
    #[cfg(any(target_os = "linux", target_os = "windows"))]
    pub fn detect_and_update_director_preletterboxed(
        &mut self,
        left_ptr: crate::interop::cuda::CUdeviceptr,
        right_ptr: crate::interop::cuda::CUdeviceptr,
        src_width: u32,
        src_height: u32,
        elapsed: std::time::Duration,
    ) -> Result<(), SessionError> {
        self.detect_and_update_director_with(elapsed, |core| {
            crate::profile_scope!("detect_preletterboxed_total");
            core.run_detection_frames(&[
                (
                    CameraId::Left,
                    DetectorFrame::CudaRgbaLetterboxed {
                        ptr: left_ptr,
                        src_width,
                        src_height,
                    },
                ),
                (
                    CameraId::Right,
                    DetectorFrame::CudaRgbaLetterboxed {
                        ptr: right_ptr,
                        src_width,
                        src_height,
                    },
                ),
            ])
        })
    }

    /// Update the director without detection.
    ///
    /// Advances the panner/tracker state without running object
    /// detection. Used when the frame residency has no detection
    /// backend (e.g. D3D11VA without CUDA).
    pub fn update_director(&mut self, elapsed: std::time::Duration) -> Result<(), SessionError> {
        self.fire_sink_and_update_director(elapsed, false)
    }

    /// Run GPU-resident detection from CUDA NV12 shared textures.
    #[cfg(target_os = "linux")]
    pub(crate) fn detect_and_update_director_gpu(
        &mut self,
        left_buf: &crate::interop::zero_copy::GpuBufInfo,
        right_buf: &crate::interop::zero_copy::GpuBufInfo,
        left_slot: u8,
        right_slot: u8,
        elapsed: std::time::Duration,
    ) -> Result<(), SessionError> {
        let lr = self.left_rotation;
        let rr = self.right_rotation;
        self.detect_and_update_director_with(elapsed, |core| {
            crate::profile_scope!("gpu_detect_total");
            let frames = cuda_nv12_frames(left_buf, right_buf, left_slot, right_slot, lr, rr);
            core.run_detection_frames(&frames);
        })
    }

    /// Run Metal-resident detection from CVPixelBuffers.
    #[cfg(any(target_os = "macos", target_os = "ios"))]
    pub(crate) fn detect_and_update_director_metal(
        &mut self,
        left_cvpb: crate::interop::metal::CVPixelBufferRef,
        right_cvpb: crate::interop::metal::CVPixelBufferRef,
        width: u32,
        height: u32,
        elapsed: std::time::Duration,
    ) -> Result<(), SessionError> {
        self.detect_and_update_director_with(elapsed, |core| {
            let frames = metal_frames(left_cvpb, right_cvpb, width, height);
            core.run_detection_frames(&frames);
        })
    }

    /// Drive the tracker/panner chain after detection.
    ///
    /// Shared tail for all detection paths (CPU, GPU, Metal,
    /// no-detection). Delegates to the engine's dispatch - one AI
    /// stack, which emits the trace events through the engine's sink -
    /// and records the outcome into the session's telemetry.
    pub(crate) fn fire_sink_and_update_director(
        &mut self,
        elapsed: std::time::Duration,
        _fresh_detection: bool,
    ) -> Result<(), SessionError> {
        let stats = self.core.dispatch_pose(
            self.frame_count,
            elapsed.as_secs_f64() * 1000.0,
            "StitchSession",
        );
        self.telemetry
            .record_detections(stats.detections, stats.active_tracks, stats.ball_present);
        Ok(())
    }
}
