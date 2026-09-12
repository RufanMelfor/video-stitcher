//! Composite detector that preprocesses wgpu NV12 views on the GPU.
//!
//! Wraps any `UnifiedDetector` (typically `CpuYoloDetector` with
//! DirectML EP) and handles `DetectorFrame::WgpuNv12` by running
//! the `WgpuPreprocessor` compute shader before delegating to the
//! inner detector with `DetectorFrame::PreprocessedChw`.

use reco_core::detect::detector::{
    DetectSplit, Detection, DetectorError, DetectorFrame, PendingDetection, UnifiedDetector,
};
use reco_core::geometry::CameraId;
use reco_detect::wgpu_preprocess::WgpuPreprocessor;

/// Detector wrapper that adds wgpu NV12 preprocessing.
///
/// Created by `setup_autocam` on Windows when CUDA detection is
/// unavailable (Pascal, AMD, Intel). The inner detector handles
/// `PreprocessedChw` (skipping its own preprocessing entirely).
pub struct WgpuPreprocessingDetector {
    inner: Box<dyn UnifiedDetector>,
    /// Built lazily on the first `WgpuNv12` frame (or rebuilt whenever
    /// the incoming frame's resolution changes) rather than required at
    /// construction time - `WgpuPreprocessor::new` bakes the source
    /// frame's letterbox scale/pad into a uniform buffer once, so a
    /// caller that doesn't know the exact frame resolution up front
    /// (e.g. `reco-io`'s raw-camera diagnostic export, which learns it
    /// from the decoder rather than a stitch session) would otherwise
    /// have to guess. Two logically-different camera streams calling
    /// the same wrapper in strict alternation (never concurrently) at
    /// different resolutions works correctly this way too, at the cost
    /// of rebuilding the preprocessor on every alternation in that
    /// specific case - acceptable since [`WgpuPreprocessor::new`] is a
    /// cheap one-time pipeline/buffer setup, not a per-frame cost.
    preprocessor: Option<WgpuPreprocessor>,
    input_size: u32,
    device: wgpu::Device,
    queue: wgpu::Queue,
}

impl WgpuPreprocessingDetector {
    /// Wrap a detector with wgpu NV12 preprocessing.
    ///
    /// `frame_width`/`frame_height` size the preprocessor eagerly when
    /// known up front (the real stitch pipeline's use case - resolution
    /// is fixed for the session). Pass `0, 0` to defer sizing to the
    /// first `WgpuNv12` frame instead (see [`Self::preprocessor`]'s doc
    /// comment) when the caller doesn't know the frame size yet.
    pub fn new(
        inner: Box<dyn UnifiedDetector>,
        device: wgpu::Device,
        queue: wgpu::Queue,
        input_size: u32,
        frame_width: u32,
        frame_height: u32,
    ) -> Self {
        let preprocessor = (frame_width > 0 && frame_height > 0)
            .then(|| WgpuPreprocessor::new(&device, &queue, input_size, frame_width, frame_height));
        Self {
            inner,
            preprocessor,
            input_size,
            device,
            queue,
        }
    }

    /// Get this call's preprocessor, (re)building it if unset or if
    /// `width`/`height` no longer match what it was sized for - see
    /// [`Self::preprocessor`]'s doc comment.
    fn preprocessor_for(&mut self, width: u32, height: u32) -> &WgpuPreprocessor {
        let needs_rebuild = match &self.preprocessor {
            Some(p) => p.frame_size() != (width, height),
            None => true,
        };
        if needs_rebuild {
            self.preprocessor = Some(WgpuPreprocessor::new(
                &self.device,
                &self.queue,
                self.input_size,
                width,
                height,
            ));
        }
        self.preprocessor.as_ref().expect("just set")
    }
}

impl UnifiedDetector for WgpuPreprocessingDetector {
    fn name(&self) -> &'static str {
        "wgpu-preprocess"
    }

    fn detect(
        &mut self,
        camera: CameraId,
        frame: &DetectorFrame<'_>,
    ) -> Result<Vec<Detection>, DetectorError> {
        match frame {
            DetectorFrame::WgpuNv12 {
                y_view,
                uv_view,
                width,
                height,
                rotation,
            } => {
                let (width, height, rotation) = (*width, *height, *rotation);
                // Clone the (Arc-backed, cheap) device/queue handles
                // before borrowing `self` mutably for
                // `preprocessor_for` - `WgpuPreprocessor::preprocess`
                // needs both alongside the mutable preprocessor
                // reference, which the borrow checker can't reconcile
                // against `&self.device`/`&self.queue` directly.
                let (device, queue) = (self.device.clone(), self.queue.clone());
                let preprocessor = self.preprocessor_for(width, height);
                let tensor = preprocessor.preprocess(&device, &queue, y_view, uv_view, rotation);
                let inner_frame = DetectorFrame::PreprocessedChw {
                    data: &tensor,
                    input_size: preprocessor.input_size(),
                    src_width: width,
                    src_height: height,
                };
                self.inner.detect(camera, &inner_frame)
            }
            // Pass through other frame types to the inner detector
            other => self.inner.detect(camera, other),
        }
    }

    fn class_names(&self) -> Option<&[String]> {
        self.inner.class_names()
    }

    /// Async split: GPU readback + preprocess stays synchronous here
    /// (the source textures are borrowed from a small reusable staging
    /// ring, not safe to hand to another thread) and produces an owned
    /// tensor; the actual inference call - the expensive part, and the
    /// part that's pure CPU/GPU-inference-engine work with no shared
    /// resource to protect - is deferred via [`DetectSplit::Pending`]
    /// for an `AsyncDetectThread` to run.
    fn detect_split(
        &mut self,
        camera: CameraId,
        frame: &DetectorFrame<'_>,
    ) -> Result<DetectSplit, DetectorError> {
        match frame {
            DetectorFrame::WgpuNv12 {
                y_view,
                uv_view,
                width,
                height,
                rotation,
            } => {
                let (width, height, rotation) = (*width, *height, *rotation);
                let (device, queue) = (self.device.clone(), self.queue.clone());
                let preprocessor = self.preprocessor_for(width, height);
                let tensor = preprocessor.preprocess(&device, &queue, y_view, uv_view, rotation);
                Ok(DetectSplit::Pending {
                    job: PendingDetection {
                        camera,
                        tensor,
                        input_size: preprocessor.input_size(),
                        src_width: width,
                        src_height: height,
                    },
                    // Nothing left for this wrapper to do once the
                    // deferred inference call returns - the inner
                    // detector's own postprocessing (coordinate
                    // un-letterboxing, normalization) already happens
                    // inside that deferred call.
                    finish: Box::new(|dets| dets),
                })
            }
            // Frame kinds this wrapper doesn't add async support for -
            // fall back to the synchronous path unchanged.
            other => Ok(DetectSplit::Done(self.inner.detect(camera, other)?)),
        }
    }
}
