// Multi-band (2-band) seam composite. Reconstructs the final stitched
// frame from each camera's full-resolution render plus its Gaussian-blurred
// low-frequency version and a blurred seam mask:
//
//   low   = blend(blur_left, blur_right, wide seam ramp)
//   high  = blend(left-blur_left, right-blur_right, narrow seam ramp)
//   final = low + high
//
// Blending low frequencies over a WIDE band hides residual near-field
// misalignment without visibly widening the crossfade on fine detail (ball,
// player edges, field lines), which is what makes a single-band crossfade
// ghost/double when widened. See `render/renderer.rs`'s
// `encode_multiband_stitch_pass` doc comment and `reco-core/FRICTION.md`.

struct CompositeUniforms {
    // x: 1.0 if the seam mask represents the RIGHT plane's own alpha ramp
    //    (i.e. right is the fading plane, matching
    //    `ViewportConfig::blend_flip_direction == false`); 0.0 if left is.
    // y: half-width (in mask-value units, 0..0.5) of the narrow-band
    //    smoothstep re-derived from the wide mask around its 0.5 crossover.
    //    Smaller = sharper high-frequency transition.
    params: vec4<f32>,
}

@group(0) @binding(0) var t_a: texture_2d<f32>;
@group(0) @binding(1) var t_b: texture_2d<f32>;
@group(0) @binding(2) var t_a_blur: texture_2d<f32>;
@group(0) @binding(3) var t_b_blur: texture_2d<f32>;
@group(0) @binding(4) var t_mask_blur: texture_2d<f32>;
@group(0) @binding(5) var s_samp: sampler;
@group(1) @binding(0) var<uniform> u: CompositeUniforms;

struct VOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

@vertex
fn vs_main(@builtin(vertex_index) idx: u32) -> VOut {
    let x = f32((idx << 1u) & 2u) * 2.0 - 1.0;
    let y = f32(idx & 2u) * 2.0 - 1.0;
    var out: VOut;
    out.pos = vec4<f32>(x, y, 0.0, 1.0);
    out.uv = vec2<f32>((x + 1.0) * 0.5, 1.0 - (y + 1.0) * 0.5);
    return out;
}

@fragment
fn fs_main(in: VOut) -> @location(0) vec4<f32> {
    let m_wide = textureSample(t_mask_blur, s_samp, in.uv).a;
    let eps = max(u.params.y, 0.005);
    let m_narrow = smoothstep(0.5 - eps, 0.5 + eps, m_wide);

    let a_full = textureSample(t_a, s_samp, in.uv);
    let b_full = textureSample(t_b, s_samp, in.uv);

    var a_blur = textureSample(t_a_blur, s_samp, in.uv);
    var b_blur = textureSample(t_b_blur, s_samp, in.uv);
    // Unpremultiply (the blur pass stores premultiplied color so blurring
    // near a coverage edge doesn't pull in black from uncovered texels).
    a_blur = vec4<f32>(a_blur.rgb / max(a_blur.a, 1e-4), a_blur.a);
    b_blur = vec4<f32>(b_blur.rgb / max(b_blur.a, 1e-4), b_blur.a);

    let a_high = a_full.rgb - a_blur.rgb;
    let b_high = b_full.rgb - b_blur.rgb;

    var low: vec3<f32>;
    var high: vec3<f32>;
    if u.params.x > 0.5 {
        // Right plane fades: mask 0 = left's side, 1 = right's side.
        low = mix(a_blur.rgb, b_blur.rgb, m_wide);
        high = mix(a_high, b_high, m_narrow);
    } else {
        low = mix(b_blur.rgb, a_blur.rgb, m_wide);
        high = mix(b_high, a_high, m_narrow);
    }

    let color = clamp(low + high, vec3<f32>(0.0), vec3<f32>(1.0));
    let coverage = max(a_full.a, b_full.a);
    return vec4<f32>(color, coverage);
}
