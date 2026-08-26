// Reco v2 -- Fisheye undistortion + YUV-space color transfer
//
// Ported from v1 GLSL (frontend/src/features/viewer/shaders/fisheye.js).
// Applies KB4 fisheye distortion correction on two 3D-positioned planes.
//
// Color transfer uses YUV space (BT.709): RGB->YUV is 3 mul + 3 add,
// apply per-channel scale+offset (3 mul + 3 add), YUV->RGB (3 mul + 3 add).
// ~18 arithmetic ops total, zero transcendentals. Replaces the previous
// Reinhard LAB pipeline (~42 transcendental ops per pixel: pow, sqrt).

struct Uniforms {
    mvp: mat4x4<f32>,
    // Camera intrinsics (normalized: fx/width, fy/height, cx/width, cy/height)
    intrinsics: vec4<f32>,
    // KB4 distortion coefficients (k1, k2, k3, k4)
    dist: vec4<f32>,
    // YUV color transfer: scale.xyz (Y, U, V), w = 1/gamma (manual
    // per-camera pre-correction, see apply_gamma)
    color_scale: vec4<f32>,
    // YUV color transfer: offset.xyz (Y, U, V), blend_width
    color_offset_blend: vec4<f32>,
    // flags.x: is_right (0 or 1)
    // flags.y: input_format (0 = YUV420P: separate U,V textures; 1 = NV12: interleaved UV
    //          in t_u; 2 = BGRA/RGBA: t_y holds packed 4-channel RGB, skip YUV conversion)
    // flags.z: flip_180 (0 or 1) - flip UV coordinates for 180-degree rotation
    //          Used by the GPU zero-copy path where buffer reversal is not possible.
    // flags.w: is_full_range (0 = limited 16-235, 1 = full 0-255)
    flags: vec4<u32>,
    // lens_preview.x: correction_amount (0.0 = no correction, 1.0 = full KB4)
    // lens_preview.y: split_view (> 0.5 = left half uncorrected, right half corrected)
    // lens_preview.z: show_seam_line (> 0.5 = draw a debug line at this
    //   plane's seam-adjacent edge; see the seam-line comment in fs_main.
    //   Only meaningful on whichever plane is fading this frame - see
    //   ground_tilt.w below)
    // lens_preview.w: seam_offset (MatchCalibration::seam_offset /
    //   ViewportConfig::seam_offset) - manual nudge of the seam-adjacent
    //   edge position, in this plane's own local UV units. 0.0 = no-op.
    //   Only meaningful on whichever plane is fading this frame.
    lens_preview: vec4<f32>,
    // ground_tilt.x: tilt parameter c (tan(theta) of the extra near-field
    //   ground-plane tilt; 0.0 = no-op, matches PlaneLayout::ground_tilt_x/z)
    // ground_tilt.y: focal-scale constant k (CameraParams::ground_tilt_k) -
    //   only meaningful when .x != 0.0
    // ground_tilt.z: this plane's aspect ratio (width / height), needed to
    //   convert the plane's own UV.y into the same "plane space" convention
    //   band_limited_ground_warp expects (see reco_calibrate::geometry)
    // ground_tilt.w: fades_at_seam (> 0.5 = this plane ramps alpha near
    //   its seam-adjacent edge this frame; see the seam-blending comment
    //   in fs_main and ViewportConfig::blend_flip_direction)
    ground_tilt: vec4<f32>,
    // top_tilt.x: tilt parameter c for the top-of-frame correction
    //   (PlaneLayout::top_tilt_x/z); 0.0 = no-op. Mirror image of
    //   ground_tilt.x - see band_limited_top_warp.
    // top_tilt.y: focal-scale constant k (same as ground_tilt.y) - only
    //   meaningful when .x != 0.0
    // top_tilt.z: PlaneLayout::ground_tilt_band_width - |t| at which
    //   ground_tilt's correction reaches full strength (repurposed padding:
    //   this plane's aspect ratio already lives in ground_tilt.z, so this
    //   slot would otherwise be unused)
    // top_tilt.w: PlaneLayout::top_tilt_band_width - same, for top_tilt's
    //   own band
    top_tilt: vec4<f32>,
};

// YUV420P plane textures (Y = full res R8Unorm, U/V = half res R8Unorm)
@group(0) @binding(0) var t_y: texture_2d<f32>;
@group(0) @binding(1) var t_u: texture_2d<f32>;
@group(0) @binding(2) var t_v: texture_2d<f32>;
@group(0) @binding(3) var s_video: sampler;
@group(1) @binding(0) var<uniform> u: Uniforms;

struct VertexInput {
    @location(0) position: vec3<f32>,
    @location(1) uv: vec2<f32>,
};

struct VertexOutput {
    @builtin(position) clip_position: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

@vertex
fn vs_main(in: VertexInput) -> VertexOutput {
    var out: VertexOutput;
    out.clip_position = u.mvp * vec4<f32>(in.position, 1.0);
    out.uv = in.uv;
    return out;
}

// ---- YUV-space color transfer ----
//
// BT.709 RGB<->YUV conversion uses only multiply-add operations (no pow/sqrt).
// The CPU computes per-channel scale+offset from source/target statistics once;
// the shader applies them every pixel with ~18 arithmetic ops total.

fn rgb_to_yuv(rgb: vec3<f32>) -> vec3<f32> {
    // BT.709 full-range RGB [0,1] -> YUV (Y [0,1], U/V [-0.5, 0.5])
    let y = 0.2126 * rgb.r + 0.7152 * rgb.g + 0.0722 * rgb.b;
    let u = -0.1146 * rgb.r - 0.3854 * rgb.g + 0.5 * rgb.b;
    let v = 0.5 * rgb.r - 0.4542 * rgb.g - 0.0458 * rgb.b;
    return vec3<f32>(y, u, v);
}

fn yuv_to_rgb(yuv: vec3<f32>) -> vec3<f32> {
    // BT.709 YUV -> full-range RGB [0,1]
    let r = yuv.x + 1.5748 * yuv.z;
    let g = yuv.x - 0.1873 * yuv.y - 0.4681 * yuv.z;
    let b = yuv.x + 1.8556 * yuv.y;
    return vec3<f32>(r, g, b);
}

// Manual per-camera gamma, applied BEFORE the automatic scale+offset.
// `inv_gamma` is 1/gamma, precomputed on the CPU so this is one pow and no
// divide. Deliberately ahead of the automatic correction: that correction
// is derived from a measurement of these same, already-gamma'd pixels
// (SYNC_WITH `render::color_match::decode_transfer_yuv`, which performs
// the identical curve per sample point before averaging). Reversing the
// order, or applying the curve in only one of the two places, would have
// the automatic stage correcting a frame that is never rendered.
fn apply_gamma(rgb: vec3<f32>, inv_gamma: f32) -> vec3<f32> {
    // The branch is on a uniform, so it is coherent across the whole draw
    // and costs nothing; the pow it skips is per pixel, per channel.
    if inv_gamma == 1.0 {
        return rgb;
    }
    return pow(max(rgb, vec3<f32>(0.0)), vec3<f32>(inv_gamma));
}

fn apply_color_transfer(
    rgb: vec3<f32>,
    scale: vec3<f32>,
    offset: vec3<f32>,
    inv_gamma: f32,
) -> vec3<f32> {
    let corrected = apply_gamma(rgb, inv_gamma);
    // Skip if identity transform (scale=1, offset=0)
    if all(scale == vec3<f32>(1.0)) && all(offset == vec3<f32>(0.0)) {
        return corrected;
    }
    var yuv = rgb_to_yuv(corrected);
    yuv = yuv * scale + offset;
    return clamp(yuv_to_rgb(yuv), vec3<f32>(0.0), vec3<f32>(1.0));
}

// ---- YUV → RGB conversion ----

/// Sample the input plane(s) and return an sRGB-domain RGB triple.
///
/// Supports three input layouts (selected by `u.flags.y`):
///   0 = YUV420P: separate R8 textures for Y, U, V (software decode).
///   1 = NV12: R8 Y texture + Rg8 UV texture with interleaved U,V (NVDEC).
///   2 = BGRA/RGBA: t_y is an `Rgba8Unorm` texture holding packed RGB
///       (source already sRGB-domain, skip YUV conversion).
///
/// H.264 uses limited range (Y: 16-235, Cb/Cr: 16-240). After the
/// BT.709 matrix we get sRGB-domain values which we write as-is to
/// the `Rgba8Unorm` render target.
fn sample_yuv(uv: vec2<f32>) -> vec4<f32> {
    // Apply 180-degree rotation for the GPU zero-copy path.
    // The CPU path reverses buffers in software; the GPU path flips UV coords instead.
    var sample_uv = uv;
    if u.flags.z == 1u {
        sample_uv = vec2<f32>(1.0 - uv.x, 1.0 - uv.y);
    }

    // BGRA / RGBA packed path: sample the full RGB triple in one fetch
    // and return without YUV conversion. The upload side is responsible
    // for delivering the triple in (R, G, B) order - swizzling BGRA is
    // handled at upload time so the shader only sees R-in-red.
    if u.flags.y == 2u {
        let rgba = textureSample(t_y, s_video, sample_uv);
        return vec4<f32>(rgba.rgb, 1.0);
    }

    let y_raw = textureSample(t_y, s_video, sample_uv).r;

    var u_raw: f32;
    var v_raw: f32;

    if u.flags.y == 1u {
        // NV12: t_u is Rg8Unorm (or Rg16Unorm for 10-bit) with interleaved (U, V)
        let uv_sample = textureSample(t_u, s_video, sample_uv);
        u_raw = uv_sample.r;
        v_raw = uv_sample.g;
    } else {
        // YUV420P: separate R8 textures
        u_raw = textureSample(t_u, s_video, sample_uv).r;
        v_raw = textureSample(t_v, s_video, sample_uv).r;
    }

    // BT.709 YCbCr -> R'G'B'. Range scaling depends on flags.w:
    //   0 = limited range (Y: 16-235, Cb/Cr: 16-240)
    //   1 = full range (Y: 0-255, Cb/Cr: 0-255)
    var y: f32;
    var cb: f32;
    var cr: f32;
    if u.flags.w == 1u {
        y = y_raw;
        cb = u_raw - 0.5;
        cr = v_raw - 0.5;
    } else {
        y = (y_raw - 16.0 / 255.0) * (255.0 / 219.0);
        cb = (u_raw - 128.0 / 255.0) * (255.0 / 224.0);
        cr = (v_raw - 128.0 / 255.0) * (255.0 / 224.0);
    }

    let r = y + 1.5748 * cr;
    let g = y - 0.1873 * cb - 0.4681 * cr;
    let b = y + 1.8556 * cb;

    let rgb = clamp(vec3<f32>(r, g, b), vec3<f32>(0.0), vec3<f32>(1.0));
    // BT.709 YCbCr->R'G'B' produces sRGB-domain values directly.
    // Render target is Rgba8Unorm, so we write sRGB values as-is.
    return vec4<f32>(rgb, 1.0);
}

// ---- Ground-plane tilt correction ----
//
// Direct port of reco_calibrate::geometry's warp_ground_y /
// band_limited_ground_warp / smoothstep (crates/reco-calibrate/src/geometry.rs).
// The ramp *shape* and its start (GROUND_TILT_BAND_START) must stay
// bit-for-bit equivalent to that Rust copy, since the manual-line-click
// fitting harnesses (FRICTION.md points 8-11, 18-20) use the Rust version's
// fixed 0.08/0.16 band to derive ground_tilt_x/z. Where the ramp reaches
// full strength is, unlike that fixed fitting-time band, a live render-time
// override (PlaneLayout::ground_tilt_band_width/top_tilt_band_width) - so a
// fitted tilt value stays meaningful (the ramp still starts at the same
// 0.08), but its default (0.16) is what's actually bit-for-bit equivalent
// to the Rust fitting harness; a custom width is an intentional departure
// from that, same as manually nudging a fitted tilt value would be.
// See PlaneLayout::ground_tilt_x/z's doc comment for the full picture.

const GROUND_TILT_BAND_START: f32 = 0.08;

fn smoothstep_band(edge0: f32, edge1: f32, x: f32) -> f32 {
    let t = clamp((x - edge0) / (edge1 - edge0), 0.0, 1.0);
    return t * t * (3.0 - 2.0 * t);
}

// warp(t, 0.0, k) == t exactly, so c == 0.0 (no ground tilt fitted) is a
// guaranteed no-op regardless of k - callers still gate on c != 0.0 below
// purely to skip the extra arithmetic, not for correctness.
fn warp_ground_y(t: f32, c: f32, k: f32) -> f32 {
    var denom = 1.0 - c * t / k;
    if abs(denom) < 1e-9 {
        denom = select(-1e-9, 1e-9, denom >= 0.0);
    }
    return (k * c + t) / denom;
}

// One-sided in t, not abs(t): t > 0 is near field (close to the camera,
// bottom of frame); t <= 0 is horizon and sky, always identity regardless
// of magnitude. SYNC_WITH geometry::band_limited_ground_warp's doc
// comment for why this has to be one-sided - a symmetric-in-abs(t)
// version visibly warps the skyline once |t| >= band_full in the negative
// direction, since every pixel (not just near-field matched points) goes
// through this function at render time.
//
// `band_full` (PlaneLayout::ground_tilt_band_width) is clamped to stay
// strictly greater than GROUND_TILT_BAND_START - at or below it, the
// smoothstep's edge0/edge1 would collapse or invert, which is meaningless
// (not just "a wider/narrower band"). The UI only exposes safe values, but
// the shader defends independently regardless of caller.
fn band_limited_ground_warp(t: f32, c: f32, k: f32, band_full: f32) -> f32 {
    if t <= 0.0 {
        return t;
    }
    let full = max(band_full, GROUND_TILT_BAND_START + 0.01);
    let weight = smoothstep_band(GROUND_TILT_BAND_START, full, t);
    if weight == 0.0 {
        return t;
    }
    return t + weight * (warp_ground_y(t, c, k) - t);
}

// Mirror image of band_limited_ground_warp: one-sided the other way.
// t >= 0.0 (horizon and the ground band below it) is always identity,
// regardless of magnitude - only t < 0.0 (top of frame: sky, distant
// background structures) ramps in, by the same smoothstep shape applied
// to |t|. Manual-only parameter (PlaneLayout::top_tilt_x/z) - no
// automatic fitting path exists yet, unlike ground_tilt.
fn band_limited_top_warp(t: f32, c: f32, k: f32, band_full: f32) -> f32 {
    if t >= 0.0 {
        return t;
    }
    let full = max(band_full, GROUND_TILT_BAND_START + 0.01);
    let weight = smoothstep_band(GROUND_TILT_BAND_START, full, -t);
    if weight == 0.0 {
        return t;
    }
    return t + weight * (warp_ground_y(t, c, k) - t);
}

// ---- Fragment shader ----

@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    // Remap UV from [0,1] to [-0.5, 1.5] in the fragment shader.
    // This extends the coordinate space so the undistortion can
    // map points outside the plane back to valid texture coords.
    // (Done here instead of the vertex shader because some embedded
    // GPU drivers pass vertex attributes directly to the fragment
    // stage, ignoring vertex shader output for user-defined varyings.)
    let uv = in.uv * 2.0 - vec2<f32>(0.5);

    // Raw mode: negative correction bypasses all projection math
    // and samples the input texture directly at the fragment UV.
    if u.lens_preview.x < 0.0 {
        if uv.x < 0.0 || uv.x > 1.0 || uv.y < 0.0 || uv.y > 1.0 {
            return vec4<f32>(0.0, 0.0, 0.0, 0.0);
        }
        let raw = sample_yuv(uv);
        return vec4<f32>(raw.rgb, 1.0);
    }

    let fx = u.intrinsics.x;
    let fy = u.intrinsics.y;
    let cx = u.intrinsics.z;
    let cy = u.intrinsics.w;

    // Ground-plane tilt: warp this plane's own local y-position (in the
    // same "plane space" convention geometry::normalize_to_plane produces
    // from AKAZE keypoints) before the KB4 lookup below, so the correction
    // fitted against real near-field measurements (FRICTION.md points
    // 18-20) has the same effect here as it did during that fit. Uses
    // `in.uv.y` (the plane's raw 0..1 texture position), not the extended
    // `uv.y` above - `ground_tilt.z` (plane aspect) converts it into the
    // same width-normalized unit `normalize_to_plane` uses.
    var uv_y = uv.y;
    if u.ground_tilt.x != 0.0 || u.top_tilt.x != 0.0 {
        var plane_y = (in.uv.y - 0.5) / u.ground_tilt.z;
        if u.ground_tilt.x != 0.0 {
            plane_y = band_limited_ground_warp(plane_y, u.ground_tilt.x, u.ground_tilt.y, u.top_tilt.z);
        }
        if u.top_tilt.x != 0.0 {
            plane_y = band_limited_top_warp(plane_y, u.top_tilt.x, u.top_tilt.y, u.top_tilt.w);
        }
        uv_y = (plane_y * u.ground_tilt.z + 0.5) * 2.0 - 0.5;
    }

    // KB4 fisheye undistortion: map from plane UV to video texture coordinate
    let x = (uv.x - cx) / fx;
    let y = (uv_y - cy) / fy;
    let r = sqrt(x * x + y * y);
    let theta = atan(r);
    let theta2 = theta * theta;
    let theta_d_full = theta * (1.0
        + u.dist.x * theta2
        + u.dist.y * theta2 * theta2
        + u.dist.z * theta2 * theta2 * theta2
        + u.dist.w * theta2 * theta2 * theta2 * theta2);

    // Lens correction amount: 1.0 = full KB4, 0.0 = identity (pinhole).
    // Split view: left half uncorrected, right half fully corrected.
    var correction = u.lens_preview.x;
    if u.lens_preview.y > 0.5 {
        correction = select(0.0, 1.0, uv.x > 0.5);
    }
    let theta_d = mix(theta, theta_d_full, correction);

    var scale = 1.0;
    if r > 0.0 {
        scale = theta_d / r;
    }

    let distorted_uv = vec2<f32>(
        fx * x * scale + cx,
        fy * y * scale + cy,
    );

    // Bounds check — v2 uses separate textures (no stacking)
    if distorted_uv.x < 0.0 || distorted_uv.x > 1.0 ||
       distorted_uv.y < 0.0 || distorted_uv.y > 1.0 {
        return vec4<f32>(0.0, 0.0, 0.0, 0.0);
    }

    let tex_color = sample_yuv(distorted_uv);
    var color = tex_color.rgb;

    // Apply YUV-space color transfer
    color = apply_color_transfer(
        color,
        u.color_scale.xyz,
        u.color_offset_blend.xyz,
        u.color_scale.w,
    );

    // Compute alpha for seam blending. `ground_tilt.w` (otherwise unused -
    // see `Uniforms`' doc above) marks which plane fades at the seam this
    // frame; the other plane stays fully opaque. Direction is chosen CPU-
    // side (`ViewportConfig::blend_flip_direction`, default = right fades
    // over a fixed left) together with draw order, so the fading plane
    // always draws second/on top of the already-opaque one - see
    // `encode_stitch_pass`'s comment for why that ordering matters.
    //
    // The two planes' seam-adjacent edges sit at opposite ends of their
    // own local `uv.x` (the right plane's quad keeps its local +X axis
    // aligned with world +X, seam-adjacent edge at uv.x=0; the left
    // plane's quad is rotated 90 degrees so its local +X maps to world
    // -Z, putting its seam-adjacent edge at the opposite end, uv.x=1).
    // `seam_offset` (see `Uniforms.lens_preview.w` above) shifts this
    // threshold so a positive value always shrinks the *fading* plane's
    // own visible extent (delays its fade-in), regardless of which plane
    // is fading - a uniform, plane-relative meaning. The apparent on-screen
    // direction that corresponds to therefore depends on
    // `ViewportConfig::blend_flip_direction`; the CPU-side drag handler
    // accounts for that so a screen-space drag feels consistent either way.
    var alpha = 1.0;
    let blend_width = u.color_offset_blend.w;
    let seam_offset = u.lens_preview.w;
    if u.ground_tilt.w > 0.5 && blend_width > 0.0 {
        if u.flags.x == 1u {
            alpha = smoothstep(seam_offset, seam_offset + blend_width, uv.x);
        } else {
            alpha = 1.0 - smoothstep(1.0 - blend_width - seam_offset, 1.0 - seam_offset, uv.x);
        }
    }

    // Split-view separator line (1px white at the midpoint)
    if u.lens_preview.y > 0.5 && abs(uv.x - 0.5) < 0.001 {
        return vec4<f32>(1.0, 1.0, 1.0, alpha);
    }

    // Seam position debug line: highlights exactly where the alpha fade's
    // threshold sits (including `seam_offset`'s manual nudge), independent
    // of how wide `blend_width` currently feathers it. Only set on the
    // fading-designated plane's uniforms CPU-side, so this never
    // double-draws. `fwidth` keeps the line a consistent ~1.5px regardless
    // of output resolution instead of a fixed UV-space width that would
    // get thinner/thicker as the render target resizes.
    if u.lens_preview.z > 0.5 {
        let seam_dist = select(1.0 - uv.x - seam_offset, uv.x - seam_offset, u.flags.x == 1u);
        let line_half_width = fwidth(seam_dist) * 1.5;
        if abs(seam_dist) < line_half_width {
            return vec4<f32>(1.0, 0.15, 0.15, 1.0);
        }
    }

    return vec4<f32>(color, alpha);
}
