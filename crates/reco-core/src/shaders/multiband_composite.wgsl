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
    // Preview-only overlap view (renderer.rs `OverlapView`):
    // x: mode (0 off, 1 right over left, 2 left over right, 3 difference)
    // y: opacity (0..1)
    // z: 1.0 = the 2-band blend is on; 0.0 = this pass only runs for the
    //    overlap view, so reproduce the plain single-band crossfade instead
    overlap: vec4<f32>,
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

    var color = clamp(low + high, vec3<f32>(0.0), vec3<f32>(1.0));
    // Mask value where 1 = the right camera's side of the seam.
    let right_side = select(1.0 - m_wide, m_wide, u.params.x > 0.5);
    if u.overlap.z < 0.5 {
        // Multi-band blend off: plain crossfade of the full-res renders,
        // close to the single-band stitch the user normally sees.
        color = mix(a_full.rgb, b_full.rgb, smoothstep(0.5 - eps, 0.5 + eps, right_side));
    }

    // Overlap view (calibration aid). a = left camera, b = right camera,
    // each with hard 0/1 coverage alpha from their own full-plane render.
    let mode = u.overlap.x;
    let opacity = u.overlap.y;
    let a_cov = a_full.a > 0.5;
    let b_cov = b_full.a > 0.5;
    if mode > 0.5 && mode < 1.5 && b_cov && right_side < 0.5 {
        // Right camera ghosted over the left camera's side.
        color = mix(color, b_full.rgb, opacity);
    } else if mode > 1.5 && mode < 2.5 && a_cov && right_side >= 0.5 {
        // Left camera ghosted over the right camera's side.
        color = mix(color, a_full.rgb, opacity);
    } else if mode > 2.5 && a_cov && b_cov {
        // Amplified per-channel difference: aligned = near black.
        let diff = clamp(abs(a_full.rgb - b_full.rgb) * 3.0, vec3<f32>(0.0), vec3<f32>(1.0));
        color = mix(color, diff, opacity);
    }

    let coverage = max(a_full.a, b_full.a);
    return vec4<f32>(color, coverage);
}
