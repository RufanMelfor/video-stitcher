//! wgpu compute shader for detection preprocessing.
//!
//! Converts NV12 texture views into a float32 CHW tensor ready for
//! ORT inference. Runs entirely on the GPU via a single compute
//! dispatch, replacing the CPU scalar bilinear resize (~300ms at
//! 1280x1280) with a GPU pass (~1ms).
//!
//! Works on any wgpu backend (DX12, Vulkan, Metal) making it the
//! universal preprocessing path for DirectML, CUDA EP, and CoreML.
//!
//! Optionally runs a local contrast boost (CLAHE, see
//! [`crate::local_contrast`]) on the tensor before readback, for fog:
//! two more dispatches, one workgroup per tile for the lookup tables and
//! one pass over the content area to apply them.

use std::borrow::Cow;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct Uniforms {
    src_w: u32,
    src_h: u32,
    dst_w: u32,
    dst_h: u32,
    pad_x: f32,
    pad_y: f32,
    scale: f32,
    flip180: u32,
}

const SHADER: &str = r#"
struct Uniforms {
    src_w: u32,
    src_h: u32,
    dst_w: u32,
    dst_h: u32,
    pad_x: f32,
    pad_y: f32,
    scale: f32,
    flip180: u32,
}

@group(0) @binding(0) var y_tex: texture_2d<f32>;
@group(0) @binding(1) var uv_tex: texture_2d<f32>;
@group(0) @binding(2) var<storage, read_write> output: array<f32>;
@group(0) @binding(3) var<uniform> u: Uniforms;
@group(0) @binding(4) var bilinear_sampler: sampler;

@compute @workgroup_size(16, 16)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let ox = gid.x;
    let oy = gid.y;
    if (ox >= u.dst_w || oy >= u.dst_h) {
        return;
    }

    let hw = u.dst_w * u.dst_h;
    let idx = oy * u.dst_w + ox;

    // Letterbox padding region
    let pad_x_u = u32(u.pad_x);
    let pad_y_u = u32(u.pad_y);
    let content_w = u.dst_w - 2u * pad_x_u;
    let content_h = u.dst_h - 2u * pad_y_u;

    if (ox < pad_x_u || ox >= pad_x_u + content_w ||
        oy < pad_y_u || oy >= pad_y_u + content_h) {
        let grey = 0.44705883;
        output[idx] = grey;
        output[idx + hw] = grey;
        output[idx + 2u * hw] = grey;
        return;
    }

    // Map to source coordinates
    var src_x = f32(ox - pad_x_u) / u.scale;
    var src_y = f32(oy - pad_y_u) / u.scale;

    if (u.flip180 != 0u) {
        src_x = f32(u.src_w - 1u) - src_x;
        src_y = f32(u.src_h - 1u) - src_y;
    }

    // Normalized coords for bilinear sampling
    let tex_x = (src_x + 0.5) / f32(u.src_w);
    let tex_y = (src_y + 0.5) / f32(u.src_h);
    let uv_coord = vec2<f32>(tex_x, tex_y);

    // Sample NV12 planes
    let y_val = textureSampleLevel(y_tex, bilinear_sampler, uv_coord, 0.0).r;
    let uv_val = textureSampleLevel(uv_tex, bilinear_sampler, uv_coord, 0.0).rg;

    // BT.709 full-range YUV to RGB
    let cb = uv_val.r - 0.5;
    let cr = uv_val.g - 0.5;
    let r = clamp(y_val + 1.5748 * cr, 0.0, 1.0);
    let g = clamp(y_val - 0.1873 * cb - 0.4681 * cr, 0.0, 1.0);
    let b = clamp(y_val + 1.8556 * cb, 0.0, 1.0);

    // CHW layout, already [0,1] normalized
    output[idx] = r;
    output[idx + hw] = g;
    output[idx + 2u * hw] = b;
}
"#;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct ClaheUniforms {
    cx: u32,
    cy: u32,
    cw: u32,
    ch: u32,
    dst_w: u32,
    dst_h: u32,
    clip_limit: f32,
    _pad: u32,
}

/// GPU port of [`crate::local_contrast::apply_chw`]; keep the two in step.
const CLAHE_SHADER: &str = r#"
struct ClaheUniforms {
    cx: u32,
    cy: u32,
    cw: u32,
    ch: u32,
    dst_w: u32,
    dst_h: u32,
    clip_limit: f32,
    _pad: u32,
}

@group(0) @binding(0) var<storage, read_write> img: array<f32>;
@group(0) @binding(1) var<storage, read_write> luts: array<f32>;
@group(0) @binding(2) var<uniform> c: ClaheUniforms;

const TILES: u32 = 8u;
const BINS: u32 = 256u;

var<workgroup> hist: array<atomic<u32>, 256>;
var<workgroup> clipped: array<f32, 256>;

fn luma(r: f32, g: f32, b: f32) -> f32 {
    return 0.2126 * r + 0.7152 * g + 0.0722 * b;
}

fn bin_of(y: f32) -> u32 {
    return u32(clamp(i32(y * f32(BINS)), 0, i32(BINS) - 1));
}

// Lookup with linear interpolation across the bin (see local_contrast.rs).
fn map(tile: u32, bin: u32, frac: f32) -> f32 {
    let base = tile * BINS;
    var lower = 0.0;
    if (bin > 0u) {
        lower = luts[base + bin - 1u];
    }
    return lower + (luts[base + bin] - lower) * frac;
}

// One workgroup per tile: histogram, clip, spread the excess, integrate.
@compute @workgroup_size(256)
fn build_luts(
    @builtin(workgroup_id) wg: vec3<u32>,
    @builtin(local_invocation_index) li: u32,
) {
    let tx = wg.x;
    let ty = wg.y;
    atomicStore(&hist[li], 0u);
    workgroupBarrier();

    let x0 = tx * c.cw / TILES;
    let x1 = (tx + 1u) * c.cw / TILES;
    let y0 = ty * c.ch / TILES;
    let y1 = (ty + 1u) * c.ch / TILES;
    let tw = x1 - x0;
    let n = tw * (y1 - y0);
    let hw = c.dst_w * c.dst_h;
    for (var i = li; i < n; i += 256u) {
        let x = c.cx + x0 + i % tw;
        let y = c.cy + y0 + i / tw;
        let idx = y * c.dst_w + x;
        let b = bin_of(luma(img[idx], img[idx + hw], img[idx + 2u * hw]));
        atomicAdd(&hist[b], 1u);
    }
    workgroupBarrier();

    let nf = f32(n);
    let clip = max(c.clip_limit * nf / f32(BINS), 1.0);
    clipped[li] = min(f32(atomicLoad(&hist[li])), clip);
    workgroupBarrier();

    if (li == 0u) {
        var kept = 0.0;
        for (var b = 0u; b < BINS; b++) {
            kept += clipped[b];
        }
        let spread = (nf - kept) / f32(BINS);
        var acc = 0.0;
        let base = (ty * TILES + tx) * BINS;
        for (var b = 0u; b < BINS; b++) {
            acc += clipped[b] + spread;
            luts[base + b] = acc / nf;
        }
    }
}

fn axis(p: u32, tile_len: f32) -> vec3<f32> {
    let g = (f32(p) + 0.5) / tile_len - 0.5;
    let t0 = min(u32(max(floor(g), 0.0)), TILES - 1u);
    let t1 = min(t0 + 1u, TILES - 1u);
    return vec3<f32>(f32(t0), f32(t1), clamp(g - f32(t0), 0.0, 1.0));
}

@compute @workgroup_size(16, 16)
fn apply(@builtin(global_invocation_id) gid: vec3<u32>) {
    if (gid.x >= c.cw || gid.y >= c.ch) {
        return;
    }
    let hw = c.dst_w * c.dst_h;
    let idx = (c.cy + gid.y) * c.dst_w + c.cx + gid.x;
    let r = img[idx];
    let g = img[idx + hw];
    let b = img[idx + 2u * hw];
    let y = luma(r, g, b);
    let bin = bin_of(y);
    let frac = clamp(y * f32(BINS) - f32(bin), 0.0, 1.0);
    let ay = axis(gid.y, f32(c.ch) / f32(TILES));
    let ax = axis(gid.x, f32(c.cw) / f32(TILES));
    let ty0 = u32(ay.x);
    let ty1 = u32(ay.y);
    let tx0 = u32(ax.x);
    let tx1 = u32(ax.y);
    let top = map(ty0 * TILES + tx0, bin, frac) * (1.0 - ax.z)
        + map(ty0 * TILES + tx1, bin, frac) * ax.z;
    let bottom = map(ty1 * TILES + tx0, bin, frac) * (1.0 - ax.z)
        + map(ty1 * TILES + tx1, bin, frac) * ax.z;
    let delta = top * (1.0 - ay.z) + bottom * ay.z - y;
    img[idx] = clamp(r + delta, 0.0, 1.0);
    img[idx + hw] = clamp(g + delta, 0.0, 1.0);
    img[idx + 2u * hw] = clamp(b + delta, 0.0, 1.0);
}
"#;

/// GPU resources for the optional local contrast boost.
struct ClaheGpu {
    build_pipeline: wgpu::ComputePipeline,
    apply_pipeline: wgpu::ComputePipeline,
    bind_group: wgpu::BindGroup,
    content_w: u32,
    content_h: u32,
    clip_limit: f32,
}

impl ClaheGpu {
    fn new(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        tensor: &wgpu::Buffer,
        input_size: crate::InputSize,
        rect: (u32, u32, u32, u32),
        clip_limit: f32,
    ) -> Self {
        use crate::local_contrast::{BINS, TILES};
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("detect_clahe"),
            source: wgpu::ShaderSource::Wgsl(Cow::Borrowed(CLAHE_SHADER)),
        });
        let storage = |binding| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::COMPUTE,
            ty: wgpu::BindingType::Buffer {
                ty: wgpu::BufferBindingType::Storage { read_only: false },
                has_dynamic_offset: false,
                min_binding_size: None,
            },
            count: None,
        };
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("detect_clahe_bgl"),
            entries: &[
                storage(0),
                storage(1),
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
            ],
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("detect_clahe_layout"),
            bind_group_layouts: &[&layout],
            immediate_size: 0,
        });
        let pipeline = |entry: &str| {
            device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                label: Some("detect_clahe_pipeline"),
                layout: Some(&pipeline_layout),
                module: &module,
                entry_point: Some(entry),
                compilation_options: Default::default(),
                cache: None,
            })
        };
        let lut_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("detect_clahe_luts"),
            size: u64::from(TILES * TILES * BINS) * 4,
            usage: wgpu::BufferUsages::STORAGE,
            mapped_at_creation: false,
        });
        let uniforms = ClaheUniforms {
            cx: rect.0,
            cy: rect.1,
            cw: rect.2,
            ch: rect.3,
            dst_w: input_size.width,
            dst_h: input_size.height,
            clip_limit,
            _pad: 0,
        };
        let uniform_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("detect_clahe_uniforms"),
            size: std::mem::size_of::<ClaheUniforms>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        queue.write_buffer(&uniform_buffer, 0, bytemuck::bytes_of(&uniforms));
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("detect_clahe_bg"),
            layout: &layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: tensor.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: lut_buffer.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: uniform_buffer.as_entire_binding(),
                },
            ],
        });
        Self {
            build_pipeline: pipeline("build_luts"),
            apply_pipeline: pipeline("apply"),
            bind_group,
            content_w: rect.2,
            content_h: rect.3,
            clip_limit,
        }
    }

    fn encode(&self, encoder: &mut wgpu::CommandEncoder) {
        use crate::local_contrast::TILES;
        {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("detect_clahe_luts"),
                timestamp_writes: None,
            });
            pass.set_pipeline(&self.build_pipeline);
            pass.set_bind_group(0, &self.bind_group, &[]);
            pass.dispatch_workgroups(TILES, TILES, 1);
        }
        let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
            label: Some("detect_clahe_apply"),
            timestamp_writes: None,
        });
        pass.set_pipeline(&self.apply_pipeline);
        pass.set_bind_group(0, &self.bind_group, &[]);
        pass.dispatch_workgroups(self.content_w.div_ceil(16), self.content_h.div_ceil(16), 1);
    }
}

/// GPU preprocessor for NV12 → float32 CHW tensor conversion.
pub struct WgpuPreprocessor {
    pipeline: wgpu::ComputePipeline,
    bind_group_layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    output_buffer: wgpu::Buffer,
    staging_buffer: wgpu::Buffer,
    uniform_buffer: wgpu::Buffer,
    input_size: crate::InputSize,
    tensor_bytes: usize,
    frame_width: u32,
    frame_height: u32,
    /// Local contrast boost, `None` when off.
    clahe: Option<ClaheGpu>,
}

impl WgpuPreprocessor {
    /// Create a preprocessor for the given model input size and frame dimensions.
    pub fn new(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        input_size: crate::InputSize,
        frame_width: u32,
        frame_height: u32,
    ) -> Self {
        let shader_module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("detect_preprocess"),
            source: wgpu::ShaderSource::Wgsl(Cow::Borrowed(SHADER)),
        });

        let bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("detect_preprocess_bgl"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Storage { read_only: false },
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 3,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 4,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });

        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("detect_preprocess_layout"),
            bind_group_layouts: &[&bind_group_layout],
            immediate_size: 0,
        });

        let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("detect_preprocess_pipeline"),
            layout: Some(&pipeline_layout),
            module: &shader_module,
            entry_point: Some("main"),
            compilation_options: Default::default(),
            cache: None,
        });

        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("detect_bilinear"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });

        let tensor_bytes = input_size.tensor_len() * 4;

        let output_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("detect_output"),
            size: tensor_bytes as u64,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
            mapped_at_creation: false,
        });

        let staging_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("detect_staging"),
            size: tensor_bytes as u64,
            usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        // The shader already indexes with dst_w/dst_h independently
        // (see its `content_w`/`idx` maths), so a non-square input needs
        // nothing from it beyond honest uniforms.
        let lb = input_size.letterbox(frame_width, frame_height);
        let scale = lb.scale;

        let uniforms = Uniforms {
            src_w: frame_width,
            src_h: frame_height,
            dst_w: input_size.width,
            dst_h: input_size.height,
            pad_x: lb.pad_x,
            pad_y: lb.pad_y,
            scale,
            flip180: 0,
        };

        let uniform_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("detect_uniforms"),
            size: std::mem::size_of::<Uniforms>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        queue.write_buffer(&uniform_buffer, 0, bytemuck::bytes_of(&uniforms));

        log::info!(
            "WgpuPreprocessor: {}x{} -> {}, scale={:.3}, pad=({:.1},{:.1}), tensor={:.1}MB",
            frame_width,
            frame_height,
            input_size,
            scale,
            uniforms.pad_x,
            uniforms.pad_y,
            tensor_bytes as f64 / 1024.0 / 1024.0,
        );

        Self {
            pipeline,
            bind_group_layout,
            sampler,
            output_buffer,
            staging_buffer,
            uniform_buffer,
            input_size,
            tensor_bytes,
            frame_width,
            frame_height,
            clahe: None,
        }
    }

    /// Turn the local contrast boost (AI contrast boost, see
    /// [`crate::local_contrast`]) on with the given clip limit, or off with
    /// `0.0`. Values outside
    /// [`validate_clip_limit`](crate::local_contrast::validate_clip_limit)'s
    /// range are rejected with a warning and leave the boost off.
    pub fn set_local_contrast(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        clip_limit: f32,
    ) {
        if let Err(e) = crate::local_contrast::validate_clip_limit(clip_limit) {
            log::warn!("WgpuPreprocessor: {e}; AI contrast boost off");
            self.clahe = None;
            return;
        }
        let rect = crate::local_contrast::content_rect(
            self.input_size,
            self.frame_width,
            self.frame_height,
        );
        if clip_limit == 0.0
            || rect.2 < crate::local_contrast::TILES
            || rect.3 < crate::local_contrast::TILES
        {
            self.clahe = None;
            return;
        }
        log::info!(
            "WgpuPreprocessor: AI contrast boost on (CLAHE clip {clip_limit:.1}, {}x{} tiles over {}x{})",
            crate::local_contrast::TILES,
            crate::local_contrast::TILES,
            rect.2,
            rect.3
        );
        self.clahe = Some(ClaheGpu::new(
            device,
            queue,
            &self.output_buffer,
            self.input_size,
            rect,
            clip_limit,
        ));
    }

    /// The active AI contrast boost clip limit, `0.0` when off.
    pub fn local_contrast(&self) -> f32 {
        self.clahe.as_ref().map_or(0.0, |c| c.clip_limit)
    }

    /// Preprocess NV12 views into a float32 CHW tensor on CPU.
    ///
    /// Takes Y (R8Unorm) and UV (Rg8Unorm) plane views, runs the compute
    /// shader, reads back the CHW float32 data.
    pub fn preprocess(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        y_view: &wgpu::TextureView,
        uv_view: &wgpu::TextureView,
        rotation: i32,
    ) -> Vec<f32> {
        let flip: u32 = if rotation == 180 { 1 } else { 0 };
        queue.write_buffer(
            &self.uniform_buffer,
            std::mem::offset_of!(Uniforms, flip180) as u64,
            bytemuck::bytes_of(&flip),
        );

        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("detect_preprocess_bg"),
            layout: &self.bind_group_layout,
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
                    resource: self.output_buffer.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: self.uniform_buffer.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 4,
                    resource: wgpu::BindingResource::Sampler(&self.sampler),
                },
            ],
        });

        let mut encoder = device.create_command_encoder(&Default::default());
        {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("detect_preprocess"),
                timestamp_writes: None,
            });
            pass.set_pipeline(&self.pipeline);
            pass.set_bind_group(0, &bind_group, &[]);
            pass.dispatch_workgroups(
                self.input_size.width.div_ceil(16),
                self.input_size.height.div_ceil(16),
                1,
            );
        }
        if let Some(ref clahe) = self.clahe {
            clahe.encode(&mut encoder);
        }
        encoder.copy_buffer_to_buffer(
            &self.output_buffer,
            0,
            &self.staging_buffer,
            0,
            self.tensor_bytes as u64,
        );
        let submission_index = queue.submit(std::iter::once(encoder.finish()));

        let slice = self.staging_buffer.slice(..);
        let (tx, rx) = std::sync::mpsc::channel();
        slice.map_async(wgpu::MapMode::Read, move |r| {
            let _ = tx.send(r);
        });
        // Wait for exactly *our* submission, not `wait_indefinitely()`'s
        // "most recent submission at the time of the poll" - on a device
        // shared with the render pipeline, an unrelated render/encode/
        // decode command buffer submitted on another thread between our
        // `submit()` above and this `poll()` call would otherwise also
        // have to finish before we're unblocked, turning a ~1ms compute
        // dispatch into a stall behind whatever else happened to be
        // in flight. Measured average before this fix: 52.3ms/call.
        let _ = device.poll(wgpu::PollType::Wait {
            submission_index: Some(submission_index),
            timeout: None,
        });
        rx.recv().unwrap().unwrap();

        let data = slice.get_mapped_range();
        let floats: Vec<f32> = bytemuck::cast_slice(&data).to_vec();
        drop(data);
        self.staging_buffer.unmap();
        floats
    }

    /// Model input dimensions - width and height may differ.
    pub fn input_size(&self) -> crate::InputSize {
        self.input_size
    }

    /// The source frame `(width, height)` this preprocessor was sized
    /// for. Callers that reuse a preprocessor across frames of varying
    /// resolution (e.g. `WgpuPreprocessingDetector::preprocessor_for`)
    /// use this to detect when a rebuild is needed - the letterbox
    /// scale/pad baked into the uniform buffer at construction time is
    /// only correct for this exact size.
    pub fn frame_size(&self) -> (u32, u32) {
        (self.frame_width, self.frame_height)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::local_contrast;

    /// Upload one NV12 frame: a hazy horizontal ramp with a little texture,
    /// neutral chroma.
    fn nv12_frame(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        w: u32,
        h: u32,
    ) -> (wgpu::TextureView, wgpu::TextureView) {
        let luma: Vec<u8> = (0..h)
            .flat_map(|y| {
                (0..w).map(move |x| {
                    let ramp = 0.45 + 0.1 * x as f32 / w as f32;
                    let texture = ((x * 7 + y * 3) % 11) as f32 / 255.0;
                    ((ramp + texture) * 255.0).round() as u8
                })
            })
            .collect();
        let chroma = vec![128u8; (w * h / 2) as usize];
        let make = |format, tw: u32, th: u32, data: &[u8], bpp: u32| {
            let tex = device.create_texture(&wgpu::TextureDescriptor {
                label: Some("test_nv12_plane"),
                size: wgpu::Extent3d {
                    width: tw,
                    height: th,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format,
                usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
                view_formats: &[],
            });
            queue.write_texture(
                wgpu::TexelCopyTextureInfo {
                    texture: &tex,
                    mip_level: 0,
                    origin: wgpu::Origin3d::ZERO,
                    aspect: wgpu::TextureAspect::All,
                },
                data,
                wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(tw * bpp),
                    rows_per_image: Some(th),
                },
                wgpu::Extent3d {
                    width: tw,
                    height: th,
                    depth_or_array_layers: 1,
                },
            );
            tex.create_view(&Default::default())
        };
        (
            make(wgpu::TextureFormat::R8Unorm, w, h, &luma, 1),
            make(wgpu::TextureFormat::Rg8Unorm, w / 2, h / 2, &chroma, 2),
        )
    }

    /// The GPU AI contrast boost must match the CPU reference in
    /// [`local_contrast::apply_chw`]. Skipped when no GPU adapter exists.
    #[test]
    fn gpu_ai_contrast_boost_matches_cpu_reference() {
        let Ok(gpu) = reco_core::gpu::GpuContext::new_blocking() else {
            eprintln!("skipping: no GPU adapter");
            return;
        };
        let (device, queue) = (gpu.device(), gpu.queue());
        let (w, h) = (640, 480);
        let input = crate::InputSize::square(320);
        let (y_view, uv_view) = nv12_frame(device, queue, w, h);

        let mut pre = WgpuPreprocessor::new(device, queue, input, w, h);
        let plain = pre.preprocess(device, queue, &y_view, &uv_view, 0);
        pre.set_local_contrast(device, queue, 3.0);
        assert_eq!(pre.local_contrast(), 3.0);
        let boosted = pre.preprocess(device, queue, &y_view, &uv_view, 0);

        let rect = local_contrast::content_rect(input, w, h);
        let mut cpu = plain.clone();
        local_contrast::apply_chw(&mut cpu, input, rect, 3.0);

        let diffs: Vec<f32> = cpu
            .iter()
            .zip(&boosted)
            .map(|(a, b)| (a - b).abs())
            .collect();
        let mean = diffs.iter().sum::<f32>() / diffs.len() as f32;
        // A luma value right on a bin edge can land in the neighbouring
        // bin on one side (different float rounding), so allow a few
        // pixels to differ by one bin's worth of the lookup table.
        let off = diffs.iter().filter(|&&d| d > 0.02).count();
        assert!(mean < 1e-3, "mean |gpu - cpu| = {mean}");
        assert!(
            off * 200 < diffs.len(),
            "{off} of {} values differ by > 0.02",
            diffs.len()
        );

        // And it really did something: more local contrast in the content.
        let rows = rect.1..rect.1 + rect.3;
        let (before, after) = (
            local_contrast::tests::local_detail(&plain, input, rows.clone()),
            local_contrast::tests::local_detail(&boosted, input, rows),
        );
        assert!(after > 1.5 * before, "detail {before} -> {after}");

        // Turning it off again restores the plain tensor.
        pre.set_local_contrast(device, queue, 0.0);
        assert_eq!(pre.preprocess(device, queue, &y_view, &uv_view, 0), plain);
    }
}
