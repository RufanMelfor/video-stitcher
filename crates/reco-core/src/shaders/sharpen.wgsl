// Universal unsharp-mask sharpening compute shader.
//
// Input:  Rgba8Unorm texture (graded, display-ready)
// Output: Rgba8Unorm texture (sharpened)
//
// Classic unsharp mask: blur the image, subtract the blur from the
// original to isolate high-frequency detail, then add that detail back
// in at `amount` strength. Runs on the final cropped viewport output
// (post-zoom), where AI-panner zoom softens detail the most.
//
// Designed to slot in right after color grading, before encode/present.

struct SharpenParams {
    // Strength of the sharpening effect. 0.0 = no-op (identity).
    // Typical range 0.0-2.0; values above ~1.5 start to show visible
    // haloing around high-contrast edges.
    amount: f32,
    // Blur sample radius in pixels. Larger radius sharpens coarser
    // detail (good when heavily zoomed); smaller radius targets fine
    // detail. Typical range 1.0-3.0.
    radius: f32,
    _pad0: f32,
    _pad1: f32,
}

@group(0) @binding(0) var input: texture_2d<f32>;
@group(0) @binding(1) var output: texture_storage_2d<rgba8unorm, write>;
@group(0) @binding(2) var<uniform> params: SharpenParams;

@compute @workgroup_size(16, 16)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let dims = textureDimensions(input);
    if (gid.x >= dims.x || gid.y >= dims.y) {
        return;
    }

    let center = vec2<i32>(gid.xy);
    let original = textureLoad(input, center, 0).rgb;

    // 3x3 box blur scaled by `radius`, sampled at integer pixel offsets
    // (nearest neighbor - the input texture format is unfiltered).
    let r = max(i32(round(params.radius)), 1);
    var blur_sum = vec3<f32>(0.0);
    var sample_count = 0.0;
    for (var dy = -1; dy <= 1; dy = dy + 1) {
        for (var dx = -1; dx <= 1; dx = dx + 1) {
            let offset = vec2<i32>(dx * r, dy * r);
            let coord = clamp(center + offset, vec2<i32>(0), vec2<i32>(dims) - vec2<i32>(1));
            blur_sum = blur_sum + textureLoad(input, coord, 0).rgb;
            sample_count = sample_count + 1.0;
        }
    }
    let blurred = blur_sum / sample_count;

    // High-frequency detail = original - blurred. Add it back at `amount`.
    let detail = original - blurred;
    let sharpened = original + detail * params.amount;

    textureStore(output, center, vec4<f32>(clamp(sharpened, vec3<f32>(0.0), vec3<f32>(1.0)), 1.0));
}
