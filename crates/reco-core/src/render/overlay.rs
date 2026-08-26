//! Generic RGBA overlay composition for post-camera graphics.
//!
//! The compositor deliberately knows nothing about sports, HTML, or browser
//! engines. Consumers upload an [`OverlayFrame`] and the compositor blends it
//! over the final camera image after stitching and viewport/autocam rendering.

use std::num::NonZeroU64;

use bytemuck::{Pod, Zeroable};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use wgpu::util::DeviceExt;

use crate::gpu::GpuContext;

const STRAIGHT_ALPHA_BLEND: wgpu::BlendState = wgpu::BlendState {
    color: wgpu::BlendComponent {
        src_factor: wgpu::BlendFactor::SrcAlpha,
        dst_factor: wgpu::BlendFactor::OneMinusSrcAlpha,
        operation: wgpu::BlendOperation::Add,
    },
    alpha: wgpu::BlendComponent {
        src_factor: wgpu::BlendFactor::One,
        dst_factor: wgpu::BlendFactor::OneMinusSrcAlpha,
        operation: wgpu::BlendOperation::Add,
    },
};

/// A complete straight-alpha RGBA8 overlay surface.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OverlayFrame {
    /// Pixel width of the actual pixel buffer in `rgba` - may be smaller
    /// than `design_size` when a producer has pre-scaled its rendering
    /// down to roughly match how large this frame will end up on
    /// screen (e.g. the scoreboard renderer picking a Chrome device
    /// scale factor for the current placement), to avoid relying on
    /// GPU minification for quality. Equal to `design_size` for every
    /// producer that doesn't do this.
    pub width: u32,
    /// Pixel height of the actual pixel buffer in `rgba` - see `width`.
    pub height: u32,
    /// The resolution this frame's placement (contain-fit + offset,
    /// see [`OverlayPlacement`]) is computed against - independent of
    /// `width`/`height`, which describe only how large the actual
    /// pixel buffer is. Keeping these separate lets a producer shrink
    /// its real pixel buffer for sharper rendering at a known target
    /// size without that shrink *also* changing where/how large the
    /// frame is placed - placement always reasons about `design_size`,
    /// texture upload/sampling always uses `width`/`height`.
    pub design_size: (u32, u32),
    /// Tightly packed RGBA8 pixels in row-major order, `width x height`.
    pub rgba: Vec<u8>,
}

impl OverlayFrame {
    /// Validate dimensions and byte length.
    pub fn validate(&self) -> Result<(), OverlayError> {
        if self.width == 0 || self.height == 0 {
            return Err(OverlayError::InvalidFrame(
                "overlay dimensions must be non-zero".into(),
            ));
        }
        if self.design_size.0 == 0 || self.design_size.1 == 0 {
            return Err(OverlayError::InvalidFrame(
                "overlay design_size must be non-zero".into(),
            ));
        }
        let expected = self
            .width
            .checked_mul(self.height)
            .and_then(|pixels| pixels.checked_mul(4))
            .ok_or_else(|| OverlayError::InvalidFrame("overlay dimensions overflow".into()))?
            as usize;
        if self.rgba.len() != expected {
            return Err(OverlayError::InvalidFrame(format!(
                "expected {expected} RGBA bytes for {}x{}, got {}",
                self.width,
                self.height,
                self.rgba.len()
            )));
        }
        Ok(())
    }
}

/// Non-blocking source of independently rendered overlay frames.
///
/// Implementations may run a browser, network client, or another renderer on
/// their own thread. [`try_frame`](Self::try_frame) must never wait for a new
/// frame: video processing reuses the previous GPU texture when it returns
/// `Ok(None)`.
pub trait OverlayFrameSource: Send {
    /// Return the newest available frame, or `None` when nothing changed.
    fn try_frame(&mut self) -> Result<Option<OverlayFrame>, String>;
}

/// A transition overlay: **one fixed image** whose visibility varies
/// over time, composited underneath any [`OverlayFrameSource`] so a
/// content overlay (a scoreboard) stays legible through it.
///
/// Deliberately separate from [`OverlayFrameSource`], which exists for
/// overlays whose *pixels* change. A transition's pixels never do, so
/// its image is uploaded to the GPU once and each frame costs a single
/// uniform write - see [`RgbaOverlayCompositor::set_opacity`]. Modelling
/// a fade as a changing image instead means rebuilding and re-uploading
/// a full-resolution frame every time, which is what made the "PAUZE"
/// dip-to-black cost more per frame than stitching and AI tracking
/// combined despite being an incomparably simpler operation.
pub trait OverlayTransitionSource: Send {
    /// The image to composite, at full opacity. Read once, when the
    /// transition is attached.
    fn card(&self) -> &OverlayFrame;

    /// Advance exactly one output frame and return that frame's opacity,
    /// `0.0` (nothing drawn) to `1.0` (fully opaque).
    ///
    /// Called once per encoded output frame, in the same order the
    /// session's frame counter advances, so an implementation can drive
    /// itself from its own counter without an external timestamp.
    fn advance(&mut self) -> f32;
}

#[repr(C)]
#[derive(Copy, Clone, Pod, Zeroable)]
struct OverlayParams {
    reference_size: [f32; 2],
    output_size: [f32; 2],
    /// Fraction of `output_size` the overlay's centerpoint is shifted from
    /// the frame's own center - `[0.0, 0.0]` (default) keeps the existing
    /// auto-centered behavior. See [`OverlayPlacement`].
    placement_offset: [f32; 2],
    /// Multiplies the auto-fit scale that fits `reference_size` into
    /// `output_size` - `1.0` (default) reproduces the previous
    /// letterboxed-and-centered behavior exactly.
    placement_scale: f32,
    /// Multiplies the sampled alpha - `1.0` (default) draws the overlay
    /// unchanged. Occupies what used to be pure padding, so a fade
    /// costs no extra uniform bandwidth. See
    /// [`RgbaOverlayCompositor::set_opacity`].
    opacity: f32,
}

/// Where and how large a composited overlay appears within the output
/// frame, independent of anything the overlay package itself knows about
/// (see this module's doc comment - the compositor stays sport/package
/// agnostic). `Default` reproduces the original centered-letterbox
/// behavior exactly, so existing callers are unaffected.
#[derive(Copy, Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct OverlayPlacement {
    /// Fraction of the output frame's width/height the overlay's
    /// centerpoint is shifted from the frame's own center, e.g. `[0.0,
    /// 0.35]` moves it most of the way toward the bottom edge.
    pub offset: (f32, f32),
    /// Multiplies the auto-fit ("contain") scale - `1.0` is the largest
    /// size that still fits entirely inside the output frame; `0.3` is
    /// roughly a third of that.
    pub scale: f32,
}

impl Default for OverlayPlacement {
    fn default() -> Self {
        Self {
            offset: (0.0, 0.0),
            scale: 1.0,
        }
    }
}

/// The scale factor at which a frame's actual pixel content will end
/// up on screen, given its `design_size`, the final `output_size` it's
/// composited into, and its `placement` - the exact same contain-fit +
/// `placement.scale` math [`RgbaOverlayCompositor`]'s shader uses.
///
/// Meant for a producer capable of choosing its own render resolution
/// (e.g. the scoreboard renderer's headless-Chrome device pixel ratio)
/// to pre-scale its actual pixel buffer to roughly match this, instead
/// of always rendering at full `design_size` and relying on GPU
/// minification for quality - see [`OverlayFrame::design_size`] for
/// why that pixel-size choice is independent of the placement math
/// itself.
pub fn contain_fit_render_scale(
    design_size: (u32, u32),
    output_size: (u32, u32),
    placement: OverlayPlacement,
) -> f32 {
    if design_size.0 == 0 || design_size.1 == 0 {
        return placement.scale;
    }
    let fit = (output_size.0 as f32 / design_size.0 as f32)
        .min(output_size.1 as f32 / design_size.1 as f32);
    fit * placement.scale
}

/// Cached GPU resources for blending one RGBA surface over render targets.
pub(crate) struct RgbaOverlayCompositor {
    pipeline: wgpu::RenderPipeline,
    bind_group_layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    texture: wgpu::Texture,
    bind_group: wgpu::BindGroup,
    params_buffer: wgpu::Buffer,
    /// The frame's `design_size` - drives the shader's contain-fit
    /// placement math. Independent of the texture's actual pixel
    /// dimensions (`texture_size`) - see [`OverlayFrame::design_size`].
    reference_size: (u32, u32),
    /// The GPU texture's actual allocated pixel dimensions - tracks
    /// the uploaded frame's `(width, height)`, which may be smaller
    /// than `reference_size`. Recreating the texture is keyed off this,
    /// not `reference_size`.
    texture_size: (u32, u32),
    output_size: (u32, u32),
    placement: OverlayPlacement,
    /// Alpha multiplier applied to every sampled texel - see
    /// [`Self::set_opacity`].
    opacity: f32,
}

impl RgbaOverlayCompositor {
    pub(crate) fn new(
        gpu: &GpuContext,
        output_format: wgpu::TextureFormat,
        output_size: (u32, u32),
        frame: &OverlayFrame,
    ) -> Result<Self, OverlayError> {
        frame.validate()?;
        let device = gpu.device();
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("reco rgba overlay shader"),
            source: wgpu::ShaderSource::Wgsl(include_str!("../shaders/rgba_overlay.wgsl").into()),
        });
        let bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("reco rgba overlay bind group layout"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: NonZeroU64::new(
                            std::mem::size_of::<OverlayParams>() as u64
                        ),
                    },
                    count: None,
                },
            ],
        });
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("reco rgba overlay pipeline layout"),
            bind_group_layouts: &[&bind_group_layout],
            immediate_size: 0,
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("reco rgba overlay pipeline"),
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                compilation_options: Default::default(),
                buffers: &[],
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_main"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format: output_format,
                    blend: Some(STRAIGHT_ALPHA_BLEND),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview_mask: None,
            cache: None,
        });
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("reco rgba overlay sampler"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        let params_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("reco rgba overlay params"),
            contents: bytemuck::bytes_of(&OverlayParams {
                reference_size: [frame.design_size.0 as f32, frame.design_size.1 as f32],
                output_size: [output_size.0 as f32, output_size.1 as f32],
                placement_offset: [0.0, 0.0],
                placement_scale: 1.0,
                opacity: 1.0,
            }),
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        });
        let texture = Self::create_texture(device, frame.width, frame.height);
        let bind_group = Self::create_bind_group(
            device,
            &bind_group_layout,
            &texture,
            &sampler,
            &params_buffer,
        );
        let mut compositor = Self {
            pipeline,
            bind_group_layout,
            sampler,
            texture,
            bind_group,
            params_buffer,
            reference_size: frame.design_size,
            texture_size: (frame.width, frame.height),
            output_size,
            placement: OverlayPlacement::default(),
            opacity: 1.0,
        };
        compositor.upload(gpu, frame)?;
        Ok(compositor)
    }

    fn create_texture(device: &wgpu::Device, width: u32, height: u32) -> wgpu::Texture {
        device.create_texture(&wgpu::TextureDescriptor {
            label: Some("reco cached rgba overlay"),
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            // Browser bytes are already in the same display-encoded space as
            // the stitcher's Rgba8Unorm output. Do not apply an sRGB decode.
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        })
    }

    fn create_bind_group(
        device: &wgpu::Device,
        layout: &wgpu::BindGroupLayout,
        texture: &wgpu::Texture,
        sampler: &wgpu::Sampler,
        params: &wgpu::Buffer,
    ) -> wgpu::BindGroup {
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("reco rgba overlay bind group"),
            layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(sampler),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: params.as_entire_binding(),
                },
            ],
        })
    }

    pub(crate) fn upload(
        &mut self,
        gpu: &GpuContext,
        frame: &OverlayFrame,
    ) -> Result<(), OverlayError> {
        frame.validate()?;
        let mut params_dirty = false;
        if self.texture_size != (frame.width, frame.height) {
            self.texture = Self::create_texture(gpu.device(), frame.width, frame.height);
            self.bind_group = Self::create_bind_group(
                gpu.device(),
                &self.bind_group_layout,
                &self.texture,
                &self.sampler,
                &self.params_buffer,
            );
            self.texture_size = (frame.width, frame.height);
        }
        if self.reference_size != frame.design_size {
            self.reference_size = frame.design_size;
            params_dirty = true;
        }
        if params_dirty {
            self.write_params(gpu);
        }
        gpu.queue().write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &self.texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            &frame.rgba,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(frame.width * 4),
                rows_per_image: Some(frame.height),
            },
            wgpu::Extent3d {
                width: frame.width,
                height: frame.height,
                depth_or_array_layers: 1,
            },
        );
        Ok(())
    }

    pub(crate) fn resize(&mut self, gpu: &GpuContext, output_size: (u32, u32)) {
        self.output_size = output_size;
        self.write_params(gpu);
    }

    /// Reposition/resize the overlay within the output frame - see
    /// [`OverlayPlacement`]. Cheap (one uniform-buffer write), safe to call
    /// every frame while a user drags an on-screen handle.
    pub(crate) fn set_placement(&mut self, gpu: &GpuContext, placement: OverlayPlacement) {
        self.placement = placement;
        self.write_params(gpu);
    }

    /// Scale the alpha of every texel this compositor draws, `0.0`
    /// (invisible) to `1.0` (unchanged).
    ///
    /// This is what makes a fade cost nothing: the texture stays exactly
    /// as uploaded and only a 32-byte uniform is rewritten, instead of
    /// rebuilding a full-frame image on the CPU and pushing it across
    /// the bus every frame (at 2560x1440 that is ~14.7MB per frame,
    /// which is precisely why the CPU-composited PAUZE transition
    /// dragged a 30fps export down to ~9fps during its fades).
    pub(crate) fn set_opacity(&mut self, gpu: &GpuContext, opacity: f32) {
        let opacity = opacity.clamp(0.0, 1.0);
        if self.opacity == opacity {
            return;
        }
        self.opacity = opacity;
        self.write_params(gpu);
    }

    fn write_params(&self, gpu: &GpuContext) {
        gpu.queue().write_buffer(
            &self.params_buffer,
            0,
            bytemuck::bytes_of(&OverlayParams {
                reference_size: [self.reference_size.0 as f32, self.reference_size.1 as f32],
                output_size: [self.output_size.0 as f32, self.output_size.1 as f32],
                placement_offset: [self.placement.offset.0, self.placement.offset.1],
                placement_scale: self.placement.scale,
                opacity: self.opacity,
            }),
        );
    }

    pub(crate) fn encode(
        &self,
        gpu: &GpuContext,
        target: &wgpu::TextureView,
    ) -> wgpu::CommandBuffer {
        let mut encoder = gpu
            .device()
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("reco rgba overlay encoder"),
            });
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("reco rgba overlay pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: target,
                    resolve_target: None,
                    depth_slice: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Load,
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            pass.set_pipeline(&self.pipeline);
            pass.set_bind_group(0, &self.bind_group, &[]);
            pass.draw(0..3, 0..1);
        }
        encoder.finish()
    }
}

/// Errors validating or preparing an overlay surface.
#[derive(Debug, Clone, Error)]
pub enum OverlayError {
    /// The supplied RGBA surface is malformed.
    #[error("invalid overlay frame: {0}")]
    InvalidFrame(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_tightly_packed_rgba() {
        let valid = OverlayFrame {
            width: 2,
            height: 3,
            design_size: (2, 3),
            rgba: vec![0; 24],
        };
        assert!(valid.validate().is_ok());
        let invalid = OverlayFrame {
            rgba: vec![0; 23],
            ..valid
        };
        assert!(invalid.validate().is_err());
    }

    #[test]
    fn rejects_zero_and_overflowing_dimensions() {
        assert!(
            OverlayFrame {
                width: 0,
                height: 1080,
                design_size: (1920, 1080),
                rgba: Vec::new(),
            }
            .validate()
            .is_err()
        );
        assert!(
            OverlayFrame {
                width: u32::MAX,
                height: u32::MAX,
                design_size: (u32::MAX, u32::MAX),
                rgba: Vec::new(),
            }
            .validate()
            .is_err()
        );
    }

    #[test]
    fn contain_fit_render_scale_matches_shader_math() {
        // 1920x1080 design, contain-fit into a 2560x1440 output: the
        // height ratio (1440/1080 = 1.333) is the binding constraint,
        // same as `min()` in the shader.
        let scale = contain_fit_render_scale(
            (1920, 1080),
            (2560, 1440),
            OverlayPlacement {
                offset: (0.0, 0.0),
                scale: 0.5,
            },
        );
        assert!((scale - (1440.0 / 1080.0 * 0.5)).abs() < 1e-5);
    }

    #[test]
    fn contain_fit_render_scale_falls_back_to_placement_scale_for_zero_design_size() {
        let placement = OverlayPlacement {
            offset: (0.0, 0.0),
            scale: 0.4,
        };
        assert_eq!(
            contain_fit_render_scale((0, 1080), (2560, 1440), placement),
            0.4
        );
    }

    #[test]
    fn uses_straight_alpha_color_and_porter_duff_alpha() {
        assert_eq!(
            STRAIGHT_ALPHA_BLEND.color.src_factor,
            wgpu::BlendFactor::SrcAlpha
        );
        assert_eq!(
            STRAIGHT_ALPHA_BLEND.color.dst_factor,
            wgpu::BlendFactor::OneMinusSrcAlpha
        );
        assert_eq!(
            STRAIGHT_ALPHA_BLEND.alpha.src_factor,
            wgpu::BlendFactor::One
        );
        assert_eq!(
            STRAIGHT_ALPHA_BLEND.alpha.dst_factor,
            wgpu::BlendFactor::OneMinusSrcAlpha
        );
    }

    #[test]
    fn gpu_compositor_writes_visible_overlay_pixels() {
        let Ok(gpu) = GpuContext::new_blocking() else {
            eprintln!("GPU unavailable; skipping overlay compositor integration test");
            return;
        };
        let size = 64;
        let target = gpu.device().create_texture(&wgpu::TextureDescriptor {
            label: Some("overlay compositor test target"),
            size: wgpu::Extent3d {
                width: size,
                height: size,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let view = target.create_view(&wgpu::TextureViewDescriptor::default());
        let mut clear_encoder =
            gpu.device()
                .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                    label: Some("overlay compositor test clear"),
                });
        {
            let _pass = clear_encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("overlay compositor test clear pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    resolve_target: None,
                    depth_slice: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
        }
        gpu.queue().submit(Some(clear_encoder.finish()));

        let frame = OverlayFrame {
            width: size,
            height: size,
            design_size: (size, size),
            rgba: [255_u8, 0, 255, 255].repeat((size * size) as usize),
        };
        let compositor =
            RgbaOverlayCompositor::new(&gpu, wgpu::TextureFormat::Rgba8Unorm, (size, size), &frame)
                .unwrap();
        let mut readback = crate::gpu::rgba_readback::RgbaReadback::new(&gpu, size, size).unwrap();
        assert!(
            readback
                .readback(&gpu, &target, compositor.encode(&gpu, &view))
                .unwrap()
                .is_none()
        );
        let pixels = readback.flush_pending(&gpu).unwrap().unwrap();
        let center = ((size / 2 * size + size / 2) * 4) as usize;
        assert_eq!(&pixels[center..center + 4], &[255, 0, 255, 255]);
    }

    /// The opacity uniform must actually fade the composited result on
    /// the GPU - this is what replaces rebuilding and re-uploading a
    /// full-frame image every frame, so it needs a real end-to-end
    /// check, not just a uniform write that compiles.
    #[test]
    fn opacity_uniform_fades_the_composited_overlay_on_the_gpu() {
        let Ok(gpu) = GpuContext::new_blocking() else {
            eprintln!("GPU unavailable; skipping overlay compositor integration test");
            return;
        };
        let size = 32;
        let make_target = || {
            gpu.device().create_texture(&wgpu::TextureDescriptor {
                label: Some("opacity test target"),
                size: wgpu::Extent3d {
                    width: size,
                    height: size,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::Rgba8Unorm,
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
                view_formats: &[],
            })
        };
        // Opaque white overlay over a black target: the composited
        // centre pixel reads back as a direct measure of opacity.
        let frame = OverlayFrame {
            width: size,
            height: size,
            design_size: (size, size),
            rgba: [255_u8, 255, 255, 255].repeat((size * size) as usize),
        };

        let mut sampled = Vec::new();
        for opacity in [1.0_f32, 0.5, 0.0] {
            let target = make_target();
            let view = target.create_view(&wgpu::TextureViewDescriptor::default());
            let mut clear = gpu
                .device()
                .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                    label: Some("opacity test clear"),
                });
            {
                let _pass = clear.begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: Some("opacity test clear pass"),
                    color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                        view: &view,
                        resolve_target: None,
                        depth_slice: None,
                        ops: wgpu::Operations {
                            load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                            store: wgpu::StoreOp::Store,
                        },
                    })],
                    depth_stencil_attachment: None,
                    timestamp_writes: None,
                    occlusion_query_set: None,
                    multiview_mask: None,
                });
            }
            gpu.queue().submit(Some(clear.finish()));

            let mut compositor = RgbaOverlayCompositor::new(
                &gpu,
                wgpu::TextureFormat::Rgba8Unorm,
                (size, size),
                &frame,
            )
            .unwrap();
            compositor.set_opacity(&gpu, opacity);
            let mut readback =
                crate::gpu::rgba_readback::RgbaReadback::new(&gpu, size, size).unwrap();
            readback
                .readback(&gpu, &target, compositor.encode(&gpu, &view))
                .unwrap();
            let pixels = readback.flush_pending(&gpu).unwrap().unwrap();
            let centre = ((size / 2 * size + size / 2) * 4) as usize;
            sampled.push(pixels[centre]);
        }

        assert_eq!(sampled[0], 255, "opacity 1.0 must draw the overlay in full");
        assert_eq!(sampled[2], 0, "opacity 0.0 must draw nothing at all");
        assert!(
            sampled[1] > 100 && sampled[1] < 155,
            "opacity 0.5 should land near half, got {}",
            sampled[1]
        );
    }

    /// Regression test for the GetData-timeout crash traced to the
    /// mip-chain approach: a producer (the scoreboard renderer) that
    /// pre-scales its actual pixel buffer down for sharper rendering
    /// must still place/size correctly - `design_size`, not the
    /// smaller `width`/`height`, drives the contain-fit placement math.
    /// Here a 32x32 texture with `design_size: (64, 64)` in a 64x64
    /// output must still cover the *entire* target (the sampler
    /// upscaling the smaller texture to fill the full 64x64 footprint),
    /// exactly like a native 64x64 texture would - not a 32x32 patch
    /// letterboxed inside it.
    #[test]
    fn placement_uses_design_size_not_actual_texture_size() {
        let Ok(gpu) = GpuContext::new_blocking() else {
            eprintln!("GPU unavailable; skipping overlay compositor integration test");
            return;
        };
        let size = 64;
        let target = gpu.device().create_texture(&wgpu::TextureDescriptor {
            label: Some("design_size test target"),
            size: wgpu::Extent3d {
                width: size,
                height: size,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let view = target.create_view(&wgpu::TextureViewDescriptor::default());

        // Actual pixel buffer is half the design/output size (stand-in
        // for a Chrome capture pre-scaled to roughly match its
        // on-screen footprint).
        let half = size / 2;
        let frame = OverlayFrame {
            width: half,
            height: half,
            design_size: (size, size),
            rgba: [0_u8, 255, 0, 255].repeat((half * half) as usize),
        };
        let compositor =
            RgbaOverlayCompositor::new(&gpu, wgpu::TextureFormat::Rgba8Unorm, (size, size), &frame)
                .unwrap();
        let mut readback = crate::gpu::rgba_readback::RgbaReadback::new(&gpu, size, size).unwrap();
        readback
            .readback(&gpu, &target, compositor.encode(&gpu, &view))
            .unwrap();
        let pixels = readback.flush_pending(&gpu).unwrap().unwrap();
        // Both the center and a corner near the target's edge are
        // covered - proof the smaller texture was fit to the full
        // design_size footprint, not left as a small patch in the
        // corner or center.
        for (x, y) in [(size / 2, size / 2), (1, 1), (size - 2, size - 2)] {
            let idx = ((y * size + x) * 4) as usize;
            assert_eq!(&pixels[idx..idx + 4], &[0, 255, 0, 255], "at ({x}, {y})");
        }
    }
}
