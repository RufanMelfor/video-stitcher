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
use super::viewport::ResolvedViewport;
use crate::calibration::{CameraParams, MatchCalibration};
use crate::gpu::GpuContext;

use bytemuck::{Pod, Zeroable};
use nalgebra::{Matrix4, Perspective3, Point3, UnitQuaternion};
use std::cell::RefCell;
use thiserror::Error;
use wgpu::util::DeviceExt;

// ---- Constants ----

/// Near clipping plane for the perspective projection.
const NEAR_PLANE: f32 = 0.01;
/// Far clipping plane for the perspective projection.
const FAR_PLANE: f32 = 5.0;
/// Aspect ratio of scene planes (matches GoPro 16:9 capture).
///
/// Deprecated: derive the aspect ratio from camera parameters instead.
/// Use [`SceneGeometry::from_layout_with_aspect`](crate::render::scene::SceneGeometry::from_layout_with_aspect)
/// with `camera.width as f32 / camera.height as f32`.
#[deprecated(
    since = "0.1.0",
    note = "derive aspect ratio from camera parameters (width/height) instead"
)]
pub const PLANE_ASPECT: f32 = 16.0 / 9.0;

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
}

/// Per-plane ground-plane tilt correction for [`build_gpu_uniforms`] and
/// [`super::single_camera::SingleCameraRenderer::render_and_readback`] - see
/// `PlaneLayout::ground_tilt_x`/`ground_tilt_z`'s doc comment
/// (`crates/reco-core/src/calibration.rs`) and `fisheye.wgsl`'s
/// `band_limited_ground_warp` for the full picture. `k` only matters when
/// `tilt != 0.0`; `Default` (both zero) is the "no correction" no-op. `pub`
/// (not `pub(crate)`): `render_and_readback` is called from other crates
/// (`reco-calibrate`'s validation examples), so this type has to be at
/// least as visible as that function.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct GroundTilt {
    pub tilt: f32,
    pub k: f32,
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
struct Vertex {
    position: [f32; 3],
    uv: [f32; 2],
}

impl Vertex {
    const LAYOUT: wgpu::VertexBufferLayout<'static> = wgpu::VertexBufferLayout {
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
    pub fn new(
        gpu: &GpuContext,
        output_width: u32,
        output_height: u32,
        input_width: u32,
        input_height: u32,
        output_format: wgpu::TextureFormat,
        input_format: InputFormat,
        scene: &SceneGeometry,
    ) -> Self {
        let device = &gpu.device;

        // Shader
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("fisheye"),
            source: wgpu::ShaderSource::Wgsl(include_str!("../shaders/fisheye.wgsl").into()),
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
                entry_point: Some("vs_main"),
                compilation_options: Default::default(),
                buffers: &[Vertex::LAYOUT],
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_main"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format: output_format,
                    blend: Some(wgpu::BlendState {
                        color: wgpu::BlendComponent {
                            src_factor: wgpu::BlendFactor::SrcAlpha,
                            dst_factor: wgpu::BlendFactor::OneMinusSrcAlpha,
                            operation: wgpu::BlendOperation::Add,
                        },
                        alpha: wgpu::BlendComponent::OVER,
                    }),
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
        calibration: &MatchCalibration,
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
            viewport.config.rig_tilt,
            viewport.config.rig_roll,
        );

        // Ground-tilt mapping is not the naive left<->left, right<->right
        // pairing it looks like: `PlaneLayout::ground_tilt_x` corrects the
        // x-plane, which - per reco_calibrate::geometry's module doc (the
        // v1-derived left/right swap) - holds the *right* camera's content
        // and is positioned at this renderer's "right" plane; ground_tilt_z
        // corrects the z-plane (*left* camera, this renderer's "left"
        // plane). Verified against `SceneGeometry::from_layout_with_aspect`:
        // its "left plane" sits at `[0,0,half_offset]` (geometry.rs's
        // z-plane translation) and "right plane" at `[half_offset,...]`
        // (geometry.rs's x-plane translation) - so this renderer's own
        // left/right naming already matches the physical cameras directly,
        // and it's only the optimizer's internal x-plane/z-plane bookkeeping
        // that's swapped.
        let left_ground_tilt = GroundTilt {
            tilt: calibration.layout.ground_tilt_z as f32,
            k: calibration.left.ground_tilt_k() as f32,
        };
        let right_ground_tilt = GroundTilt {
            tilt: calibration.layout.ground_tilt_x as f32,
            k: calibration.right.ground_tilt_k() as f32,
        };

        let left_mvp = projection * view * scene.model_matrix_left();
        let correction = viewport.config.lens_correction_amount;
        let mut left_uniforms = build_gpu_uniforms(
            &left_mvp,
            &calibration.left,
            false,
            blend_width,
            self.input_format,
            self.flip_180[0],
            self.is_full_range,
            left_ground_tilt,
        );
        left_uniforms.lens_preview[0] = correction;

        let right_mvp = projection * view * scene.model_matrix_right();
        let mut right_uniforms = build_gpu_uniforms(
            &right_mvp,
            &calibration.right,
            true,
            blend_width,
            self.input_format,
            self.flip_180[1],
            self.is_full_range,
            right_ground_tilt,
        );
        right_uniforms.lens_preview[0] = correction;

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
        let flip = viewport.config.blend_flip_direction;
        left_uniforms.ground_tilt[3] = if flip { 1.0 } else { 0.0 };
        right_uniforms.ground_tilt[3] = if flip { 0.0 } else { 1.0 };

        // Seam-line debug overlay and the manual seam_offset nudge both
        // only apply to the fading-designated plane, same convention as
        // the fade flag above - see the shader's seam-line comment and
        // `MatchCalibration::seam_offset`'s doc.
        if show_seam_line {
            if flip {
                left_uniforms.lens_preview[2] = 1.0;
            } else {
                right_uniforms.lens_preview[2] = 1.0;
            }
        }
        if flip {
            left_uniforms.lens_preview[3] = viewport.config.seam_offset;
        } else {
            right_uniforms.lens_preview[3] = viewport.config.seam_offset;
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
        calibration: &MatchCalibration,
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
            viewport.config.rig_tilt,
            viewport.config.rig_roll,
        );

        let left_ground_tilt = GroundTilt {
            tilt: calibration.layout.ground_tilt_z as f32,
            k: calibration.left.ground_tilt_k() as f32,
        };
        let right_ground_tilt = GroundTilt {
            tilt: calibration.layout.ground_tilt_x as f32,
            k: calibration.right.ground_tilt_k() as f32,
        };

        let left_mvp = projection * view * scene.model_matrix_left();
        let correction_amount = viewport.config.lens_correction_amount;
        let mut left_uniforms = build_gpu_uniforms(
            &left_mvp,
            &calibration.left,
            false,
            blend_width,
            self.input_format,
            self.flip_180[0],
            self.is_full_range,
            left_ground_tilt,
        );
        left_uniforms.lens_preview[0] = correction_amount;
        left_uniforms.color_offset_blend[0] = color_correction.left_offset[0];
        left_uniforms.color_offset_blend[1] = color_correction.left_offset[1];
        left_uniforms.color_offset_blend[2] = color_correction.left_offset[2];
        left_uniforms.ground_tilt[3] = 0.0; // tex_a: hard FOV coverage, never fades.

        let right_mvp = projection * view * scene.model_matrix_right();
        let mut right_uniforms = build_gpu_uniforms(
            &right_mvp,
            &calibration.right,
            true,
            blend_width,
            self.input_format,
            self.flip_180[1],
            self.is_full_range,
            right_ground_tilt,
        );
        right_uniforms.lens_preview[0] = correction_amount;
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
        let flip = viewport.config.blend_flip_direction;
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
        mask_uniforms.lens_preview[3] = viewport.config.seam_offset;

        // Seam-line debug overlay: set on tex_a/tex_b (whichever is the
        // fading-designated side) *after* mask_uniforms was copied above,
        // so the mask-only pass (never visibly composited) doesn't also
        // carry the flag. See `encode_stitch_pass`'s matching comment.
        if show_seam_line {
            if fading_is_right {
                right_uniforms.lens_preview[2] = 1.0;
                right_uniforms.lens_preview[3] = viewport.config.seam_offset;
            } else {
                left_uniforms.lens_preview[2] = 1.0;
                left_uniforms.lens_preview[3] = viewport.config.seam_offset;
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
        let texel_size = [1.0 / target_width as f32, 1.0 / target_height as f32];
        let sigma_px = (blend_width * target_width as f32 * 0.12).clamp(2.0, 10.0);

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
        calibration: &MatchCalibration,
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
        calibration: &MatchCalibration,
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

/// Build the view matrix for the virtual camera.
///
/// Camera sits at `position` and looks at the origin (corner where the two
/// planes meet) by default. This matches v1 Three.js where the OrbitControls
/// target is `[0, 0, 0]`. `yaw` rotates around Y (left/right from center),
/// `pitch` rotates around X (up/down).
fn view_matrix(
    position: &[f32; 3],
    yaw: f32,
    pitch: f32,
    rig_tilt: f32,
    rig_roll: f32,
) -> Matrix4<f32> {
    // Basis is owned by VirtualCamera so view_matrix and the yaw/pitch
    // decomposition in projection.rs share a single source of truth.
    // The right-handed `(base_forward, base_right, world_up)` triple
    // makes view_matrix's yaw sign agree with
    // `direction_to_yaw_pitch` without any downstream sign reconciliation.
    let cam = crate::projection::VirtualCamera::new(position);
    let eye = Point3::from(cam.eye);
    let mut base_forward = cam.base_forward;
    let base_right = cam.base_right;
    let mut world_up = crate::projection::VirtualCamera::world_up();

    // Rig tilt: rotate the entire reference frame around the base right axis.
    // This tilts "up" and "forward" so that yaw/pitch operate in the tilted
    // coordinate system. Panning in this tilted frame naturally introduces
    // roll that compensates for edge distortion from a tilted camera rig.
    if rig_tilt.abs() > 1e-6 {
        let tilt_q =
            UnitQuaternion::from_axis_angle(&nalgebra::Unit::new_normalize(base_right), rig_tilt);
        base_forward = tilt_q * base_forward;
        world_up = tilt_q * world_up;
    }

    // Rig roll: rotate around the forward axis to correct lateral lean.
    // Negated because roll describes the camera's lean direction, and we
    // need to rotate the opposite way to straighten the horizon.
    // (Tilt is not negated because it shifts the view center to match
    // where the camera points, which is the same direction.)
    if rig_roll.abs() > 1e-6 {
        let roll_q = UnitQuaternion::from_axis_angle(
            &nalgebra::Unit::new_normalize(base_forward),
            -rig_roll,
        );
        world_up = roll_q * world_up;
    }

    // Yaw: rotate around the (possibly tilted) up axis
    let up_axis = nalgebra::Unit::new_normalize(world_up);
    let yaw_q = UnitQuaternion::from_axis_angle(&up_axis, yaw);
    // Pitch: rotate around the yaw-rotated right axis
    let right = yaw_q * base_right;
    let pitch_q = UnitQuaternion::from_axis_angle(&nalgebra::Unit::new_normalize(right), pitch);
    let rotation = pitch_q * yaw_q;
    let forward = rotation * base_forward;
    let up = rotation * world_up;
    let target = Point3::from(eye.coords + forward);
    nalgebra::Isometry3::look_at_rh(&eye, &target, &up).to_homogeneous()
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
#[allow(clippy::too_many_arguments)]
pub(crate) fn build_gpu_uniforms(
    mvp: &Matrix4<f32>,
    camera: &CameraParams,
    is_right: bool,
    blend_width: f32,
    input_format: InputFormat,
    flip_180: bool,
    is_full_range: bool,
    ground_tilt: GroundTilt,
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
            camera.d[0] as f32,
            camera.d[1] as f32,
            camera.d[2] as f32,
            camera.d[3] as f32,
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
    }
}

/// Convert a nalgebra `Matrix4` to column-major `[[f32; 4]; 4]` for wgpu.
pub(crate) fn matrix4_to_columns(m: &Matrix4<f32>) -> [[f32; 4]; 4] {
    let s = m.as_slice();
    [
        [s[0], s[1], s[2], s[3]],
        [s[4], s[5], s[6], s[7]],
        [s[8], s[9], s[10], s[11]],
        [s[12], s[13], s[14], s[15]],
    ]
}

/// OpenGL to wgpu clip space correction: Z from \[-1,1\] to \[0,1\].
///
/// nalgebra's `Perspective3` uses OpenGL conventions. wgpu expects
/// clip space Z in [0, 1], so we apply this correction.
#[rustfmt::skip]
pub(crate) fn opengl_to_wgpu_matrix() -> Matrix4<f32> {
    Matrix4::new(
        1.0, 0.0, 0.0, 0.0,
        0.0, 1.0, 0.0, 0.0,
        0.0, 0.0, 0.5, 0.5,
        0.0, 0.0, 0.0, 1.0,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uniforms_are_normalized() {
        let camera = CameraParams {
            width: 3840,
            height: 2160,
            fx: 1796.32,
            fy: 1797.22,
            cx: 1919.37,
            cy: 1063.17,
            d: [0.0342, 0.0677, -0.0741, 0.0299],
        };
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
    }

    #[test]
    fn ground_tilt_uniform_packs_plane_aspect() {
        let camera = CameraParams {
            width: 3840,
            height: 2880,
            fx: 1457.07,
            fy: 1457.07,
            cx: 1920.0,
            cy: 1440.0,
            d: [0.0; 4],
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
            },
        );
        assert_eq!(u.ground_tilt[0], -0.09);
        assert!((u.ground_tilt[1] - 0.1897).abs() < 1e-6);
        // plane_aspect = width / height = 3840 / 2880
        assert!((u.ground_tilt[2] - (3840.0 / 2880.0)).abs() < 1e-6);
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
}
