// Reco -- seam-band texel gather for the automatic color match.
//
// Reads the NV12 planes at a list of precomputed positions and writes the
// raw texel values out. That is all it does: no range expansion, no
// YUV/RGB conversion, no gamma, no averaging. Every bit of that stays in
// `render::color_match` on the CPU, so the curve the measurement applies
// is defined in exactly one place and cannot drift from the one
// `fisheye.wgsl` renders.
//
// Exists because the automatic color match measured pixel data on the CPU,
// which the zero-copy decode paths never expose - making the correction
// silently inactive in a hardware-decoded export. See reco-core's
// FRICTION.md.
//
// Positions come from `color_match::band_sample_positions` and are in the
// raw (still distorted) frame, same as the CPU sampler uses.

struct GatherParams {
    // Index of this camera's first slot in `coords` / `samples`. One
    // dispatch per camera writes into its own half of the buffers.
    base: u32,
    // Real sample count for this camera; the rest of its half is stale
    // and must not be read back.
    count: u32,
    pad0: u32,
    pad1: u32,
};

@group(0) @binding(0) var y_plane: texture_2d<f32>;
@group(0) @binding(1) var uv_plane: texture_2d<f32>;
@group(0) @binding(2) var<storage, read> coords: array<vec2<u32>>;
@group(0) @binding(3) var<storage, read_write> samples: array<vec4<f32>>;
@group(0) @binding(4) var<uniform> params: GatherParams;

@compute @workgroup_size(64)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let i = gid.x;
    if i >= params.count {
        return;
    }
    let slot = params.base + i;
    let c = vec2<i32>(coords[slot]);

    // NV12: full-resolution Y, half-resolution interleaved UV. Integer
    // load, not a sampler - the CPU path reads the nearest texel too, and
    // any filtering here would measure something the shader never draws.
    let y = textureLoad(y_plane, c, 0).r;
    let uv = textureLoad(uv_plane, c / 2, 0).rg;

    samples[slot] = vec4<f32>(y, uv.x, uv.y, 1.0);
}
