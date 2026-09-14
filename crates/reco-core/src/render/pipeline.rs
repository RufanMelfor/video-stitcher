//! Stitch pipeline orchestration.
//!
//! The [`StitchPipeline`] coordinates all stages: GPU setup, frame ingestion,
//! rendering, viewport cropping, and output encoding. It is the primary
//! entry point for consumers of `reco-core`.
//!
//! ## Usage
//!
//! Most consumers should use [`StitchSession`](crate::session::StitchSession)
//! instead of `StitchPipeline` directly. The pipeline is exposed for advanced
//! use cases like preview windows that need direct surface rendering.
//!
//! ```rust,no_run,compile_fail
//! use reco_core::render::pipeline::StitchPipeline;
//! use reco_core::gpu::GpuContext;
//!
//! let gpu = pollster::block_on(GpuContext::new())?;
//! let pipeline = StitchPipeline::with_gpu(
//!     gpu, calibration, viewport, 1920, 1080,
//!     wgpu::TextureFormat::Rgba8UnormSrgb,
//!     reco_core::render::renderer::InputFormat::Yuv420p,
//! )?;
//! ```

use super::overlay::{OverlayFrame, RgbaOverlayCompositor};
use super::renderer::{InputFormat, RenderError, Renderer};
use super::scene::SceneGeometry;
use super::viewport::{ResolvedViewport, ViewportConfig};
use crate::calibration::Calibration;
use crate::geometry::ViewportPosition;
use crate::gpu::color_grade::{ColorGradeParams, ColorGradePass};
use crate::gpu::sharpen::{SharpenParams, SharpenPass};
use crate::gpu::{GpuContext, GpuError};

use thiserror::Error;

pub use super::planes::{BgraPlanes, FramePlaneView, Nv12Planes, StridedYuvPlanes, YuvPlanes};

/// Errors from the stitch pipeline. `Clone + Send + Sync` so consumers
/// posting results to worker threads can carry the typed error.
#[derive(Debug, Clone, Error)]
pub enum PipelineError {
    /// GPU initialization failed.
    #[error("GPU error: {0}")]
    Gpu(#[from] GpuError),

    /// The calibration document is invalid.
    #[error("invalid calibration: {0}")]
    Calibration(#[from] crate::calibration::CalibrationError),

    /// Render error.
    #[error("render error: {0}")]
    Render(#[from] RenderError),

    /// Wrong StereoFrame variant for this render method.
    #[error("unsupported frame variant: {reason}")]
    UnsupportedFrameVariant {
        /// Description of the mismatch.
        reason: &'static str,
    },

    /// Invalid configuration.
    #[error("invalid config: {reason}")]
    InvalidConfig {
        /// What is wrong.
        reason: String,
    },
}

/// The main stitching pipeline.
///
/// Owns the GPU context, scene geometry, and renderer. Consumers provide
/// YUV420P or NV12 frames and receive stitched RGBA output via
/// [`Self::render_to_target`] or [`Self::render_to_target_nv12`].
pub struct StitchPipeline {
    /// GPU device and queue.
    pub(crate) gpu: GpuContext,
    /// 3D scene layout computed from calibration.
    pub(crate) scene: SceneGeometry,
    /// Calibration data (camera intrinsics + layout).
    pub(crate) calibration: Calibration,
    /// Output viewport configuration.
    pub(crate) viewport: ViewportConfig,
    /// Draw a debug line at the exact seam position. Pure visualization
    /// (calibration-tuning aid), not calibration state - deliberately not
    /// part of `Calibration` so toggling it never touches the saved file.
    pub(crate) show_seam_line: bool,
    /// GPU renderer (textures, pipelines, bind groups).
    renderer: Renderer,
    /// Input frame dimensions.
    input_width: u32,
    input_height: u32,
    /// Periodic exposure/color-matching state (see [`super::color_match`]).
    /// `&self` render methods need interior mutability here.
    color_match: std::sync::Mutex<super::color_match::ColorMatchState>,
    /// GPU seam-band sampler for the zero-copy paths, where the CPU never
    /// sees pixel data (see [`super::band_gather`]). Created on first use
    /// rather than at construction: the CPU-decode paths never need it,
    /// and building a compute pipeline that nothing dispatches would cost
    /// every consumer a shader compile.
    band_gather: std::sync::Mutex<Option<super::band_gather::BandGather>>,
    /// Format shared by the stitch and optional overlay render passes.
    output_format: wgpu::TextureFormat,
    /// Lazily created only when a consumer enables an overlay.
    overlay: Option<RgbaOverlayCompositor>,
    /// Desired overlay position/size, applied to `overlay` immediately
    /// when set and (re-)applied whenever a new compositor is created -
    /// stored independently of `overlay` so a placement set before the
    /// first overlay frame ever arrives isn't lost. Default reproduces
    /// the original centered-letterbox behavior.
    overlay_placement: super::overlay::OverlayPlacement,
    /// Second, independent overlay slot for a *transition* - one fixed
    /// image faded in and out via its opacity uniform alone (see
    /// [`super::overlay::OverlayTransitionSource`]). Drawn **before**
    /// `overlay`, so a content overlay such as a scoreboard stays
    /// legible on top of it rather than dimming with the video.
    ///
    /// A separate compositor rather than another layer of `overlay`
    /// because the two change on completely different terms: `overlay`
    /// re-uploads a texture whenever its pixels change, while this one
    /// uploads once and then only rewrites 32 bytes per frame.
    transition: Option<RgbaOverlayCompositor>,
    /// Universal color grade (brightness/saturation/gamma), applied
    /// right after the stitch render, before overlay/transition
    /// compositing - so a "Vivid" boost affects the whole frame the
    /// same way an overlay/scoreboard on top of it would not want
    /// touched. Lazily created on first non-identity `set_color_grade`
    /// call, like `sharpen` and `band_gather` - no consumer that leaves
    /// it off pays for a shader compile or scratch texture.
    color_grade: Option<ColorGradePass>,
    /// Current color grade parameters, kept even before `color_grade`
    /// exists so a call to [`Self::set_color_grade`] before the pass is
    /// created isn't lost (mirrors `overlay_placement`'s pattern).
    color_grade_params: ColorGradeParams,
    /// Scratch texture the color grade compute pass writes into (a
    /// compute pass can't read and write the same texture). Same
    /// size/format as `render_target`; rebuilt in [`Self::resize`].
    color_grade_scratch: std::sync::Mutex<Option<wgpu::Texture>>,
    /// Unsharp-mask sharpening, applied last (after overlay/transition
    /// compositing) so it sharpens exactly what the viewer/export sees,
    /// compensating for detail loss when the AI panner zooms into a crop
    /// of the panorama. Lazily created on first non-identity
    /// `set_sharpen_params` call, same reasoning as `color_grade`.
    sharpen: Option<SharpenPass>,
    /// Current sharpen parameters, kept even before `sharpen` exists so a
    /// call to [`Self::set_sharpen_params`] before the pass is created
    /// isn't lost (mirrors `color_grade_params`'s pattern).
    sharpen_params: SharpenParams,
    /// Scratch texture the sharpen compute pass writes into. Same
    /// size/format as `render_target`; rebuilt in [`Self::resize`].
    sharpen_scratch: std::sync::Mutex<Option<wgpu::Texture>>,
}

/// Pre-built bind groups for GPU-resident zero-copy sources.
///
/// Created by [`StitchPipeline::configure_gpu_source`]. Each slot
/// corresponds to a double-buffer index used by the decode thread.
#[cfg(target_os = "linux")]
pub struct GpuSourceBindGroups {
    left: [wgpu::BindGroup; 2],
    right: [wgpu::BindGroup; 2],
}

impl StitchPipeline {
    /// Create a pipeline with an existing GPU context and custom output format.
    ///
    /// Used by the preview window which needs a specific surface format
    /// and provides its own GPU context (selected with surface compatibility).
    pub fn with_gpu(
        gpu: GpuContext,
        program: &crate::render::GpuProgram,
        calibration: Calibration,
        viewport: ViewportConfig,
        input_width: u32,
        input_height: u32,
        output_format: impl Into<wgpu::TextureFormat>,
        input_format: InputFormat,
    ) -> Result<Self, PipelineError> {
        // Validate inputs before GPU resource creation. This is THE
        // enforcement boundary for in-memory calibrations: every
        // constructor (StitchCore, StitchSession, the preview bridge,
        // StitchJob) funnels through here, so a wrong lens count or a
        // NaN surfaces as a typed error instead of an index panic or a
        // GPU hang further down.
        calibration.validate()?;
        if let Err(e) = viewport.validate() {
            return Err(PipelineError::InvalidConfig { reason: e });
        }
        if input_width == 0 || input_height == 0 {
            return Err(PipelineError::InvalidConfig {
                reason: format!("input dimensions must be > 0, got {input_width}x{input_height}"),
            });
        }
        if input_width > crate::calibration::MAX_DIM || input_height > crate::calibration::MAX_DIM {
            return Err(PipelineError::InvalidConfig {
                reason: format!(
                    "input dimensions {input_width}x{input_height} exceed MAX_DIM ({})",
                    crate::calibration::MAX_DIM
                ),
            });
        }

        let output_format = output_format.into();
        let aspect = calibration.lenses[0].width as f32 / calibration.lenses[0].height as f32;
        let scene = SceneGeometry::new(&calibration.topology, &calibration.framing, aspect);
        let renderer = Renderer::new(
            &gpu,
            program,
            viewport.width,
            viewport.height,
            input_width,
            input_height,
            output_format,
            input_format,
            &scene,
        );

        log::info!(
            "Pipeline initialized: {}x{} output, GPU: {}",
            viewport.width,
            viewport.height,
            gpu.adapter_info.name
        );

        // Initialized from the calibration's own stored value (not just
        // `ColorGradeParams::default()`) so a calibration saved with
        // "Vivid" already on renders correctly from the first frame,
        // without needing an explicit `set_color_grade` call first -
        // mirrors how `color_gamma_left`/`_right` are read straight from
        // `self.calibration.topology` every frame.
        let initial_color_grade = ColorGradeParams::new(
            calibration.topology.color_grade_brightness,
            calibration.topology.color_grade_saturation,
            calibration.topology.color_grade_gamma,
        );
        let color_grade = (!initial_color_grade.is_identity())
            .then(|| ColorGradePass::new(&gpu, &initial_color_grade));

        // Same reasoning as `initial_color_grade` above, for sharpening.
        let initial_sharpen = SharpenParams::new(
            calibration.topology.sharpen_amount,
            calibration.topology.sharpen_radius,
        );
        let sharpen =
            (!initial_sharpen.is_identity()).then(|| SharpenPass::new(&gpu, &initial_sharpen));

        Ok(Self {
            gpu,
            scene,
            calibration,
            viewport,
            show_seam_line: false,
            renderer,
            input_width,
            input_height,
            color_match: std::sync::Mutex::new(super::color_match::ColorMatchState::default()),
            band_gather: std::sync::Mutex::new(None),
            output_format,
            overlay: None,
            overlay_placement: super::overlay::OverlayPlacement::default(),
            transition: None,
            color_grade,
            color_grade_params: initial_color_grade,
            color_grade_scratch: std::sync::Mutex::new(None),
            sharpen,
            sharpen_params: initial_sharpen,
            sharpen_scratch: std::sync::Mutex::new(None),
        })
    }

    /// The name of the GPU this pipeline is running on.
    pub fn gpu_name(&self) -> &str {
        self.gpu.gpu_name()
    }

    /// Shared reference to the GPU context.
    ///
    /// Needed by consumers that create their own wgpu resources
    /// (e.g. surface configuration for a preview window).
    pub fn gpu(&self) -> &GpuContext {
        &self.gpu
    }

    /// The calibration data this pipeline was created with.
    pub fn calibration(&self) -> &Calibration {
        &self.calibration
    }

    /// The current output viewport configuration.
    pub fn viewport(&self) -> &ViewportConfig {
        &self.viewport
    }

    /// Input frame dimensions as `(width, height)`.
    pub fn source_info(&self) -> (u32, u32) {
        (self.input_width, self.input_height)
    }

    /// Input pixel format the pipeline was built for. Needed by the
    /// stacked-video GPU packer so it can pick the matching shader
    /// kernel variant (separate R8 planes for YUV420P vs interleaved
    /// Rg8 UV for NV12) without the consumer passing the format
    /// through a second time.
    pub(crate) fn input_format(&self) -> super::renderer::InputFormat {
        self.renderer.input_format()
    }

    /// Left-side source plane views (Y/U/V texture views). Used by
    /// the stacked-video GPU packer to read the same uploaded
    /// source data the stitch shader samples; the pack runs in
    /// parallel with the panorama render into its own atlas buffer.
    /// For NV12 inputs the `U` view is the interleaved UV texture
    /// and the `V` view is a 1×1 dummy.
    pub(crate) fn left_plane_views(
        &self,
    ) -> (wgpu::TextureView, wgpu::TextureView, wgpu::TextureView) {
        self.renderer.left_plane_views()
    }

    /// Right-side counterpart to [`Self::left_plane_views`].
    pub(crate) fn right_plane_views(
        &self,
    ) -> (wgpu::TextureView, wgpu::TextureView, wgpu::TextureView) {
        self.renderer.right_plane_views()
    }

    /// Update the viewport metadata (aspect ratio, projection matrix).
    ///
    /// **Important:** this does NOT recreate GPU textures or the render
    /// target. Use this for viewport-metadata changes (e.g. surface
    /// reconfigure in a preview window). For actual output resolution
    /// changes, rebuild the pipeline with [`Self::with_gpu`].
    /// Returns `Some((width, height))` on success, or `None` if the
    /// dimensions were zero (ignored). Consumers that own external
    /// staging buffers (e.g.
    /// [`RgbaReadback`](crate::gpu::rgba_readback::RgbaReadback)) should
    /// recreate them when the returned size differs from the previous.
    pub fn resize(&mut self, width: u32, height: u32) -> Option<(u32, u32)> {
        if width == 0 || height == 0 {
            log::warn!("resize({width}, {height}) ignored: dimensions must be non-zero");
            return None;
        }
        self.viewport.width = width;
        self.viewport.height = height;
        let gpu = self.gpu.clone();
        if let Some(overlay) = self.overlay.as_mut() {
            overlay.resize(&gpu, (width, height));
        }
        if let Some(transition) = self.transition.as_mut() {
            transition.resize(&gpu, (width, height));
        }
        // Scratch texture is rebuilt lazily (size-checked) on next
        // color grade encode rather than eagerly here - avoids
        // allocating it on every resize when grading is off.
        *self.color_grade_scratch.lock().unwrap() = None;
        // Same reasoning, for the sharpen pass's scratch texture.
        *self.sharpen_scratch.lock().unwrap() = None;
        Some((width, height))
    }

    /// Upload a new straight-alpha RGBA overlay surface.
    ///
    /// GPU resources are allocated lazily on the first call. Subsequent calls
    /// update the cached texture; intervening video frames reuse it without an
    /// additional upload.
    pub fn set_overlay_frame(&mut self, frame: &OverlayFrame) -> Result<(), PipelineError> {
        if let Some(overlay) = self.overlay.as_mut() {
            overlay
                .upload(&self.gpu, frame)
                .map_err(|e| PipelineError::InvalidConfig {
                    reason: e.to_string(),
                })?;
        } else {
            let mut overlay = RgbaOverlayCompositor::new(
                &self.gpu,
                self.output_format,
                (self.viewport.width, self.viewport.height),
                frame,
            )
            .map_err(|e| PipelineError::InvalidConfig {
                reason: e.to_string(),
            })?;
            overlay.set_placement(&self.gpu, self.overlay_placement);
            self.overlay = Some(overlay);
        }
        Ok(())
    }

    /// Disable composition and release all overlay GPU resources.
    pub fn clear_overlay(&mut self) {
        self.overlay = None;
    }

    /// Upload the transition overlay's fixed image, replacing any
    /// previous one. Call once when a transition is attached - the
    /// per-frame cost afterwards is [`Self::set_transition_opacity`]
    /// alone.
    pub fn set_transition_frame(&mut self, frame: &OverlayFrame) -> Result<(), PipelineError> {
        if let Some(transition) = self.transition.as_mut() {
            transition
                .upload(&self.gpu, frame)
                .map_err(|e| PipelineError::InvalidConfig {
                    reason: e.to_string(),
                })?;
        } else {
            let mut transition = RgbaOverlayCompositor::new(
                &self.gpu,
                self.output_format,
                (self.viewport.width, self.viewport.height),
                frame,
            )
            .map_err(|e| PipelineError::InvalidConfig {
                reason: e.to_string(),
            })?;
            // A transition covers the frame on its own terms; it is not
            // subject to the content overlay's placement.
            transition.set_placement(&self.gpu, super::overlay::OverlayPlacement::default());
            self.transition = Some(transition);
        }
        Ok(())
    }

    /// Set how visible the transition overlay is this frame, `0.0`
    /// (nothing drawn) to `1.0` (fully opaque). One uniform write; a
    /// no-op when the value is unchanged.
    pub fn set_transition_opacity(&mut self, opacity: f32) {
        if let Some(transition) = self.transition.as_mut() {
            transition.set_opacity(&self.gpu, opacity);
        }
    }

    /// Release the transition overlay's GPU resources.
    pub fn clear_transition(&mut self) {
        self.transition = None;
    }

    /// Whether a transition overlay is currently attached.
    pub fn has_transition(&self) -> bool {
        self.transition.is_some()
    }

    /// Whether an overlay texture is currently active.
    pub fn has_overlay(&self) -> bool {
        self.overlay.is_some()
    }

    /// Reposition/resize the composited overlay - see
    /// [`super::overlay::OverlayPlacement`]. Takes effect immediately if an
    /// overlay is already active, and is (re-)applied to any overlay
    /// created afterward, so it's safe to call before the first overlay
    /// frame ever arrives.
    pub fn set_overlay_placement(&mut self, placement: super::overlay::OverlayPlacement) {
        self.overlay_placement = placement;
        if let Some(overlay) = self.overlay.as_mut() {
            overlay.set_placement(&self.gpu, placement);
        }
    }

    fn composite_target_commands(
        &self,
        stitch_commands: wgpu::CommandBuffer,
    ) -> wgpu::CommandBuffer {
        let target = self.renderer.render_target();

        // Color grade runs first (on the raw stitched frame, before any
        // overlay/transition compositing) - a "Vivid" boost is meant for
        // the video content, not a scoreboard or PAUZE card drawn on top
        // of it. See `Self::color_grade_target_commands`.
        let stitch_commands = self.color_grade_target_commands(stitch_commands, target);

        // Transition first so the content overlay draws on top of it -
        // see the `transition` field's doc comment.
        let passes: Vec<&RgbaOverlayCompositor> = [self.transition.as_ref(), self.overlay.as_ref()]
            .into_iter()
            .flatten()
            .collect();
        let Some((last, leading)) = passes.split_last() else {
            return self.sharpen_target_commands(stitch_commands, target);
        };

        // The extra submissions exist only while the feature is enabled.
        // Queue ordering guarantees each overlay pass loads the result of
        // everything before it, and that the completed camera frame is in
        // place before NV12 conversion or RGBA readback begins.
        self.gpu.queue().submit(std::iter::once(stitch_commands));
        let target_view = target.create_view(&wgpu::TextureViewDescriptor::default());
        for pass in leading {
            self.gpu
                .queue()
                .submit(std::iter::once(pass.encode(&self.gpu, &target_view)));
        }
        let overlay_commands = last.encode(&self.gpu, &target_view);

        // Sharpen runs last (after overlay/transition compositing), so it
        // sharpens exactly what the viewer/export sees - unlike color
        // grade, which deliberately runs before compositing so it only
        // touches the video content. See `Self::sharpen_target_commands`.
        self.sharpen_target_commands(overlay_commands, target)
    }

    /// Encode the sharpen pass (if enabled) after overlay/transition
    /// compositing, writing the result back into `target`. No-op
    /// (returns `commands` unchanged) when sharpening is off. Same
    /// scratch-texture-and-blit-back shape as
    /// [`Self::color_grade_target_commands`] - see its doc comment,
    /// including why `target` is a parameter rather than always
    /// `self.renderer.render_target()`.
    fn sharpen_target_commands(
        &self,
        commands: wgpu::CommandBuffer,
        target: &wgpu::Texture,
    ) -> wgpu::CommandBuffer {
        let Some(pass) = self.sharpen.as_ref() else {
            return commands;
        };
        if pass.is_identity() {
            return commands;
        }

        self.gpu.queue().submit(std::iter::once(commands));

        let size = target.size();
        let mut scratch_guard = self.sharpen_scratch.lock().unwrap();
        let make_scratch = || {
            self.gpu.device().create_texture(&wgpu::TextureDescriptor {
                label: Some("sharpen_scratch"),
                size,
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::Rgba8Unorm,
                usage: wgpu::TextureUsages::TEXTURE_BINDING
                    | wgpu::TextureUsages::STORAGE_BINDING
                    | wgpu::TextureUsages::COPY_SRC
                    | wgpu::TextureUsages::COPY_DST,
                view_formats: &[],
            })
        };
        let scratch = scratch_guard.get_or_insert_with(make_scratch);
        if scratch.size() != size {
            *scratch = make_scratch();
        }

        let mut encoder =
            self.gpu
                .device()
                .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                    label: Some("sharpen_pass"),
                });
        pass.encode(&self.gpu, &mut encoder, target, scratch);
        encoder.copy_texture_to_texture(scratch.as_image_copy(), target.as_image_copy(), size);
        encoder.finish()
    }

    /// Encode the color grade pass (if enabled) on the raw stitched
    /// frame, before overlay/transition compositing, writing the result
    /// back into `target` so every downstream consumer keeps reading a
    /// single texture without knowing color grade exists. No-op (returns
    /// `commands` unchanged) when grading is off.
    ///
    /// `target` is `self.renderer.render_target()` on the export path, or
    /// the caller-owned preview texture on the live-preview path (see
    /// [`Self::composite_view_if_enabled`]) - both same-format
    /// (`Rgba8Unorm`), so the same scratch-texture-and-blit-back approach
    /// works for either. Compute passes can't read and write the same
    /// texture, so this runs the pass into a same-sized scratch texture,
    /// then blits the result back - both GPU-side, no CPU round trip.
    /// Mirrors `Self::sharpen_target_commands`'s shape (see the sharpen
    /// pass).
    fn color_grade_target_commands(
        &self,
        commands: wgpu::CommandBuffer,
        target: &wgpu::Texture,
    ) -> wgpu::CommandBuffer {
        let Some(pass) = self.color_grade.as_ref() else {
            return commands;
        };
        if pass.is_identity() {
            return commands;
        }

        self.gpu.queue().submit(std::iter::once(commands));

        let size = target.size();
        let mut scratch_guard = self.color_grade_scratch.lock().unwrap();
        let make_scratch = || {
            self.gpu.device().create_texture(&wgpu::TextureDescriptor {
                label: Some("color_grade_scratch"),
                size,
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::Rgba8Unorm,
                usage: wgpu::TextureUsages::TEXTURE_BINDING
                    | wgpu::TextureUsages::STORAGE_BINDING
                    | wgpu::TextureUsages::COPY_SRC
                    | wgpu::TextureUsages::COPY_DST,
                view_formats: &[],
            })
        };
        let scratch = scratch_guard.get_or_insert_with(make_scratch);
        if scratch.size() != size {
            *scratch = make_scratch();
        }

        let mut encoder =
            self.gpu
                .device()
                .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                    label: Some("color_grade_pass"),
                });
        pass.encode(&self.gpu, &mut encoder, target, scratch);
        encoder.copy_texture_to_texture(scratch.as_image_copy(), target.as_image_copy(), size);
        encoder.finish()
    }

    /// Apply the transition/overlay compositors, then color grade and
    /// sharpen, directly to a caller-owned preview texture - the
    /// live-preview counterpart to [`Self::composite_target_commands`]
    /// (which operates on `self.renderer.render_target()` for the export
    /// path instead). `target` must be the same texture `target_view`
    /// was created from, with `TEXTURE_BINDING | STORAGE_BINDING |
    /// COPY_SRC | COPY_DST` usage - see `reco_gui::preview`'s caller for
    /// why. Without `target`, color grade/sharpen only ever apply on
    /// export, not to what the Color Mapping panel's sliders show live.
    fn composite_view_if_enabled(&self, target_view: &wgpu::TextureView, target: &wgpu::Texture) {
        for pass in [self.transition.as_ref(), self.overlay.as_ref()]
            .into_iter()
            .flatten()
        {
            self.gpu
                .queue()
                .submit(std::iter::once(pass.encode(&self.gpu, target_view)));
        }
        let commands = self
            .gpu
            .device()
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("color_grade_sharpen_noop"),
            })
            .finish();
        let commands = self.color_grade_target_commands(commands, target);
        let commands = self.sharpen_target_commands(commands, target);
        self.gpu.queue().submit(std::iter::once(commands));
    }

    /// Set the vertical field of view in degrees.
    ///
    /// Values are clamped to `[1.0, 179.0]` to prevent degenerate
    /// projection matrices (0 or 180 would produce NaN/Inf).
    pub fn set_fov(&mut self, fov_degrees: f32) {
        self.viewport.fov_degrees = fov_degrees.clamp(1.0, 179.0);
    }

    /// Get the current field of view in degrees.
    pub fn fov(&self) -> f32 {
        self.viewport.fov_degrees
    }

    /// Set universal color grading (brightness/saturation/gamma) applied
    /// to the whole composited frame - used for the "Vivid" preset that
    /// perks up grey/dull footage. `(1.0, 1.0, 1.0)` disables grading
    /// entirely (identity - the pass is skipped and costs nothing). GPU
    /// resources are created lazily on the first non-identity call;
    /// subsequent calls just update the uniform buffer.
    ///
    /// Also mirrors into `self.calibration.topology.color_grade_*` (same
    /// reasoning as [`Self::set_color_gamma`]) so it's part of the saved
    /// calibration document, not app-level runtime-only state - the GUI's
    /// Color Mapping panel edits this live, same as manual gamma.
    pub fn set_color_grade(&mut self, brightness: f32, saturation: f32, gamma: f32) {
        self.calibration.topology.color_grade_brightness = brightness;
        self.calibration.topology.color_grade_saturation = saturation;
        self.calibration.topology.color_grade_gamma = gamma;
        self.color_grade_params = ColorGradeParams::new(brightness, saturation, gamma);
        if let Some(pass) = self.color_grade.as_mut() {
            pass.update_params(&self.gpu, &self.color_grade_params);
        } else if !self.color_grade_params.is_identity() {
            self.color_grade = Some(ColorGradePass::new(&self.gpu, &self.color_grade_params));
        }
    }

    /// Current color grade parameters, `(1.0, 1.0, 1.0)` if grading is off.
    /// Reads the stored calibration value - see [`Self::set_color_grade`].
    pub fn color_grade(&self) -> (f32, f32, f32) {
        (
            self.calibration.topology.color_grade_brightness,
            self.calibration.topology.color_grade_saturation,
            self.calibration.topology.color_grade_gamma,
        )
    }

    /// Set unsharp-mask sharpening strength and radius, applied to the
    /// final composited frame (after overlay/transition compositing).
    /// `amount` of `0.0` disables sharpening entirely (identity - the
    /// pass is skipped and costs nothing). GPU resources are created
    /// lazily on the first non-identity call; subsequent calls just
    /// update the uniform buffer.
    ///
    /// Also mirrors into `self.calibration.topology.sharpen_*` (same
    /// reasoning as [`Self::set_color_grade`]) so it's part of the saved
    /// calibration document - the GUI's Color Mapping panel edits this
    /// live.
    pub fn set_sharpen_params(&mut self, amount: f32, radius: f32) {
        self.calibration.topology.sharpen_amount = amount;
        self.calibration.topology.sharpen_radius = radius;
        self.sharpen_params = SharpenParams::new(amount, radius);
        if let Some(pass) = self.sharpen.as_mut() {
            pass.update_params(&self.gpu, &self.sharpen_params);
        } else if !self.sharpen_params.is_identity() {
            self.sharpen = Some(SharpenPass::new(&self.gpu, &self.sharpen_params));
        }
    }

    /// Current sharpen parameters as `(amount, radius)`, `(0.0, 1.0)` if
    /// sharpening is off. Reads the stored calibration value - see
    /// [`Self::set_sharpen_params`].
    pub fn sharpen_params(&self) -> (f32, f32) {
        (
            self.calibration.topology.sharpen_amount,
            self.calibration.topology.sharpen_radius,
        )
    }

    /// Set the lens distortion correction amount for every lens (per-frame
    /// uniform; no scene rebuild).
    pub fn set_lens_correction_amount(&mut self, amount: f32) {
        let c = amount.clamp(0.0, 1.0);
        for lens in &mut self.calibration.lenses {
            lens.correction = c;
        }
    }

    /// Set the seam blend width (per-frame uniform; no scene rebuild).
    pub fn set_blend_width(&mut self, width: f32) {
        self.calibration.topology.blend_width = width;
    }

    /// Flip which camera's content fades over the other at the blend seam.
    /// See [`crate::calibration::Topology::blend_flip_direction`].
    pub fn set_blend_flip_direction(&mut self, flip: bool) {
        self.calibration.topology.blend_flip_direction = flip;
    }

    /// Set the manual seam-position nudge (per-frame uniform; no scene
    /// rebuild). See [`crate::calibration::Topology::seam_offset`].
    pub fn set_seam_offset(&mut self, offset: f32) {
        self.calibration.topology.seam_offset = offset;
    }

    /// Current manual seam-position nudge. See
    /// [`crate::calibration::Topology::seam_offset`].
    pub fn seam_offset(&self) -> f32 {
        self.calibration.topology.seam_offset
    }

    /// Enable/disable the 2-band spatial seam blend. See
    /// [`crate::calibration::Topology::multiband_blend_enabled`].
    pub fn set_multiband_blend_enabled(&mut self, enabled: bool) {
        self.calibration.topology.multiband_blend_enabled = enabled;
    }

    /// Toggle the seam debug line (per-frame uniform; no scene rebuild).
    /// Draws a thin highlight at the exact rendered seam position by
    /// reusing the same alpha-threshold math the blend itself uses, so
    /// it can never disagree with where the blend actually sits. Purely a
    /// visualization aid - deliberately not part of `Calibration` so
    /// toggling it never touches the saved file.
    pub fn set_show_seam_line(&mut self, show: bool) {
        self.show_seam_line = show;
    }

    /// Set the ground-plane tilt correction for the x-plane (per-frame
    /// uniform; no scene rebuild). See
    /// [`crate::calibration::Topology::ground_tilt_x`].
    pub fn set_ground_tilt_x(&mut self, tilt: f32) {
        self.calibration.topology.ground_tilt_x = tilt as f64;
    }

    /// Set the ground-plane tilt correction for the z-plane. See
    /// [`crate::calibration::Topology::ground_tilt_z`].
    pub fn set_ground_tilt_z(&mut self, tilt: f32) {
        self.calibration.topology.ground_tilt_z = tilt as f64;
    }

    /// Set the top-of-frame tilt correction for the x-plane. See
    /// [`crate::calibration::Topology::top_tilt_x`].
    pub fn set_top_tilt_x(&mut self, tilt: f32) {
        self.calibration.topology.top_tilt_x = tilt as f64;
    }

    /// Set the top-of-frame tilt correction for the z-plane. See
    /// [`crate::calibration::Topology::top_tilt_z`].
    pub fn set_top_tilt_z(&mut self, tilt: f32) {
        self.calibration.topology.top_tilt_z = tilt as f64;
    }

    /// Set the ground-tilt band's full-strength threshold. See
    /// [`crate::calibration::Topology::ground_tilt_band_width`].
    pub fn set_ground_tilt_band_width(&mut self, width: f32) {
        self.calibration.topology.ground_tilt_band_width = width as f64;
    }

    /// Set the top-tilt band's full-strength threshold. See
    /// [`crate::calibration::Topology::top_tilt_band_width`].
    pub fn set_top_tilt_band_width(&mut self, width: f32) {
        self.calibration.topology.top_tilt_band_width = width as f64;
    }

    /// Enable/disable automatic per-camera seam-band exposure/color
    /// matching. See [`crate::calibration::Topology::color_match_enabled`].
    pub fn set_color_match_enabled(&mut self, enabled: bool) {
        self.calibration.topology.color_match_enabled = enabled;
        self.force_color_match_remeasure();
    }

    /// Width of the color-match measurement/blend band. See
    /// [`crate::calibration::Topology::color_match_band_width`].
    pub fn set_color_match_band_width(&mut self, width: f32) {
        self.calibration.topology.color_match_band_width = width;
        self.force_color_match_remeasure();
    }

    /// Columns in the color-match sampling grid. See
    /// [`crate::calibration::Topology::color_match_grid_cols`].
    pub fn set_color_match_grid_cols(&mut self, cols: u32) {
        self.calibration.topology.color_match_grid_cols = cols;
        self.force_color_match_remeasure();
    }

    /// Rows in the color-match sampling grid. See
    /// [`crate::calibration::Topology::color_match_grid_rows`].
    pub fn set_color_match_grid_rows(&mut self, rows: u32) {
        self.calibration.topology.color_match_grid_rows = rows;
        self.force_color_match_remeasure();
    }

    /// Frame interval between color-match re-measurements. See
    /// [`crate::calibration::Topology::color_match_interval_frames`].
    pub fn set_color_match_interval_frames(&mut self, frames: u32) {
        self.calibration.topology.color_match_interval_frames = frames;
        self.force_color_match_remeasure();
    }

    /// EMA smoothing factor for color-match updates. See
    /// [`crate::calibration::Topology::color_match_ema_alpha`].
    pub fn set_color_match_ema_alpha(&mut self, alpha: f32) {
        self.calibration.topology.color_match_ema_alpha = alpha;
        self.force_color_match_remeasure();
    }

    /// Manual gamma applied to one camera before the automatic match.
    /// See [`crate::calibration::Topology::color_gamma_left`].
    ///
    /// Forces a re-measure like the other color setters, and for a
    /// stronger reason than a slider feeling responsive: the automatic
    /// offsets currently in flight were derived from the *previous*
    /// curve, so leaving them in place would show a correction for an
    /// image that no longer exists until the next scheduled measurement.
    pub fn set_color_gamma(&mut self, left: f32, right: f32) {
        self.calibration.topology.color_gamma_left = left;
        self.calibration.topology.color_gamma_right = right;
        self.force_color_match_remeasure();
    }

    /// Current manual per-camera gamma, as `(left, right)`. The value the
    /// manual sliders show and write to - **not** affected by
    /// `color_match_auto_gamma`, which overrides what's actually rendered
    /// without touching this stored value, so switching auto off again
    /// resumes from whatever was here before. See
    /// [`Self::color_match_correction`] for the value actually in effect
    /// right now.
    pub fn color_gamma(&self) -> (f32, f32) {
        (
            self.calibration.topology.color_gamma_left,
            self.calibration.topology.color_gamma_right,
        )
    }

    /// Enable/disable automatic per-camera gamma fitting, re-measured
    /// alongside the additive offset on the same
    /// `color_match_interval_frames` schedule instead of using the manual
    /// `color_gamma_left`/`_right` value verbatim. See
    /// [`crate::calibration::Topology::color_match_auto_gamma`].
    ///
    /// A flat additive YUV offset alone cannot correct a real sensor-gain
    /// (ISO) mismatch between two independently-metering cameras - it can
    /// only shift the average, not reshape the tone curve a gain
    /// difference actually produces. Manual gamma (`set_color_gamma`) was
    /// added for exactly this reason; this lets the same correction track
    /// the mismatch automatically as it changes through a match (cloud
    /// cover, sun angle) instead of staying fixed at whatever a user
    /// tuned it to for one moment - a fixed value tuned on an extreme
    /// moment measurably *overshot* (visibly reversed the mismatch) at a
    /// calmer point in the same real match footage this was diagnosed on.
    pub fn set_color_match_auto_gamma(&mut self, enabled: bool) {
        self.calibration.topology.color_match_auto_gamma = enabled;
        self.force_color_match_remeasure();
    }

    /// Whether automatic gamma fitting is currently on. See
    /// [`Self::set_color_match_auto_gamma`].
    pub fn color_match_auto_gamma(&self) -> bool {
        self.calibration.topology.color_match_auto_gamma
    }

    /// Maximum luma offset the color match may apply. See
    /// [`crate::calibration::Topology::color_match_max_y_offset`].
    pub fn set_color_match_max_y_offset(&mut self, offset: f32) {
        self.calibration.topology.color_match_max_y_offset = offset;
        self.force_color_match_remeasure();
    }

    /// Maximum chroma offset the color match may apply. See
    /// [`crate::calibration::Topology::color_match_max_chroma_offset`].
    pub fn set_color_match_max_chroma_offset(&mut self, offset: f32) {
        self.calibration.topology.color_match_max_chroma_offset = offset;
        self.force_color_match_remeasure();
    }

    /// Current virtual camera position `[x, y, z]` in scene space.
    pub fn camera_position(&self) -> [f32; 3] {
        self.scene.camera_position
    }

    /// Override the virtual camera position (e.g. to reset free-fly).
    ///
    /// Takes effect on the next render (the eye is read from the scene each
    /// frame). The position is never allowed to reach the scene origin,
    /// where the look-toward-origin basis would be undefined.
    pub fn set_camera_position(&mut self, pos: [f32; 3]) {
        let norm = (pos[0] * pos[0] + pos[1] * pos[1] + pos[2] * pos[2]).sqrt();
        if norm > 1e-2 {
            self.scene.camera_position = pos;
        }
    }

    /// Translate the virtual camera in its current view frame (free-fly).
    ///
    /// `local` is `[right, up, forward]` in scene-space distance units,
    /// evaluated at the given `yaw`/`pitch` so movement follows where the
    /// camera looks. The basis matches `view_matrix`'s yaw-around-up,
    /// pitch-around-right convention (rig tilt/roll are ignored here; this
    /// is a debug/preview navigation aid, not a render path). Vertical
    /// (`up`) uses world up so it stays level regardless of pitch.
    pub fn fly_camera(&mut self, local: [f32; 3], yaw: f32, pitch: f32) {
        use crate::geometry::VirtualCamera;
        use nalgebra::{Unit, UnitQuaternion};

        let cam = VirtualCamera::new(&self.scene.camera_position);
        let world_up = VirtualCamera::world_up();
        let yaw_q = UnitQuaternion::from_axis_angle(&Unit::new_normalize(world_up), yaw);
        let right = yaw_q * cam.base_right;
        let pitch_q = UnitQuaternion::from_axis_angle(&Unit::new_normalize(right), pitch);
        let forward = (pitch_q * yaw_q) * cam.base_forward;

        let delta = right * local[0] + world_up * local[1] + forward * local[2];
        let next = [
            self.scene.camera_position[0] + delta.x,
            self.scene.camera_position[1] + delta.y,
            self.scene.camera_position[2] + delta.z,
        ];
        self.set_camera_position(next);
    }

    /// Update calibration parameters. Recomputes [`SceneGeometry`] from the
    /// new layout. Takes effect on the next render call (uniforms are rebuilt
    /// each frame from the stored calibration and scene).
    ///
    /// No GPU pipeline recreation needed - only the uniform data changes.
    ///
    /// Also forces a color-match remeasure: any topology/framing change can
    /// move where the visible seam sits (a rig tilt/roll, a plane rotation,
    /// or a seam-offset drag), and the color-match correction was measured
    /// against the *previous* geometry - stale until this call, it can look
    /// actively wrong (not just slightly off) rather than merely outdated,
    /// since it was tuned for content that's no longer at the seam. This
    /// previously only happened on the interval-based automatic remeasure
    /// (up to `color_match_interval_frames` rendered frames later, or never
    /// while paused) - see `force_color_match_remeasure`'s own doc.
    pub fn update_calibration(&mut self, calibration: Calibration) {
        let aspect = calibration.lenses[0].width as f32 / calibration.lenses[0].height as f32;
        self.scene = SceneGeometry::new(&calibration.topology, &calibration.framing, aspect);
        self.calibration = calibration;
        self.force_color_match_remeasure();
        log::debug!("Pipeline calibration updated");
    }

    /// Replace the topology (plane placement + seam), rebuilding the scene.
    pub fn update_topology(&mut self, topology: crate::calibration::Topology) {
        let mut cal = self.calibration.clone();
        cal.topology = topology;
        self.update_calibration(cal);
    }

    /// Replace the framing (axis offset, tilt, roll), rebuilding the scene.
    pub fn update_framing(&mut self, framing: crate::calibration::Framing) {
        let mut cal = self.calibration.clone();
        cal.framing = framing;
        self.update_calibration(cal);
    }

    /// Update per-camera intrinsics (focal, principal point, distortion)
    /// for one or both cameras without touching the plane layout or rig
    /// orientation.
    ///
    /// Intended for interactive lens tweaking in a GUI: each `Lens`
    /// change is written into the shader's per-frame uniform buffer, so the
    /// next render call reflects the new values. No GPU pipeline or scene
    /// recreation is needed - cheap enough (~microseconds) to call on
    /// every slider drag.
    ///
    /// `left`/`right` are `None` to leave that side untouched. If both are
    /// `None` this is a no-op. Passing `Some` for a side replaces that
    /// side's `Lens` on the stored calibration; the next render
    /// picks it up automatically.
    ///
    /// Does not recompute `SceneGeometry` because the plane layout is
    /// unchanged; only the camera intrinsics (which live on the stored
    /// calibration and are re-read each frame) need updating.
    pub fn update_camera_params(
        &mut self,
        left: Option<crate::calibration::Lens>,
        right: Option<crate::calibration::Lens>,
    ) {
        if left.is_none() && right.is_none() {
            return;
        }
        if let Some(l) = left {
            self.calibration.lenses[0] = l;
        }
        if let Some(r) = right {
            self.calibration.lenses[1] = r;
        }
        log::debug!("Pipeline camera params updated");
    }

    /// Set up bind groups for GPU-resident zero-copy input.
    ///
    /// Creates bind groups for the provided shared textures (Y + UV per slot
    /// per camera). Call once during setup, then pass the result to
    /// [`Self::render_gpu_frame`] each frame.
    #[cfg(target_os = "linux")]
    pub fn configure_gpu_source(
        &mut self,
        left_textures: [(
            &crate::interop::vulkan::SharedTexture,
            &crate::interop::vulkan::SharedTexture,
        ); 2],
        right_textures: [(
            &crate::interop::vulkan::SharedTexture,
            &crate::interop::vulkan::SharedTexture,
        ); 2],
    ) -> GpuSourceBindGroups {
        let left_bg_0 = self.renderer.create_texture_bind_group(
            &left_textures[0].0.texture,
            &left_textures[0].1.texture,
            "left_slot0",
        );
        let left_bg_1 = self.renderer.create_texture_bind_group(
            &left_textures[1].0.texture,
            &left_textures[1].1.texture,
            "left_slot1",
        );
        let right_bg_0 = self.renderer.create_texture_bind_group(
            &right_textures[0].0.texture,
            &right_textures[0].1.texture,
            "right_slot0",
        );
        let right_bg_1 = self.renderer.create_texture_bind_group(
            &right_textures[1].0.texture,
            &right_textures[1].1.texture,
            "right_slot1",
        );
        GpuSourceBindGroups {
            left: [left_bg_0, left_bg_1],
            right: [right_bg_0, right_bg_1],
        }
    }

    /// Select bind groups for a GPU-resident frame and render.
    ///
    /// Call this instead of manually setting bind groups on the renderer.
    #[cfg(target_os = "linux")]
    pub fn render_gpu_frame(
        &mut self,
        bind_groups: &GpuSourceBindGroups,
        left_slot: u8,
        right_slot: u8,
        yaw: f32,
        pitch: f32,
    ) -> wgpu::CommandBuffer {
        self.renderer
            .set_left_bind_group(bind_groups.left[left_slot as usize].clone());
        self.renderer
            .set_right_bind_group(bind_groups.right[right_slot as usize].clone());
        self.render_to_target_gpu(yaw, pitch)
    }

    /// Create a texture bind group from Y + UV textures.
    pub fn create_texture_bind_group(
        &self,
        y_texture: &wgpu::Texture,
        uv_texture: &wgpu::Texture,
        label: &str,
    ) -> wgpu::BindGroup {
        self.renderer
            .create_texture_bind_group(y_texture, uv_texture, label)
    }

    /// Render from pre-built bind groups (VRAM pool path).
    ///
    /// **Does not measure the seam band.** Bind groups alone cannot be
    /// sampled by the color-match gather, so a caller using only this
    /// gets whatever the last completed measurement produced - identity
    /// if none ever ran. Callers that *can* supply plane views should
    /// prefer [`Self::render_with_bind_groups_measured`]; this plain
    /// form is for paths that genuinely have no views to offer.
    pub fn render_with_bind_groups(
        &mut self,
        left_bg: &wgpu::BindGroup,
        right_bg: &wgpu::BindGroup,
        yaw: f32,
        pitch: f32,
    ) -> wgpu::CommandBuffer {
        self.renderer.set_left_bind_group(left_bg.clone());
        self.renderer.set_right_bind_group(right_bg.clone());
        self.render_to_target_gpu(yaw, pitch)
    }

    /// Render from pre-built bind groups, measuring the seam band from
    /// the matching plane views first.
    ///
    /// The buffered/lookahead path (VRAM pool) renders from bind groups
    /// rather than through [`Self::render_imported_views`], so it never
    /// reached that function's `gather_band_samples` call and the
    /// automatic color match stayed silently identity there - the same
    /// class of bug that made color match inactive under hardware decode
    /// in the first place, resurfacing on a second render path. The
    /// views must be over the *same* textures the bind groups were built
    /// from, or the measurement describes a different frame than the one
    /// being rendered.
    /// `planes` is `(left_y, left_uv, right_y, right_uv)` - the shape
    /// `VramPool::plane_views` already returns, kept as a tuple so the
    /// four views travel together and cannot be passed in the wrong
    /// order as separate arguments.
    pub fn render_with_bind_groups_measured(
        &mut self,
        left_bg: &wgpu::BindGroup,
        right_bg: &wgpu::BindGroup,
        planes: (
            &wgpu::TextureView,
            &wgpu::TextureView,
            &wgpu::TextureView,
            &wgpu::TextureView,
        ),
        yaw: f32,
        pitch: f32,
    ) -> wgpu::CommandBuffer {
        self.renderer.set_left_bind_group(left_bg.clone());
        self.renderer.set_right_bind_group(right_bg.clone());
        let (left_y, left_uv, right_y, right_uv) = planes;
        self.gather_band_samples(left_y, left_uv, right_y, right_uv);
        self.render_to_target_gpu(yaw, pitch)
    }

    /// Render from imported GPU textures (e.g. Metal/VideoToolbox zero-copy).
    ///
    /// Takes raw Y + UV texture references for each camera, creates bind groups,
    /// and renders. Unlike [`Self::render_gpu_frame`] which uses pre-built
    /// double-buffered bind groups, this creates them per-frame (the overhead
    /// is negligible compared to decode time).
    pub fn render_imported_textures(
        &mut self,
        left_y: &wgpu::Texture,
        left_uv: &wgpu::Texture,
        right_y: &wgpu::Texture,
        right_uv: &wgpu::Texture,
        yaw: f32,
        pitch: f32,
    ) -> wgpu::CommandBuffer {
        let left_bg = self
            .renderer
            .create_texture_bind_group(left_y, left_uv, "metal_left");
        let right_bg = self
            .renderer
            .create_texture_bind_group(right_y, right_uv, "metal_right");
        self.renderer.set_left_bind_group(left_bg);
        self.renderer.set_right_bind_group(right_bg);
        self.render_to_target_gpu(yaw, pitch)
    }

    /// Render from pre-built GPU texture views.
    ///
    /// Used by the D3D11VA zero-copy path where NV12 plane views are
    /// created from `TextureAspect::Plane0` / `Plane1`.
    pub fn render_imported_views(
        &mut self,
        left_y: &wgpu::TextureView,
        left_uv: &wgpu::TextureView,
        right_y: &wgpu::TextureView,
        right_uv: &wgpu::TextureView,
        yaw: f32,
        pitch: f32,
    ) -> wgpu::CommandBuffer {
        let left_bg = self
            .renderer
            .create_bind_group_from_views(left_y, left_uv, "d3d11_left");
        let right_bg = self
            .renderer
            .create_bind_group_from_views(right_y, right_uv, "d3d11_right");
        self.renderer.set_left_bind_group(left_bg);
        self.renderer.set_right_bind_group(right_bg);
        // Measure the seam band from the same textures the render is
        // about to sample. Without this the zero-copy path has no pixel
        // access at all and the color match is silently identity - see
        // `super::band_gather`.
        self.gather_band_samples(left_y, left_uv, right_y, right_uv);
        self.render_to_target_gpu(yaw, pitch)
    }

    /// Process a CPU-resident stereo frame and return the render command buffer.
    ///
    /// Handles YUV420P vs NV12 format differences internally.
    /// For GPU-resident frames, use [`Self::render_gpu_frame`] instead.
    pub fn render_stereo_frame(
        &self,
        frame: &crate::source::StereoFrame,
        yaw: f32,
        pitch: f32,
    ) -> Result<wgpu::CommandBuffer, PipelineError> {
        use crate::source::StereoFrame;
        match frame {
            StereoFrame::Yuv420p(pair) => {
                let left = YuvPlanes {
                    y: &pair.left.y,
                    u: &pair.left.u,
                    v: &pair.left.v,
                };
                let right = YuvPlanes {
                    y: &pair.right.y,
                    u: &pair.right.u,
                    v: &pair.right.v,
                };
                self.render_to_target(&left, &right, yaw, pitch)
            }
            StereoFrame::Nv12(pair) => {
                let left = Nv12Planes {
                    y: &pair.left.y,
                    uv: &pair.left.uv,
                };
                let right = Nv12Planes {
                    y: &pair.right.y,
                    uv: &pair.right.uv,
                };
                self.render_to_target_nv12(&left, &right, yaw, pitch)
            }
            StereoFrame::GpuResident { .. } => Err(PipelineError::UnsupportedFrameVariant {
                reason: "GpuResident frames must use render_gpu_frame()",
            }),
            #[allow(unreachable_patterns)]
            _ => Err(PipelineError::UnsupportedFrameVariant {
                reason: "unsupported StereoFrame variant for CPU render path",
            }),
        }
    }

    /// Bundle the live `topology.color_match_*` fields into the internal
    /// params struct `ColorMatchState` expects.
    ///
    /// `gamma_left`/`gamma_right`: when `color_match_auto_gamma` is off,
    /// the static calibration value, exactly as every other field here.
    /// When it's on, the *live fitted* value from `ColorMatchState` itself
    /// instead - this is what `measure_band_mean`/`decode_transfer_yuv`
    /// actually decode the band with, and it has to agree with what
    /// `ColorMatchState::apply_measurement`'s `undo_gamma` step assumes
    /// was applied (its own last-fitted `self.left_gamma`/`right_gamma`),
    /// or the two disagree about which gamma produced the pixels just
    /// measured and the fit runs away rather than converging - caught by
    /// a real run pinning both cameras at the `[0.5, 2.0]` clamp instead
    /// of settling on a stable value.
    fn color_match_params(&self) -> super::color_match::ColorMatchParams {
        let t = &self.calibration.topology;
        let (gamma_left, gamma_right) = if t.color_match_auto_gamma {
            let c = self.color_match.lock().unwrap().current();
            (c.left_gamma, c.right_gamma)
        } else {
            (t.color_gamma_left, t.color_gamma_right)
        };
        super::color_match::ColorMatchParams {
            band_width: t.color_match_band_width,
            grid_cols: t.color_match_grid_cols,
            grid_rows: t.color_match_grid_rows,
            measure_interval_frames: t.color_match_interval_frames,
            ema_alpha: t.color_match_ema_alpha,
            max_y_offset: t.color_match_max_y_offset,
            max_chroma_offset: t.color_match_max_chroma_offset,
            seam_offset: t.seam_offset,
            blend_flip_direction: t.blend_flip_direction,
            gamma_left,
            gamma_right,
            auto_gamma: t.color_match_auto_gamma,
        }
    }

    /// `ColorCorrection` for whenever the automatic measurement did not
    /// run this frame (color match disabled, or a path - BGRA - that never
    /// measures at all): zero additive offset, but gamma still resolved
    /// from the calibration's static `color_gamma_left`/`_right`, because
    /// manual gamma has always applied unconditionally, independent of
    /// the additive match - see `ColorCorrection`'s own doc for why
    /// `ColorCorrection::default()` alone is never the right call here.
    fn identity_correction_with_static_gamma(&self) -> super::renderer::ColorCorrection {
        super::renderer::ColorCorrection {
            left_gamma: self.calibration.topology.color_gamma_left,
            right_gamma: self.calibration.topology.color_gamma_right,
            ..Default::default()
        }
    }

    /// Force the color-match measurement to run again on the very next
    /// frame, bypassing `topology.color_match_interval_frames`. Call after
    /// changing any `color_match_*` topology field so a live-tuning slider
    /// is reflected immediately instead of waiting up to
    /// `color_match_interval_frames` frames.
    ///
    /// Public so a consumer GUI can also expose an explicit "remeasure now"
    /// action - useful while paused (the periodic interval only advances on
    /// rendered frames, so nothing re-measures on its own without one) or
    /// after seeking to a frame with different lighting than whatever was
    /// last measured.
    pub fn force_color_match_remeasure(&self) {
        self.color_match.lock().unwrap().force_remeasure();
    }

    /// The current smoothed color-match correction, without advancing or
    /// re-measuring. See [`super::color_match::ColorMatchState::current`].
    pub fn color_match_correction(&self) -> super::renderer::ColorCorrection {
        self.color_match.lock().unwrap().current()
    }

    /// Measure (or reuse the last smoothed) per-camera color-offset
    /// correction for a YUV420P frame pair. Identity when
    /// `topology.color_match_enabled` is `false`.
    fn color_correction_yuv420p(
        &self,
        left: &YuvPlanes<'_>,
        right: &YuvPlanes<'_>,
    ) -> super::renderer::ColorCorrection {
        if !self.calibration.topology.color_match_enabled {
            return self.identity_correction_with_static_gamma();
        }
        let params = self.color_match_params();
        self.color_match.lock().unwrap().update_yuv420p(
            (left.y, left.u, left.v),
            (right.y, right.u, right.v),
            self.input_width,
            self.input_height,
            &self.calibration.lenses[0],
            &self.calibration.lenses[1],
            self.renderer.is_full_range(),
            &params,
        )
    }

    /// NV12 counterpart to [`Self::color_correction_yuv420p`].
    fn color_correction_nv12(
        &self,
        left: &Nv12Planes<'_>,
        right: &Nv12Planes<'_>,
    ) -> super::renderer::ColorCorrection {
        if !self.calibration.topology.color_match_enabled {
            return self.identity_correction_with_static_gamma();
        }
        let params = self.color_match_params();
        self.color_match.lock().unwrap().update_nv12(
            (left.y, left.uv),
            (right.y, right.uv),
            self.input_width,
            self.input_height,
            &self.calibration.lenses[0],
            &self.calibration.lenses[1],
            self.renderer.is_full_range(),
            &params,
        )
    }

    /// Drive one step of the GPU seam-band measurement for the zero-copy
    /// path: collect whatever previous gather has finished, then start a
    /// new one if the interval says it is due.
    ///
    /// Cheap on the frames in between (a state check and an atomic-free
    /// mutex lock); the compute pass itself samples 128 texels by default
    /// and the readback is ~1KB, once every `color_match_interval_frames`.
    fn gather_band_samples(
        &self,
        left_y: &wgpu::TextureView,
        left_uv: &wgpu::TextureView,
        right_y: &wgpu::TextureView,
        right_uv: &wgpu::TextureView,
    ) {
        if !self.calibration.topology.color_match_enabled {
            return;
        }
        let params = self.color_match_params();
        let mut gather = self.band_gather.lock().unwrap();
        let gather = gather.get_or_insert_with(|| super::band_gather::BandGather::new(&self.gpu));

        // 1. Pick up a finished measurement, if any. Runs first so a
        //    result is consumed on the earliest possible frame.
        if let Some((left, right)) = gather.try_take(&self.gpu) {
            let is_full_range = self.renderer.is_full_range();
            let left_mean = super::color_match::mean_of_normalized_samples(
                &left,
                is_full_range,
                params.gamma_left,
            );
            let right_mean = super::color_match::mean_of_normalized_samples(
                &right,
                is_full_range,
                params.gamma_right,
            );
            if let (Some(l), Some(r)) = (left_mean, right_mean) {
                self.color_match
                    .lock()
                    .unwrap()
                    .apply_measurement(l, r, &params);
            }
        }

        // 2. Is another one due? Ask the same state the CPU path asks, so
        //    both honour `measure_interval_frames` and "remeasure now"
        //    identically.
        let due = {
            let mut state = self.color_match.lock().unwrap();
            state.set_interval(params.measure_interval_frames);
            state.measurement_due()
        };
        if !due || !gather.is_idle() {
            return;
        }

        // 3. Refresh the sample positions only when the band geometry
        //    moved - they are pure lens/parameter geometry, so on a
        //    steady calibration this uploads once for the whole session.
        let flip = self.renderer.flip_180();
        let key = super::band_gather::PositionKey {
            band_width: params.band_width,
            grid_cols: params.grid_cols,
            grid_rows: params.grid_rows,
            seam_offset: params.seam_offset,
            blend_flip_direction: params.blend_flip_direction,
            input_width: self.input_width,
            input_height: self.input_height,
            flip_180: flip,
        };
        if !gather.positions_current(&key) {
            let positions = |lens_index: usize, is_right: bool| {
                let p = super::color_match::band_sample_positions(
                    self.input_width,
                    self.input_height,
                    &self.calibration.lenses[lens_index],
                    is_right,
                    &params,
                );
                // The shader samples a rotated source at 1-uv instead of
                // reversing the buffer, so the raw texel this position
                // lands on has to be mirrored to match.
                if flip[lens_index] {
                    super::band_gather::mirrored_180(&p, self.input_width, self.input_height)
                } else {
                    p
                }
            };
            let left = positions(0, false);
            let right = positions(1, true);
            gather.set_positions(&self.gpu, &left, &right, key);
        }

        gather.dispatch(&self.gpu, left_y, left_uv, right_y, right_uv);
        self.color_match.lock().unwrap().measurement_issued();
    }

    /// Render a frame directly to a texture view (for window display).
    ///
    /// Unlike the encode path, this does NOT read back to CPU — the result
    /// stays on the GPU and is presented to the surface.
    ///
    /// `target` must be the same texture `target_view` was created from,
    /// with `TEXTURE_BINDING | STORAGE_BINDING | COPY_SRC | COPY_DST`
    /// usage - needed so color grade/sharpen (Color Mapping panel) update
    /// this live preview, not just the export path. See
    /// [`Self::composite_view_if_enabled`].
    pub fn render_to_view(
        &self,
        left: &YuvPlanes<'_>,
        right: &YuvPlanes<'_>,
        yaw: f32,
        pitch: f32,
        target_view: &wgpu::TextureView,
        target: &wgpu::Texture,
    ) -> Result<(), PipelineError> {
        let color_correction = self.color_correction_yuv420p(left, right);
        self.renderer
            .upload_left_yuv(&self.gpu, left.y, left.u, left.v)?;
        self.renderer
            .upload_right_yuv(&self.gpu, right.y, right.u, right.v)?;

        let viewport = ResolvedViewport {
            config: self.viewport.clone(),
            position: ViewportPosition {
                yaw,
                pitch,
                fov_degrees: None,
            },
        };

        self.renderer.render_to_view(
            &self.gpu,
            &self.scene,
            &self.calibration,
            &viewport,
            self.calibration.topology.blend_width,
            color_correction,
            self.calibration.topology.multiband_blend_enabled,
            self.show_seam_line,
            target_view,
        );
        self.composite_view_if_enabled(target_view, target);
        Ok(())
    }

    /// Render NV12 frames directly to a texture view (for window display).
    ///
    /// Like [`Self::render_to_view`] but accepts NV12 input (Y + interleaved
    /// UV) instead of YUV420P. Requires the pipeline to be initialized with
    /// `InputFormat::Nv12`. See [`Self::render_to_view`] for `target`'s
    /// required usage flags.
    pub fn render_nv12_to_view(
        &self,
        left: &Nv12Planes<'_>,
        right: &Nv12Planes<'_>,
        yaw: f32,
        pitch: f32,
        target_view: &wgpu::TextureView,
        target: &wgpu::Texture,
    ) -> Result<(), PipelineError> {
        let color_correction = self.color_correction_nv12(left, right);
        self.renderer.upload_left_nv12(&self.gpu, left.y, left.uv)?;
        self.renderer
            .upload_right_nv12(&self.gpu, right.y, right.uv)?;

        let viewport = ResolvedViewport {
            config: self.viewport.clone(),
            position: ViewportPosition {
                yaw,
                pitch,
                fov_degrees: None,
            },
        };

        self.renderer.render_to_view(
            &self.gpu,
            &self.scene,
            &self.calibration,
            &viewport,
            self.calibration.topology.blend_width,
            color_correction,
            self.calibration.topology.multiband_blend_enabled,
            self.show_seam_line,
            target_view,
        );
        self.composite_view_if_enabled(target_view, target);
        Ok(())
    }

    /// Render a frame to the internal render target without CPU readback.
    ///
    /// Uploads YUV planes and returns the render `CommandBuffer` without
    /// submitting. The caller must submit it (typically together with NV12
    /// conversion commands via the NV12 converter).
    #[cfg_attr(
        feature = "profiling",
        tracing::instrument(skip_all, name = "render_to_target")
    )]
    pub fn render_to_target(
        &self,
        left: &YuvPlanes<'_>,
        right: &YuvPlanes<'_>,
        yaw: f32,
        pitch: f32,
    ) -> Result<wgpu::CommandBuffer, PipelineError> {
        let color_correction = self.color_correction_yuv420p(left, right);
        self.renderer
            .upload_left_yuv(&self.gpu, left.y, left.u, left.v)?;
        self.renderer
            .upload_right_yuv(&self.gpu, right.y, right.u, right.v)?;

        let viewport = ResolvedViewport {
            config: self.viewport.clone(),
            position: ViewportPosition {
                yaw,
                pitch,
                fov_degrees: None,
            },
        };

        Ok(
            self.composite_target_commands(self.renderer.render_to_target(
                &self.gpu,
                &self.scene,
                &self.calibration,
                &viewport,
                self.calibration.topology.blend_width,
                color_correction,
                self.calibration.topology.multiband_blend_enabled,
                self.show_seam_line,
            )),
        )
    }

    /// Upload NV12 frames and render to the internal target.
    ///
    /// Like `render_to_target` but accepts NV12 input (Y + interleaved UV)
    /// instead of YUV420P. Requires the pipeline to be initialized with
    /// `InputFormat::Nv12`.
    #[cfg_attr(
        feature = "profiling",
        tracing::instrument(skip_all, name = "render_to_target_nv12")
    )]
    pub fn render_to_target_nv12(
        &self,
        left: &Nv12Planes<'_>,
        right: &Nv12Planes<'_>,
        yaw: f32,
        pitch: f32,
    ) -> Result<wgpu::CommandBuffer, PipelineError> {
        let color_correction = self.color_correction_nv12(left, right);
        self.renderer.upload_left_nv12(&self.gpu, left.y, left.uv)?;
        self.renderer
            .upload_right_nv12(&self.gpu, right.y, right.uv)?;

        let viewport = ResolvedViewport {
            config: self.viewport.clone(),
            position: ViewportPosition {
                yaw,
                pitch,
                fov_degrees: None,
            },
        };

        Ok(
            self.composite_target_commands(self.renderer.render_to_target(
                &self.gpu,
                &self.scene,
                &self.calibration,
                &viewport,
                self.calibration.topology.blend_width,
                color_correction,
                self.calibration.topology.multiband_blend_enabled,
                self.show_seam_line,
            )),
        )
    }

    /// Upload packed BGRA/RGBA frames and render to the internal target.
    ///
    /// Expects each plane as `width * height * 4` bytes in (R, G, B, A) byte
    /// order. Use [`BgraPlanes::from_bgra_swizzle_into`] when the source
    /// is BGRA. Requires the pipeline to be initialized with
    /// [`InputFormat::Bgra`](crate::render::renderer::InputFormat#variant.Bgra).
    ///
    /// No exposure/color matching (see [`super::color_match`]): its band
    /// measurement needs raw YCbCr bytes, which packed RGBA doesn't carry.
    /// Always renders with identity color correction.
    #[cfg_attr(
        feature = "profiling",
        tracing::instrument(skip_all, name = "render_to_target_bgra")
    )]
    pub fn render_to_target_bgra(
        &self,
        left: &BgraPlanes<'_>,
        right: &BgraPlanes<'_>,
        yaw: f32,
        pitch: f32,
    ) -> Result<wgpu::CommandBuffer, PipelineError> {
        self.renderer.upload_left_bgra(&self.gpu, left.rgba)?;
        self.renderer.upload_right_bgra(&self.gpu, right.rgba)?;

        let viewport = ResolvedViewport {
            config: self.viewport.clone(),
            position: ViewportPosition {
                yaw,
                pitch,
                fov_degrees: None,
            },
        };

        Ok(
            self.composite_target_commands(self.renderer.render_to_target(
                &self.gpu,
                &self.scene,
                &self.calibration,
                &viewport,
                self.calibration.topology.blend_width,
                self.identity_correction_with_static_gamma(),
                self.calibration.topology.multiband_blend_enabled,
                self.show_seam_line,
            )),
        )
    }

    /// Render from GPU-resident RGBA textures (e.g. Bayer demosaic output).
    ///
    /// Copies source textures into the input planes, then renders the
    /// stitch to the internal target. Returns the complete command buffer.
    /// The caller submits the demosaic encoder first, then this one.
    /// Requires `InputFormat::Bgra`.
    pub fn render_from_gpu_rgba(
        &self,
        left_rgba: &wgpu::Texture,
        right_rgba: &wgpu::Texture,
        yaw: f32,
        pitch: f32,
    ) -> wgpu::CommandBuffer {
        // Copy demosaiced textures into stitch pipeline input planes
        let mut copy_encoder =
            self.gpu
                .device
                .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                    label: Some("bayer_copy"),
                });
        self.renderer
            .copy_texture_to_left(&mut copy_encoder, left_rgba);
        self.renderer
            .copy_texture_to_right(&mut copy_encoder, right_rgba);
        self.gpu
            .queue
            .submit(std::iter::once(copy_encoder.finish()));

        // Render stitch (reads from the just-populated input textures)
        self.render_to_target_gpu(yaw, pitch)
    }

    /// Render to the internal target without upload or readback (zero-copy path).
    ///
    /// Returns the render `CommandBuffer` without submitting. Assumes textures
    /// are already populated via CUDA/Vulkan shared memory.
    #[cfg_attr(
        feature = "profiling",
        tracing::instrument(skip_all, name = "render_to_target_gpu")
    )]
    /// Render to the internal target using whatever textures are currently
    /// bound. Call [`Self::render_imported_textures`] once to set up
    /// bind groups, then use this for subsequent frames with the same
    /// textures to avoid per-frame bind group allocation.
    ///
    /// Exposure/color matching applies here too, as of the GPU band
    /// gather (see [`super::band_gather`]): the correction is whatever
    /// the last completed asynchronous measurement produced, identity
    /// until the first one lands. Callers that never call
    /// [`Self::gather_band_samples`] - anything binding textures this
    /// pipeline cannot sample, e.g. the Bayer RGBA path - keep getting
    /// identity, which is what they got before.
    pub fn render_to_target_gpu(&self, yaw: f32, pitch: f32) -> wgpu::CommandBuffer {
        let viewport = ResolvedViewport {
            config: self.viewport.clone(),
            position: ViewportPosition {
                yaw,
                pitch,
                fov_degrees: None,
            },
        };

        let correction = if self.calibration.topology.color_match_enabled {
            self.color_match.lock().unwrap().current()
        } else {
            self.identity_correction_with_static_gamma()
        };

        self.composite_target_commands(self.renderer.render_to_target(
            &self.gpu,
            &self.scene,
            &self.calibration,
            &viewport,
            self.calibration.topology.blend_width,
            correction,
            self.calibration.topology.multiband_blend_enabled,
            self.show_seam_line,
        ))
    }

    /// Enable 180-degree UV flip for the GPU zero-copy path.
    ///
    /// When set, the shader flips texture coordinates before sampling,
    /// equivalent to the CPU path's buffer reversal for rotated video
    /// (e.g., DJI cameras with rotation=180 metadata).
    pub fn set_flip_180(&mut self, left: bool, right: bool) {
        self.renderer.set_flip_180(left, right);
    }

    pub fn set_full_range(&mut self, full_range: bool) {
        self.renderer.set_full_range(full_range);
    }

    /// Access the rendered RGBA texture for NV12 conversion.
    pub fn render_target(&self) -> &wgpu::Texture {
        self.renderer.render_target()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::render::planes::copy_plane_tight;

    /// Build a test plane where row `r` contains byte value `r` for the first
    /// `width` bytes, followed by `0xFF` padding up to `stride`.
    fn padded_plane(width: u32, height: u32, stride: u32) -> Vec<u8> {
        let mut buf = vec![0xFF; (stride * height) as usize];
        for r in 0..height {
            for c in 0..width {
                buf[(r * stride + c) as usize] = r as u8;
            }
        }
        buf
    }

    #[test]
    fn copy_into_strips_row_padding() {
        // 4-pixel wide plane padded to 8-byte rows (typical OBS alignment).
        let y_data = padded_plane(4, 3, 8);
        let u_data = padded_plane(2, 2, 4);
        let v_data = padded_plane(2, 2, 4);
        let strided = StridedYuvPlanes {
            y: FramePlaneView {
                data: &y_data,
                stride: 8,
                width: 4,
                height: 3,
            },
            u: FramePlaneView {
                data: &u_data,
                stride: 4,
                width: 2,
                height: 2,
            },
            v: FramePlaneView {
                data: &v_data,
                stride: 4,
                width: 2,
                height: 2,
            },
        };

        let mut buffer = Vec::new();
        let tight = strided.copy_into(&mut buffer);

        assert_eq!(tight.y.len(), 12);
        assert_eq!(tight.u.len(), 4);
        assert_eq!(tight.v.len(), 4);
        // Row 0 should be [0,0,0,0], row 1 [1,1,1,1], etc - no 0xFF padding.
        assert_eq!(tight.y, &[0, 0, 0, 0, 1, 1, 1, 1, 2, 2, 2, 2]);
        assert_eq!(tight.u, &[0, 0, 1, 1]);
        assert_eq!(tight.v, &[0, 0, 1, 1]);
    }

    #[test]
    fn copy_into_fast_path_when_tight() {
        // stride == width means no padding - fast path takes a single memcpy.
        let y_data: Vec<u8> = (0..12).collect();
        let u_data: Vec<u8> = (0..4).collect();
        let v_data: Vec<u8> = (4..8).collect();
        let strided = StridedYuvPlanes {
            y: FramePlaneView {
                data: &y_data,
                stride: 4,
                width: 4,
                height: 3,
            },
            u: FramePlaneView {
                data: &u_data,
                stride: 2,
                width: 2,
                height: 2,
            },
            v: FramePlaneView {
                data: &v_data,
                stride: 2,
                width: 2,
                height: 2,
            },
        };
        let mut buffer = Vec::new();
        let tight = strided.copy_into(&mut buffer);
        assert_eq!(tight.y, y_data.as_slice());
        assert_eq!(tight.u, u_data.as_slice());
        assert_eq!(tight.v, v_data.as_slice());
    }

    #[test]
    fn copy_into_reuses_buffer_without_realloc() {
        let plane = padded_plane(4, 3, 8);
        let strided = StridedYuvPlanes {
            y: FramePlaneView {
                data: &plane,
                stride: 8,
                width: 4,
                height: 3,
            },
            u: FramePlaneView {
                data: &plane,
                stride: 8,
                width: 2,
                height: 2,
            },
            v: FramePlaneView {
                data: &plane,
                stride: 8,
                width: 2,
                height: 2,
            },
        };

        let mut buffer = Vec::with_capacity(64);
        let cap_before = buffer.capacity();
        let _tight = strided.copy_into(&mut buffer);
        // 12 + 4 + 4 = 20 bytes needed, 64 capacity, no realloc.
        assert_eq!(buffer.capacity(), cap_before);

        // Second call with same dims: still no realloc.
        let _tight2 = strided.copy_into(&mut buffer);
        assert_eq!(buffer.capacity(), cap_before);
    }

    // ── B-24 regression: copy_plane_tight must not panic on malformed input

    #[test]
    fn copy_plane_tight_handles_stride_less_than_width() {
        // Pathological: caller declares width=8 but stride=4.
        // Before B-24 this would overlap rows and panic on slice
        // index. Now it zero-fills and logs.
        let data = vec![0xAA_u8; 16]; // 4 rows * 4 stride
        let src = FramePlaneView {
            data: &data,
            stride: 4,
            width: 8,
            height: 4,
        };
        let mut dst = vec![0xFF_u8; 32]; // 8*4
        copy_plane_tight(&src, &mut dst);
        assert!(
            dst.iter().all(|&b| b == 0),
            "zero-fill expected on stride<width"
        );
    }

    #[test]
    fn copy_plane_tight_handles_short_source_buffer() {
        let data = vec![0x77_u8; 4]; // Way too small for 8*4 claim.
        let src = FramePlaneView {
            data: &data,
            stride: 8,
            width: 8,
            height: 4,
        };
        let mut dst = vec![0xFF_u8; 32];
        copy_plane_tight(&src, &mut dst);
        assert!(dst.iter().all(|&b| b == 0));
    }

    #[test]
    fn copy_plane_tight_handles_dst_size_mismatch() {
        let data = vec![0xAB_u8; 32];
        let src = FramePlaneView {
            data: &data,
            stride: 8,
            width: 8,
            height: 4,
        };
        let mut dst = vec![0xFF_u8; 16]; // half of what's claimed
        copy_plane_tight(&src, &mut dst);
        assert!(dst.iter().all(|&b| b == 0));
    }

    #[test]
    fn copy_plane_tight_still_fast_path_when_tight() {
        let data: Vec<u8> = (0..32).collect();
        let src = FramePlaneView {
            data: &data,
            stride: 8,
            width: 8,
            height: 4,
        };
        let mut dst = vec![0; 32];
        copy_plane_tight(&src, &mut dst);
        assert_eq!(dst.as_slice(), data.as_slice());
    }
}
