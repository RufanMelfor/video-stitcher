//! Single-camera isolated render, for calibration-quality validation only.
//!
//! Renders exactly one camera plane (left OR right) through the same
//! `fisheye.wgsl` shader and MVP construction as the production stitch
//! pass ([`super::renderer::Renderer::encode_stitch_pass`]), but as a
//! single draw call into a render target cleared to
//! [`wgpu::Color::TRANSPARENT`] (not the production `BLACK`), so the
//! output alpha channel is a clean per-pixel coverage mask: `1.0` where
//! this camera's undistorted fisheye image is valid, `0.0` everywhere
//! else (both "outside the fisheye circle" - `fisheye.wgsl`'s own bounds
//! check - and "outside the plane's screen footprint entirely", which
//! the production `BLACK` clear can't distinguish from real coverage).
//! `blend_width` is always passed as `0.0`, so `fisheye.wgsl`'s seam
//! feathering (which only fires for the right camera when `blend_width
//! > 0.0`) never softens this coverage signal.
//!
//! Not used by any production render path - `StitchRenderer`,
//! `StitchPipeline`, and `Renderer` are untouched. Exists solely so
//! `reco-calibrate`'s photometric-alignment validation harness
//! (`examples/fit_photometric.rs`) can compare each camera's rendered
//! contribution in isolation, without touching production rendering code.
//! See `crates/reco-calibrate/FRICTION.md` for the background: the
//! existing AKAZE-feature-based calibration objective has a documented
//! near-field seam residual, and this renderer exists to test whether a
//! direct pixel-comparison (ZNCC) objective can refine it further.

use super::renderer::{InputFormat, build_gpu_uniforms, opengl_to_wgpu_matrix};
use super::scene::SceneGeometry;
use crate::calibration::CameraParams;
use crate::gpu::GpuContext;
use crate::projection::VirtualCamera;

use bytemuck::{Pod, Zeroable};
use nalgebra::{Isometry3, Perspective3, Point3};
use wgpu::util::DeviceExt;

const NEAR_PLANE: f32 = 0.01;
const FAR_PLANE: f32 = 5.0;

#[repr(C)]
#[derive(Copy, Clone, Pod, Zeroable)]
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

/// Same quad shape as `renderer::quad_vertices` - kept private/duplicated
/// here rather than sharing the private helper, mirroring how
/// `lens::undistort::GpuUndistort` already duplicates this exact quad
/// builder for the same reason (small, stable, not worth a shared export).
fn quad_vertices(plane_aspect: f32) -> [Vertex; 6] {
    let hw = 0.5;
    let hh = 0.5 / plane_aspect;
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

/// Renders one camera's contribution to the stitch scene in isolation.
///
/// YUV420P input only (the two formats this harness's real footage
/// pipeline actually produces); unlike the production `Renderer`, there
/// is no NV12/BGRA variant here since nothing zero-copy touches this
/// path.
pub struct SingleCameraRenderer {
    pipeline: wgpu::RenderPipeline,
    vertex_buffer: wgpu::Buffer,
    y_texture: wgpu::Texture,
    u_texture: wgpu::Texture,
    v_texture: wgpu::Texture,
    texture_bind_group: wgpu::BindGroup,
    uniform_buffer: wgpu::Buffer,
    uniform_bind_group: wgpu::BindGroup,
    render_target: wgpu::Texture,
    render_target_view: wgpu::TextureView,
    readback_buffer: wgpu::Buffer,
    input_width: u32,
    input_height: u32,
    eval_width: u32,
    eval_height: u32,
}

impl SingleCameraRenderer {
    /// `input_width`/`input_height` are the source camera frame's
    /// dimensions (the actual YUV420P plane sizes). `eval_width`/
    /// `eval_height` are the output evaluation resolution - kept small
    /// (e.g. 1280x720) so a Nelder-Mead loop can afford two renders
    /// (left-only, right-only) per candidate parameter set.
    /// `plane_aspect` is the source camera's `width / height`, matching
    /// `SceneGeometry::plane_aspect` for the same rig.
    pub fn new(
        gpu: &GpuContext,
        input_width: u32,
        input_height: u32,
        eval_width: u32,
        eval_height: u32,
        plane_aspect: f32,
    ) -> Self {
        let device = &gpu.device;

        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("single_camera_fisheye"),
            source: wgpu::ShaderSource::Wgsl(include_str!("../shaders/fisheye.wgsl").into()),
        });

        let vertices = quad_vertices(plane_aspect);
        let vertex_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("single_camera_quad"),
            contents: bytemuck::cast_slice(&vertices),
            usage: wgpu::BufferUsages::VERTEX,
        });

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
            label: Some("single_camera_tex_layout"),
            entries: &[
                texture_entry(0),
                texture_entry(1),
                texture_entry(2),
                wgpu::BindGroupLayoutEntry {
                    binding: 3,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });

        let uniform_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("single_camera_uniform_layout"),
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

        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("single_camera_pipeline_layout"),
            bind_group_layouts: &[&texture_layout, &uniform_layout],
            immediate_size: 0,
        });

        // Same alpha-over blend as the production stitch pass. With only
        // one draw call per render target here, blending never actually
        // composites against a second camera - it's kept identical
        // purely for shader/pipeline fidelity with production.
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("single_camera_pipeline"),
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
                    format: wgpu::TextureFormat::Rgba8Unorm,
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
                cull_mode: None,
                ..Default::default()
            },
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview_mask: None,
            cache: None,
        });

        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("single_camera_sampler"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });

        let usage = wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST;
        let create_tex = |label: &str, w: u32, h: u32| {
            device.create_texture(&wgpu::TextureDescriptor {
                label: Some(label),
                size: wgpu::Extent3d {
                    width: w,
                    height: h,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::R8Unorm,
                usage,
                view_formats: &[],
            })
        };

        let y_texture = create_tex("single_camera_y", input_width, input_height);
        let u_texture = create_tex("single_camera_u", input_width / 2, input_height / 2);
        let v_texture = create_tex("single_camera_v", input_width / 2, input_height / 2);

        let y_view = y_texture.create_view(&Default::default());
        let u_view = u_texture.create_view(&Default::default());
        let v_view = v_texture.create_view(&Default::default());

        let texture_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("single_camera_tex_bg"),
            layout: &texture_layout,
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
                    resource: wgpu::BindingResource::Sampler(&sampler),
                },
            ],
        });

        let uniform_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("single_camera_uniforms"),
            size: std::mem::size_of::<super::renderer::GpuUniforms>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let uniform_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("single_camera_uniform_bg"),
            layout: &uniform_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: uniform_buffer.as_entire_binding(),
            }],
        });

        let render_target = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("single_camera_target"),
            size: wgpu::Extent3d {
                width: eval_width,
                height: eval_height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let render_target_view = render_target.create_view(&Default::default());

        let aligned_bpr = (eval_width * 4).div_ceil(256) * 256;
        let readback_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("single_camera_readback"),
            size: (aligned_bpr * eval_height) as u64,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });

        Self {
            pipeline,
            vertex_buffer,
            y_texture,
            u_texture,
            v_texture,
            texture_bind_group,
            uniform_buffer,
            uniform_bind_group,
            render_target,
            render_target_view,
            readback_buffer,
            input_width,
            input_height,
            eval_width,
            eval_height,
        }
    }

    /// Render this one camera's contribution for the given scene/camera
    /// params, looking straight at the seam (yaw=0, pitch=0, no rig
    /// tilt/roll - production's arbitrary viewport panning isn't needed
    /// for calibration validation). Blocking single-shot readback
    /// (mirrors `lens::undistort::GpuUndistort::undistort`'s
    /// `map_async` + `poll` pattern), so every call returns one
    /// deterministic RGBA answer - safe to call in a tight optimizer
    /// loop.
    ///
    /// Returns `eval_width * eval_height * 4` RGBA8 bytes; alpha is the
    /// coverage mask described in the module doc.
    #[allow(clippy::too_many_arguments)]
    pub fn render_and_readback(
        &self,
        gpu: &GpuContext,
        scene: &SceneGeometry,
        camera_params: &CameraParams,
        is_right: bool,
        fov_degrees: f32,
        y: &[u8],
        u: &[u8],
        v: &[u8],
    ) -> Vec<u8> {
        upload_plane(
            &gpu.queue,
            &self.y_texture,
            y,
            self.input_width,
            self.input_height,
        );
        upload_plane(
            &gpu.queue,
            &self.u_texture,
            u,
            self.input_width / 2,
            self.input_height / 2,
        );
        upload_plane(
            &gpu.queue,
            &self.v_texture,
            v,
            self.input_width / 2,
            self.input_height / 2,
        );

        let aspect = self.eval_width as f32 / self.eval_height as f32;
        let projection = opengl_to_wgpu_matrix()
            * Perspective3::new(aspect, fov_degrees.to_radians(), NEAR_PLANE, FAR_PLANE)
                .to_homogeneous();

        // Straight-ahead view: the yaw=pitch=rig_tilt=rig_roll=0 special
        // case of `renderer::view_matrix` (private to that module, not
        // reachable here). With all four at zero, `view_matrix` reduces
        // exactly to `look_at_rh(eye, eye + base_forward, world_up)`.
        let cam = VirtualCamera::new(&scene.camera_position);
        let eye = Point3::from(cam.eye);
        let target = Point3::from(eye.coords + cam.base_forward);
        let up = VirtualCamera::world_up();
        let view = Isometry3::look_at_rh(&eye, &target, &up).to_homogeneous();

        let model = if is_right {
            scene.model_matrix_right()
        } else {
            scene.model_matrix_left()
        };
        let mvp = projection * view * model;

        // blend_width = 0.0: never soften the alpha coverage signal (see
        // module doc). flip_180/is_full_range = false: this harness's
        // real-footage loader (VideoDecoder) already normalizes rotation
        // and range before frames reach here.
        let uniforms = build_gpu_uniforms(
            &mvp,
            camera_params,
            is_right,
            0.0,
            InputFormat::Yuv420p,
            false,
            false,
        );
        gpu.queue
            .write_buffer(&self.uniform_buffer, 0, bytemuck::bytes_of(&uniforms));

        let mut encoder = gpu
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("single_camera_encode"),
            });

        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("single_camera_pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &self.render_target_view,
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
            pass.set_bind_group(0, &self.texture_bind_group, &[]);
            pass.set_bind_group(1, &self.uniform_bind_group, &[]);
            pass.draw(0..6, 0..1);
        }

        let aligned_bpr = (self.eval_width * 4).div_ceil(256) * 256;
        encoder.copy_texture_to_buffer(
            self.render_target.as_image_copy(),
            wgpu::TexelCopyBufferInfo {
                buffer: &self.readback_buffer,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(aligned_bpr),
                    rows_per_image: Some(self.eval_height),
                },
            },
            wgpu::Extent3d {
                width: self.eval_width,
                height: self.eval_height,
                depth_or_array_layers: 1,
            },
        );

        gpu.queue.submit(Some(encoder.finish()));

        let slice = self.readback_buffer.slice(..);
        slice.map_async(wgpu::MapMode::Read, |_| {});
        gpu.device
            .poll(wgpu::PollType::wait_indefinitely())
            .unwrap();

        let mapped = slice.get_mapped_range();
        let mut rgba = Vec::with_capacity((self.eval_width * self.eval_height * 4) as usize);
        for row in 0..self.eval_height {
            let start = (row * aligned_bpr) as usize;
            let end = start + (self.eval_width * 4) as usize;
            rgba.extend_from_slice(&mapped[start..end]);
        }
        drop(mapped);
        self.readback_buffer.unmap();

        rgba
    }
}

/// Upload a single R8Unorm plane to a GPU texture (mirrors the private
/// helper of the same name in `lens::undistort`).
fn upload_plane(
    queue: &wgpu::Queue,
    texture: &wgpu::Texture,
    data: &[u8],
    width: u32,
    height: u32,
) {
    queue.write_texture(
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
