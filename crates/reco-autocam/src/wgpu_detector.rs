//! Composite detector that preprocesses wgpu NV12 views on the GPU.
//!
//! Wraps any `UnifiedDetector` (typically `CpuYoloDetector` with
//! DirectML EP) and handles `DetectorFrame::WgpuNv12` by running
//! the `WgpuPreprocessor` compute shader before delegating to the
//! inner detector with `DetectorFrame::PreprocessedChw`.
//!
//! It also fills [`Detection::color`] with each box's torso colour, read
//! from the same RGB tensor the model saw (see [`sample_torso_colors`]),
//! so consumers can tell teams apart without a frame readback of their own.

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
    input_size: reco_detect::InputSize,
    device: wgpu::Device,
    queue: wgpu::Queue,
    /// Fog boost clip limit, `0.0` = off; applied to every (re)built
    /// preprocessor. See [`Self::with_fog_boost`].
    fog_boost: f32,
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
        input_size: reco_detect::InputSize,
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
            fog_boost: 0.0,
        }
    }

    /// Enable the fog boost: CLAHE with this clip limit on the
    /// preprocessed tensor (see [`reco_detect::local_contrast`]). `0.0`
    /// turns it off; an out-of-range value is logged and leaves it off.
    pub fn with_fog_boost(mut self, clip_limit: f32) -> Self {
        self.fog_boost = clip_limit;
        if let Some(ref mut p) = self.preprocessor {
            p.set_local_contrast(&self.device, &self.queue, clip_limit);
        }
        self
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
            let mut p =
                WgpuPreprocessor::new(&self.device, &self.queue, self.input_size, width, height);
            if self.fog_boost > 0.0 {
                p.set_local_contrast(&self.device, &self.queue, self.fog_boost);
            }
            self.preprocessor = Some(p);
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
                let input_size = preprocessor.input_size();
                let inner_frame = DetectorFrame::PreprocessedChw {
                    data: &tensor,
                    input_w: input_size.width,
                    input_h: input_size.height,
                    src_width: width,
                    src_height: height,
                };
                let mut dets = self.inner.detect(camera, &inner_frame)?;
                sample_torso_colors(&mut dets, camera, &tensor, input_size, width, height);
                Ok(dets)
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
                let tensor = std::sync::Arc::new(
                    preprocessor.preprocess(&device, &queue, y_view, uv_view, rotation),
                );
                let input_size = preprocessor.input_size();
                let pixels = std::sync::Arc::clone(&tensor);
                Ok(DetectSplit::Pending {
                    job: PendingDetection {
                        camera,
                        tensor,
                        input_w: input_size.width,
                        input_h: input_size.height,
                        src_width: width,
                        src_height: height,
                    },
                    // The inner detector's own postprocessing (coordinate
                    // un-letterboxing, normalization) already happens
                    // inside the deferred call; this only adds the torso
                    // colours, from the same shared tensor.
                    finish: Box::new(move |mut dets| {
                        sample_torso_colors(&mut dets, camera, &pixels, input_size, width, height);
                        dets
                    }),
                })
            }
            // Frame kinds this wrapper doesn't add async support for -
            // fall back to the synchronous path unchanged.
            other => Ok(DetectSplit::Done(self.inner.detect(camera, other)?)),
        }
    }
}

/// Torso band sampled for the shirt colour: this fraction range of the box
/// height, from the top (shoulders to belly; skips head and legs).
const TORSO_BAND: (f32, f32) = (0.18, 0.50);
/// Half-width of the sampled band as a fraction of the box width (skips
/// arms and the background beside the body).
const TORSO_HALF_WIDTH: f32 = 0.22;
/// Samples per band axis; the median of up to `N x N` pixels is robust
/// against a number, skin or grass in the band.
const TORSO_SAMPLES_PER_AXIS: usize = 10;

/// Fill each detection's [`Detection::color`] with the per-channel median
/// of its torso band, read from the preprocessed CHW tensor (RGB in
/// `0..=1`, the source frame letterboxed into `input`). Detections are
/// normalized to the `src_w x src_h` frame, the same space the tensor was
/// letterboxed from. A band that falls outside the tensor leaves `color`
/// as `None`. Only detections of `camera` are touched: in async detection the
/// merged detections of both cameras pass through every camera's finisher.
fn sample_torso_colors(
    dets: &mut [Detection],
    camera: CameraId,
    tensor: &[f32],
    input: reco_detect::InputSize,
    src_w: u32,
    src_h: u32,
) {
    let (iw, ih) = (input.width as usize, input.height as usize);
    let plane = iw * ih;
    if plane == 0 || tensor.len() < 3 * plane || src_w == 0 || src_h == 0 {
        return;
    }
    let lb = input.letterbox(src_w, src_h);
    let to_x = |nx: f32| nx * src_w as f32 * lb.scale + lb.pad_x;
    let to_y = |ny: f32| ny * src_h as f32 * lb.scale + lb.pad_y;
    let mut channels: [Vec<u8>; 3] = Default::default();
    for d in dets.iter_mut().filter(|d| d.camera == camera) {
        let top = d.center_y - d.height / 2.0;
        let x0 = to_x(d.center_x - d.width * TORSO_HALF_WIDTH).max(0.0);
        let x1 = to_x(d.center_x + d.width * TORSO_HALF_WIDTH).min((iw - 1) as f32);
        let y0 = to_y(top + d.height * TORSO_BAND.0).max(0.0);
        let y1 = to_y(top + d.height * TORSO_BAND.1).min((ih - 1) as f32);
        if !(x1 >= x0 && y1 >= y0) {
            continue;
        }
        let (x0, x1, y0, y1) = (x0 as usize, x1 as usize, y0 as usize, y1 as usize);
        let step_x = ((x1 - x0) / TORSO_SAMPLES_PER_AXIS).max(1);
        let step_y = ((y1 - y0) / TORSO_SAMPLES_PER_AXIS).max(1);
        for c in channels.iter_mut() {
            c.clear();
        }
        for y in (y0..=y1).step_by(step_y) {
            for x in (x0..=x1).step_by(step_x) {
                let i = y * iw + x;
                for (c, ch) in channels.iter_mut().enumerate() {
                    ch.push((tensor[c * plane + i].clamp(0.0, 1.0) * 255.0).round() as u8);
                }
            }
        }
        let median = |v: &mut Vec<u8>| {
            v.sort_unstable();
            v[v.len() / 2]
        };
        d.color = Some([
            median(&mut channels[0]),
            median(&mut channels[1]),
            median(&mut channels[2]),
        ]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn player(cx: f32, cy: f32, w: f32, h: f32) -> Detection {
        Detection {
            camera: CameraId::Left,
            class_id: 0,
            confidence: 0.9,
            center_x: cx,
            center_y: cy,
            width: w,
            height: h,
            color: None,
        }
    }

    /// A 40x30 input showing an 80x60 source frame (letterbox scale 0.5,
    /// no padding): grass everywhere, a red shirt under a skin-coloured
    /// head.
    #[test]
    fn torso_colour_is_the_shirt_not_head_or_grass() {
        let input = reco_detect::InputSize::new(40, 30);
        let (iw, ih) = (40usize, 30usize);
        let plane = iw * ih;
        let mut t = vec![0.0f32; 3 * plane];
        let mut put = |x: usize, y: usize, rgb: [f32; 3]| {
            for (c, v) in rgb.iter().enumerate() {
                t[c * plane + y * iw + x] = *v;
            }
        };
        for y in 0..ih {
            for x in 0..iw {
                put(x, y, [0.1, 0.5, 0.1]);
            }
        }
        // input pixels = source / 2
        for y in 10..16 {
            for x in 15..25 {
                put(x, y, [0.9, 0.1, 0.1]);
            }
        }
        for y in 7..10 {
            for x in 18..22 {
                put(x, y, [0.9, 0.7, 0.6]);
            }
        }
        // player box in source pixels: x 30..50, y 14..54 (head at the top)
        let mut dets = vec![player(40.0 / 80.0, 34.0 / 60.0, 20.0 / 80.0, 40.0 / 60.0)];
        sample_torso_colors(&mut dets, CameraId::Left, &t, input, 80, 60);
        let c = dets[0].color.expect("band inside the tensor");
        assert!(c[0] > 200 && c[1] < 60 && c[2] < 60, "{c:?}");
    }

    #[test]
    fn short_tensor_samples_nothing() {
        let input = reco_detect::InputSize::new(40, 30);
        let mut dets = vec![player(0.5, 0.5, 0.1, 0.2)];
        sample_torso_colors(&mut dets, CameraId::Left, &[0.0; 10], input, 80, 60);
        assert!(dets[0].color.is_none());
    }

    #[test]
    fn other_cameras_detections_are_left_alone() {
        let input = reco_detect::InputSize::new(40, 30);
        let t = vec![0.5f32; 3 * 40 * 30];
        let mut right = player(0.5, 0.5, 0.2, 0.4);
        right.camera = CameraId::Right;
        let mut dets = vec![player(0.5, 0.5, 0.2, 0.4), right];
        sample_torso_colors(&mut dets, CameraId::Left, &t, input, 80, 60);
        assert!(dets[0].color.is_some());
        assert!(dets[1].color.is_none());
    }
}
