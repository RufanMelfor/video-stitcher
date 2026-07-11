// Separable Gaussian blur, one direction (horizontal or vertical) per draw.
// Used by the multi-band seam blend (`Renderer::encode_multiband_stitch_pass`)
// to build the low-frequency (blurred) layer of each camera plus a smoothly
// widened seam mask, without needing a full mipmap chain.

struct BlurUniforms {
    // (1/width, 1/height) of the source texture.
    texel_size: vec2<f32>,
    // (1,0) for a horizontal pass, (0,1) for a vertical pass.
    direction: vec2<f32>,
    // x: Gaussian sigma in texels. y: premultiply-alpha-on-read flag (>0.5 = yes).
    // The first pass of a two-pass blur reads straight-alpha source content
    // and must premultiply before averaging (otherwise transparent texels
    // pull black into the blurred edge); the second pass reads the first
    // pass's already-premultiplied output and must not premultiply again.
    params: vec4<f32>,
}

@group(0) @binding(0) var t_src: texture_2d<f32>;
@group(0) @binding(1) var s_src: sampler;
@group(1) @binding(0) var<uniform> u: BlurUniforms;

struct VOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

// Full-screen triangle: 3 vertices, no vertex buffer needed.
@vertex
fn vs_main(@builtin(vertex_index) idx: u32) -> VOut {
    let x = f32((idx << 1u) & 2u) * 2.0 - 1.0;
    let y = f32(idx & 2u) * 2.0 - 1.0;
    var out: VOut;
    out.pos = vec4<f32>(x, y, 0.0, 1.0);
    out.uv = vec2<f32>((x + 1.0) * 0.5, 1.0 - (y + 1.0) * 0.5);
    return out;
}

// Fixed max tap radius so the loop bound is a compile-time constant. Actual
// radius used is min(MAX_RADIUS, ceil(3*sigma)) - a wider sigma than
// MAX_RADIUS/3 texels undersamples (soft approximation, not exact), which
// is an acceptable tradeoff for a visual seam-softening pass.
const MAX_RADIUS: i32 = 32;

fn gaussian_weight(x: f32, sigma: f32) -> f32 {
    let s = max(sigma, 0.6);
    return exp(-0.5 * (x * x) / (s * s));
}

@fragment
fn fs_main(in: VOut) -> @location(0) vec4<f32> {
    let sigma = u.params.x;
    let premultiply = u.params.y > 0.5;
    let radius = i32(min(f32(MAX_RADIUS), ceil(sigma * 3.0)));

    var sum = vec4<f32>(0.0, 0.0, 0.0, 0.0);
    var wsum = 0.0;
    for (var i = -radius; i <= radius; i = i + 1) {
        let offset = f32(i) * u.direction * u.texel_size;
        var s = textureSample(t_src, s_src, in.uv + offset);
        if premultiply {
            s = vec4<f32>(s.rgb * s.a, s.a);
        }
        let w = gaussian_weight(f32(i), sigma);
        sum = sum + s * w;
        wsum = wsum + w;
    }
    return sum / max(wsum, 1e-5);
}
