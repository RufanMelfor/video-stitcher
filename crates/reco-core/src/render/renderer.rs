//! GPU renderer for the panoramic stitching pipeline.
//!
//! Manages wgpu render pipelines, textures, and bind groups for rendering
//! two fisheye-corrected camera planes into a stitched panoramic output.
//!
//! ## Pipeline
//!
//! ```text
//! YUV420P path:
//!   Left Y/U/V planes ──► 3 textures ──┐
//!                                       ├──► Render pass (YUV→RGB + fisheye) ──► RGBA output
//!   Right Y/U/V planes ──► 3 textures ──┘
//!
//! NV12 path:
//!   Left Y + UV planes ──► 2 textures ──┐
//!                                        ├──► Render pass (NV12→RGB + fisheye) ──► RGBA output
//!   Right Y + UV planes ──► 2 textures ──┘
//! ```
//!
//! Each plane is a textured quad positioned in 3D space (L-shape geometry).
//! YUV/NV12 to RGB conversion (BT.709), fisheye undistortion, and color
//! correction all happen in the fragment shader. Uploading YUV directly
//! reduces CPU-GPU transfer from 8.3 MB to 3.1 MB per frame (62% less
//! bandwidth) and eliminates CPU-side swscale color conversion entirely.

use super::scene::SceneGeometry;
use super::viewport::{ResolvedViewport, ViewportConfig};
use crate::calibration::{Calibration, Lens};
use crate::geometry::{
    FAR_PLANE, NEAR_PLANE, matrix4_to_columns, opengl_to_wgpu_matrix, view_matrix,
};
use crate::gpu::GpuContext;

use bytemuck::{Pod, Zeroable};
use nalgebra::{Matrix4, Perspective3, Vector4};
use std::cell::RefCell;
use thiserror::Error;
use wgpu::util::DeviceExt;

// ---- Constants ----

/// Errors from the renderer.
#[derive(Debug, Clone, Error)]
pub enum RenderError {
    /// Frame data has wrong size.
    #[error("frame data size mismatch: expected {expected} bytes, got {actual}")]
    FrameSizeMismatch { expected: usize, actual: usize },
}

// ---- GPU-side structs ----

/// Uniform buffer layout (must match `Uniforms` in fisheye.wgsl exactly).
#[repr(C)]
#[derive(Copy, Clone, Debug, Pod, Zeroable)]
pub(crate) struct GpuUniforms {
    mvp: [[f32; 4]; 4],
    intrinsics: [f32; 4],
    dist: [f32; 4],
    color_scale: [f32; 4],
    color_offset_blend: [f32; 4],
    flags: [u32; 4],
    pub(crate) lens_preview: [f32; 4],
    ground_tilt: [f32; 4],
    top_tilt: [f32; 4],
}

/// Per-plane ground-plane tilt correction for [`build_gpu_uniforms`] and
/// [`super::single_camera::SingleCameraRenderer::render_and_readback`] - see
/// `Topology::ground_tilt_x`/`ground_tilt_z`'s doc comment
/// (`crates/reco-core/src/calibration.rs`) and `fisheye.wgsl`'s
/// `band_limited_ground_warp` for the full picture. `k` and `band_full`
/// only matter when `tilt != 0.0`; `Default` (all zero) is the "no
/// correction" no-op - `band_full` defaulting to `0.0` instead of the real
/// `Topology::ground_tilt_band_width` default (`0.16`) is harmless here
/// since it's never read while `tilt == 0.0`. `pub` (not `pub(crate)`):
/// `render_and_readback` is called from other crates (`reco-calibrate`'s
/// validation examples), so this type has to be at least as visible as
/// that function.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct GroundTilt {
    pub tilt: f32,
    pub k: f32,
    /// `Topology::ground_tilt_band_width` - `|t|` at which the
    /// correction reaches full strength (the ramp always starts at the
    /// fixed `0.08`).
    pub band_full: f32,
}

/// Per-plane top-of-frame tilt correction, mirroring [`GroundTilt`] - see
/// `Topology::top_tilt_x`/`top_tilt_z`'s doc comment
/// (`crates/reco-core/src/calibration.rs`) and `fisheye.wgsl`'s
/// `band_limited_top_warp`. Unlike `GroundTilt`, this plane aspect ratio
/// doesn't need its own copy in the packed uniform - the shader reuses
/// `ground_tilt.z` for both bands, since it's the same plane's own aspect
/// ratio either way. `Default` (all zero) is the "no correction" no-op.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct TopTilt {
    pub tilt: f32,
    pub k: f32,
    /// `Topology::top_tilt_band_width` - see [`GroundTilt::band_full`].
    pub band_full: f32,
}

/// Per-plane YUV offset applied by `apply_color_transfer` in `fisheye.wgsl`,
/// computed by [`super::color_match`] from a seam-adjacent band of each
/// camera's raw frame. Nudges both cameras toward a shared mean color so an
/// exposure/white-balance mismatch between the two cameras doesn't show up
/// as a visible seam independent of geometric alignment. `Default` (both
/// zero) is a no-op - `apply_color_transfer` has an explicit identity
/// fast-path for `scale == 1 && offset == 0`. `pub` (not `pub(crate)`):
/// [`super::stitch_renderer::StitchRenderer::color_match_correction`]
/// surfaces this to other crates (e.g. a GUI's live diagnostic readout).
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct ColorCorrection {
    pub left_offset: [f32; 3],
    pub right_offset: [f32; 3],
}

// ---- Multi-band (2-band) spatial seam blur ----
//
// See `Renderer::encode_multiband_stitch_pass` for the full algorithm.
// Opt-in (`ViewportConfig::multiband_blend_enabled`), reuses the existing
// fisheye pipeline for the per-camera and seam-mask renders (no shader
// changes needed there - just different uniform values), and adds two new
// shaders: `shaders/blur.wgsl` (separable Gaussian) and
// `shaders/multiband_composite.wgsl` (final 2-band reconstruction).

#[repr(C)]
#[derive(Copy, Clone, Debug, Pod, Zeroable)]
struct BlurUniforms {
    texel_size: [f32; 2],
    direction: [f32; 2],
    /// x: sigma in texels. y: premultiply-on-read flag (1.0/0.0). z, w: pad.
    params: [f32; 4],
}

#[repr(C)]
#[derive(Copy, Clone, Debug, Pod, Zeroable)]
struct CompositeUniforms {
    /// x: 1.0 if the right plane is the fading/mask plane, else 0.0.
    /// y: narrow-band smoothstep half-width. z, w: pad.
    params: [f32; 4],
}

/// The size-dependent half of [`MultibandResources`]: textures, views, and
/// the bind groups that reference them. Rebuilt by
/// [`MultibandResources::ensure_size`] whenever the render target's
/// dimensions change (window resize) - `Renderer::output_width/height` are
/// fixed at construction, but `render_to_view`'s target (interactive GUI
/// preview) can be a different, live-resizable size.
struct MultibandSized {
    width: u32,
    height: u32,
    view_a: wgpu::TextureView,
    view_b: wgpu::TextureView,
    view_mask: wgpu::TextureView,
    view_tmp: wgpu::TextureView,
    view_a_blur: wgpu::TextureView,
    view_b_blur: wgpu::TextureView,
    view_mask_blur: wgpu::TextureView,
    bg_read_a: wgpu::BindGroup,
    bg_read_b: wgpu::BindGroup,
    bg_read_mask: wgpu::BindGroup,
    bg_read_tmp: wgpu::BindGroup,
    bg_composite_textures: wgpu::BindGroup,
}

impl MultibandSized {
    fn new(
        device: &wgpu::Device,
        sampler: &wgpu::Sampler,
        blur_texture_layout: &wgpu::BindGroupLayout,
        composite_texture_layout: &wgpu::BindGroupLayout,
        width: u32,
        height: u32,
        format: wgpu::TextureFormat,
    ) -> Self {
        let make_texture = |label: &str| {
            let tex = device.create_texture(&wgpu::TextureDescriptor {
                label: Some(label),
                size: wgpu::Extent3d {
                    width,
                    height,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format,
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT
                    | wgpu::TextureUsages::TEXTURE_BINDING,
                view_formats: &[],
            });
            let view = tex.create_view(&wgpu::TextureViewDescriptor::default());
            (tex, view)
        };
        let read_bind_group = |view: &wgpu::TextureView, label: &str| {
            device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some(label),
                layout: blur_texture_layout,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: wgpu::BindingResource::TextureView(view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::Sampler(sampler),
                    },
                ],
            })
        };

        let (_tex_a, view_a) = make_texture("multiband_a");
        let (_tex_b, view_b) = make_texture("multiband_b");
        let (_tex_mask, view_mask) = make_texture("multiband_mask");
        let (_tex_tmp, view_tmp) = make_texture("multiband_tmp");
        let (_tex_a_blur, view_a_blur) = make_texture("multiband_a_blur");
        let (_tex_b_blur, view_b_blur) = make_texture("multiband_b_blur");
        let (_tex_mask_blur, view_mask_blur) = make_texture("multiband_mask_blur");

        let bg_read_a = read_bind_group(&view_a, "multiband_read_a");
        let bg_read_b = read_bind_group(&view_b, "multiband_read_b");
        let bg_read_mask = read_bind_group(&view_mask, "multiband_read_mask");
        let bg_read_tmp = read_bind_group(&view_tmp, "multiband_read_tmp");

        let bg_composite_textures = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("multiband_composite_textures"),
            layout: composite_texture_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&view_a),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(&view_b),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::TextureView(&view_a_blur),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: wgpu::BindingResource::TextureView(&view_b_blur),
                },
                wgpu::BindGroupEntry {
                    binding: 4,
                    resource: wgpu::BindingResource::TextureView(&view_mask_blur),
                },
                wgpu::BindGroupEntry {
                    binding: 5,
                    resource: wgpu::BindingResource::Sampler(sampler),
                },
            ],
        });

        Self {
            width,
            height,
            view_a,
            view_b,
            view_mask,
            view_tmp,
            view_a_blur,
            view_b_blur,
            view_mask_blur,
            bg_read_a,
            bg_read_b,
            bg_read_mask,
            bg_read_tmp,
            bg_composite_textures,
        }
    }
}

/// Persistent GPU resources for the multi-band spatial blur, owned by
/// [`Renderer`] and created eagerly (fixed pipeline/layout cost; the sized
/// textures are rebuilt lazily on first use / resize via
/// [`Self::ensure_size`], not on every frame).
struct MultibandResources {
    blur_texture_layout: wgpu::BindGroupLayout,
    blur_pipeline: wgpu::RenderPipeline,
    composite_texture_layout: wgpu::BindGroupLayout,
    composite_pipeline: wgpu::RenderPipeline,
    /// Single-uniform-buffer layout, reused for both blur and composite
    /// uniform bind groups (same shape: one uniform buffer at binding 0).
    small_uniform_layout: wgpu::BindGroupLayout,
    /// Dedicated uniform buffer for the seam-mask pass, distinct from
    /// `Renderer::left/right`'s own uniform buffers so writing it doesn't
    /// clobber the value those buffers need for the `tex_a`/`tex_b` passes
    /// in the same frame (all writes happen before one `submit()` - see
    /// the doc comment on `encode_multiband_stitch_pass`).
    mask_uniform_buffer: wgpu::Buffer,
    /// Bound against `Renderer::uniform_layout` (not `small_uniform_layout`)
    /// so it's compatible with the seam-mask pass's reuse of the main
    /// fisheye pipeline.
    mask_uniform_bind_group: wgpu::BindGroup,
    format: wgpu::TextureFormat,
    sized: RefCell<MultibandSized>,
}

impl MultibandResources {
    fn new(
        device: &wgpu::Device,
        uniform_layout: &wgpu::BindGroupLayout,
        width: u32,
        height: u32,
        format: wgpu::TextureFormat,
    ) -> Self {
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("multiband_sampler"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });

        let single_texture_entry = |binding: u32| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::FRAGMENT,
            ty: wgpu::BindingType::Texture {
                sample_type: wgpu::TextureSampleType::Float { filterable: true },
                view_dimension: wgpu::TextureViewDimension::D2,
                multisampled: false,
            },
            count: None,
        };
        let sampler_entry = |binding: u32| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::FRAGMENT,
            ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
            count: None,
        };

        let blur_texture_layout =
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("multiband_blur_texture_layout"),
                entries: &[single_texture_entry(0), sampler_entry(1)],
            });
        let composite_texture_layout =
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("multiband_composite_texture_layout"),
                entries: &[
                    single_texture_entry(0),
                    single_texture_entry(1),
                    single_texture_entry(2),
                    single_texture_entry(3),
                    single_texture_entry(4),
                    sampler_entry(5),
                ],
            });
        let small_uniform_layout =
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("multiband_small_uniform_layout"),
                entries: &[wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::VERTEX | wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                }],
            });

        let full_screen_target = |blend: Option<wgpu::BlendState>| wgpu::ColorTargetState {
            format,
            blend,
            write_mask: wgpu::ColorWrites::ALL,
        };

        let blur_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("multiband_blur"),
            source: wgpu::ShaderSource::Wgsl(include_str!("../shaders/blur.wgsl").into()),
        });
        let blur_pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("multiband_blur_pipeline_layout"),
            bind_group_layouts: &[&blur_texture_layout, &small_uniform_layout],
            immediate_size: 0,
        });
        let blur_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("multiband_blur_pipeline"),
            layout: Some(&blur_pipeline_layout),
            vertex: wgpu::VertexState {
                module: &blur_shader,
                entry_point: Some("vs_main"),
                compilation_options: Default::default(),
                buffers: &[],
            },
            fragment: Some(wgpu::FragmentState {
                module: &blur_shader,
                entry_point: Some("fs_main"),
                compilation_options: Default::default(),
                targets: &[Some(full_screen_target(None))],
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleList,
                ..Default::default()
            },
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview_mask: None,
            cache: None,
        });

        let composite_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("multiband_composite"),
            source: wgpu::ShaderSource::Wgsl(
                include_str!("../shaders/multiband_composite.wgsl").into(),
            ),
        });
        let composite_pipeline_layout =
            device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("multiband_composite_pipeline_layout"),
                bind_group_layouts: &[&composite_texture_layout, &small_uniform_layout],
                immediate_size: 0,
            });
        let composite_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("multiband_composite_pipeline"),
            layout: Some(&composite_pipeline_layout),
            vertex: wgpu::VertexState {
                module: &composite_shader,
                entry_point: Some("vs_main"),
                compilation_options: Default::default(),
                buffers: &[],
            },
            fragment: Some(wgpu::FragmentState {
                module: &composite_shader,
                entry_point: Some("fs_main"),
                compilation_options: Default::default(),
                targets: &[Some(full_screen_target(None))],
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleList,
                ..Default::default()
            },
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview_mask: None,
            cache: None,
        });

        let mask_uniform_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("multiband_mask_uniform"),
            size: std::mem::size_of::<GpuUniforms>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let mask_uniform_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("multiband_mask_uniform_bind_group"),
            layout: uniform_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: mask_uniform_buffer.as_entire_binding(),
            }],
        });

        let sized = RefCell::new(MultibandSized::new(
            device,
            &sampler,
            &blur_texture_layout,
            &composite_texture_layout,
            width,
            height,
            format,
        ));

        Self {
            blur_texture_layout,
            blur_pipeline,
            composite_texture_layout,
            composite_pipeline,
            small_uniform_layout,
            mask_uniform_buffer,
            mask_uniform_bind_group,
            format,
            sized,
        }
    }

    /// Rebuild the size-dependent textures/bind groups if `width`/`height`
    /// don't match what's currently allocated. The interactive GUI preview
    /// target can be resized without recreating `Renderer` (unlike the
    /// fixed `output_width`/`output_height` internal render target), so
    /// this is checked on every multiband render rather than only at
    /// construction.
    fn ensure_size(&self, device: &wgpu::Device, sampler: &wgpu::Sampler, width: u32, height: u32) {
        let needs_resize = {
            let sized = self.sized.borrow();
            sized.width != width || sized.height != height
        };
        if needs_resize {
            *self.sized.borrow_mut() = MultibandSized::new(
                device,
                sampler,
                &self.blur_texture_layout,
                &self.composite_texture_layout,
                width,
                height,
                self.format,
            );
        }
    }
}

/// Vertex with 3D position and UV coordinates.
#[repr(C)]
#[derive(Copy, Clone, Debug, Pod, Zeroable)]
pub(crate) struct Vertex {
    position: [f32; 3],
    uv: [f32; 2],
}

impl Vertex {
    pub(crate) const LAYOUT: wgpu::VertexBufferLayout<'static> = wgpu::VertexBufferLayout {
        array_stride: std::mem::size_of::<Vertex>() as u64,
        step_mode: wgpu::VertexStepMode::Vertex,
        attributes: &[
            wgpu::VertexAttribute {
                format: wgpu::VertexFormat::Float32x3,
                offset: 0,
                shader_location: 0,
            },
            wgpu::VertexAttribute {
                format: wgpu::VertexFormat::Float32x2,
                offset: 12,
                shader_location: 1,
            },
        ],
    };
}

/// Generate quad vertices for a plane (1.0 wide, given aspect ratio).
///
/// The quad lies in the XY plane, centered at origin. The model matrix
/// positions and rotates it to match the v1 Three.js `PlaneGeometry`.
fn quad_vertices(aspect: f32) -> [Vertex; 6] {
    let hw = 0.5; // half width
    let hh = 0.5 / aspect; // half height
    [
        Vertex {
            position: [-hw, -hh, 0.0],
            uv: [0.0, 1.0],
        },
        Vertex {
            position: [hw, -hh, 0.0],
            uv: [1.0, 1.0],
        },
        Vertex {
            position: [hw, hh, 0.0],
            uv: [1.0, 0.0],
        },
        Vertex {
            position: [-hw, -hh, 0.0],
            uv: [0.0, 1.0],
        },
        Vertex {
            position: [hw, hh, 0.0],
            uv: [1.0, 0.0],
        },
        Vertex {
            position: [-hw, hh, 0.0],
            uv: [0.0, 0.0],
        },
    ]
}

// ---- Renderer ----

/// Per-plane GPU resources (YUV textures + uniform buffer + bind groups).
struct PlaneResources {
    y_texture: wgpu::Texture,
    u_texture: wgpu::Texture,
    v_texture: wgpu::Texture,
    texture_bind_group: wgpu::BindGroup,
    uniform_buffer: wgpu::Buffer,
    uniform_bind_group: wgpu::BindGroup,
    width: u32,
    height: u32,
}

/// Input pixel format for the renderer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputFormat {
    /// YUV420P: three separate R8 textures (Y full-res, U half-res, V half-res).
    /// Used with software decode or CPU-side conversion.
    Yuv420p,
    /// NV12: Y as R8 (full-res) + interleaved UV as Rg8 (half-res).
    /// NVDEC native output format. V texture is a 1x1 dummy.
    Nv12,
    /// Packed BGRA / RGBA: a single `Rgba8Unorm` texture at full resolution
    /// holding pre-composited sRGB-domain RGB. Used by OBS Browser Source,
    /// screen capture, WebRTC ingest, and other non-camera sources whose
    /// native format is already RGB. The shader samples the single plane
    /// and writes the RGB triple straight out (no YUV conversion).
    /// `u_texture` / `v_texture` are 1x1 dummies in this mode.
    Bgra,
}

/// GPU-side pixel format for NV12-family zero-copy decode output.
///
/// Determines texture formats and byte widths for CUDA/Vulkan shared
/// texture creation. The shader works unchanged for all variants because
/// wgpu's Unorm normalization maps both 8-bit `[0, 255]` and 16-bit
/// `[0, 65535]` values to `[0.0, 1.0]` in the fragment shader.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum GpuPixelFormat {
    /// 8-bit NV12 (standard H.264/HEVC decode output).
    /// Y plane: `R8Unorm`, UV plane: `Rg8Unorm`, 1 byte per sample.
    Nv12,
    /// 10-bit P010 (e.g. DJI Action 4 HEVC 10-bit).
    /// Y plane: `R16Unorm`, UV plane: `Rg16Unorm`, 2 bytes per sample.
    /// NVDEC stores 10-bit values in the upper bits of each `u16`.
    P010,
}

impl GpuPixelFormat {
    /// wgpu texture format for the Y (luma) plane.
    pub fn y_format(self) -> wgpu::TextureFormat {
        match self {
            Self::Nv12 => wgpu::TextureFormat::R8Unorm,
            Self::P010 => wgpu::TextureFormat::R16Unorm,
        }
    }

    /// wgpu texture format for the UV (chroma) plane.
    pub fn uv_format(self) -> wgpu::TextureFormat {
        match self {
            Self::Nv12 => wgpu::TextureFormat::Rg8Unorm,
            Self::P010 => wgpu::TextureFormat::Rg16Unorm,
        }
    }

    /// Bytes per luma/chroma sample (1 for 8-bit, 2 for 10-bit).
    pub fn bytes_per_sample(self) -> usize {
        match self {
            Self::Nv12 => 1,
            Self::P010 => 2,
        }
    }

    /// wgpu texture format for the full NV12/P010 texture (used for D3D11 import).
    pub fn wgpu_format(self) -> wgpu::TextureFormat {
        match self {
            Self::Nv12 => wgpu::TextureFormat::NV12,
            Self::P010 => wgpu::TextureFormat::P010,
        }
    }
}

// ---- Seam-line screen-space projection (for UI hit-testing) ----

/// Project the seam debug line (see `fisheye.wgsl`'s `show_seam_line`
/// comment) to normalized screen-space endpoints `(x, y)` in `0.0..=1.0`,
/// `(0,0)` = top-left, for hit-testing a mouse cursor against it in a GUI.
///
/// `yaw`/`pitch` should be the *live* current viewport position (e.g.
/// `PoseControl::current_pose()`), not assumed to be centered - callers
/// have this on hand already (it's what the real render uses), so there's
/// no need to approximate. `tilt`/`roll` come from `calibration.framing`
/// itself, same as the real render.
///
/// `output_aspect` should match the actual displayed preview's aspect
/// ratio (width / height), not necessarily `viewport.width/height` - the
/// displayed box may be letterboxed to a different ratio.
///
/// Returns `None` when there's nothing meaningful to point at (`blend_width
/// <= 0.0`, a hard cut with no feathered seam to visualize) or when either
/// endpoint projects behind the camera.
pub fn seam_line_screen_points(
    calibration: &Calibration,
    viewport: &ViewportConfig,
    yaw: f32,
    pitch: f32,
    output_aspect: f32,
) -> Option<((f32, f32), (f32, f32))> {
    let plane_aspect = calibration.lenses[0].width as f32 / calibration.lenses[0].height as f32;
    let scene = SceneGeometry::new(&calibration.topology, &calibration.framing, plane_aspect);

    // Same left/right fading convention as `encode_stitch_pass`: `false`
    // (default) = right fades over a fixed left.
    let is_right_fading = !calibration.topology.blend_flip_direction;
    let model = if is_right_fading {
        scene.model_matrix_right()
    } else {
        scene.model_matrix_left()
    };

    // fisheye.wgsl's seam_dist test doesn't run on the raw vertex uv - the
    // fragment shader remaps it first: `uv = in.uv * 2.0 - 0.5` (extends
    // [0,1] to [-0.5,1.5] for undistortion sampling beyond the plane's own
    // edges), and it's *that* extended `uv.x` the seam_dist formula
    // compares against `seam_offset`. Solving `in.uv.x * 2 - 0.5 ==
    // seam_offset` for `in.uv.x`, then converting to local-x via
    // `quad_vertices`' mapping (`local_x = in.uv.x - 0.5`), collapses to
    // `(seam_offset - 0.5) / 2` - missing this factor of 2 was a real bug
    // (confirmed against a real calibration: computed column landed at the
    // wrong screen fraction by a wide, non-approximation-sized margin).
    // The fading plane's seam-adjacent edge sits at (extended) uv.x=0 if
    // it's the right plane, uv.x=1 if it's the left plane (see the
    // shader's own comment for the geometric reason), with `seam_offset`
    // shifting it the same way in both cases.
    let seam_offset = calibration.topology.seam_offset;
    let local_x = if is_right_fading {
        (seam_offset - 0.5) / 2.0
    } else {
        (0.5 - seam_offset) / 2.0
    };
    let half_height = 0.5 / plane_aspect;

    let projection = opengl_to_wgpu_matrix()
        * Perspective3::new(
            output_aspect,
            viewport.fov_degrees.to_radians(),
            NEAR_PLANE,
            FAR_PLANE,
        )
        .to_homogeneous();
    let view = view_matrix(
        &scene.camera_position,
        yaw,
        pitch,
        calibration.framing.tilt as f32,
        calibration.framing.roll as f32,
    );
    let mvp = projection * view * model;

    let top = project_to_screen_fraction(&mvp, local_x, half_height)?;
    let bottom = project_to_screen_fraction(&mvp, local_x, -half_height)?;
    Some((top, bottom))
}

/// Project one local-space point on a plane (z=0) through an MVP matrix to
/// a normalized `0.0..=1.0` screen fraction, `(0,0)` = top-left. `None` if
/// the point is behind the camera (`w <= 0`), where the perspective divide
/// is meaningless.
fn project_to_screen_fraction(
    mvp: &Matrix4<f32>,
    local_x: f32,
    local_y: f32,
) -> Option<(f32, f32)> {
    let clip = mvp * Vector4::new(local_x, local_y, 0.0, 1.0);
    if clip.w <= 1e-4 {
        return None;
    }
    let ndc_x = clip.x / clip.w;
    let ndc_y = clip.y / clip.w;
    Some(((ndc_x + 1.0) * 0.5, (1.0 - ndc_y) * 0.5))
}

/// The GPU renderer for panoramic stitching.
///
/// Holds all wgpu resources: pipelines, textures, bind groups, and buffers.
/// Created once per pipeline and reused for every frame.
pub(crate) struct Renderer {
    pipeline: wgpu::RenderPipeline,
    vertex_buffer: wgpu::Buffer,
    left: PlaneResources,
    right: PlaneResources,
    render_target: wgpu::Texture,
    render_target_view: wgpu::TextureView,
    output_width: u32,
    output_height: u32,
    /// Opt-in 2-band spatial blur seam compositor. See
    /// [`Self::encode_multiband_stitch_pass`].
    multiband: MultibandResources,
    /// Input pixel format (YUV420P or NV12).
    input_format: InputFormat,
    /// Full-range YUV (0-255) instead of limited (16-235).
    is_full_range: bool,
    /// Stored for creating bind groups from external textures (zero-copy).
    texture_layout: wgpu::BindGroupLayout,
    /// Shared sampler, stored for bind group creation.
    sampler: wgpu::Sampler,
    /// Device handle for creating bind groups (Arc-based, cheap to clone).
    device: wgpu::Device,
    /// Whether to flip UV coordinates for 180-degree rotation per camera [left, right].
    /// Set by the zero-copy path when the source video has rotation metadata.
    /// The CPU decode path handles rotation by reversing buffers instead.
    flip_180: [bool; 2],
}

impl Renderer {
    /// Create a new renderer with all GPU resources.
    ///
    /// Allocates textures, buffers, and compiles the shader pipeline.
    /// This is called once during pipeline initialization.
    ///
    /// `input_format` selects between YUV420P (3 separate planes) and
    /// NV12 (Y + interleaved UV). NV12 is the native NVDEC output format.
    #[allow(clippy::too_many_arguments)] // construction-only plumbing
    pub fn new(
        gpu: &GpuContext,
        program: &crate::render::GpuProgram,
        output_width: u32,
        output_height: u32,
        input_width: u32,
        input_height: u32,
        output_format: wgpu::TextureFormat,
        input_format: InputFormat,
        scene: &SceneGeometry,
    ) -> Self {
        let device = &gpu.device;

        // Shader: compiled from the projection's GPU program descriptor -
        // the render pipeline builds exactly what the projection declares.
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("projection_composite"),
            source: wgpu::ShaderSource::Wgsl(program.wgsl.into()),
        });

        // Vertex buffer (quad for both planes — same shape, different model matrices)
        let vertices = quad_vertices(scene.plane_aspect);
        let vertex_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("quad_vertices"),
            contents: bytemuck::cast_slice(&vertices),
            usage: wgpu::BufferUsages::VERTEX,
        });

        // Bind group layouts — YUV420P: 3 plane textures + 1 sampler
        let texture_entry = |binding: u32| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::FRAGMENT,
            ty: wgpu::BindingType::Texture {
                sample_type: wgpu::TextureSampleType::Float { filterable: true },
                view_dimension: wgpu::TextureViewDimension::D2,
                multisampled: false,
            },
            count: None,
        };
        let texture_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("texture_layout"),
            entries: &[
                texture_entry(0), // Y plane
                texture_entry(1), // U plane
                texture_entry(2), // V plane
                wgpu::BindGroupLayoutEntry {
                    binding: 3,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });

        let uniform_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("uniform_layout"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX | wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            }],
        });

        // Pipeline layout
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("stitch_pipeline_layout"),
            bind_group_layouts: &[&texture_layout, &uniform_layout],
            immediate_size: 0,
        });

        // Render pipeline with alpha blending for seam transition
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("stitch_render_pipeline"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some(program.vs_entry),
                compilation_options: Default::default(),
                buffers: std::slice::from_ref(&program.vertex_layout),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some(program.fs_entry),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format: output_format,
                    blend: Some(program.blend),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleList,
                front_face: wgpu::FrontFace::Ccw,
                cull_mode: None, // Both sides visible
                ..Default::default()
            },
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview_mask: None,
            cache: None,
        });

        // Sampler (shared by both planes)
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("video_sampler"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });

        // Per-plane resources
        let left = Self::create_plane_resources(
            device,
            &texture_layout,
            &uniform_layout,
            &sampler,
            input_width,
            input_height,
            input_format,
            "left",
        );
        let right = Self::create_plane_resources(
            device,
            &texture_layout,
            &uniform_layout,
            &sampler,
            input_width,
            input_height,
            input_format,
            "right",
        );

        // Render target (output-sized, RGBA)
        let render_target = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("render_target"),
            size: wgpu::Extent3d {
                width: output_width,
                height: output_height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: output_format,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT
                | wgpu::TextureUsages::COPY_SRC
                | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });
        let render_target_view = render_target.create_view(&wgpu::TextureViewDescriptor::default());

        let multiband = MultibandResources::new(
            device,
            &uniform_layout,
            output_width,
            output_height,
            output_format,
        );

        Self {
            pipeline,
            vertex_buffer,
            left,
            right,
            render_target,
            render_target_view,
            output_width,
            output_height,
            multiband,
            input_format,
            is_full_range: false,
            texture_layout,
            sampler,
            device: device.clone(),
            flip_180: [false, false],
        }
    }

    fn create_plane_resources(
        device: &wgpu::Device,
        texture_layout: &wgpu::BindGroupLayout,
        uniform_layout: &wgpu::BindGroupLayout,
        sampler: &wgpu::Sampler,
        width: u32,
        height: u32,
        input_format: InputFormat,
        label: &str,
    ) -> PlaneResources {
        let usage = wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST;

        let create_texture = |name: &str, w: u32, h: u32, format: wgpu::TextureFormat| {
            device.create_texture(&wgpu::TextureDescriptor {
                label: Some(&format!("{label}_{name}")),
                size: wgpu::Extent3d {
                    width: w,
                    height: h,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format,
                usage,
                view_formats: &[],
            })
        };

        // Y plane format + u/v plane selection depend on input format.
        // For Bgra the "Y" slot actually holds a full-res Rgba8Unorm texture;
        // u/v are 1x1 dummies that the shader never samples.
        let y_texture = match input_format {
            InputFormat::Yuv420p | InputFormat::Nv12 => {
                create_texture("y", width, height, wgpu::TextureFormat::R8Unorm)
            }
            InputFormat::Bgra => {
                create_texture("rgba", width, height, wgpu::TextureFormat::Rgba8Unorm)
            }
        };

        let (u_texture, v_texture) = match input_format {
            InputFormat::Yuv420p => {
                // YUV420P: separate R8 U and V at half resolution
                let u = create_texture("u", width / 2, height / 2, wgpu::TextureFormat::R8Unorm);
                let v = create_texture("v", width / 2, height / 2, wgpu::TextureFormat::R8Unorm);
                (u, v)
            }
            InputFormat::Nv12 => {
                // NV12: interleaved UV as Rg8Unorm at half resolution
                let uv = create_texture("uv", width / 2, height / 2, wgpu::TextureFormat::Rg8Unorm);
                // Dummy V texture - shader won't sample it in NV12 mode
                let v_dummy = create_texture("v_dummy", 1, 1, wgpu::TextureFormat::R8Unorm);
                (uv, v_dummy)
            }
            InputFormat::Bgra => {
                // Shader skips u/v sampling on the Bgra path; 1x1 dummies
                // satisfy the bind group layout without wasting memory.
                let u_dummy = create_texture("u_dummy", 1, 1, wgpu::TextureFormat::R8Unorm);
                let v_dummy = create_texture("v_dummy", 1, 1, wgpu::TextureFormat::R8Unorm);
                (u_dummy, v_dummy)
            }
        };

        let y_view = y_texture.create_view(&wgpu::TextureViewDescriptor::default());
        let u_view = u_texture.create_view(&wgpu::TextureViewDescriptor::default());
        let v_view = v_texture.create_view(&wgpu::TextureViewDescriptor::default());

        let texture_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some(&format!("{label}_texture_bg")),
            layout: texture_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&y_view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(&u_view),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::TextureView(&v_view),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: wgpu::BindingResource::Sampler(sampler),
                },
            ],
        });

        let uniform_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some(&format!("{label}_uniforms")),
            size: std::mem::size_of::<GpuUniforms>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let uniform_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some(&format!("{label}_uniform_bg")),
            layout: uniform_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: uniform_buffer.as_entire_binding(),
            }],
        });

        PlaneResources {
            y_texture,
            u_texture,
            v_texture,
            texture_bind_group,
            uniform_buffer,
            uniform_bind_group,
            width,
            height,
        }
    }

    /// Upload YUV420P planes to the left camera textures.
    #[cfg_attr(
        feature = "profiling",
        tracing::instrument(skip_all, name = "gpu_upload")
    )]
    pub fn upload_left_yuv(
        &self,
        gpu: &GpuContext,
        y: &[u8],
        u: &[u8],
        v: &[u8],
    ) -> Result<(), RenderError> {
        upload_yuv(gpu, &self.left, y, u, v)
    }

    /// Upload YUV420P planes to the right camera textures.
    pub fn upload_right_yuv(
        &self,
        gpu: &GpuContext,
        y: &[u8],
        u: &[u8],
        v: &[u8],
    ) -> Result<(), RenderError> {
        upload_yuv(gpu, &self.right, y, u, v)
    }

    /// Upload NV12 planes to the left camera textures.
    ///
    /// Y is R8Unorm at full resolution, UV is Rg8Unorm at half resolution.
    /// Requires the renderer to be initialized with `InputFormat::Nv12`.
    #[cfg_attr(
        feature = "profiling",
        tracing::instrument(skip_all, name = "gpu_upload_nv12")
    )]
    pub fn upload_left_nv12(
        &self,
        gpu: &GpuContext,
        y: &[u8],
        uv: &[u8],
    ) -> Result<(), RenderError> {
        debug_assert_eq!(
            self.input_format,
            InputFormat::Nv12,
            "upload_left_nv12 requires InputFormat::Nv12"
        );
        upload_nv12(gpu, &self.left, y, uv)
    }

    /// Upload NV12 planes to the right camera textures.
    pub fn upload_right_nv12(
        &self,
        gpu: &GpuContext,
        y: &[u8],
        uv: &[u8],
    ) -> Result<(), RenderError> {
        debug_assert_eq!(
            self.input_format,
            InputFormat::Nv12,
            "upload_right_nv12 requires InputFormat::Nv12"
        );
        upload_nv12(gpu, &self.right, y, uv)
    }

    /// Upload a packed BGRA/RGBA plane to the left camera texture.
    ///
    /// Data must be `width * height * 4` bytes in (R, G, B, A) byte order.
    /// Upload-side swizzling translates BGRA sources before calling this -
    /// see [`pipeline::BgraPlanes`](crate::render::pipeline::BgraPlanes).
    pub fn upload_left_bgra(&self, gpu: &GpuContext, rgba: &[u8]) -> Result<(), RenderError> {
        debug_assert_eq!(
            self.input_format,
            InputFormat::Bgra,
            "upload_left_bgra requires InputFormat::Bgra"
        );
        upload_bgra(gpu, &self.left, rgba)
    }

    /// Upload a packed BGRA/RGBA plane to the right camera texture.
    /// See [`Self::upload_left_bgra`].
    pub fn upload_right_bgra(&self, gpu: &GpuContext, rgba: &[u8]) -> Result<(), RenderError> {
        debug_assert_eq!(
            self.input_format,
            InputFormat::Bgra,
            "upload_right_bgra requires InputFormat::Bgra"
        );
        upload_bgra(gpu, &self.right, rgba)
    }

    /// Copy a GPU texture into the left input plane (BGRA mode).
    ///
    /// Zero-copy alternative to [`upload_left_bgra`]: the source texture
    /// must be `Rgba8Unorm` with matching dimensions. The copy is
    /// appended to `encoder` as a GPU-side blit with no CPU involvement.
    pub fn copy_texture_to_left(&self, encoder: &mut wgpu::CommandEncoder, source: &wgpu::Texture) {
        copy_texture_to_plane(encoder, source, &self.left);
    }

    /// Copy a GPU texture into the right input plane (BGRA mode).
    pub fn copy_texture_to_right(
        &self,
        encoder: &mut wgpu::CommandEncoder,
        source: &wgpu::Texture,
    ) {
        copy_texture_to_plane(encoder, source, &self.right);
    }

    /// Create a texture bind group from external textures.
    ///
    /// Used for CUDA/Vulkan zero-copy: pre-build one bind group per
    /// double-buffer slot (before the render loop), then select the active
    /// slot each frame by cloning the appropriate pre-built group (cheap
    /// Arc refcount increment) and passing it to
    /// [`Self::set_left_bind_group`] / [`Self::set_right_bind_group`].
    /// `wgpu::BindGroup` implements `Clone`, so no GPU allocation occurs on
    /// the per-frame path.
    pub fn create_texture_bind_group(
        &self,
        y_texture: &wgpu::Texture,
        uv_texture: &wgpu::Texture,
        label: &str,
    ) -> wgpu::BindGroup {
        let y_view = y_texture.create_view(&wgpu::TextureViewDescriptor::default());
        let uv_view = uv_texture.create_view(&wgpu::TextureViewDescriptor::default());
        let v_view = self
            .left
            .v_texture
            .create_view(&wgpu::TextureViewDescriptor::default());

        self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some(label),
            layout: &self.texture_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&y_view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(&uv_view),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::TextureView(&v_view),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: wgpu::BindingResource::Sampler(&self.sampler),
                },
            ],
        })
    }

    /// Create a texture bind group from pre-built texture views.
    ///
    /// Used by the D3D11VA zero-copy path where NV12 plane views require
    /// explicit `TextureAspect::Plane0` / `Plane1` and cannot be obtained
    /// from `TextureViewDescriptor::default()`.
    pub fn create_bind_group_from_views(
        &self,
        y_view: &wgpu::TextureView,
        uv_view: &wgpu::TextureView,
        label: &str,
    ) -> wgpu::BindGroup {
        let v_view = self
            .left
            .v_texture
            .create_view(&wgpu::TextureViewDescriptor::default());
        self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some(label),
            layout: &self.texture_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(y_view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(uv_view),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::TextureView(&v_view),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: wgpu::BindingResource::Sampler(&self.sampler),
                },
            ],
        })
    }

    /// Set the left plane's texture bind group for the next render.
    pub fn set_left_bind_group(&mut self, bind_group: wgpu::BindGroup) {
        self.left.texture_bind_group = bind_group;
    }

    /// Set the right plane's texture bind group for the next render.
    pub fn set_right_bind_group(&mut self, bind_group: wgpu::BindGroup) {
        self.right.texture_bind_group = bind_group;
    }

    /// Enable 180-degree UV flip for the GPU zero-copy path.
    ///
    /// When set, the shader flips texture coordinates before sampling,
    /// equivalent to the CPU path's buffer reversal for rotated video.
    pub fn set_flip_180(&mut self, left: bool, right: bool) {
        self.flip_180 = [left, right];
    }

    /// Set full-range YUV mode for the shader.
    pub fn set_full_range(&mut self, full_range: bool) {
        self.is_full_range = full_range;
    }

    /// Current full-range YUV mode, set via [`Self::set_full_range`].
    ///
    /// Surfaced so [`super::color_match`]'s CPU-side band measurement can
    /// decode raw YCbCr bytes with the same range convention the shader's
    /// `sample_yuv` uses, keeping the measured offset in the same color
    /// space `apply_color_transfer` applies it in.
    pub(crate) fn is_full_range(&self) -> bool {
        self.is_full_range
    }

    /// Create fresh `TextureView`s for the left plane's Y/U/V
    /// textures. Needed by [`crate::gpu::yuv_stack_packer::YuvStackPacker`]
    /// when replay recording is enabled: the packer samples the same
    /// uploaded source data the stitch shader reads, producing a
    /// tiled atlas in parallel with the panorama render.
    ///
    /// For NV12 inputs the returned `U` view is the interleaved UV
    /// texture (Rg8Unorm) and the `V` view is the 1×1 dummy; the
    /// packer's NV12 kernel ignores the V binding.
    pub(crate) fn left_plane_views(
        &self,
    ) -> (wgpu::TextureView, wgpu::TextureView, wgpu::TextureView) {
        Self::plane_views(&self.left)
    }

    /// Right-side counterpart to [`Self::left_plane_views`].
    pub(crate) fn right_plane_views(
        &self,
    ) -> (wgpu::TextureView, wgpu::TextureView, wgpu::TextureView) {
        Self::plane_views(&self.right)
    }

    fn plane_views(
        plane: &PlaneResources,
    ) -> (wgpu::TextureView, wgpu::TextureView, wgpu::TextureView) {
        (
            plane
                .y_texture
                .create_view(&wgpu::TextureViewDescriptor::default()),
            plane
                .u_texture
                .create_view(&wgpu::TextureViewDescriptor::default()),
            plane
                .v_texture
                .create_view(&wgpu::TextureViewDescriptor::default()),
        )
    }

    /// Input format the renderer was built for. Surfaced so the
    /// stacked-video packer can pick the right shader variant
    /// (YUV420P vs NV12) at session setup.
    pub(crate) fn input_format(&self) -> InputFormat {
        self.input_format
    }

    /// Encode the shared stitch render pass: projection, uniforms, and draw calls.
    ///
    /// Returns the command encoder with the render pass already recorded.
    /// Callers handle submission, readback, or further encoding as needed.
    #[allow(clippy::too_many_arguments)]
    #[allow(clippy::too_many_arguments)]
    fn encode_stitch_pass(
        &self,
        gpu: &GpuContext,
        scene: &SceneGeometry,
        calibration: &Calibration,
        viewport: &ResolvedViewport,
        blend_width: f32,
        color_correction: ColorCorrection,
        show_seam_line: bool,
        target_view: &wgpu::TextureView,
        aspect: f32,
        encoder_label: &str,
    ) -> wgpu::CommandEncoder {
        let projection = opengl_to_wgpu_matrix()
            * Perspective3::new(
                aspect,
                viewport.config.fov_degrees.to_radians(),
                NEAR_PLANE,
                FAR_PLANE,
            )
            .to_homogeneous();
        let view = view_matrix(
            &scene.camera_position,
            viewport.position.yaw,
            viewport.position.pitch,
            calibration.framing.tilt as f32,
            calibration.framing.roll as f32,
        );

        // Ground-tilt mapping is not the naive left<->left, right<->right
        // pairing it looks like: `Topology::ground_tilt_x` corrects the
        // x-plane, which - per reco_calibrate::geometry's module doc (the
        // v1-derived left/right swap) - holds the *right* camera's content
        // and is positioned at this renderer's "right" plane; ground_tilt_z
        // corrects the z-plane (*left* camera, this renderer's "left"
        // plane). Verified against `SceneGeometry::new`:
        // its "left plane" sits at `[0,0,half_offset]` (geometry.rs's
        // z-plane translation) and "right plane" at `[half_offset,...]`
        // (geometry.rs's x-plane translation) - so this renderer's own
        // left/right naming already matches the physical cameras directly,
        // and it's only the optimizer's internal x-plane/z-plane bookkeeping
        // that's swapped.
        // band_full is a whole-calibration setting (not per-plane), so both
        // left/right share the same value here.
        let left_ground_tilt = GroundTilt {
            tilt: calibration.topology.ground_tilt_z as f32,
            k: calibration.lenses[0].ground_tilt_k() as f32,
            band_full: calibration.topology.ground_tilt_band_width as f32,
        };
        let right_ground_tilt = GroundTilt {
            tilt: calibration.topology.ground_tilt_x as f32,
            k: calibration.lenses[1].ground_tilt_k() as f32,
            band_full: calibration.topology.ground_tilt_band_width as f32,
        };
        // Same left/right-vs-x/z-plane swap as ground_tilt above.
        let left_top_tilt = TopTilt {
            tilt: calibration.topology.top_tilt_z as f32,
            k: calibration.lenses[0].ground_tilt_k() as f32,
            band_full: calibration.topology.top_tilt_band_width as f32,
        };
        let right_top_tilt = TopTilt {
            tilt: calibration.topology.top_tilt_x as f32,
            k: calibration.lenses[1].ground_tilt_k() as f32,
            band_full: calibration.topology.top_tilt_band_width as f32,
        };

        let left_mvp = projection * view * scene.model_matrix_left();
        let mut left_uniforms = build_gpu_uniforms(
            &left_mvp,
            &calibration.lenses[0],
            false,
            blend_width,
            self.input_format,
            self.flip_180[0],
            self.is_full_range,
            left_ground_tilt,
            left_top_tilt,
        );
        left_uniforms.lens_preview[0] = calibration.lenses[0].correction;

        let right_mvp = projection * view * scene.model_matrix_right();
        let mut right_uniforms = build_gpu_uniforms(
            &right_mvp,
            &calibration.lenses[1],
            true,
            blend_width,
            self.input_format,
            self.flip_180[1],
            self.is_full_range,
            right_ground_tilt,
            right_top_tilt,
        );
        right_uniforms.lens_preview[0] = calibration.lenses[1].correction;

        // Per-camera color-transfer offset (see `ColorCorrection`'s doc).
        // `color_offset_blend[3]` already holds `blend_width` from
        // `build_gpu_uniforms` above - only overwrite the Y/U/V offset.
        left_uniforms.color_offset_blend[0] = color_correction.left_offset[0];
        left_uniforms.color_offset_blend[1] = color_correction.left_offset[1];
        left_uniforms.color_offset_blend[2] = color_correction.left_offset[2];
        right_uniforms.color_offset_blend[0] = color_correction.right_offset[0];
        right_uniforms.color_offset_blend[1] = color_correction.right_offset[1];
        right_uniforms.color_offset_blend[2] = color_correction.right_offset[2];

        // Seam blend direction: which plane fades in over the other at the
        // seam. `false` (default) = right fades over a fixed left, drawn
        // left-then-right so "over" compositing works (the fading plane
        // must draw second, on top of the opaque one). `true` flips both
        // which plane fades (`ground_tilt.w`, otherwise-unused - see
        // `GroundTilt`'s doc and the shader's alpha-blend comment) and the
        // draw order, so left fades over a fixed right instead. Purely a
        // rendering choice: doesn't move the seam or touch calibration
        // geometry.
        let flip = calibration.topology.blend_flip_direction;
        left_uniforms.ground_tilt[3] = if flip { 1.0 } else { 0.0 };
        right_uniforms.ground_tilt[3] = if flip { 0.0 } else { 1.0 };

        // Seam-line debug overlay and the manual seam_offset nudge both
        // only apply to the fading-designated plane, same convention as
        // the fade flag above - see the shader's seam-line comment and
        // `Topology::seam_offset`'s doc.
        if show_seam_line {
            if flip {
                left_uniforms.lens_preview[2] = 1.0;
            } else {
                right_uniforms.lens_preview[2] = 1.0;
            }
        }
        if flip {
            left_uniforms.lens_preview[3] = calibration.topology.seam_offset;
        } else {
            right_uniforms.lens_preview[3] = calibration.topology.seam_offset;
        }

        gpu.queue.write_buffer(
            &self.left.uniform_buffer,
            0,
            bytemuck::bytes_of(&left_uniforms),
        );
        gpu.queue.write_buffer(
            &self.right.uniform_buffer,
            0,
            bytemuck::bytes_of(&right_uniforms),
        );

        let mut encoder = gpu
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some(encoder_label),
            });

        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("stitch_pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: target_view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                        store: wgpu::StoreOp::Store,
                    },
                    depth_slice: None,
                })],
                depth_stencil_attachment: None,
                occlusion_query_set: None,
                timestamp_writes: None,
                multiview_mask: None,
            });

            pass.set_pipeline(&self.pipeline);
            pass.set_vertex_buffer(0, self.vertex_buffer.slice(..));

            let (first, second) = if flip {
                (&self.right, &self.left)
            } else {
                (&self.left, &self.right)
            };
            pass.set_bind_group(0, &first.texture_bind_group, &[]);
            pass.set_bind_group(1, &first.uniform_bind_group, &[]);
            pass.draw(0..6, 0..1);

            pass.set_bind_group(0, &second.texture_bind_group, &[]);
            pass.set_bind_group(1, &second.uniform_bind_group, &[]);
            pass.draw(0..6, 0..1);
        }

        encoder
    }

    /// Opt-in alternative to [`Self::encode_stitch_pass`]: a 2-band spatial
    /// blend instead of a single alpha crossfade. Blends low frequencies
    /// (blurred content) over a wide band and high frequencies (fine
    /// detail) over a narrow band, then sums them - this is what lets the
    /// visible transition be wide (hiding residual near-field misalignment)
    /// without doubling sharp structure the way widening the single-band
    /// crossfade does. See `reco-core/FRICTION.md`'s multi-band entry.
    ///
    /// Ten passes, all still cheaper than a real Laplacian pyramid: render
    /// each camera alone (`tex_a`, `tex_b` - hard 0/1 FOV-coverage alpha,
    /// no seam fade), render the fading plane again with a near-zero blend
    /// width for a hard seam-position mask (`tex_mask`), separably
    /// Gaussian-blur all three, then reconstruct in one composite pass. The
    /// narrow-band mask is derived from the blurred wide mask via a steep
    /// `smoothstep` around its 0.5 crossover in the composite shader,
    /// rather than a second blur pass.
    ///
    /// All `gpu.queue.write_buffer` calls below happen before this
    /// function's single implicit "submit" boundary (the caller submits
    /// the returned encoder) - each uniform buffer here is written exactly
    /// once and read by exactly one pass, which is required: `write_buffer`
    /// only orders correctly relative to `queue.submit()`, not relative to
    /// when passes are *encoded*, so a buffer written twice before one
    /// submit would show only the last value to every pass that reads it.
    #[allow(clippy::too_many_arguments)]
    fn encode_multiband_stitch_pass(
        &self,
        gpu: &GpuContext,
        scene: &SceneGeometry,
        calibration: &Calibration,
        viewport: &ResolvedViewport,
        blend_width: f32,
        color_correction: ColorCorrection,
        show_seam_line: bool,
        target_view: &wgpu::TextureView,
        target_width: u32,
        target_height: u32,
        aspect: f32,
        encoder_label: &str,
    ) -> wgpu::CommandEncoder {
        self.multiband
            .ensure_size(&gpu.device, &self.sampler, target_width, target_height);

        let projection = opengl_to_wgpu_matrix()
            * Perspective3::new(
                aspect,
                viewport.config.fov_degrees.to_radians(),
                NEAR_PLANE,
                FAR_PLANE,
            )
            .to_homogeneous();
        let view = view_matrix(
            &scene.camera_position,
            viewport.position.yaw,
            viewport.position.pitch,
            calibration.framing.tilt as f32,
            calibration.framing.roll as f32,
        );

        let left_ground_tilt = GroundTilt {
            tilt: calibration.topology.ground_tilt_z as f32,
            k: calibration.lenses[0].ground_tilt_k() as f32,
            band_full: calibration.topology.ground_tilt_band_width as f32,
        };
        let right_ground_tilt = GroundTilt {
            tilt: calibration.topology.ground_tilt_x as f32,
            k: calibration.lenses[1].ground_tilt_k() as f32,
            band_full: calibration.topology.ground_tilt_band_width as f32,
        };
        let left_top_tilt = TopTilt {
            tilt: calibration.topology.top_tilt_z as f32,
            k: calibration.lenses[0].ground_tilt_k() as f32,
            band_full: calibration.topology.top_tilt_band_width as f32,
        };
        let right_top_tilt = TopTilt {
            tilt: calibration.topology.top_tilt_x as f32,
            k: calibration.lenses[1].ground_tilt_k() as f32,
            band_full: calibration.topology.top_tilt_band_width as f32,
        };

        let left_mvp = projection * view * scene.model_matrix_left();
        let mut left_uniforms = build_gpu_uniforms(
            &left_mvp,
            &calibration.lenses[0],
            false,
            blend_width,
            self.input_format,
            self.flip_180[0],
            self.is_full_range,
            left_ground_tilt,
            left_top_tilt,
        );
        left_uniforms.lens_preview[0] = calibration.lenses[0].correction;
        left_uniforms.color_offset_blend[0] = color_correction.left_offset[0];
        left_uniforms.color_offset_blend[1] = color_correction.left_offset[1];
        left_uniforms.color_offset_blend[2] = color_correction.left_offset[2];
        left_uniforms.ground_tilt[3] = 0.0; // tex_a: hard FOV coverage, never fades.

        let right_mvp = projection * view * scene.model_matrix_right();
        let mut right_uniforms = build_gpu_uniforms(
            &right_mvp,
            &calibration.lenses[1],
            true,
            blend_width,
            self.input_format,
            self.flip_180[1],
            self.is_full_range,
            right_ground_tilt,
            right_top_tilt,
        );
        right_uniforms.lens_preview[0] = calibration.lenses[1].correction;
        right_uniforms.color_offset_blend[0] = color_correction.right_offset[0];
        right_uniforms.color_offset_blend[1] = color_correction.right_offset[1];
        right_uniforms.color_offset_blend[2] = color_correction.right_offset[2];
        right_uniforms.ground_tilt[3] = 0.0; // tex_b: hard FOV coverage, never fades.

        // Seam mask: a copy of whichever plane fades (see
        // `encode_stitch_pass`'s flip-convention comment), rendered with a
        // near-zero blend width so its alpha is a hard step exactly at the
        // true seam position instead of the visible feathered ramp. `1e-4`
        // (not `0.0`) matters: the shader's fade branch is gated on
        // `blend_width > 0.0`, so a literal zero would skip it entirely.
        const MASK_HARD_BLEND_WIDTH: f32 = 1e-4;
        let flip = calibration.topology.blend_flip_direction;
        let fading_is_right = !flip;
        let (mut mask_uniforms, mask_plane) = if fading_is_right {
            (right_uniforms, &self.right)
        } else {
            (left_uniforms, &self.left)
        };
        mask_uniforms.ground_tilt[3] = 1.0;
        mask_uniforms.color_offset_blend[3] = MASK_HARD_BLEND_WIDTH;
        // The mask *is* what defines the composited seam position, so it
        // must carry the manual offset - without this, seam_offset would
        // have no effect at all in multiband mode (tex_a/tex_b never fade,
        // so their own lens_preview.w is only used by the debug line).
        mask_uniforms.lens_preview[3] = calibration.topology.seam_offset;

        // Seam-line debug overlay: set on tex_a/tex_b (whichever is the
        // fading-designated side) *after* mask_uniforms was copied above,
        // so the mask-only pass (never visibly composited) doesn't also
        // carry the flag. See `encode_stitch_pass`'s matching comment.
        if show_seam_line {
            if fading_is_right {
                right_uniforms.lens_preview[2] = 1.0;
                right_uniforms.lens_preview[3] = calibration.topology.seam_offset;
            } else {
                left_uniforms.lens_preview[2] = 1.0;
                left_uniforms.lens_preview[3] = calibration.topology.seam_offset;
            }
        }

        gpu.queue.write_buffer(
            &self.left.uniform_buffer,
            0,
            bytemuck::bytes_of(&left_uniforms),
        );
        gpu.queue.write_buffer(
            &self.right.uniform_buffer,
            0,
            bytemuck::bytes_of(&right_uniforms),
        );
        gpu.queue.write_buffer(
            &self.multiband.mask_uniform_buffer,
            0,
            bytemuck::bytes_of(&mask_uniforms),
        );

        let mut encoder = gpu
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some(encoder_label),
            });

        let sized = self.multiband.sized.borrow();

        let single_plane_pass = |encoder: &mut wgpu::CommandEncoder,
                                 view: &wgpu::TextureView,
                                 texture_bg: &wgpu::BindGroup,
                                 uniform_bg: &wgpu::BindGroup,
                                 label: &str| {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some(label),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                        store: wgpu::StoreOp::Store,
                    },
                    depth_slice: None,
                })],
                depth_stencil_attachment: None,
                occlusion_query_set: None,
                timestamp_writes: None,
                multiview_mask: None,
            });
            pass.set_pipeline(&self.pipeline);
            pass.set_vertex_buffer(0, self.vertex_buffer.slice(..));
            pass.set_bind_group(0, texture_bg, &[]);
            pass.set_bind_group(1, uniform_bg, &[]);
            pass.draw(0..6, 0..1);
        };

        single_plane_pass(
            &mut encoder,
            &sized.view_a,
            &self.left.texture_bind_group,
            &self.left.uniform_bind_group,
            "multiband_render_a",
        );
        single_plane_pass(
            &mut encoder,
            &sized.view_b,
            &self.right.texture_bind_group,
            &self.right.uniform_bind_group,
            "multiband_render_b",
        );
        single_plane_pass(
            &mut encoder,
            &sized.view_mask,
            &mask_plane.texture_bind_group,
            &self.multiband.mask_uniform_bind_group,
            "multiband_render_mask",
        );

        // Blur radius derivation: capped well below `blend_width`'s full
        // UV-space width so the fixed-tap shader (`blur.wgsl`'s
        // `MAX_RADIUS`) stays well-sampled rather than approximating a
        // wide Gaussian from too few taps. This is a deliberate ceiling,
        // not the "true" width `blend_width` might suggest - a real
        // N-level pyramid would scale better; see FRICTION.md.
        //
        // When color match is active, widen the *driver* of this blur to
        // at least `color_match_band_width` instead of using `blend_width`
        // alone: a very narrow `blend_width` (a deliberately sharp,
        // near-hard seam) otherwise collapses this blur to its 2px floor,
        // which is nowhere near wide enough to hide the residual color
        // step color-match leaves behind (measurement noise, EMA lag, the
        // safety clamp) - the exact thing multiband blending exists to
        // hide. `color_match_band_width` is the region the measurement
        // itself considers comparable between the two cameras, so it's a
        // reasonable stand-in for "how wide a color transition is
        // actually needed here," independent of how sharp the *structural*
        // seam is meant to look. Left untouched when color match is off,
        // so plain multiband use (no color correction) keeps its existing
        // blend_width-only behavior.
        let blur_width_driver = if calibration.topology.color_match_enabled {
            blend_width.max(calibration.topology.color_match_band_width)
        } else {
            blend_width
        };
        let texel_size = [1.0 / target_width as f32, 1.0 / target_height as f32];
        let sigma_px = (blur_width_driver * target_width as f32 * 0.12).clamp(2.0, 10.0);

        let make_blur_uniform_bg = |sigma: f32, direction: [f32; 2], premultiply: bool| {
            let uniforms = BlurUniforms {
                texel_size,
                direction,
                params: [sigma, if premultiply { 1.0 } else { 0.0 }, 0.0, 0.0],
            };
            let buffer = gpu
                .device
                .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                    label: Some("multiband_blur_uniform"),
                    contents: bytemuck::bytes_of(&uniforms),
                    usage: wgpu::BufferUsages::UNIFORM,
                });
            gpu.device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("multiband_blur_uniform_bg"),
                layout: &self.multiband.small_uniform_layout,
                entries: &[wgpu::BindGroupEntry {
                    binding: 0,
                    resource: buffer.as_entire_binding(),
                }],
            })
        };

        let blur_pass = |encoder: &mut wgpu::CommandEncoder,
                         src_bg: &wgpu::BindGroup,
                         dst_view: &wgpu::TextureView,
                         uniform_bg: &wgpu::BindGroup,
                         label: &str| {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some(label),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: dst_view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                        store: wgpu::StoreOp::Store,
                    },
                    depth_slice: None,
                })],
                depth_stencil_attachment: None,
                occlusion_query_set: None,
                timestamp_writes: None,
                multiview_mask: None,
            });
            pass.set_pipeline(&self.multiband.blur_pipeline);
            pass.set_bind_group(0, src_bg, &[]);
            pass.set_bind_group(1, uniform_bg, &[]);
            pass.draw(0..3, 0..1);
        };

        // tex_a -> tmp (horizontal) -> tex_a_blur (vertical).
        let ubg = make_blur_uniform_bg(sigma_px, [1.0, 0.0], true);
        blur_pass(
            &mut encoder,
            &sized.bg_read_a,
            &sized.view_tmp,
            &ubg,
            "multiband_blur_a_h",
        );
        let ubg = make_blur_uniform_bg(sigma_px, [0.0, 1.0], false);
        blur_pass(
            &mut encoder,
            &sized.bg_read_tmp,
            &sized.view_a_blur,
            &ubg,
            "multiband_blur_a_v",
        );

        // tex_b -> tmp -> tex_b_blur.
        let ubg = make_blur_uniform_bg(sigma_px, [1.0, 0.0], true);
        blur_pass(
            &mut encoder,
            &sized.bg_read_b,
            &sized.view_tmp,
            &ubg,
            "multiband_blur_b_h",
        );
        let ubg = make_blur_uniform_bg(sigma_px, [0.0, 1.0], false);
        blur_pass(
            &mut encoder,
            &sized.bg_read_tmp,
            &sized.view_b_blur,
            &ubg,
            "multiband_blur_b_v",
        );

        // tex_mask -> tmp -> tex_mask_blur (the wide seam-position ramp).
        let ubg = make_blur_uniform_bg(sigma_px, [1.0, 0.0], true);
        blur_pass(
            &mut encoder,
            &sized.bg_read_mask,
            &sized.view_tmp,
            &ubg,
            "multiband_blur_mask_h",
        );
        let ubg = make_blur_uniform_bg(sigma_px, [0.0, 1.0], false);
        blur_pass(
            &mut encoder,
            &sized.bg_read_tmp,
            &sized.view_mask_blur,
            &ubg,
            "multiband_blur_mask_v",
        );

        // Composite: reconstruct low + high bands into the real target.
        let composite_uniforms = CompositeUniforms {
            params: [if fading_is_right { 1.0 } else { 0.0 }, 0.04, 0.0, 0.0],
        };
        let composite_uniform_buffer =
            gpu.device
                .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                    label: Some("multiband_composite_uniform"),
                    contents: bytemuck::bytes_of(&composite_uniforms),
                    usage: wgpu::BufferUsages::UNIFORM,
                });
        let composite_uniform_bg = gpu.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("multiband_composite_uniform_bg"),
            layout: &self.multiband.small_uniform_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: composite_uniform_buffer.as_entire_binding(),
            }],
        });

        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("multiband_composite"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: target_view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                        store: wgpu::StoreOp::Store,
                    },
                    depth_slice: None,
                })],
                depth_stencil_attachment: None,
                occlusion_query_set: None,
                timestamp_writes: None,
                multiview_mask: None,
            });
            pass.set_pipeline(&self.multiband.composite_pipeline);
            pass.set_bind_group(0, &sized.bg_composite_textures, &[]);
            pass.set_bind_group(1, &composite_uniform_bg, &[]);
            pass.draw(0..3, 0..1);
        }

        drop(sized);
        encoder
    }

    /// Render a stitched frame to the internal render target, without readback.
    ///
    /// Returns the recorded `CommandBuffer` without submitting it.
    /// The caller should submit it (typically together with NV12 conversion
    /// commands) to ensure proper GPU synchronization.
    /// Use [`Self::render_target`] to get a reference to the output texture.
    #[cfg_attr(
        feature = "profiling",
        tracing::instrument(skip_all, name = "gpu_render_to_target")
    )]
    #[allow(clippy::too_many_arguments)]
    pub fn render_to_target(
        &self,
        gpu: &GpuContext,
        scene: &SceneGeometry,
        calibration: &Calibration,
        viewport: &ResolvedViewport,
        blend_width: f32,
        color_correction: ColorCorrection,
        multiband_enabled: bool,
        show_seam_line: bool,
    ) -> wgpu::CommandBuffer {
        let aspect = self.output_width as f32 / self.output_height as f32;
        let encoder = if multiband_enabled {
            self.encode_multiband_stitch_pass(
                gpu,
                scene,
                calibration,
                viewport,
                blend_width,
                color_correction,
                show_seam_line,
                &self.render_target_view,
                self.output_width,
                self.output_height,
                aspect,
                "stitch_to_target_multiband",
            )
        } else {
            self.encode_stitch_pass(
                gpu,
                scene,
                calibration,
                viewport,
                blend_width,
                color_correction,
                show_seam_line,
                &self.render_target_view,
                aspect,
                "stitch_to_target",
            )
        };
        encoder.finish()
    }

    /// Access the internal render target texture.
    ///
    /// Used by [`Nv12Converter`](crate::gpu::nv12_converter::Nv12Converter) to read
    /// the RGBA output without an intermediate CPU copy.
    pub fn render_target(&self) -> &wgpu::Texture {
        &self.render_target
    }

    /// Render a stitched frame directly to a texture view (e.g., a window surface).
    ///
    /// Unlike [`Self::render_to_target`], this does NOT read back the result to CPU.
    /// Used for interactive preview windows.
    #[allow(clippy::too_many_arguments)]
    pub fn render_to_view(
        &self,
        gpu: &GpuContext,
        scene: &SceneGeometry,
        calibration: &Calibration,
        viewport: &ResolvedViewport,
        blend_width: f32,
        color_correction: ColorCorrection,
        multiband_enabled: bool,
        show_seam_line: bool,
        target_view: &wgpu::TextureView,
    ) {
        let aspect = viewport.config.width as f32 / viewport.config.height as f32;
        let encoder = if multiband_enabled {
            self.encode_multiband_stitch_pass(
                gpu,
                scene,
                calibration,
                viewport,
                blend_width,
                color_correction,
                show_seam_line,
                target_view,
                viewport.config.width,
                viewport.config.height,
                aspect,
                "preview_frame_multiband",
            )
        } else {
            self.encode_stitch_pass(
                gpu,
                scene,
                calibration,
                viewport,
                blend_width,
                color_correction,
                show_seam_line,
                target_view,
                aspect,
                "preview_frame",
            )
        };
        gpu.queue.submit(Some(encoder.finish()));
    }
}

// ---- Helper functions ----

/// Upload a packed RGBA plane (4 bytes per pixel) to a GPU texture.
///
/// Expects `width * height * 4` bytes in (R, G, B, A) order.
/// Callers with BGRA source data need to swizzle to RGBA before
/// this call - the shader samples `rgba.rgb` directly.
fn upload_bgra(gpu: &GpuContext, plane: &PlaneResources, rgba: &[u8]) -> Result<(), RenderError> {
    let w = plane.width;
    let h = plane.height;
    let expected = (w * h * 4) as usize;
    if rgba.len() != expected {
        return Err(RenderError::FrameSizeMismatch {
            expected,
            actual: rgba.len(),
        });
    }
    gpu.queue.write_texture(
        wgpu::TexelCopyTextureInfo {
            texture: &plane.y_texture,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        rgba,
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(w * 4),
            rows_per_image: Some(h),
        },
        wgpu::Extent3d {
            width: w,
            height: h,
            depth_or_array_layers: 1,
        },
    );
    Ok(())
}

/// GPU-to-GPU texture copy into a plane's y_texture.
fn copy_texture_to_plane(
    encoder: &mut wgpu::CommandEncoder,
    source: &wgpu::Texture,
    plane: &PlaneResources,
) {
    let size = wgpu::Extent3d {
        width: plane.width,
        height: plane.height,
        depth_or_array_layers: 1,
    };
    encoder.copy_texture_to_texture(
        wgpu::TexelCopyTextureInfo {
            texture: source,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        wgpu::TexelCopyTextureInfo {
            texture: &plane.y_texture,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        size,
    );
}

/// Upload a single R8Unorm plane to a GPU texture.
fn upload_plane(gpu: &GpuContext, texture: &wgpu::Texture, data: &[u8], width: u32, height: u32) {
    gpu.queue.write_texture(
        wgpu::TexelCopyTextureInfo {
            texture,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        data,
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(width),
            rows_per_image: Some(height),
        },
        wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
    );
}

/// Upload YUV420P planes (Y full-res, U/V half-res) to GPU textures.
fn upload_yuv(
    gpu: &GpuContext,
    plane: &PlaneResources,
    y: &[u8],
    u: &[u8],
    v: &[u8],
) -> Result<(), RenderError> {
    let w = plane.width;
    let h = plane.height;
    let uv_w = w / 2;
    let uv_h = h / 2;

    if y.len() != (w * h) as usize {
        return Err(RenderError::FrameSizeMismatch {
            expected: (w * h) as usize,
            actual: y.len(),
        });
    }
    if u.len() != (uv_w * uv_h) as usize {
        return Err(RenderError::FrameSizeMismatch {
            expected: (uv_w * uv_h) as usize,
            actual: u.len(),
        });
    }
    if v.len() != (uv_w * uv_h) as usize {
        return Err(RenderError::FrameSizeMismatch {
            expected: (uv_w * uv_h) as usize,
            actual: v.len(),
        });
    }

    upload_plane(gpu, &plane.y_texture, y, w, h);
    upload_plane(gpu, &plane.u_texture, u, uv_w, uv_h);
    upload_plane(gpu, &plane.v_texture, v, uv_w, uv_h);
    Ok(())
}

/// Upload NV12 planes (Y full-res, interleaved UV half-res) to GPU textures.
///
/// UV plane is `Rg8Unorm` at half resolution in each dimension.
/// Each texel contains (U, V) as two bytes.
fn upload_nv12(
    gpu: &GpuContext,
    plane: &PlaneResources,
    y: &[u8],
    uv: &[u8],
) -> Result<(), RenderError> {
    let w = plane.width;
    let h = plane.height;
    let uv_w = w / 2;
    let uv_h = h / 2;

    if y.len() != (w * h) as usize {
        return Err(RenderError::FrameSizeMismatch {
            expected: (w * h) as usize,
            actual: y.len(),
        });
    }
    if uv.len() != (uv_w * uv_h * 2) as usize {
        return Err(RenderError::FrameSizeMismatch {
            expected: (uv_w * uv_h * 2) as usize,
            actual: uv.len(),
        });
    }

    upload_plane(gpu, &plane.y_texture, y, w, h);
    // UV plane is Rg8Unorm: 2 bytes per texel, so bytes_per_row = uv_w * 2
    gpu.queue.write_texture(
        wgpu::TexelCopyTextureInfo {
            texture: &plane.u_texture,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        uv,
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
    Ok(())
}

/// Build the GPU uniform struct for one plane.
///
/// `flip_180`: when true, the shader flips UV coordinates to apply
/// 180-degree rotation. Used by the GPU zero-copy path where the CPU
/// buffer-reversal trick from the software decode path is not possible.
///
/// `ground_tilt`: this plane's near-field ground correction (zero/default
/// for any caller not rendering a calibrated stitch pair - single-camera
/// preview and lens-correction-tuning paths have no plane-pair placement
/// context for it to apply to).
///
/// `top_tilt`: this plane's top-of-frame correction, same caveat as
/// `ground_tilt` above.
#[allow(clippy::too_many_arguments)]
pub(crate) fn build_gpu_uniforms(
    mvp: &Matrix4<f32>,
    camera: &Lens,
    is_right: bool,
    blend_width: f32,
    input_format: InputFormat,
    flip_180: bool,
    is_full_range: bool,
    ground_tilt: GroundTilt,
    top_tilt: TopTilt,
) -> GpuUniforms {
    let w = camera.width as f32;
    let h = camera.height as f32;
    GpuUniforms {
        mvp: matrix4_to_columns(mvp),
        intrinsics: [
            camera.fx as f32 / w,
            camera.fy as f32 / h,
            camera.cx as f32 / w,
            camera.cy as f32 / h,
        ],
        dist: [
            camera.distortion[0] as f32,
            camera.distortion[1] as f32,
            camera.distortion[2] as f32,
            camera.distortion[3] as f32,
        ],
        color_scale: [1.0, 1.0, 1.0, 0.0],
        color_offset_blend: [0.0, 0.0, 0.0, blend_width],
        flags: [
            is_right as u32,
            match input_format {
                InputFormat::Yuv420p => 0,
                InputFormat::Nv12 => 1,
                InputFormat::Bgra => 2,
            },
            flip_180 as u32,
            is_full_range as u32,
        ],
        // Full correction for normal stitching. LensPreviewRenderer
        // overrides this field for the single-camera preview mode.
        lens_preview: [1.0, 0.0, 0.0, 0.0],
        ground_tilt: [ground_tilt.tilt, ground_tilt.k, w / h, 0.0],
        // .z/.w were otherwise-unused padding (this plane's aspect ratio
        // already lives in ground_tilt.z) - reused to carry both bands'
        // adjustable full-strength thresholds instead of adding a whole
        // new uniform slot for two scalars, same "repurpose spare padding"
        // pattern as lens_preview.z/.w above.
        top_tilt: [
            top_tilt.tilt,
            top_tilt.k,
            ground_tilt.band_full,
            top_tilt.band_full,
        ],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uniforms_are_normalized() {
        let camera = Lens::fisheye(
            3840,
            2160,
            1796.32,
            1797.22,
            1919.37,
            1063.17,
            [0.0342, 0.0677, -0.0741, 0.0299],
        );
        let mvp = Matrix4::identity();
        let u = build_gpu_uniforms(
            &mvp,
            &camera,
            false,
            0.0,
            InputFormat::Yuv420p,
            false,
            false,
            GroundTilt::default(),
            TopTilt::default(),
        );

        // fx/width ≈ 0.4678
        assert!((u.intrinsics[0] - 1796.32 / 3840.0).abs() < 1e-4);
        // cy/height ≈ 0.4922
        assert!((u.intrinsics[3] - 1063.17 / 2160.0).abs() < 1e-4);
        // is_right = 0, use_nv12 = 0
        assert_eq!(u.flags[0], 0);
        assert_eq!(u.flags[1], 0);
        // ground_tilt defaulted to zero (no-op) - not asserting the
        // aspect-ratio slot here, see ground_tilt_uniform_packs_plane_aspect
        assert_eq!(u.ground_tilt[0], 0.0);
        assert_eq!(u.ground_tilt[1], 0.0);
        assert_eq!(u.top_tilt[0], 0.0);
        assert_eq!(u.top_tilt[1], 0.0);
    }

    #[test]
    fn ground_tilt_uniform_packs_plane_aspect() {
        let camera = Lens {
            width: 3840,
            height: 2880,
            fx: 1457.07,
            fy: 1457.07,
            cx: 1920.0,
            cy: 1440.0,
            distortion: [0.0; 4],
            correction: 1.0,
        };
        let mvp = Matrix4::identity();
        let u = build_gpu_uniforms(
            &mvp,
            &camera,
            true,
            0.0,
            InputFormat::Yuv420p,
            false,
            false,
            GroundTilt {
                tilt: -0.09,
                k: 0.1897,
                band_full: 0.2,
            },
            TopTilt::default(),
        );
        assert_eq!(u.ground_tilt[0], -0.09);
        assert!((u.ground_tilt[1] - 0.1897).abs() < 1e-6);
        // plane_aspect = width / height = 3840 / 2880
        assert!((u.ground_tilt[2] - (3840.0 / 2880.0)).abs() < 1e-6);
        // ground_tilt's band_full is packed into top_tilt.z, not ground_tilt itself.
        assert_eq!(u.top_tilt[2], 0.2);
    }

    #[test]
    fn top_tilt_uniform_packs_correctly() {
        let camera = Lens {
            width: 3840,
            height: 2880,
            fx: 1457.07,
            fy: 1457.07,
            cx: 1920.0,
            cy: 1440.0,
            distortion: [0.0; 4],
            correction: 1.0,
        };
        let mvp = Matrix4::identity();
        let u = build_gpu_uniforms(
            &mvp,
            &camera,
            true,
            0.0,
            InputFormat::Yuv420p,
            false,
            false,
            GroundTilt::default(),
            TopTilt {
                tilt: 0.05,
                k: 0.1897,
                band_full: 0.18,
            },
        );
        assert_eq!(u.top_tilt[0], 0.05);
        assert!((u.top_tilt[1] - 0.1897).abs() < 1e-6);
        // ground_tilt untouched by a top_tilt-only call.
        assert_eq!(u.ground_tilt[0], 0.0);
        assert_eq!(u.top_tilt[3], 0.18);
    }

    #[test]
    fn opengl_to_wgpu_maps_z() {
        let m = opengl_to_wgpu_matrix();
        // Point at Z = -1 (OpenGL near) should map to Z = 0 (wgpu near)
        let p = m * nalgebra::Vector4::new(0.0, 0.0, -1.0, 1.0);
        assert!((p.z - (-0.5 + 0.5)).abs() < 1e-5); // -0.5 + 0.5 = 0
        // Point at Z = 1 (OpenGL far) should map to Z = 1 (wgpu far)
        let p = m * nalgebra::Vector4::new(0.0, 0.0, 1.0, 1.0);
        assert!((p.z - 1.0).abs() < 1e-5);
    }

    #[test]
    fn view_matrix_self_consistent_with_direction_to_yaw_pitch() {
        // Step 1e (un-ignored by Step 2's VirtualCamera basis fix):
        // directions synthesized at a known (yaw, pitch), run through
        // direction_to_yaw_pitch, then fed to view_matrix, must
        // transform a point on the dir ray to the camera's -Z axis
        // (the right-hand convention nalgebra::Isometry3::look_at_rh
        // uses).
        //
        // rig_tilt and rig_roll are both zero here: direction_to_yaw_pitch
        // does not take them (Model 4), so any non-zero tilt/roll
        // would break the round-trip by definition. Step 4 lands
        // RigCorrection and unblocks the full (yaw, pitch, tilt, roll)
        // version of this test.
        let camera_position = [0.24_f32, 0.0, 0.24];
        let yaw_steps = [-1.0_f32, -0.5, -0.1, 0.0, 0.1, 0.5, 1.0];
        let pitch_steps = [-0.6_f32, -0.2, 0.0, 0.2, 0.6];

        for &yaw in &yaw_steps {
            for &pitch in &pitch_steps {
                let dir = crate::projection::yaw_pitch_to_direction(yaw, pitch, &camera_position);
                let pos = crate::projection::direction_to_yaw_pitch(&dir, &camera_position);

                let view = view_matrix(&camera_position, pos.yaw, pos.pitch, 0.0, 0.0);

                // A point at eye + dir (unit step along the direction)
                // must land on camera-space -Z at distance 1.
                let target = nalgebra::Vector4::new(
                    camera_position[0] + dir.x,
                    camera_position[1] + dir.y,
                    camera_position[2] + dir.z,
                    1.0,
                );
                let cam = view * target;

                assert!(
                    cam.x.abs() < 1e-4,
                    "x should be zero (on camera forward axis), got {} at yaw={yaw} pitch={pitch}",
                    cam.x
                );
                assert!(
                    cam.y.abs() < 1e-4,
                    "y should be zero (on camera forward axis), got {} at yaw={yaw} pitch={pitch}",
                    cam.y
                );
                assert!(
                    (cam.z + 1.0).abs() < 1e-4,
                    "z should be -1 (camera looks down -Z), got {} at yaw={yaw} pitch={pitch}",
                    cam.z
                );
            }
        }
    }

    fn seam_test_calibration() -> Calibration {
        let lens = Lens {
            width: 1920,
            height: 1080,
            fx: 900.0,
            fy: 900.0,
            cx: 960.0,
            cy: 540.0,
            distortion: [0.0; 4],
            correction: 1.0,
        };
        Calibration {
            schema_version: 1,
            lenses: vec![lens.clone(), lens],
            topology: crate::calibration::Topology {
                intersect: 0.54,
                x_ty: 0.0,
                x_rz: 0.0,
                z_rx: 0.0,
                x_rx: 0.0,
                z_rz: 0.0,
                blend_width: 0.05,
                blend_flip_direction: false,
                seam_offset: 0.0,
                multiband_blend_enabled: false,
                color_match_enabled: true,
                color_match_band_width: 0.15,
                color_match_grid_cols: 8,
                color_match_grid_rows: 16,
                color_match_interval_frames: 15,
                color_match_ema_alpha: 0.15,
                color_match_max_y_offset: 0.06,
                color_match_max_chroma_offset: 0.04,
                ground_tilt_x: 0.0,
                ground_tilt_z: 0.0,
                top_tilt_x: 0.0,
                top_tilt_z: 0.0,
                ground_tilt_band_width: 0.16,
                top_tilt_band_width: 0.16,
            },
            framing: crate::calibration::Framing {
                axis_offset: 0.24,
                tilt: 0.0,
                roll: 0.0,
            },
            sync_offset: 0,
            field_roi: None,
            goal_geometry: None,
            autocam_defaults: None,
        }
    }

    #[test]
    fn seam_line_screen_points_some_when_blend_width_zero() {
        // A hard cut (no crossfade band) still has a seam *position* -
        // `blend_width` shapes the crossfade curve, it doesn't move the
        // seam, so the debug line/hit-test must stay usable at 0.0 (the
        // shader draws the line regardless of blend_width; the hit-test
        // used to disagree, making the line visible but undraggable).
        let mut cal = seam_test_calibration();
        cal.topology.blend_width = 0.0;
        let viewport = ViewportConfig::default();
        assert!(seam_line_screen_points(&cal, &viewport, 0.0, 0.0, 16.0 / 9.0).is_some());
    }

    #[test]
    fn seam_line_screen_points_returns_finite_points_near_center() {
        let cal = seam_test_calibration();
        let viewport = ViewportConfig::default();
        let (top, bottom) = seam_line_screen_points(&cal, &viewport, 0.0, 0.0, 16.0 / 9.0)
            .expect("seam offset 0 straight ahead should project on-screen");
        for (x, y) in [top, bottom] {
            assert!(x.is_finite() && y.is_finite());
            // Loosely bounded - a straight-ahead seam at the default rig
            // geometry should land roughly within the visible frame, not
            // off in the extreme numeric distance.
            assert!((-1.0..=2.0).contains(&x), "x={x} out of expected range");
            assert!((-1.0..=2.0).contains(&y), "y={y} out of expected range");
        }
    }

    #[test]
    fn seam_line_screen_points_shifts_monotonically_with_seam_offset() {
        let base_cal = seam_test_calibration();
        let viewport = ViewportConfig::default();

        let at = |offset: f32| {
            let mut cal = base_cal.clone();
            cal.topology.seam_offset = offset;
            seam_line_screen_points(&cal, &viewport, 0.0, 0.0, 16.0 / 9.0)
                .expect("should project")
                .0
                .0
        };

        let x_neg = at(-0.1);
        let x_zero = at(0.0);
        let x_pos = at(0.1);
        assert!(
            x_neg < x_zero && x_zero < x_pos,
            "seam_offset should shift the projected x monotonically: {x_neg} < {x_zero} < {x_pos}"
        );
    }

    #[test]
    fn seam_line_screen_points_tracks_yaw_not_just_straight_ahead() {
        let cal = seam_test_calibration();
        let viewport = ViewportConfig::default();
        let x_at = |yaw: f32| {
            seam_line_screen_points(&cal, &viewport, yaw, 0.0, 16.0 / 9.0)
                .expect("should project")
                .0
                .0
        };
        let straight = x_at(0.0);
        let panned = x_at(0.3);
        assert!(
            (straight - panned).abs() > 0.05,
            "panning yaw should measurably move the projected seam column: \
             straight={straight} panned={panned}"
        );
    }

    #[test]
    fn seam_line_screen_points_matches_real_screenshot_measurement() {
        let lens = Lens {
            width: 3840,
            height: 2880,
            fx: 1457.07373046875,
            fy: 1457.07373046875,
            cx: 1920.0,
            cy: 1440.0,
            distortion: [
                0.15513110160827637,
                0.1371408998966217,
                -0.0938614010810852,
                0.0041704000905156136,
            ],
            correction: 1.0,
        };
        let cal = Calibration {
            schema_version: 1,
            lenses: vec![lens.clone(), lens],
            topology: crate::calibration::Topology {
                intersect: 0.656978189945221,
                x_ty: -0.00602530796897377,
                x_rz: 0.004217210506111289,
                z_rx: -0.02375206433034046,
                x_rx: 0.0,
                z_rz: 0.0,
                blend_width: 0.19363637,
                blend_flip_direction: false,
                seam_offset: 0.15829112,
                multiband_blend_enabled: true,
                color_match_enabled: true,
                color_match_band_width: 0.4,
                color_match_grid_cols: 8,
                color_match_grid_rows: 16,
                color_match_interval_frames: 15,
                color_match_ema_alpha: 0.15,
                color_match_max_y_offset: 0.06,
                color_match_max_chroma_offset: 0.0,
                ground_tilt_x: -0.007000000681728125,
                ground_tilt_z: 0.0010000000474974513,
                top_tilt_x: 0.0,
                top_tilt_z: 0.0,
                ground_tilt_band_width: 0.16,
                top_tilt_band_width: 0.16,
            },
            framing: crate::calibration::Framing {
                axis_offset: 0.18876110017299652,
                tilt: 0.0,
                roll: 0.0,
            },
            sync_offset: 3,
            field_roi: None,
            goal_geometry: None,
            autocam_defaults: None,
        };
        let viewport = ViewportConfig {
            fov_degrees: 75.0,
            ..ViewportConfig::default()
        };

        // Real-world ground truth: a screenshot of this exact calibration
        // with "Show seam line" on had the red line's reddest pixel at
        // x=779-780 out of a 1020px-wide, 0-offset content box (measured
        // programmatically, not eyeballed) - i.e. a screen fraction of
        // ~0.499-0.501. This test exists to catch a regression back to the
        // bug this was fixed against (missing the fragment shader's `uv =
        // in.uv * 2.0 - 0.5` remap, which put the computed column at 0.36
        // instead of ~0.50 - a 140px error, not a rounding difference).
        let (top, bottom) = seam_line_screen_points(&cal, &viewport, 0.0, 0.0, 1020.0 / 688.0)
            .expect("should project");
        eprintln!("top_frac={top:?} bottom_frac={bottom:?}");
        assert!(
            (top.0 - 0.5).abs() < 0.02,
            "expected top.x near 0.5 (measured ~0.499-0.501 from a real screenshot), got {}",
            top.0
        );
        assert!(
            (bottom.0 - 0.5).abs() < 0.02,
            "expected bottom.x near 0.5, got {}",
            bottom.0
        );
    }
}
