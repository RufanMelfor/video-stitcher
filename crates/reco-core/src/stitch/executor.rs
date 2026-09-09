//! Executor selection: one interface to stitch a frame on the GPU or the CPU.
//!
//! [`StitchExecutor`] is a deliberately narrow, synchronous contract -
//! "NV12 planes + pan -> RGBA bytes" - the common denominator both backends
//! produce naturally. It does NOT try to unify the GPU pipeline's specialised
//! paths (zero-copy import, triple-buffered streaming readback, GUI texture
//! handoff); those are GPU-only by nature and stay inherent to
//! [`GpuExecutor`], reached through [`crate::core::StitchCore`], which owns
//! one executor as its render substrate.
//!
//! - [`CpuExecutor`] binds a [`Projection`] and drives the pure-Rust gather.
//! - [`GpuExecutor`] owns the wgpu [`StitchPipeline`] (there is no other
//!   owner) plus a private blocking-readback ring for the synchronous
//!   contract.
//!
//! [`GpuExecutor`] lives behind the `gpu` feature (default-on);
//! [`CpuExecutor`] is unconditional - it is the render path for
//! wgpu-free builds.

use crate::calibration::{Calibration, Framing, Lens, Topology};
#[cfg(feature = "gpu")]
use crate::gpu::GpuContext;
#[cfg(feature = "gpu")]
use crate::gpu::rgba_readback::{RgbaReadback, RgbaReadbackError};
#[cfg(feature = "gpu")]
use crate::render::pipeline::{PipelineError, StitchPipeline};
use crate::render::planes::{Nv12Planes, YuvPlanes};
#[cfg(feature = "gpu")]
use crate::render::renderer::InputFormat;
use crate::render::scene::SceneGeometry;
use crate::render::viewport::ViewportConfig;

use crate::projection::Projection;

use super::cpu::stitch_rgba;

/// Errors a stitch executor can return.
///
/// `Clone` so the engine's error type (which wraps this) can stay
/// `Clone + Send + Sync` for worker-thread channels.
#[derive(Debug, Clone, thiserror::Error)]
pub enum StitchError {
    /// The GPU pipeline failed to record or upload a frame.
    #[cfg(feature = "gpu")]
    #[error("gpu pipeline: {0}")]
    Pipeline(#[from] PipelineError),
    /// The GPU readback failed.
    #[cfg(feature = "gpu")]
    #[error("gpu readback: {0}")]
    Readback(#[from] RgbaReadbackError),
    /// Backend configuration is invalid (e.g. degenerate dimensions).
    #[error("invalid stitch config: {0}")]
    InvalidConfig(String),
    /// A source plane is smaller than the configured frame size.
    #[error("frame size mismatch: plane has {actual} bytes, need at least {expected}")]
    FrameSizeMismatch {
        /// Minimum bytes the plane must contain for the configured dimensions.
        expected: usize,
        /// Bytes the supplied plane actually contains.
        actual: usize,
    },
}

/// One frame's stitch, GPU or CPU, behind a single interface.
///
/// Backends are configured for a fixed source size and output viewport at
/// construction; [`stitch`](Self::stitch) takes only the per-frame planes and
/// pan. Output is `width * height * 4` sRGB-domain RGBA, identical in layout
/// across backends (the GPU and CPU agree to ~1 LSB).
pub trait StitchExecutor {
    /// Stitch one NV12 frame pair to RGBA at the configured output size.
    fn stitch(
        &mut self,
        left: &Nv12Planes,
        right: &Nv12Planes,
        yaw: f32,
        pitch: f32,
    ) -> Result<Vec<u8>, StitchError>;

    /// Output dimensions `(width, height)` in pixels.
    fn output_dims(&self) -> (u32, u32);

    /// Short backend name for logs and diagnostics.
    fn name(&self) -> &'static str;
}

/// CPU software backend - pure Rust, no GPU. The portable / GPU-less path.
pub struct CpuExecutor {
    /// The bound projection: dispatches the per-frame surface maps.
    pub(crate) projection: Box<dyn Projection>,
    pub(crate) calib: Calibration,
    pub(crate) config: ViewportConfig,
    pub(crate) cam: (u32, u32),
    pub(crate) full_range: bool,
    /// Plane-placement geometry derived from `calib`, cached for the
    /// engine's coverage construction (the stitch kernel re-derives its
    /// own per call). Rebuilt by the [`Executor`] mutation methods.
    pub(crate) scene: SceneGeometry,
}

impl CpuExecutor {
    /// Configure a CPU executor: bind a projection to a fixed source size
    /// and output viewport.
    pub fn new(
        projection: Box<dyn Projection>,
        calib: Calibration,
        config: ViewportConfig,
        cam_w: u32,
        cam_h: u32,
        full_range: bool,
    ) -> Result<Self, StitchError> {
        calib
            .validate()
            .map_err(|e| StitchError::InvalidConfig(e.to_string()))?;
        config.validate().map_err(StitchError::InvalidConfig)?;
        if usize::from(projection.camera_count()) != calib.lenses.len() {
            return Err(StitchError::InvalidConfig(format!(
                "projection '{}' consumes {} cameras but the calibration has {} lenses",
                projection.name(),
                projection.camera_count(),
                calib.lenses.len()
            )));
        }
        if cam_w < 2 || cam_h < 2 {
            return Err(StitchError::InvalidConfig(format!(
                "source dimensions must be >= 2, got {cam_w}x{cam_h}"
            )));
        }
        let scene = derive_scene(&calib);
        Ok(Self {
            projection,
            calib,
            config,
            cam: (cam_w, cam_h),
            full_range,
            scene,
        })
    }

    /// Stitch one NV12 frame pair to RGBA at the configured output
    /// size. `&self` on purpose: the CPU stitch is stateless per call
    /// (the [`StitchExecutor`] trait's `&mut self` accommodates the
    /// GPU arm's readback ring).
    pub fn stitch_nv12(
        &self,
        left: &Nv12Planes<'_>,
        right: &Nv12Planes<'_>,
        yaw: f32,
        pitch: f32,
    ) -> Result<Vec<u8>, StitchError> {
        stitch_rgba(
            self.projection.as_ref(),
            left,
            right,
            self.cam,
            &self.calib,
            &self.config,
            yaw,
            pitch,
            self.full_range,
        )
    }

    /// Stitch one YUV420P frame pair to RGBA at the configured output
    /// size. The CPU kernel is format-flexible per call (unlike the
    /// GPU pipeline, which fixes its input format at construction);
    /// the [`StitchExecutor`] trait covers the NV12 contract, this
    /// inherent entry covers planar YUV sources (file decode).
    pub fn stitch_yuv(
        &self,
        left: &YuvPlanes<'_>,
        right: &YuvPlanes<'_>,
        yaw: f32,
        pitch: f32,
    ) -> Result<Vec<u8>, StitchError> {
        super::cpu::stitch_rgba_yuv420p(
            self.projection.as_ref(),
            left,
            right,
            self.cam,
            &self.calib,
            &self.config,
            yaw,
            pitch,
            self.full_range,
        )
    }
}

/// Plane-placement geometry for a calibration document (both stereo
/// cameras share the lens aspect). One derivation, shared by the CPU
/// executor's cache and its mutation paths - mirrors what
/// `StitchPipeline::update_calibration` does on the GPU side.
fn derive_scene(calib: &Calibration) -> SceneGeometry {
    let aspect = calib.lenses[0].width as f32 / calib.lenses[0].height as f32;
    SceneGeometry::new(&calib.topology, &calib.framing, aspect)
}

impl StitchExecutor for CpuExecutor {
    fn stitch(
        &mut self,
        left: &Nv12Planes,
        right: &Nv12Planes,
        yaw: f32,
        pitch: f32,
    ) -> Result<Vec<u8>, StitchError> {
        // Plane-size + dimension validation lives in stitch_rgba, which
        // returns a typed error instead of panicking on a short/truncated frame.
        self.stitch_nv12(left, right, yaw, pitch)
    }

    fn output_dims(&self) -> (u32, u32) {
        (self.config.width, self.config.height)
    }

    fn name(&self) -> &'static str {
        "cpu"
    }
}

/// Configuration for building a [`GpuExecutor`].
///
/// Owns everything the GPU pipeline needs to know about the frames it
/// will stitch: the calibration document, the output viewport, source
/// dimensions and pixel formats, and the projection. Engine-level
/// concerns (detection, trackers, replay) deliberately live on
/// [`StitchCore`](crate::core::StitchCore), not here.
#[cfg(feature = "gpu")]
pub struct GpuExecutorConfig {
    /// Camera calibration document.
    pub calibration: Calibration,
    /// Output viewport (dimensions, FOV).
    pub viewport: ViewportConfig,
    /// Input frame width in pixels (per camera).
    pub input_width: u32,
    /// Input frame height in pixels (per camera).
    pub input_height: u32,
    /// Input pixel format.
    pub input_format: InputFormat,
    /// GPU render-target format. `Rgba8Unorm` suits every compositor
    /// consumer; `Bgra8Unorm` matches native Windows DirectX surfaces
    /// for consumers that prefer to swizzle on upload instead of on
    /// readback.
    pub output_format: wgpu::TextureFormat,
    /// Projection to stitch through. `None` selects the two-camera
    /// L-shape ([`LShapeProjection`](crate::projection::LShapeProjection)).
    pub projection: Option<Box<dyn Projection>>,
    /// Whether source YUV uses full-range (JPEG) quantization.
    pub full_range: bool,
}

#[cfg(feature = "gpu")]
impl GpuExecutorConfig {
    /// New config with required fields only; defaults everywhere else
    /// (1080p viewport, `Rgba8Unorm` output, L-shape projection,
    /// limited-range YUV).
    pub fn new(
        calibration: Calibration,
        input_width: u32,
        input_height: u32,
        input_format: InputFormat,
    ) -> Self {
        Self {
            calibration,
            viewport: ViewportConfig {
                width: 1920,
                height: 1080,
                ..Default::default()
            },
            input_width,
            input_height,
            input_format,
            output_format: wgpu::TextureFormat::Rgba8Unorm,
            projection: None,
            full_range: false,
        }
    }
}

/// GPU executor - the sole owner of the wgpu [`StitchPipeline`] and of
/// the [`Projection`] bound to it.
///
/// [`StitchCore`](crate::core::StitchCore) holds one of these as its
/// render substrate and drives the streaming paths (pipelined readback,
/// zero-copy imports, preview-to-view) through it. The synchronous
/// `stitch()` path (crate-internal until the executor trait goes
/// public) renders one frame and blocks on a private readback ring,
/// for callers that want "planes in, RGBA out" with no pipelining.
#[cfg(feature = "gpu")]
pub struct GpuExecutor {
    pub(crate) pipeline: StitchPipeline,
    /// The bound projection: supplied the pipeline's GPU program at
    /// construction and dispatches coverage construction for the engine.
    pub(crate) projection: Box<dyn Projection>,
    /// Readback ring for the synchronous [`StitchExecutor::stitch`]
    /// path, created on first use so engine-embedded executors (which
    /// read back through the engine's own pipelined ring) never
    /// allocate it. Keyed by the output dims it was built for so a
    /// resize recreates it.
    sync_readback: Option<(RgbaReadback, (u32, u32))>,
}

#[cfg(feature = "gpu")]
impl GpuExecutor {
    /// Build a GPU executor. `gpu` is injected so reco-core does not
    /// pull an async runtime into non-test code; callers create it via
    /// [`GpuContext::new`].
    pub fn new(gpu: GpuContext, config: GpuExecutorConfig) -> Result<Self, StitchError> {
        let projection: Box<dyn Projection> = config
            .projection
            .unwrap_or_else(|| Box::new(crate::projection::LShapeProjection));
        if usize::from(projection.camera_count()) != config.calibration.lenses.len() {
            return Err(StitchError::InvalidConfig(format!(
                "projection '{}' consumes {} cameras but the calibration has {} lenses",
                projection.name(),
                projection.camera_count(),
                config.calibration.lenses.len()
            )));
        }
        log::info!(
            "GpuExecutor: projection '{}' supplies the GPU program and coverage",
            projection.name()
        );
        // Calibration validation happens once, inside with_gpu.
        let mut pipeline = StitchPipeline::with_gpu(
            gpu,
            &projection.gpu_program(),
            config.calibration,
            config.viewport,
            config.input_width,
            config.input_height,
            config.output_format,
            config.input_format,
        )?;
        pipeline.set_full_range(config.full_range);
        Ok(Self {
            pipeline,
            projection,
            sync_readback: None,
        })
    }
}

#[cfg(feature = "gpu")]
impl StitchExecutor for GpuExecutor {
    fn stitch(
        &mut self,
        left: &Nv12Planes,
        right: &Nv12Planes,
        yaw: f32,
        pitch: f32,
    ) -> Result<Vec<u8>, StitchError> {
        // The synchronous contract is NV12-specific; the underlying
        // upload only debug-asserts the format, so guard it here with
        // a typed error instead of corrupting textures in release.
        if self.pipeline.input_format() != InputFormat::Nv12 {
            return Err(StitchError::InvalidConfig(format!(
                "stitch() consumes NV12 planes but the executor was built \
                 for {:?} input",
                self.pipeline.input_format()
            )));
        }
        // (Re)create the private ring on first use or after a resize.
        let dims = self.output_dims();
        if !matches!(&self.sync_readback, Some((_, d)) if *d == dims) {
            let ring = RgbaReadback::new(self.pipeline.gpu(), dims.0, dims.1)?;
            self.sync_readback = Some((ring, dims));
        }
        // Record the frame, submit it via the readback, then drain it
        // synchronously: one render in, this frame's RGBA out.
        let cmd = self
            .pipeline
            .render_to_target_nv12(left, right, yaw, pitch)?;
        let tex = self.pipeline.render_target();
        let (ring, _) = self.sync_readback.as_mut().expect("created above");
        ring.readback(self.pipeline.gpu(), tex, cmd)?;
        // A frame was just submitted, so flush_pending always drains it.
        let frame = ring
            .flush_pending(self.pipeline.gpu())?
            .expect("flush_pending yields the just-submitted frame");
        Ok(frame.to_vec())
    }

    fn output_dims(&self) -> (u32, u32) {
        let v = self.pipeline.viewport();
        (v.width, v.height)
    }

    fn name(&self) -> &'static str {
        "gpu"
    }
}

/// The closed executor set the engine dispatches over (L2): one CPU
/// software path, one GPU pipeline owner.
///
/// [`StitchCore`](crate::core::StitchCore) holds exactly one and
/// resolves every render + live-config operation through it. The
/// GPU-only streaming surface (command-buffer renders, resident-frame
/// imports, readback rings) is reached via [`Executor::gpu`] /
/// [`Executor::gpu_mut`] - a typed accessor, no downcasting.
///
/// The live-config methods dispatch per arm: the GPU arm forwards to
/// the pipeline's update machinery (uniforms, scene rebuild), the CPU
/// arm mutates the executor's own document and rebuilds its cached
/// scene - the stitch kernel reads the document per call, so there is
/// no second copy to drift.
pub enum Executor {
    /// Pure-Rust software stitch - the GPU-less path. Boxed (like the
    /// GPU arm) so the enum itself stays pointer-sized inside the
    /// engine.
    Cpu(Box<CpuExecutor>),
    /// wgpu pipeline owner - the streaming path. Boxed: the pipeline
    /// state dwarfs the CPU variant and the enum lives inside every
    /// engine.
    #[cfg(feature = "gpu")]
    Gpu(Box<GpuExecutor>),
}

impl Executor {
    /// The GPU executor, when this is the GPU strategy - the typed
    /// accessor to the streaming/zero-copy surface.
    #[cfg(feature = "gpu")]
    pub fn gpu(&self) -> Option<&GpuExecutor> {
        match self {
            Executor::Gpu(g) => Some(g),
            Executor::Cpu(_) => None,
        }
    }

    /// Mutable [`Self::gpu`].
    #[cfg(feature = "gpu")]
    pub fn gpu_mut(&mut self) -> Option<&mut GpuExecutor> {
        match self {
            Executor::Gpu(g) => Some(g),
            Executor::Cpu(_) => None,
        }
    }

    /// The active calibration document.
    pub fn calibration(&self) -> &Calibration {
        match self {
            Executor::Cpu(c) => &c.calib,
            #[cfg(feature = "gpu")]
            Executor::Gpu(g) => g.pipeline.calibration(),
        }
    }

    /// The output viewport (dimensions + FOV).
    pub fn viewport(&self) -> &ViewportConfig {
        match self {
            Executor::Cpu(c) => &c.config,
            #[cfg(feature = "gpu")]
            Executor::Gpu(g) => g.pipeline.viewport(),
        }
    }

    /// The derived plane-placement geometry for the active calibration.
    pub fn scene(&self) -> &SceneGeometry {
        match self {
            Executor::Cpu(c) => &c.scene,
            #[cfg(feature = "gpu")]
            Executor::Gpu(g) => &g.pipeline.scene,
        }
    }

    /// The bound projection.
    pub fn projection(&self) -> &dyn Projection {
        match self {
            Executor::Cpu(c) => c.projection.as_ref(),
            #[cfg(feature = "gpu")]
            Executor::Gpu(g) => g.projection.as_ref(),
        }
    }

    /// Source frame dimensions `(width, height)` per camera.
    pub fn source_info(&self) -> (u32, u32) {
        match self {
            Executor::Cpu(c) => c.cam,
            #[cfg(feature = "gpu")]
            Executor::Gpu(g) => g.pipeline.source_info(),
        }
    }

    /// Current vertical field of view in degrees.
    pub fn fov(&self) -> f32 {
        match self {
            Executor::Cpu(c) => c.config.fov_degrees,
            #[cfg(feature = "gpu")]
            Executor::Gpu(g) => g.pipeline.fov(),
        }
    }

    /// Set the vertical field of view in degrees, clamped to
    /// `[1.0, 179.0]` on both arms (the CPU projection math degenerates
    /// at 0/180 exactly like the GPU perspective matrix would).
    pub fn set_fov(&mut self, fov_degrees: f32) {
        match self {
            // Mirrors StitchPipeline::set_fov's clamp so the executors
            // cannot diverge on out-of-range input.
            Executor::Cpu(c) => c.config.fov_degrees = fov_degrees.clamp(1.0, 179.0),
            #[cfg(feature = "gpu")]
            Executor::Gpu(g) => g.pipeline.set_fov(fov_degrees),
        }
    }

    /// Resize the output viewport. Returns the accepted `(width,
    /// height)`, or `None` when the request was rejected (zero dim).
    pub fn resize(&mut self, width: u32, height: u32) -> Option<(u32, u32)> {
        match self {
            Executor::Cpu(c) => {
                if width == 0 || height == 0 {
                    log::warn!("resize({width}, {height}) ignored: dimensions must be non-zero");
                    return None;
                }
                c.config.width = width;
                c.config.height = height;
                Some((width, height))
            }
            #[cfg(feature = "gpu")]
            Executor::Gpu(g) => g.pipeline.resize(width, height),
        }
    }

    /// Set the seam blend width (document field; no geometry rebuild).
    pub fn set_blend_width(&mut self, width: f32) {
        match self {
            Executor::Cpu(c) => c.calib.topology.blend_width = width,
            #[cfg(feature = "gpu")]
            Executor::Gpu(g) => g.pipeline.set_blend_width(width),
        }
    }

    /// Show/hide the seam-position debug line. A no-op on the CPU
    /// executor - it's a headless correctness oracle with no interactive
    /// preview to draw a debug overlay onto.
    pub fn set_show_seam_line(&mut self, show: bool) {
        match self {
            Executor::Cpu(_) => {}
            #[cfg(feature = "gpu")]
            Executor::Gpu(g) => g.pipeline.set_show_seam_line(show),
        }
    }

    /// Set the universal color grade (brightness/saturation/gamma). A
    /// no-op on the CPU executor - it's a headless correctness oracle
    /// with no GPU compute pass to run the color grade shader on.
    pub fn set_color_grade(&mut self, brightness: f32, saturation: f32, gamma: f32) {
        match self {
            Executor::Cpu(_) => {}
            #[cfg(feature = "gpu")]
            Executor::Gpu(g) => g.pipeline.set_color_grade(brightness, saturation, gamma),
        }
    }

    /// Set unsharp-mask sharpening strength/radius. A no-op on the CPU
    /// executor - it's a headless correctness oracle with no GPU compute
    /// pass to run the sharpen shader on.
    pub fn set_sharpen_params(&mut self, amount: f32, radius: f32) {
        match self {
            Executor::Cpu(_) => {}
            #[cfg(feature = "gpu")]
            Executor::Gpu(g) => g.pipeline.set_sharpen_params(amount, radius),
        }
    }

    /// Set the lens-correction strength on every lens, clamped to `[0, 1]`.
    pub fn set_lens_correction_amount(&mut self, amount: f32) {
        match self {
            Executor::Cpu(c) => {
                let amount = amount.clamp(0.0, 1.0);
                for lens in &mut c.calib.lenses {
                    lens.correction = amount;
                }
            }
            #[cfg(feature = "gpu")]
            Executor::Gpu(g) => g.pipeline.set_lens_correction_amount(amount),
        }
    }

    /// Replace the whole calibration document, rebuilding derived geometry.
    pub fn update_calibration(&mut self, calibration: Calibration) {
        match self {
            Executor::Cpu(c) => {
                c.scene = derive_scene(&calibration);
                c.calib = calibration;
            }
            #[cfg(feature = "gpu")]
            Executor::Gpu(g) => g.pipeline.update_calibration(calibration),
        }
    }

    /// Replace the topology (plane placement + seam), rebuilding geometry.
    pub fn update_topology(&mut self, topology: Topology) {
        match self {
            Executor::Cpu(c) => {
                c.calib.topology = topology;
                c.scene = derive_scene(&c.calib);
            }
            #[cfg(feature = "gpu")]
            Executor::Gpu(g) => g.pipeline.update_topology(topology),
        }
    }

    /// Replace the framing (axis offset, tilt, roll), rebuilding geometry.
    pub fn update_framing(&mut self, framing: Framing) {
        match self {
            Executor::Cpu(c) => {
                c.calib.framing = framing;
                c.scene = derive_scene(&c.calib);
            }
            #[cfg(feature = "gpu")]
            Executor::Gpu(g) => g.pipeline.update_framing(framing),
        }
    }

    /// Replace one or both cameras' intrinsics, rebuilding geometry.
    pub fn update_camera_params(&mut self, left: Option<Lens>, right: Option<Lens>) {
        match self {
            Executor::Cpu(c) => {
                if let Some(l) = left {
                    c.calib.lenses[0] = l;
                }
                if let Some(r) = right {
                    c.calib.lenses[1] = r;
                }
                c.scene = derive_scene(&c.calib);
            }
            #[cfg(feature = "gpu")]
            Executor::Gpu(g) => g.pipeline.update_camera_params(left, right),
        }
    }
}

impl StitchExecutor for Executor {
    fn stitch(
        &mut self,
        left: &Nv12Planes,
        right: &Nv12Planes,
        yaw: f32,
        pitch: f32,
    ) -> Result<Vec<u8>, StitchError> {
        match self {
            Executor::Cpu(c) => c.stitch(left, right, yaw, pitch),
            #[cfg(feature = "gpu")]
            Executor::Gpu(g) => g.stitch(left, right, yaw, pitch),
        }
    }

    fn output_dims(&self) -> (u32, u32) {
        match self {
            Executor::Cpu(c) => c.output_dims(),
            #[cfg(feature = "gpu")]
            Executor::Gpu(g) => g.output_dims(),
        }
    }

    fn name(&self) -> &'static str {
        match self {
            Executor::Cpu(c) => c.name(),
            #[cfg(feature = "gpu")]
            Executor::Gpu(g) => g.name(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::stitch::test_support::calib;
    #[cfg(feature = "gpu")]
    use crate::stitch::test_support::{Agreement, AgreementBounds, gpu_or_skip, nv12};

    /// The no-black guarantee, end to end: flat mid-grey input pushed
    /// through the real render path (`safe_clamp` -> `world_to_render_pose`
    /// -> CPU stitch) must not produce black (uncovered) pixels, even at
    /// extreme clamped targets on tilted/rolled rigs. Before the
    /// roll-aware clamp margins, the axis-aligned margin model leaked
    /// 4-9% black at ~19 deg tilt (worse zoomed in); the residual bound
    /// here covers slice-resolution imprecision only (#334, measured
    /// 0.34% worst-case independent of tilt/roll).
    ///
    /// CPU-only on purpose: the property is about the clamp + pose
    /// geometry, which the GPU shares verbatim via `l_shape_plane_maps`.
    #[test]
    fn clamped_poses_render_no_black_edges() {
        use crate::geometry::VirtualCamera;
        use crate::geometry::resolve_render_pose;
        use crate::projection::CoverageBoundary;
        use crate::render::scene::SceneGeometry;

        let (cam_w, cam_h) = (256u32, 144u32);
        let (out_w, out_h) = (192u32, 108u32);
        let gray_y = vec![128u8; (cam_w * cam_h) as usize];
        let gray_uv = vec![128u8; (cam_w * (cam_h / 2)) as usize];
        let planes = Nv12Planes {
            y: &gray_y,
            uv: &gray_uv,
        };
        let aspect_out = out_w as f32 / out_h as f32;
        let black_frac = |rgba: &[u8]| {
            let black = rgba
                .chunks_exact(4)
                .filter(|p| p[0] < 2 && p[1] < 2 && p[2] < 2)
                .count();
            black as f64 / (out_w * out_h) as f64
        };

        // (tilt, roll, fov as a fraction of the coverage max): level rig,
        // moderate and gameday tilt, tilt+roll, and the zoomed-in regime
        // where the rotated-corner overhang is proportionally largest.
        for &(tilt, roll, fov_factor) in &[
            (0.0f64, 0.0f64, 0.9f32),
            (0.15, 0.0, 0.9),
            (0.33, 0.0, 0.9),
            (0.33, 0.12, 0.9),
            (0.33, 0.12, 0.5),
        ] {
            let mut cal = calib(cam_w, cam_h);
            cal.framing.tilt = tilt;
            cal.framing.roll = roll;
            let plane_aspect = cal.lenses[0].width as f32 / cal.lenses[0].height as f32;
            let scene = SceneGeometry::new(&cal.topology, &cal.framing, plane_aspect);
            let coverage = CoverageBoundary::from_calibration(&cal, &scene);
            let cam = VirtualCamera::new(&scene.camera_position);
            let fov = (coverage.max_fov_degrees() * fov_factor).min(60.0);
            let config = ViewportConfig {
                width: out_w,
                height: out_h,
                fov_degrees: fov,
            };
            let mut backend = CpuExecutor::new(
                Box::new(crate::projection::LShapeProjection),
                cal.clone(),
                config,
                cam_w,
                cam_h,
                false,
            )
            .expect("cpu");

            for &(wy, wp) in &[
                (0.0f32, 0.0f32),
                (-3.0, -1.5),
                (-3.0, 1.5),
                (3.0, -1.5),
                (3.0, 1.5),
                (0.0, -1.5),
                (0.0, 1.5),
                (-3.0, 0.0),
                (3.0, 0.0),
            ] {
                // Through the real authority (clamp + orient in one call),
                // so this test tracks any stage it grows in Steps 6-8.
                let (ry, rp) = resolve_render_pose(
                    &coverage,
                    &cam,
                    tilt as f32,
                    roll as f32,
                    wy,
                    wp,
                    fov,
                    aspect_out,
                );
                let frac = black_frac(&backend.stitch(&planes, &planes, ry, rp).unwrap());
                assert!(
                    frac < 0.01,
                    "black fraction {frac:.4} at tilt={tilt} roll={roll} fov={fov:.1} target=({wy},{wp})"
                );
            }
        }
    }

    #[test]
    fn cpu_backend_reports_dims_and_name() {
        let (w, h) = (64u32, 36u32);
        let backend = CpuExecutor::new(
            Box::new(crate::projection::LShapeProjection),
            calib(w, h),
            ViewportConfig {
                width: w,
                height: h,
                ..Default::default()
            },
            w,
            h,
            false,
        )
        .expect("cpu backend");
        assert_eq!(backend.output_dims(), (w, h));
        assert_eq!(backend.name(), "cpu");
    }

    #[test]
    fn cpu_backend_rejects_undersized_planes() {
        let (w, h) = (64u32, 36u32);
        let mut backend = CpuExecutor::new(
            Box::new(crate::projection::LShapeProjection),
            calib(w, h),
            ViewportConfig {
                width: w,
                height: h,
                ..Default::default()
            },
            w,
            h,
            false,
        )
        .expect("cpu backend");
        let short = vec![0u8; 10];
        let planes = Nv12Planes {
            y: &short,
            uv: &short,
        };
        // Must return a typed error, not panic (matches the GPU backend).
        let err = backend.stitch(&planes, &planes, 0.0, 0.0).unwrap_err();
        assert!(matches!(err, StitchError::FrameSizeMismatch { .. }));
    }

    /// The zero-copy path measures and corrects color, end to end.
    ///
    /// This is the whole point of the GPU band gather, and it is the one
    /// thing no CPU-side test can show: it drives `render_imported_views`
    /// with externally-owned NV12 textures - the same entry point the
    /// D3D11VA decoder uses - and checks that a real brightness
    /// difference between the two cameras ends up in the smoothed
    /// correction. Before the gather this path always rendered with
    /// identity, which is exactly the bug the user hit on a whole
    /// season's exports.
    ///
    /// Iterates a few times because the readback is deliberately
    /// asynchronous: the measurement dispatched on one frame is consumed
    /// one or two frames later.
    #[test]
    #[cfg(feature = "gpu")]
    fn zero_copy_path_measures_and_corrects_color() {
        let Some(gpu) = gpu_or_skip() else {
            return;
        };
        let (cam_w, cam_h) = (192u32, 108u32);
        let (out_w, out_h) = (160u32, 90u32);

        let mut cal = calib(cam_w, cam_h);
        cal.topology.color_match_enabled = true;
        // Converge in one measurement instead of waiting out the EMA, and
        // keep the clamp out of the way of a deliberately large gap.
        cal.topology.color_match_interval_frames = 1;
        cal.topology.color_match_ema_alpha = 1.0;
        cal.topology.color_match_max_y_offset = 0.5;
        cal.topology.color_match_max_chroma_offset = 0.5;

        let mut exec = GpuExecutor::new(
            gpu,
            GpuExecutorConfig {
                viewport: ViewportConfig {
                    width: out_w,
                    height: out_h,
                    ..Default::default()
                },
                ..GpuExecutorConfig::new(cal, cam_w, cam_h, InputFormat::Nv12)
            },
        )
        .expect("gpu backend");

        // Externally-owned NV12 planes, as the decoder would hand over:
        // a flat, clearly different luma per camera, neutral chroma.
        let make_plane = |gpu: &crate::gpu::GpuContext,
                          w: u32,
                          h: u32,
                          format: wgpu::TextureFormat,
                          bytes_per_texel: u32,
                          fill: &[u8],
                          label: &str| {
            let texture = gpu.device().create_texture(&wgpu::TextureDescriptor {
                label: Some(label),
                size: wgpu::Extent3d {
                    width: w,
                    height: h,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format,
                usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
                view_formats: &[],
            });
            let row = bytes_per_texel * w;
            let data: Vec<u8> = fill
                .iter()
                .copied()
                .cycle()
                .take((row * h) as usize)
                .collect();
            gpu.queue().write_texture(
                wgpu::TexelCopyTextureInfo {
                    texture: &texture,
                    mip_level: 0,
                    origin: wgpu::Origin3d::ZERO,
                    aspect: wgpu::TextureAspect::All,
                },
                &data,
                wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(row),
                    rows_per_image: Some(h),
                },
                wgpu::Extent3d {
                    width: w,
                    height: h,
                    depth_or_array_layers: 1,
                },
            );
            texture
        };

        let gpu_ref = exec.pipeline.gpu();
        let left_y_tex = make_plane(
            gpu_ref,
            cam_w,
            cam_h,
            wgpu::TextureFormat::R8Unorm,
            1,
            &[160],
            "test_left_y",
        );
        let right_y_tex = make_plane(
            gpu_ref,
            cam_w,
            cam_h,
            wgpu::TextureFormat::R8Unorm,
            1,
            &[120],
            "test_right_y",
        );
        let left_uv_tex = make_plane(
            gpu_ref,
            cam_w / 2,
            cam_h / 2,
            wgpu::TextureFormat::Rg8Unorm,
            2,
            &[128, 128],
            "test_left_uv",
        );
        let right_uv_tex = make_plane(
            gpu_ref,
            cam_w / 2,
            cam_h / 2,
            wgpu::TextureFormat::Rg8Unorm,
            2,
            &[128, 128],
            "test_right_uv",
        );
        let view = |t: &wgpu::Texture| t.create_view(&wgpu::TextureViewDescriptor::default());
        let (ly, ry, luv, ruv) = (
            view(&left_y_tex),
            view(&right_y_tex),
            view(&left_uv_tex),
            view(&right_uv_tex),
        );

        for _ in 0..8 {
            let cmd = exec
                .pipeline
                .render_imported_views(&ly, &luv, &ry, &ruv, 0.0, 0.0);
            let gpu_ref = exec.pipeline.gpu();
            gpu_ref.queue().submit(std::iter::once(cmd));
            let _ = gpu_ref.device().poll(wgpu::PollType::wait_indefinitely());
        }

        let c = exec.pipeline.color_match_correction();
        assert!(
            c.left_offset[0] < -0.02,
            "left camera is the brighter one and must be pulled down, got {:?}",
            c.left_offset
        );
        assert!(
            c.right_offset[0] > 0.02,
            "right camera is the darker one and must be lifted, got {:?}",
            c.right_offset
        );
        assert!(
            (c.left_offset[0] + c.right_offset[0]).abs() < 1e-3,
            "both cameras aim at their shared mean, so the offsets mirror: {:?} vs {:?}",
            c.left_offset,
            c.right_offset
        );
    }

    /// A 180-degree-rotated source is measured where it is *drawn*, not
    /// where it is stored.
    ///
    /// The zero-copy path renders a rotated camera by sampling at `1-uv`
    /// rather than by reversing the buffer, so a gather that reads raw
    /// texels has to mirror its sample positions to match. Caught on real
    /// DJI footage, where the left camera carries `rotation=-180` and the
    /// right does not: the correction came out about twice its true size
    /// and jittered, because the left camera's "seam band" samples were
    /// actually being taken from the far side of the field.
    ///
    /// The left plane is a horizontal ramp, so the band's own position
    /// decides the measured mean. If the flip were ignored, both runs
    /// below would sample identical texels and produce identical
    /// corrections - which is exactly what this asserts against.
    #[test]
    #[cfg(feature = "gpu")]
    fn a_rotated_camera_is_measured_where_it_is_drawn() {
        let measure_with_flip = |flip_left: bool| -> [f32; 3] {
            let Some(gpu) = gpu_or_skip() else {
                panic!("GPU required for this test - RECO_REQUIRE_GPU or run manually");
            };
            let (cam_w, cam_h) = (192u32, 108u32);
            let (out_w, out_h) = (160u32, 90u32);

            let mut cal = calib(cam_w, cam_h);
            cal.topology.color_match_enabled = true;
            cal.topology.color_match_interval_frames = 1;
            cal.topology.color_match_ema_alpha = 1.0;
            cal.topology.color_match_max_y_offset = 0.5;
            cal.topology.color_match_max_chroma_offset = 0.5;

            let mut exec = GpuExecutor::new(
                gpu,
                GpuExecutorConfig {
                    viewport: ViewportConfig {
                        width: out_w,
                        height: out_h,
                        ..Default::default()
                    },
                    ..GpuExecutorConfig::new(cal, cam_w, cam_h, InputFormat::Nv12)
                },
            )
            .expect("gpu backend");
            exec.pipeline.set_flip_180(flip_left, false);

            let gpu_ref = exec.pipeline.gpu();
            let upload = |w: u32, h: u32, format, bpt: u32, data: Vec<u8>, label: &str| {
                let texture = gpu_ref.device().create_texture(&wgpu::TextureDescriptor {
                    label: Some(label),
                    size: wgpu::Extent3d {
                        width: w,
                        height: h,
                        depth_or_array_layers: 1,
                    },
                    mip_level_count: 1,
                    sample_count: 1,
                    dimension: wgpu::TextureDimension::D2,
                    format,
                    usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
                    view_formats: &[],
                });
                gpu_ref.queue().write_texture(
                    wgpu::TexelCopyTextureInfo {
                        texture: &texture,
                        mip_level: 0,
                        origin: wgpu::Origin3d::ZERO,
                        aspect: wgpu::TextureAspect::All,
                    },
                    &data,
                    wgpu::TexelCopyBufferLayout {
                        offset: 0,
                        bytes_per_row: Some(bpt * w),
                        rows_per_image: Some(h),
                    },
                    wgpu::Extent3d {
                        width: w,
                        height: h,
                        depth_or_array_layers: 1,
                    },
                );
                texture
            };

            // Left: dark on one side, bright on the other, so mirroring
            // the band changes the mean by a lot.
            let ramp: Vec<u8> = (0..cam_h)
                .flat_map(|_| (0..cam_w).map(|x| (40 + (x * 160 / cam_w)) as u8))
                .collect();
            let left_y = upload(
                cam_w,
                cam_h,
                wgpu::TextureFormat::R8Unorm,
                1,
                ramp,
                "flip_left_y",
            );
            let right_y = upload(
                cam_w,
                cam_h,
                wgpu::TextureFormat::R8Unorm,
                1,
                vec![120u8; (cam_w * cam_h) as usize],
                "flip_right_y",
            );
            let neutral_uv = vec![128u8; (cam_w * cam_h / 2) as usize];
            let left_uv = upload(
                cam_w / 2,
                cam_h / 2,
                wgpu::TextureFormat::Rg8Unorm,
                2,
                neutral_uv.clone(),
                "flip_left_uv",
            );
            let right_uv = upload(
                cam_w / 2,
                cam_h / 2,
                wgpu::TextureFormat::Rg8Unorm,
                2,
                neutral_uv,
                "flip_right_uv",
            );
            let view = |t: &wgpu::Texture| t.create_view(&wgpu::TextureViewDescriptor::default());
            let (ly, ry, luv, ruv) = (
                view(&left_y),
                view(&right_y),
                view(&left_uv),
                view(&right_uv),
            );

            for _ in 0..8 {
                let cmd = exec
                    .pipeline
                    .render_imported_views(&ly, &luv, &ry, &ruv, 0.0, 0.0);
                let g = exec.pipeline.gpu();
                g.queue().submit(std::iter::once(cmd));
                let _ = g.device().poll(wgpu::PollType::wait_indefinitely());
            }
            exec.pipeline.color_match_correction().left_offset
        };

        let unflipped = measure_with_flip(false);
        let flipped = measure_with_flip(true);
        assert!(
            (unflipped[0] - flipped[0]).abs() > 0.01,
            "the flip must move the sampled band; identical corrections mean the gather \
             ignored it: {unflipped:?} vs {flipped:?}"
        );
    }

    /// The buffered/lookahead render path measures the seam band too.
    ///
    /// `render_with_bind_groups` renders from pre-built bind groups,
    /// which cannot be sampled, so it never reaches
    /// `render_imported_views`'s gather call. Every export with
    /// lookahead / AI tracking enabled takes that path (the VRAM pool
    /// branch of `frame_processing`), so the automatic color match was
    /// silently identity there - the same bug hardware decode already
    /// had, resurfacing on a second render path after the first was
    /// fixed. Found from a real user export where the preview (immediate
    /// path) matched correctly and the exported file did not.
    ///
    /// Asserts both halves of the contract in one test: the `_measured`
    /// form produces a real correction, and the plain form still does
    /// not - so this fails if the gather is ever dropped from the
    /// buffered path again, *and* documents why the plain form exists.
    #[test]
    #[cfg(feature = "gpu")]
    fn the_buffered_bind_group_path_measures_the_seam_band() {
        let Some(gpu) = gpu_or_skip() else {
            panic!("GPU required for this test - RECO_REQUIRE_GPU or run manually");
        };
        let (cam_w, cam_h) = (192u32, 108u32);
        let (out_w, out_h) = (160u32, 90u32);

        let mut cal = calib(cam_w, cam_h);
        cal.topology.color_match_enabled = true;
        cal.topology.color_match_interval_frames = 1;
        cal.topology.color_match_ema_alpha = 1.0;
        cal.topology.color_match_max_y_offset = 0.5;
        cal.topology.color_match_max_chroma_offset = 0.5;

        let mut exec = GpuExecutor::new(
            gpu,
            GpuExecutorConfig {
                viewport: ViewportConfig {
                    width: out_w,
                    height: out_h,
                    ..Default::default()
                },
                ..GpuExecutorConfig::new(cal, cam_w, cam_h, InputFormat::Nv12)
            },
        )
        .expect("gpu backend");

        let gpu_ref = exec.pipeline.gpu();
        let upload = |w: u32, h: u32, format, bpt: u32, data: Vec<u8>, label: &str| {
            let texture = gpu_ref.device().create_texture(&wgpu::TextureDescriptor {
                label: Some(label),
                size: wgpu::Extent3d {
                    width: w,
                    height: h,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format,
                usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
                view_formats: &[],
            });
            gpu_ref.queue().write_texture(
                wgpu::TexelCopyTextureInfo {
                    texture: &texture,
                    mip_level: 0,
                    origin: wgpu::Origin3d::ZERO,
                    aspect: wgpu::TextureAspect::All,
                },
                &data,
                wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(bpt * w),
                    rows_per_image: Some(h),
                },
                wgpu::Extent3d {
                    width: w,
                    height: h,
                    depth_or_array_layers: 1,
                },
            );
            texture
        };

        // A large, unambiguous brightness split between the cameras, so
        // any real measurement produces a correction far above noise.
        let left_y = upload(
            cam_w,
            cam_h,
            wgpu::TextureFormat::R8Unorm,
            1,
            vec![200u8; (cam_w * cam_h) as usize],
            "buf_left_y",
        );
        let right_y = upload(
            cam_w,
            cam_h,
            wgpu::TextureFormat::R8Unorm,
            1,
            vec![60u8; (cam_w * cam_h) as usize],
            "buf_right_y",
        );
        let neutral_uv = vec![128u8; (cam_w * cam_h / 2) as usize];
        let left_uv = upload(
            cam_w / 2,
            cam_h / 2,
            wgpu::TextureFormat::Rg8Unorm,
            2,
            neutral_uv.clone(),
            "buf_left_uv",
        );
        let right_uv = upload(
            cam_w / 2,
            cam_h / 2,
            wgpu::TextureFormat::Rg8Unorm,
            2,
            neutral_uv,
            "buf_right_uv",
        );

        // Exactly what the VRAM pool holds per slot: bind groups to
        // render from, plus views over those same textures to measure.
        let left_bg = exec
            .pipeline
            .create_texture_bind_group(&left_y, &left_uv, "buf_left");
        let right_bg = exec
            .pipeline
            .create_texture_bind_group(&right_y, &right_uv, "buf_right");
        let view = |t: &wgpu::Texture| t.create_view(&wgpu::TextureViewDescriptor::default());
        let (ly, luv, ry, ruv) = (
            view(&left_y),
            view(&left_uv),
            view(&right_y),
            view(&right_uv),
        );

        // The plain form first: renders fine, must leave the correction
        // at identity because nothing ever sampled the frame.
        for _ in 0..8 {
            let cmd = exec
                .pipeline
                .render_with_bind_groups(&left_bg, &right_bg, 0.0, 0.0);
            let g = exec.pipeline.gpu();
            g.queue().submit(std::iter::once(cmd));
            let _ = g.device().poll(wgpu::PollType::wait_indefinitely());
        }
        let unmeasured = exec.pipeline.color_match_correction().left_offset;
        assert!(
            unmeasured[0].abs() < 1e-6,
            "render_with_bind_groups cannot sample anything, so the correction must stay \
             identity; got {unmeasured:?}"
        );

        // The measured form on the identical frame must now converge on
        // a real, non-trivial correction.
        for _ in 0..8 {
            let cmd = exec.pipeline.render_with_bind_groups_measured(
                &left_bg,
                &right_bg,
                (&ly, &luv, &ry, &ruv),
                0.0,
                0.0,
            );
            let g = exec.pipeline.gpu();
            g.queue().submit(std::iter::once(cmd));
            let _ = g.device().poll(wgpu::PollType::wait_indefinitely());
        }
        let measured = exec.pipeline.color_match_correction().left_offset;
        assert!(
            measured[0] < -0.02,
            "left camera is the bright one and must be pulled down by the buffered path's \
             own measurement; got {measured:?} (identity here means the lookahead/VRAM-pool \
             export path stopped measuring again)"
        );
    }

    /// The manual per-camera gamma actually reaches rendered pixels, and
    /// reaches only the camera it was set on.
    ///
    /// This is the mechanism the whole manual-correction feature rests on
    /// (a single `pow` in `fisheye.wgsl` driven by `color_scale.w`), and
    /// the most likely way to wire it wrong is to write the exponent into
    /// one plane's uniform and read it for both - which a whole-frame
    /// brightness check alone would not catch. Color match is disabled
    /// here on purpose: with it on, a one-sided gamma is partly corrected
    /// away, which is the intended interaction but hides what is being
    /// tested.
    #[test]
    #[cfg(feature = "gpu")]
    fn manual_gamma_reaches_rendered_pixels_per_camera() {
        let (cam_w, cam_h) = (192u32, 108u32);
        let (out_w, out_h) = (160u32, 90u32);
        // Flat mid-grey, not the usual gradient: gamma moves mid-tones
        // most, so a known 0.5-ish input gives an unambiguous shift.
        let grey_y = vec![128u8; (cam_w * cam_h) as usize];
        let grey_uv = vec![128u8; (cam_w * (cam_h / 2)) as usize];
        let planes = Nv12Planes {
            y: &grey_y,
            uv: &grey_uv,
        };

        let sample_at = |rgba: &[u8], frac_x: f32| -> u8 {
            let x = (out_w as f32 * frac_x) as u32;
            let y = out_h / 2;
            rgba[((y * out_w + x) * 4) as usize]
        };

        let render = |gamma_left: f32, gamma_right: f32| -> (u8, u8) {
            let Some(gpu) = gpu_or_skip() else {
                panic!("GPU required for this test - RECO_REQUIRE_GPU or run manually");
            };
            let mut cal = calib(cam_w, cam_h);
            cal.topology.color_match_enabled = false;
            cal.topology.color_gamma_left = gamma_left;
            cal.topology.color_gamma_right = gamma_right;
            let mut exec = GpuExecutor::new(
                gpu,
                GpuExecutorConfig {
                    viewport: ViewportConfig {
                        width: out_w,
                        height: out_h,
                        ..Default::default()
                    },
                    ..GpuExecutorConfig::new(cal, cam_w, cam_h, InputFormat::Nv12)
                },
            )
            .expect("gpu backend");
            let rgba = exec.stitch(&planes, &planes, 0.0, 0.0).expect("stitch");
            (sample_at(&rgba, 0.15), sample_at(&rgba, 0.85))
        };

        let (base_left, base_right) = render(1.0, 1.0);
        let (lifted_left, lifted_right) = render(2.0, 2.0);
        assert!(
            lifted_left as i32 - base_left as i32 >= 20
                && lifted_right as i32 - base_right as i32 >= 20,
            "gamma 2.0 on both cameras should visibly brighten both halves:              left {base_left} -> {lifted_left}, right {base_right} -> {lifted_right}"
        );

        let (only_right_left, only_right_right) = render(1.0, 2.0);
        assert!(
            (only_right_left as i32 - base_left as i32).abs() <= 2,
            "gamma on the right camera must leave the left half alone:              {base_left} -> {only_right_left}"
        );
        assert!(
            only_right_right as i32 - base_right as i32 >= 20,
            "gamma on the right camera must brighten the right half:              {base_right} -> {only_right_right}"
        );
    }

    /// Diagnostic for a user report: "auto color match looks broken (one
    /// whole side goes extremely bright/dark) once Seam blend is very
    /// narrow (0.01)". The measured L/R offset the user saw was tiny
    /// (+/-0.021), which shouldn't produce an extreme visual result if
    /// the correction is applied correctly - this checks whether the
    /// actual *rendered pixel* shift from color-match is consistent
    /// between a narrow and a wide blend_width, using a known, injected
    /// brightness bias between the two cameras (not a measurement of the
    /// user's real footage, which isn't available here).
    #[test]
    #[cfg(feature = "gpu")]
    fn color_match_pixel_shift_is_consistent_across_blend_width() {
        let (cam_w, cam_h) = (192u32, 108u32);
        let (out_w, out_h) = (160u32, 90u32);
        // Right camera's Y plane is uniformly 40/255 brighter than left's -
        // a known, sizeable bias for color-match to correct toward the
        // shared mean.
        let (ly, luv) = nv12(cam_w, cam_h, 0);
        let (ry, ruv) = nv12(cam_w, cam_h, 40);
        let left = Nv12Planes { y: &ly, uv: &luv };
        let right = Nv12Planes { y: &ry, uv: &ruv };
        let (yaw, pitch) = (0.0f32, 0.0f32);

        // Sample well away from the seam on the right half, same point
        // for every run so only blend_width/color_match differ.
        let sample_x = (out_w as f32 * 0.85) as u32;
        let sample_y = out_h / 2;
        let sample = |rgba: &[u8]| -> [u8; 3] {
            let i = ((sample_y * out_w + sample_x) * 4) as usize;
            [rgba[i], rgba[i + 1], rgba[i + 2]]
        };

        let render = |blend_width: f32, color_match_enabled: bool| -> [u8; 3] {
            let Some(gpu) = gpu_or_skip() else {
                panic!("GPU required for this diagnostic - RECO_REQUIRE_GPU or run manually");
            };
            let mut cal = calib(cam_w, cam_h);
            cal.topology.blend_width = blend_width;
            cal.topology.color_match_enabled = color_match_enabled;
            // Converge in one frame instead of waiting/averaging over many,
            // and don't let the safety clamp mask a real over-correction.
            cal.topology.color_match_interval_frames = 1;
            cal.topology.color_match_ema_alpha = 1.0;
            cal.topology.color_match_max_y_offset = 0.5;
            cal.topology.color_match_max_chroma_offset = 0.5;
            let config = ViewportConfig {
                width: out_w,
                height: out_h,
                ..Default::default()
            };
            let mut exec = GpuExecutor::new(
                gpu,
                GpuExecutorConfig {
                    viewport: config,
                    ..GpuExecutorConfig::new(cal, cam_w, cam_h, InputFormat::Nv12)
                },
            )
            .expect("gpu backend");
            // A few frames so the periodic measurement (interval=1) has
            // definitely run and the EMA (alpha=1) has fully converged.
            let mut rgba = exec.stitch(&left, &right, yaw, pitch).expect("stitch");
            for _ in 0..3 {
                rgba = exec.stitch(&left, &right, yaw, pitch).expect("stitch");
            }
            sample(&rgba)
        };

        let narrow_raw = render(0.001, false);
        let narrow_corrected = render(0.001, true);
        let wide_raw = render(0.2, false);
        let wide_corrected = render(0.2, true);

        let shift =
            |raw: [u8; 3], corrected: [u8; 3]| -> i32 { corrected[0] as i32 - raw[0] as i32 };
        let narrow_shift = shift(narrow_raw, narrow_corrected);
        let wide_shift = shift(wide_raw, wide_corrected);

        eprintln!(
            "narrow blend_width=0.001: raw={narrow_raw:?} corrected={narrow_corrected:?} shift={narrow_shift}\n\
             wide   blend_width=0.2:   raw={wide_raw:?} corrected={wide_corrected:?} shift={wide_shift}"
        );

        assert!(
            (narrow_shift - wide_shift).abs() <= 5,
            "color-match's pixel shift should be consistent regardless of blend_width - \
             narrow gave {narrow_shift}, wide gave {wide_shift} (diff {})",
            (narrow_shift - wide_shift).abs()
        );
    }

    /// Diagnostic for a follow-up user report: with the same fixed
    /// camera bias, the L/R color-match correction appeared to keep
    /// growing frame after frame (during export) instead of settling -
    /// this drives many repeated `stitch()` calls (realistic default
    /// `ema_alpha`/`measure_interval_frames`, not the instant-converge
    /// settings the other diagnostic uses) against an *unchanging* bias
    /// and checks whether the correction actually converges to a stable
    /// value, or keeps drifting/growing across iterations.
    #[test]
    #[cfg(feature = "gpu")]
    fn color_match_converges_not_diverges_under_repeated_ticks() {
        let Some(gpu) = gpu_or_skip() else {
            return;
        };
        let (cam_w, cam_h) = (192u32, 108u32);
        let (out_w, out_h) = (160u32, 90u32);
        // A modest, fixed 15/255 (~6%) bias - unchanging every call.
        let (ly, luv) = nv12(cam_w, cam_h, 0);
        let (ry, ruv) = nv12(cam_w, cam_h, 15);
        let left = Nv12Planes { y: &ly, uv: &luv };
        let right = Nv12Planes { y: &ry, uv: &ruv };

        let mut cal = calib(cam_w, cam_h);
        cal.topology.color_match_enabled = true;
        // Realistic defaults, not the instant-converge settings the
        // other diagnostic uses - this is what actually runs in the app.
        cal.topology.color_match_interval_frames = 15;
        cal.topology.color_match_ema_alpha = 0.15;
        cal.topology.color_match_max_y_offset = 0.06;
        cal.topology.color_match_max_chroma_offset = 0.04;
        let config = ViewportConfig {
            width: out_w,
            height: out_h,
            ..Default::default()
        };
        let mut exec = GpuExecutor::new(
            gpu,
            GpuExecutorConfig {
                viewport: config,
                ..GpuExecutorConfig::new(cal, cam_w, cam_h, InputFormat::Nv12)
            },
        )
        .expect("gpu backend");

        // Enough iterations to cross the 15-frame measure interval many
        // times over (~13 remeasures) - a converging EMA should be flat
        // by the end; a diverging one should be obviously still growing
        // or pinned at the clamp from way earlier than expected.
        let mut left_y_history = Vec::new();
        for i in 0..900 {
            exec.stitch(&left, &right, 0.0, 0.0).expect("stitch");
            if i % 60 == 0 || i == 899 {
                let c = exec.pipeline.color_match_correction();
                left_y_history.push((i, c.left_offset[0]));
            }
        }

        eprintln!("left Y offset over iterations: {left_y_history:?}");

        let last = left_y_history.last().unwrap().1;
        let second_last = left_y_history[left_y_history.len() - 2].1;
        assert!(
            (last - second_last).abs() < 0.0005,
            "correction should have settled by iteration ~840-900, still moving: \
             {second_last} -> {last} (diff {})",
            (last - second_last).abs()
        );
        assert!(
            last.abs() <= 0.06 + 1e-4,
            "converged value {last} exceeds the configured max_y_offset clamp"
        );
    }

    #[test]
    #[cfg(feature = "gpu")]
    fn cpu_and_gpu_backends_agree() {
        let Some(gpu) = gpu_or_skip() else {
            return;
        };

        let (cam_w, cam_h) = (192u32, 108u32);
        let (out_w, out_h) = (160u32, 90u32);
        let calib = calib(cam_w, cam_h);
        let config = ViewportConfig {
            width: out_w,
            height: out_h,
            ..Default::default()
        };
        let (ly, luv) = nv12(cam_w, cam_h, 0);
        let (ry, ruv) = nv12(cam_w, cam_h, 30);
        let left = Nv12Planes { y: &ly, uv: &luv };
        let right = Nv12Planes { y: &ry, uv: &ruv };
        let (yaw, pitch) = (0.08f32, -0.04f32);

        let mut cpu = CpuExecutor::new(
            Box::new(crate::projection::LShapeProjection),
            calib.clone(),
            config.clone(),
            cam_w,
            cam_h,
            false,
        )
        .expect("cpu backend");
        let mut gpu = GpuExecutor::new(
            gpu,
            GpuExecutorConfig {
                viewport: config,
                ..GpuExecutorConfig::new(calib, cam_w, cam_h, InputFormat::Nv12)
            },
        )
        .expect("gpu backend");

        // Drive both through the trait object to prove selection works.
        let backends: [&mut dyn StitchExecutor; 2] = [&mut cpu, &mut gpu];
        let mut outputs = Vec::new();
        for b in backends {
            assert_eq!(b.output_dims(), (out_w, out_h));
            outputs.push(b.stitch(&left, &right, yaw, pitch).expect("stitch"));
        }
        let (cpu_rgba, gpu_rgba) = (&outputs[0], &outputs[1]);
        assert_eq!(cpu_rgba.len(), (out_w * out_h * 4) as usize);
        Agreement::compare(gpu_rgba, cpu_rgba)
            .assert_within(AgreementBounds::DEFAULT, "backend cpu-vs-gpu");
    }
}
