//! GPU-side seam-band sampling for the automatic color match.
//!
//! [`super::color_match`] measures each camera's color in the band next to
//! the seam by reading raw YUV bytes on the CPU. The zero-copy decode
//! paths (D3D11VA on Windows, VideoToolbox on macOS) never hand those
//! bytes to the CPU, so on the export path that virtually every Windows
//! user actually takes, the correction was silently identity - see
//! reco-core's `FRICTION.md`.
//!
//! This module closes that hole by gathering the same sample points from
//! the bound NV12 textures with a small compute pass and reading the
//! result back asynchronously.
//!
//! **It gathers texels and nothing else.** Range expansion, the YUV/RGB
//! conversion, the manual gamma and the averaging all stay in
//! `color_match` (`mean_of_normalized_samples`), which is also what the
//! CPU sampler calls. The alternative - reimplementing the color maths in
//! the compute shader - would have created a third copy of a curve that
//! already has to stay in step between `fisheye.wgsl` and
//! `decode_transfer_yuv`, for no gain: the readback is ~1KB either way.
//!
//! **Nothing here ever blocks.** The result is mapped one frame after it
//! is encoded and picked up whenever it happens to be ready. A
//! synchronous readback would stall the pipeline for milliseconds per
//! measurement, which would make a hardware-decoded export *slower* -
//! the opposite of the point. The correction is EMA-smoothed across
//! measurements 15 frames apart, so a frame or two of latency is
//! invisible.

use std::sync::mpsc::{Receiver, Sender, channel};

use wgpu::util::DeviceExt;

use crate::gpu::GpuContext;

/// Upper bound on sample points per camera, matching the largest grid the
/// GUI can ask for (32 columns x 64 rows). Allocating for the maximum
/// once keeps the buffers fixed for the session - at 16 bytes per sample
/// that is 64KB for both cameras, so there is nothing to gain from
/// resizing when a slider moves.
const MAX_SAMPLES_PER_CAMERA: usize = 32 * 64;

/// Where the asynchronous readback currently is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Readback {
    /// No gather in flight.
    Idle,
    /// Encoded and submitted; the map has not been requested yet. Mapping
    /// only becomes safe once the copy has actually been submitted, which
    /// is why this is a separate state rather than mapping at encode time.
    Submitted,
    /// `map_async` issued, waiting for the callback.
    Mapping,
}

/// Compute pass + buffers for one pipeline's band sampling.
pub(crate) struct BandGather {
    pipeline: wgpu::ComputePipeline,
    layout: wgpu::BindGroupLayout,
    /// Sample positions for both cameras: left occupies
    /// `[0, MAX_SAMPLES_PER_CAMERA)`, right the second half.
    coords: wgpu::Buffer,
    samples: wgpu::Buffer,
    staging: wgpu::Buffer,
    params_left: wgpu::Buffer,
    params_right: wgpu::Buffer,
    left_count: u32,
    right_count: u32,
    /// Fingerprint of the positions currently uploaded, so they are only
    /// recomputed and re-uploaded when the band geometry actually changes.
    uploaded_for: Option<PositionKey>,
    state: Readback,
    map_tx: Sender<Result<(), wgpu::BufferAsyncError>>,
    map_rx: Receiver<Result<(), wgpu::BufferAsyncError>>,
}

/// One completed gather: the raw texels for the left camera, then the
/// right. Named rather than returned as a bare tuple of vectors so the
/// two halves cannot be swapped at a call site by accident.
pub(crate) type GatheredSamples = (Vec<[f32; 4]>, Vec<[f32; 4]>);

/// Everything the sample positions depend on. Deliberately not the frame
/// contents: positions are pure geometry, so they survive until the
/// calibration or the band parameters move.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct PositionKey {
    pub(crate) band_width: f32,
    pub(crate) grid_cols: u32,
    pub(crate) grid_rows: u32,
    pub(crate) seam_offset: f32,
    pub(crate) blend_flip_direction: bool,
    pub(crate) input_width: u32,
    pub(crate) input_height: u32,
    /// 180-degree flip per camera. Part of the key because it changes the
    /// texel a given band position maps to, so a source whose rotation
    /// metadata differs from the last one must re-upload.
    pub(crate) flip_180: [bool; 2],
}

/// Mirror sample positions for a camera whose frame the shader flips.
///
/// SYNC_WITH `fisheye.wgsl`'s `sample_yuv`, which renders a
/// 180-degree-rotated source by sampling at `1 - uv` rather than by
/// reversing the buffer the way the CPU decode path does. The gather
/// reads raw texels, so without this it measures the diagonally opposite
/// corner of the frame from the one being drawn - which on a real DJI
/// pair (left camera tagged `rotation=-180`, right not) produced a
/// correction about twice the true size and visibly noisy, because the
/// left camera's "seam band" samples were actually coming from the far
/// side of the field.
pub(crate) fn mirrored_180(positions: &[[u32; 2]], width: u32, height: u32) -> Vec<[u32; 2]> {
    positions
        .iter()
        .map(|[x, y]| [width.saturating_sub(1 + *x), height.saturating_sub(1 + *y)])
        .collect()
}

#[repr(C)]
#[derive(Copy, Clone, bytemuck::Pod, bytemuck::Zeroable)]
struct GatherParams {
    base: u32,
    count: u32,
    _pad: [u32; 2],
}

impl BandGather {
    pub(crate) fn new(gpu: &GpuContext) -> Self {
        let device = &gpu.device;
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("color_gather"),
            source: wgpu::ShaderSource::Wgsl(include_str!("../shaders/color_gather.wgsl").into()),
        });

        let texture_entry = |binding: u32| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::COMPUTE,
            ty: wgpu::BindingType::Texture {
                // Matches how the render pipeline binds the same views;
                // this pass uses textureLoad, so no sampler is needed.
                sample_type: wgpu::TextureSampleType::Float { filterable: true },
                view_dimension: wgpu::TextureViewDimension::D2,
                multisampled: false,
            },
            count: None,
        };
        let storage_entry = |binding: u32, read_only: bool| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::COMPUTE,
            ty: wgpu::BindingType::Buffer {
                ty: wgpu::BufferBindingType::Storage { read_only },
                has_dynamic_offset: false,
                min_binding_size: None,
            },
            count: None,
        };
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("color_gather_layout"),
            entries: &[
                texture_entry(0),
                texture_entry(1),
                storage_entry(2, true),
                storage_entry(3, false),
                wgpu::BindGroupLayoutEntry {
                    binding: 4,
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
            label: Some("color_gather_pipeline_layout"),
            bind_group_layouts: &[&layout],
            immediate_size: 0,
        });
        let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("color_gather"),
            layout: Some(&pipeline_layout),
            module: &shader,
            entry_point: Some("main"),
            compilation_options: Default::default(),
            cache: None,
        });

        let coords_bytes = (MAX_SAMPLES_PER_CAMERA * 2 * std::mem::size_of::<[u32; 2]>()) as u64;
        let samples_bytes = (MAX_SAMPLES_PER_CAMERA * 2 * std::mem::size_of::<[f32; 4]>()) as u64;

        let coords = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("color_gather_coords"),
            size: coords_bytes,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let samples = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("color_gather_samples"),
            size: samples_bytes,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
            mapped_at_creation: false,
        });
        let staging = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("color_gather_staging"),
            size: samples_bytes,
            usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let make_params = |label: &str, base: u32| {
            device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some(label),
                contents: bytemuck::bytes_of(&GatherParams {
                    base,
                    count: 0,
                    _pad: [0; 2],
                }),
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            })
        };

        let (map_tx, map_rx) = channel();
        Self {
            pipeline,
            layout,
            coords,
            samples,
            staging,
            params_left: make_params("color_gather_params_left", 0),
            params_right: make_params("color_gather_params_right", MAX_SAMPLES_PER_CAMERA as u32),
            left_count: 0,
            right_count: 0,
            uploaded_for: None,
            state: Readback::Idle,
            map_tx,
            map_rx,
        }
    }

    /// Whether the positions on the GPU still match `key`.
    pub(crate) fn positions_current(&self, key: &PositionKey) -> bool {
        self.uploaded_for.as_ref() == Some(key)
    }

    /// Replace the sample positions. Counts are clamped to the buffer's
    /// capacity rather than reallocating: a grid larger than the GUI can
    /// request would only be reachable from a hand-edited calibration, and
    /// measuring the first 2048 points of it is a better failure than
    /// refusing to measure at all.
    pub(crate) fn set_positions(
        &mut self,
        gpu: &GpuContext,
        left: &[[u32; 2]],
        right: &[[u32; 2]],
        key: PositionKey,
    ) {
        let left_n = left.len().min(MAX_SAMPLES_PER_CAMERA);
        let right_n = right.len().min(MAX_SAMPLES_PER_CAMERA);

        gpu.queue
            .write_buffer(&self.coords, 0, bytemuck::cast_slice(&left[..left_n]));
        gpu.queue.write_buffer(
            &self.coords,
            (MAX_SAMPLES_PER_CAMERA * std::mem::size_of::<[u32; 2]>()) as u64,
            bytemuck::cast_slice(&right[..right_n]),
        );

        self.left_count = left_n as u32;
        self.right_count = right_n as u32;
        gpu.queue.write_buffer(
            &self.params_left,
            0,
            bytemuck::bytes_of(&GatherParams {
                base: 0,
                count: self.left_count,
                _pad: [0; 2],
            }),
        );
        gpu.queue.write_buffer(
            &self.params_right,
            0,
            bytemuck::bytes_of(&GatherParams {
                base: MAX_SAMPLES_PER_CAMERA as u32,
                count: self.right_count,
                _pad: [0; 2],
            }),
        );
        self.uploaded_for = Some(key);
    }

    /// Whether a gather can be started right now. False while one is
    /// still in flight - measurements are 15 frames apart by default, so
    /// overlapping them would mean the readback is falling badly behind,
    /// and skipping is better than queueing up stale work.
    pub(crate) fn is_idle(&self) -> bool {
        self.state == Readback::Idle
    }

    /// Encode and submit one gather over both cameras.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn dispatch(
        &mut self,
        gpu: &GpuContext,
        left_y: &wgpu::TextureView,
        left_uv: &wgpu::TextureView,
        right_y: &wgpu::TextureView,
        right_uv: &wgpu::TextureView,
    ) {
        if self.left_count == 0 || self.right_count == 0 {
            return;
        }
        let bind = |y: &wgpu::TextureView, uv: &wgpu::TextureView, params: &wgpu::Buffer, label| {
            gpu.device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some(label),
                layout: &self.layout,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: wgpu::BindingResource::TextureView(y),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::TextureView(uv),
                    },
                    wgpu::BindGroupEntry {
                        binding: 2,
                        resource: self.coords.as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: 3,
                        resource: self.samples.as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: 4,
                        resource: params.as_entire_binding(),
                    },
                ],
            })
        };
        let left_bg = bind(left_y, left_uv, &self.params_left, "color_gather_left");
        let right_bg = bind(right_y, right_uv, &self.params_right, "color_gather_right");

        let mut encoder = gpu
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("color_gather"),
            });
        {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("color_gather"),
                timestamp_writes: None,
            });
            pass.set_pipeline(&self.pipeline);
            for (bg, count) in [(&left_bg, self.left_count), (&right_bg, self.right_count)] {
                pass.set_bind_group(0, bg, &[]);
                pass.dispatch_workgroups(count.div_ceil(64), 1, 1);
            }
        }
        encoder.copy_buffer_to_buffer(&self.samples, 0, &self.staging, 0, self.staging.size());
        gpu.queue.submit(std::iter::once(encoder.finish()));
        self.state = Readback::Submitted;
    }

    /// Collect a finished gather, or `None` if none is ready.
    ///
    /// Drives the map itself: the frame after `dispatch` it requests the
    /// map, and from then on it polls without ever waiting. Call once per
    /// frame, before `dispatch`.
    pub(crate) fn try_take(&mut self, gpu: &GpuContext) -> Option<GatheredSamples> {
        match self.state {
            Readback::Idle => None,
            Readback::Submitted => {
                let tx = self.map_tx.clone();
                self.staging
                    .slice(..)
                    .map_async(wgpu::MapMode::Read, move |r| {
                        let _ = tx.send(r);
                    });
                self.state = Readback::Mapping;
                None
            }
            Readback::Mapping => {
                // Non-blocking on purpose - see the module doc. If the GPU
                // hasn't finished, the next frame asks again.
                let _ = gpu.device.poll(wgpu::PollType::Poll);
                match self.map_rx.try_recv() {
                    Ok(Ok(())) => {
                        let out = {
                            let view = self.staging.slice(..).get_mapped_range();
                            let all: &[[f32; 4]] = bytemuck::cast_slice(&view);
                            let left = all[..self.left_count as usize].to_vec();
                            let right_start = MAX_SAMPLES_PER_CAMERA;
                            let right =
                                all[right_start..right_start + self.right_count as usize].to_vec();
                            (left, right)
                        };
                        self.staging.unmap();
                        self.state = Readback::Idle;
                        Some(out)
                    }
                    Ok(Err(_)) => {
                        // A failed map is not worth retrying against the
                        // same buffer state; drop back to idle so the next
                        // interval starts a clean gather.
                        log::warn!("color gather: buffer map failed, skipping this measurement");
                        self.state = Readback::Idle;
                        None
                    }
                    Err(_) => None,
                }
            }
        }
    }
}
