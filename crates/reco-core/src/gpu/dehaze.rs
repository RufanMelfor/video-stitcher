//! GPU fog/haze removal pass.
//!
//! Removes the grey veil fog lays over the far field while leaving the near
//! field alone: how much haze is removed grows with the ground distance of
//! each pixel, which comes from the view geometry (the ray's angle below
//! the horizon) rather than from the image. The fog colour is measured from
//! the sky in view and smoothed over frames. See `shaders/dehaze.wgsl` for
//! the model.
//!
//! Runs on the stitched viewport right before color grading, so it sees the
//! video content only (no scoreboard or other overlay).

use super::GpuContext;
use wgpu::util::DeviceExt;

/// Side of the square pixel cells the low-resolution statistics are taken
/// over. Must match `CELL` in `dehaze.wgsl`.
const CELL: u32 = 16;

/// Haze removed at the far limit is capped at this multiple of the local
/// darkness (min channel relative to the fog colour): keeps dark near
/// objects that stand above the horizon (a stand, a roof) from turning
/// black.
const DARK_CAP: f32 = 0.7;
/// Angle below the horizon (degrees) from which ground counts as "near":
/// everything closer to the horizon than this gets the full strength.
const FAR_ANGLE_DEG: f32 = 2.0;
/// Pixels within this fraction of the fog's brightness are left alone (the
/// sky - removing "haze" there only brings out blotches).
const GUARD_BAND: f32 = 0.3;
/// Per-frame smoothing of the measured fog colour.
const AIRLIGHT_EMA_ALPHA: f32 = 0.05;
/// A cell counts as sky when its centre ray is at least this many degrees
/// above the horizon.
const SKY_MIN_ELEVATION_DEG: f32 = 3.0;
/// Upper bound on the brightness given back after the veil is removed.
const MAX_BRIGHTNESS_GIVE_BACK: f32 = 2.0;

/// View geometry the dehaze pass needs for one frame: where "up" is in
/// camera space and the projection's shape.
#[derive(Debug, Clone, Copy)]
pub struct DehazeView {
    /// True (level) world up expressed in camera space (camera looks down
    /// -Z, +Y up on screen). Need not be normalized.
    pub up_cam: [f32; 3],
    /// Vertical field of view in degrees.
    pub fov_v_deg: f32,
    /// Output aspect ratio (width / height).
    pub aspect: f32,
}

#[repr(C)]
#[derive(Debug, Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct DehazeUniforms {
    up_cam: [f32; 4],
    shape: [f32; 4],
    tuning: [f32; 4],
}

/// Whether `strength` disables the pass (identity).
pub fn is_identity(strength: f32) -> bool {
    strength.abs() < 1e-6
}

/// GPU compute passes for fog/haze removal.
///
/// Holds the measured fog colour across frames (a 16-byte storage buffer),
/// so one instance belongs to one video stream. The low-resolution
/// statistics texture is rebuilt when the frame size changes.
pub struct DehazePass {
    stats_pipeline: wgpu::ComputePipeline,
    airlight_pipeline: wgpu::ComputePipeline,
    apply_pipeline: wgpu::ComputePipeline,
    uniforms: wgpu::Buffer,
    state: wgpu::Buffer,
    sampler: wgpu::Sampler,
    stats: std::sync::Mutex<Option<wgpu::Texture>>,
    strength: f32,
}

impl DehazePass {
    /// Create the pipelines. `strength` is the initial
    /// [`crate::calibration::Topology::dehaze_strength`].
    pub fn new(gpu: &GpuContext, strength: f32) -> Self {
        let device = &gpu.device;
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("dehaze"),
            source: wgpu::ShaderSource::Wgsl(include_str!("../shaders/dehaze.wgsl").into()),
        });
        // Each entry point binds a different subset of the module's
        // bindings, so each pipeline gets its own derived layout.
        let make = |entry: &str| {
            device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                label: Some(entry),
                layout: None,
                module: &shader,
                entry_point: Some(entry),
                compilation_options: Default::default(),
                cache: None,
            })
        };
        let uniforms = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("dehaze_uniforms"),
            size: std::mem::size_of::<DehazeUniforms>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        // Zeroed = "no fog colour measured yet" (w = 0): the apply pass
        // passes frames through until the first sky is in view.
        let state = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("dehaze_state"),
            contents: &[0u8; 16],
            usage: wgpu::BufferUsages::STORAGE,
        });
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("dehaze_stats_sampler"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        Self {
            stats_pipeline: make("stats_main"),
            airlight_pipeline: make("airlight_main"),
            apply_pipeline: make("apply_main"),
            uniforms,
            state,
            sampler,
            stats: std::sync::Mutex::new(None),
            strength,
        }
    }

    /// Change the strength without rebuilding anything.
    pub fn set_strength(&mut self, strength: f32) {
        self.strength = strength;
    }

    /// Whether the current strength makes the pass a no-op.
    pub fn is_identity(&self) -> bool {
        is_identity(self.strength)
    }

    /// Encode the three dispatches: statistics of `input`, fog colour
    /// update, and the dehazed frame into `output`. `input` must be
    /// `Rgba8Unorm` with `TEXTURE_BINDING`, `output` the same size with
    /// `STORAGE_BINDING`. No-op when [`Self::is_identity`].
    pub fn encode(
        &self,
        gpu: &GpuContext,
        encoder: &mut wgpu::CommandEncoder,
        input: &wgpu::Texture,
        output: &wgpu::Texture,
        view: &DehazeView,
    ) {
        if self.is_identity() {
            return;
        }
        let device = &gpu.device;
        let size = input.size();
        let cells = wgpu::Extent3d {
            width: size.width.div_ceil(CELL),
            height: size.height.div_ceil(CELL),
            depth_or_array_layers: 1,
        };
        let mut stats_guard = self.stats.lock().unwrap();
        let make_stats = || {
            device.create_texture(&wgpu::TextureDescriptor {
                label: Some("dehaze_stats"),
                size: cells,
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::Rgba16Float,
                usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::STORAGE_BINDING,
                view_formats: &[],
            })
        };
        let stats = stats_guard.get_or_insert_with(make_stats);
        if stats.size() != cells {
            *stats = make_stats();
        }

        let uniforms = DehazeUniforms {
            up_cam: [
                view.up_cam[0],
                view.up_cam[1],
                view.up_cam[2],
                (view.fov_v_deg.to_radians() * 0.5).tan(),
            ],
            shape: [
                view.aspect,
                self.strength,
                DARK_CAP,
                FAR_ANGLE_DEG.to_radians().tan(),
            ],
            tuning: [
                GUARD_BAND,
                AIRLIGHT_EMA_ALPHA,
                SKY_MIN_ELEVATION_DEG.to_radians().sin(),
                MAX_BRIGHTNESS_GIVE_BACK,
            ],
        };
        gpu.queue
            .write_buffer(&self.uniforms, 0, bytemuck::bytes_of(&uniforms));

        let input_view = input.create_view(&wgpu::TextureViewDescriptor::default());
        let output_view = output.create_view(&wgpu::TextureViewDescriptor::default());
        let stats_view = stats.create_view(&wgpu::TextureViewDescriptor::default());

        let stats_bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("dehaze_stats_bg"),
            layout: &self.stats_pipeline.get_bind_group_layout(0),
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&input_view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(&stats_view),
                },
            ],
        });
        let airlight_bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("dehaze_airlight_bg"),
            layout: &self.airlight_pipeline.get_bind_group_layout(0),
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::TextureView(&stats_view),
                },
                wgpu::BindGroupEntry {
                    binding: 4,
                    resource: self.state.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 5,
                    resource: self.uniforms.as_entire_binding(),
                },
            ],
        });
        let apply_bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("dehaze_apply_bg"),
            layout: &self.apply_pipeline.get_bind_group_layout(0),
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&input_view),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::TextureView(&stats_view),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: wgpu::BindingResource::Sampler(&self.sampler),
                },
                wgpu::BindGroupEntry {
                    binding: 4,
                    resource: self.state.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 5,
                    resource: self.uniforms.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 6,
                    resource: wgpu::BindingResource::TextureView(&output_view),
                },
            ],
        });

        let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
            label: Some("dehaze"),
            timestamp_writes: None,
        });
        pass.set_pipeline(&self.stats_pipeline);
        pass.set_bind_group(0, &stats_bg, &[]);
        pass.dispatch_workgroups(cells.width.div_ceil(8), cells.height.div_ceil(8), 1);
        pass.set_pipeline(&self.airlight_pipeline);
        pass.set_bind_group(0, &airlight_bg, &[]);
        pass.dispatch_workgroups(1, 1, 1);
        pass.set_pipeline(&self.apply_pipeline);
        pass.set_bind_group(0, &apply_bg, &[]);
        pass.dispatch_workgroups(size.width.div_ceil(16), size.height.div_ceil(16), 1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zero_strength_is_identity() {
        assert!(is_identity(0.0));
        assert!(!is_identity(0.35));
    }

    #[test]
    fn uniforms_are_three_vec4s() {
        // WGSL `DehazeParams` is three vec4<f32>.
        assert_eq!(std::mem::size_of::<DehazeUniforms>(), 48);
    }
}
