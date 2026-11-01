// Distance-aware fog/haze removal, three compute entry points run in order
// on the stitched viewport (before color grade and overlays):
//
//   stats    one thread per 16x16-pixel cell: mean RGB + mean min-channel
//            into a small rgba16float texture.
//   airlight one workgroup: the fog colour A = mean of the brightest cells
//            above the horizon, smoothed over frames in `state` (kept while
//            no sky is in view, e.g. the camera looking down at the pitch).
//   apply    per pixel, haze model J = (I - A v) / (1 - v) in display space:
//              v = min(strength * dist, cap * dark_low) * guard
//            dist     1 at the horizon, ~1/distance below it (ground plane
//                     seen from the camera; elevation from the view geometry)
//            dark_low local min-channel / A: caps v on dark near objects that
//                     stand above the horizon (a stand, a roof)
//            guard    leaves pixels as bright as the fog alone (the sky)
//            then the local brightness the veil removal took away is given
//            back (ratio in [1, 2]) raised to `keep_light`: 1 = the result
//            gains colour and contrast without getting darker (thick fog),
//            0 = full veil removal, darker and punchier (thin fog, where
//            giving brightness back looks milky).

const CELL: i32 = 16;
const LUMA: vec3<f32> = vec3<f32>(0.2126, 0.7152, 0.0722);
// Upper bound on the brightness give-back ratio.
const MAX_GIVE_BACK: f32 = 2.0;

struct DehazeParams {
    // xyz: true up (level world vertical) in camera space; w: tan(vfov / 2).
    up_cam: vec4<f32>,
    // x: aspect (w / h), y: strength, z: cap, w: tan(far angle).
    shape: vec4<f32>,
    // x: guard band (fraction of A's luma), y: airlight EMA alpha,
    // z: min sin(elevation) for a sky cell, w: keep_light (0..1).
    tuning: vec4<f32>,
}

// rgb: smoothed airlight; w: 1 once estimated, 0 before.
struct DehazeState {
    airlight: vec4<f32>,
}

@group(0) @binding(0) var input: texture_2d<f32>;
@group(0) @binding(1) var stats_out: texture_storage_2d<rgba16float, write>;
@group(0) @binding(2) var stats: texture_2d<f32>;
@group(0) @binding(3) var stats_sampler: sampler;
@group(0) @binding(4) var<storage, read_write> state: DehazeState;
@group(0) @binding(5) var<uniform> params: DehazeParams;
@group(0) @binding(6) var output: texture_storage_2d<rgba8unorm, write>;

// sin(elevation) of the view ray through normalized image position `uv`
// (0..1, y down). Positive = above the horizon.
fn sin_elevation(uv: vec2<f32>) -> f32 {
    let ndc = vec2<f32>(uv.x * 2.0 - 1.0, 1.0 - uv.y * 2.0);
    let t = params.up_cam.w;
    let dir = normalize(vec3<f32>(ndc.x * t * params.shape.x, ndc.y * t, -1.0));
    return dot(dir, params.up_cam.xyz);
}

@compute @workgroup_size(8, 8)
fn stats_main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let cells = textureDimensions(stats_out);
    if (gid.x >= cells.x || gid.y >= cells.y) {
        return;
    }
    let dims = vec2<i32>(textureDimensions(input));
    let origin = vec2<i32>(gid.xy) * CELL;
    var rgb_sum = vec3<f32>(0.0);
    var dark_sum = 0.0;
    var n = 0.0;
    for (var y = 0; y < CELL; y = y + 1) {
        for (var x = 0; x < CELL; x = x + 1) {
            let p = origin + vec2<i32>(x, y);
            if (p.x < dims.x && p.y < dims.y) {
                let c = textureLoad(input, p, 0).rgb;
                rgb_sum = rgb_sum + c;
                dark_sum = dark_sum + min(c.r, min(c.g, c.b));
                n = n + 1.0;
            }
        }
    }
    let inv = 1.0 / max(n, 1.0);
    textureStore(stats_out, vec2<i32>(gid.xy), vec4<f32>(rgb_sum * inv, dark_sum * inv));
}

const AIR_THREADS: u32 = 256u;
var<workgroup> wg_max: array<f32, 256>;
var<workgroup> wg_sum: array<vec4<f32>, 256>;

@compute @workgroup_size(256)
fn airlight_main(@builtin(local_invocation_index) lid: u32) {
    let cells = textureDimensions(stats);
    let total = cells.x * cells.y;
    let min_sin = params.tuning.z;

    // Pass 1: brightest sky cell.
    var local_max = 0.0;
    for (var i = lid; i < total; i = i + AIR_THREADS) {
        let c = vec2<u32>(i % cells.x, i / cells.x);
        let uv = (vec2<f32>(c) + 0.5) / vec2<f32>(cells);
        if (sin_elevation(uv) > min_sin) {
            local_max = max(local_max, dot(textureLoad(stats, c, 0).rgb, LUMA));
        }
    }
    wg_max[lid] = local_max;
    workgroupBarrier();
    for (var s = AIR_THREADS / 2u; s > 0u; s = s / 2u) {
        if (lid < s) {
            wg_max[lid] = max(wg_max[lid], wg_max[lid + s]);
        }
        workgroupBarrier();
    }
    let sky_max = wg_max[0];

    // Pass 2: mean colour of the sky cells within 3% of the brightest.
    var local_sum = vec4<f32>(0.0);
    for (var i = lid; i < total; i = i + AIR_THREADS) {
        let c = vec2<u32>(i % cells.x, i / cells.x);
        let uv = (vec2<f32>(c) + 0.5) / vec2<f32>(cells);
        if (sin_elevation(uv) > min_sin) {
            let rgb = textureLoad(stats, c, 0).rgb;
            if (dot(rgb, LUMA) >= 0.97 * sky_max) {
                local_sum = local_sum + vec4<f32>(rgb, 1.0);
            }
        }
    }
    wg_sum[lid] = local_sum;
    workgroupBarrier();
    for (var s = AIR_THREADS / 2u; s > 0u; s = s / 2u) {
        if (lid < s) {
            wg_sum[lid] = wg_sum[lid] + wg_sum[lid + s];
        }
        workgroupBarrier();
    }

    if (lid == 0u) {
        let sum = wg_sum[0];
        // Too little sky in view to trust: keep the last estimate. A dark
        // "sky" (a stand roof filling the top) is not fog either.
        let a_new = sum.rgb / max(sum.w, 1.0);
        if (sum.w >= 4.0 && dot(a_new, LUMA) > 0.35) {
            if (state.airlight.w < 0.5) {
                state.airlight = vec4<f32>(a_new, 1.0);
            } else {
                let a = mix(state.airlight.rgb, a_new, params.tuning.y);
                state.airlight = vec4<f32>(a, 1.0);
            }
        }
    }
}

// Low-res stats at `uv` smoothed with a 3x3 tent of bilinear taps one cell
// apart (stands in for the prototype's Gaussian blur).
fn stats_low(uv: vec2<f32>) -> vec4<f32> {
    let cell = 1.0 / vec2<f32>(textureDimensions(stats));
    var acc = vec4<f32>(0.0);
    for (var dy = -1; dy <= 1; dy = dy + 1) {
        for (var dx = -1; dx <= 1; dx = dx + 1) {
            let w = (2.0 - abs(f32(dx))) * (2.0 - abs(f32(dy))) / 16.0;
            let p = uv + vec2<f32>(f32(dx), f32(dy)) * cell;
            acc = acc + w * textureSampleLevel(stats, stats_sampler, p, 0.0);
        }
    }
    return acc;
}

@compute @workgroup_size(16, 16)
fn apply_main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let dims = textureDimensions(input);
    if (gid.x >= dims.x || gid.y >= dims.y) {
        return;
    }
    let p = vec2<i32>(gid.xy);
    let rgb = textureLoad(input, p, 0).rgb;
    let a4 = state.airlight;
    if (a4.w < 0.5) {
        // No fog colour estimated yet (no sky seen): pass through.
        textureStore(output, p, vec4<f32>(rgb, 1.0));
        return;
    }
    let a = a4.rgb;
    let a_l = max(dot(a, LUMA), 1e-3);

    let uv = (vec2<f32>(gid.xy) + 0.5) / vec2<f32>(dims);
    // Depression angle below the horizon -> relative ground distance.
    let s = sin_elevation(uv);
    let tan_far = params.shape.w;
    var dist = 1.0;
    let depression_sin = -s;
    if (depression_sin > 0.0) {
        let tan_dep = depression_sin / sqrt(max(1.0 - depression_sin * depression_sin, 1e-6));
        dist = min(tan_far / max(tan_dep, 1e-4), 1.0);
    }

    let low = stats_low(uv);
    let l_low = dot(low.rgb, LUMA);
    let dark_low = low.a / a_l;
    let guard = clamp((a_l - dot(rgb, LUMA)) / (params.tuning.x * a_l), 0.0, 1.0);
    let v = min(params.shape.y * dist, params.shape.z * dark_low) * guard;

    var out = (rgb - a * v) / (1.0 - v);
    let ratio = clamp(
        l_low * (1.0 - v) / max(l_low - a_l * v, 1e-3),
        1.0,
        MAX_GIVE_BACK,
    );
    out = out * pow(ratio, params.tuning.w);
    textureStore(output, p, vec4<f32>(clamp(out, vec3<f32>(0.0), vec3<f32>(1.0)), 1.0));
}
