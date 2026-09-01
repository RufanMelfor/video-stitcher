//! CPU-side exposure/color matching between the two cameras.
//!
//! Two independently-metering action cameras rarely agree on exposure or
//! white balance, which can show up as a visible brightness/color step at
//! the stitch seam even when the geometric alignment is perfect. This
//! module periodically samples a coarse grid of points in the seam-adjacent
//! band of each camera's *raw* (still-distorted) frame, maps them into the
//! rendered plane's color-transfer space, and derives a small per-camera
//! YUV offset that nudges both cameras toward their shared mean. The result
//! feeds the already-existing (previously always-identity)
//! `color_offset_blend` uniform in `fisheye.wgsl`.
//!
//! Only available where CPU-decoded YUV420P/NV12 planes exist ahead of GPU
//! upload (`StitchPipeline::render_to_target`/`render_to_view` and their
//! NV12 counterparts). The BGRA and GPU zero-copy paths have no CPU-side
//! pixel access at the point frames become available, so they always pass
//! [`ColorCorrection::default()`] (identity) - documented limitation, not a
//! bug.

use super::renderer::ColorCorrection;
use crate::calibration::Lens;
use crate::lens::undistorted_to_distorted;

/// Convert a user-facing gamma into the exponent both the shader and
/// [`decode_transfer_yuv`] actually apply (`out = in^(1/gamma)`, so a
/// gamma above 1.0 lifts the mid-tones).
///
/// Non-finite or non-positive input falls back to identity: `pow` with
/// such an exponent produces NaN or a flat frame across every pixel, and
/// a calibration file is user-editable, so this is a real input to
/// validate rather than an unreachable case.
pub(crate) fn inv_gamma(gamma: f32) -> f32 {
    if gamma.is_finite() && gamma > 0.0 {
        1.0 / gamma
    } else {
        1.0
    }
}

/// Tunable knobs for [`ColorMatchState`]. Flat fields on
/// [`crate::render::viewport::ViewportConfig`] (`color_match_*`) hold the
/// live values a consumer GUI lets the user adjust; this struct just bundles
/// them for passing around internally instead of a 7-argument function
/// signature.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct ColorMatchParams {
    /// How wide a band (in plane UV space, from the seam-adjacent edge) to
    /// sample. Independent of `blend_width` - `blend_width` shapes the
    /// visual crossfade curve, but the physical camera overlap doesn't
    /// change with it, so this is a separate knob.
    pub(crate) band_width: f32,
    /// Sample grid size. More points = a more stable mean, at a (small,
    /// since `undistorted_to_distorted` is closed-form - no trig, no
    /// iteration) linear cost per measurement.
    pub(crate) grid_cols: u32,
    pub(crate) grid_rows: u32,
    /// Re-measure every N rendered frames. Exposure/white-balance drift is
    /// slow (cloud cover, sun angle) - no need to pay the sampling cost
    /// every frame.
    pub(crate) measure_interval_frames: u32,
    /// Exponential-moving-average smoothing factor applied to each new
    /// measurement, so a single noisy frame (e.g. a bright shirt passing
    /// through the band) doesn't snap the correction.
    pub(crate) ema_alpha: f32,
    /// Safety clamp on the correction magnitude, so a pathological
    /// measurement (band mostly out of FOV, band dominated by one
    /// saturated color) can't push the correction far enough to be
    /// visually worse than doing nothing.
    pub(crate) max_y_offset: f32,
    pub(crate) max_chroma_offset: f32,
    /// `ViewportConfig::seam_offset` - SYNC_WITH `shaders/fisheye.wgsl`'s
    /// `fs_main` alpha-blend threshold. The sampling band must track the
    /// same seam-adjacent edge the shader actually feathers, or a manual
    /// seam drag leaves this measuring scene content nowhere near the real
    /// seam (e.g. crowd/sky on one side, pitch on the other) and derives a
    /// correction that has nothing to do with the two cameras' actual
    /// exposure difference - see `measure_band_mean`.
    ///
    /// Only shifts the band for whichever plane is *currently fading* -
    /// `seam_offset` only ever moves the fading plane's alpha threshold
    /// (`fisheye.wgsl`'s `if u.ground_tilt.w > 0.5` gate); the other,
    /// fixed/opaque plane renders at alpha=1.0 unconditionally, so its
    /// on-screen boundary never moves with `seam_offset` at all. Which
    /// plane is fading depends on `blend_flip_direction`, hence that field
    /// below - see `seam_band_bounds`.
    pub(crate) seam_offset: f32,
    /// Manual per-camera gamma (`Topology::color_gamma_left`/`_right`).
    /// SYNC_WITH `fisheye.wgsl`'s `apply_gamma`: the measurement must
    /// sample the same curve the shader renders, or the offsets derived
    /// here describe a frame that is never displayed - the correction
    /// then fights the gamma slider instead of complementing it.
    pub(crate) gamma_left: f32,
    pub(crate) gamma_right: f32,
    /// `ViewportConfig::blend_flip_direction` - SYNC_WITH
    /// `renderer.rs`'s `left_uniforms.ground_tilt[3] = if flip {1.0} else
    /// {0.0}` (and the mirrored assignment for right). Needed to know
    /// which plane `seam_offset` actually applies to - see this struct's
    /// `seam_offset` doc.
    pub(crate) blend_flip_direction: bool,
}

impl Default for ColorMatchParams {
    fn default() -> Self {
        Self {
            band_width: 0.15,
            grid_cols: 8,
            grid_rows: 16,
            measure_interval_frames: 15,
            ema_alpha: 0.15,
            max_y_offset: 0.06,
            max_chroma_offset: 0.04,
            seam_offset: 0.0,
            blend_flip_direction: false,
            gamma_left: 1.0,
            gamma_right: 1.0,
        }
    }
}

/// Persistent per-pipeline state: the current smoothed correction plus the
/// countdown to the next re-measurement.
pub(crate) struct ColorMatchState {
    left_offset: [f32; 3],
    right_offset: [f32; 3],
    frames_since_measure: u32,
    /// Last known `measure_interval_frames`, so [`Self::measurement_due`]
    /// can be asked without threading the whole parameter block through
    /// the asynchronous path. Refreshed by `tick` and by the GPU path
    /// before it asks.
    interval: u32,
}

impl Default for ColorMatchState {
    fn default() -> Self {
        Self {
            left_offset: [0.0; 3],
            right_offset: [0.0; 3],
            // Due for a measurement on the very first frame regardless of
            // `measure_interval_frames`.
            frames_since_measure: u32::MAX - 1,
            interval: 1,
        }
    }
}

impl ColorMatchState {
    /// Force the next `update_yuv420p`/`update_nv12` call to re-measure
    /// immediately, bypassing `measure_interval_frames`. Called whenever a
    /// consumer GUI changes any `ColorMatchParams` field, so a slider drag
    /// is reflected on the very next rendered frame instead of waiting up
    /// to `measure_interval_frames` frames.
    pub(crate) fn force_remeasure(&mut self) {
        self.frames_since_measure = u32::MAX - 1;
    }

    /// Refresh the cached `measure_interval_frames` - the asynchronous
    /// path calls this before [`Self::measurement_due`], where `tick`
    /// would have done it inline.
    pub(crate) fn set_interval(&mut self, interval: u32) {
        self.interval = interval;
    }

    /// The current smoothed correction, without advancing or re-measuring.
    /// Lets a consumer GUI show a live readout of what's actually being
    /// applied - e.g. to confirm a parameter change is having any effect,
    /// or that the measurement isn't silently stuck at identity (which
    /// happens if every sample point maps outside the raw frame - see
    /// `measure_band_mean`'s `None` case).
    pub(crate) fn current(&self) -> ColorCorrection {
        ColorCorrection {
            left_offset: self.left_offset,
            right_offset: self.right_offset,
        }
    }

    /// Advance one frame, re-measuring and updating the smoothed correction
    /// every `params.measure_interval_frames` calls. Always returns the
    /// current (possibly stale-by-a-few-frames) smoothed correction.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn update_yuv420p(
        &mut self,
        left: (&[u8], &[u8], &[u8]),
        right: (&[u8], &[u8], &[u8]),
        width: u32,
        height: u32,
        left_params: &Lens,
        right_params: &Lens,
        is_full_range: bool,
        params: &ColorMatchParams,
    ) -> ColorCorrection {
        self.tick(params, || {
            let (ly, lu, lv) = left;
            let (ry, ru, rv) = right;
            let get_left = yuv420p_sampler(ly, lu, lv, width);
            let get_right = yuv420p_sampler(ry, ru, rv, width);
            (
                measure_band_mean(
                    width,
                    height,
                    left_params,
                    false,
                    is_full_range,
                    params,
                    get_left,
                ),
                measure_band_mean(
                    width,
                    height,
                    right_params,
                    true,
                    is_full_range,
                    params,
                    get_right,
                ),
            )
        })
    }

    /// NV12 counterpart to [`Self::update_yuv420p`].
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn update_nv12(
        &mut self,
        left: (&[u8], &[u8]),
        right: (&[u8], &[u8]),
        width: u32,
        height: u32,
        left_params: &Lens,
        right_params: &Lens,
        is_full_range: bool,
        params: &ColorMatchParams,
    ) -> ColorCorrection {
        self.tick(params, || {
            let (ly, luv) = left;
            let (ry, ruv) = right;
            let get_left = nv12_sampler(ly, luv, width);
            let get_right = nv12_sampler(ry, ruv, width);
            (
                measure_band_mean(
                    width,
                    height,
                    left_params,
                    false,
                    is_full_range,
                    params,
                    get_left,
                ),
                measure_band_mean(
                    width,
                    height,
                    right_params,
                    true,
                    is_full_range,
                    params,
                    get_right,
                ),
            )
        })
    }

    /// Whether a measurement is due this frame, advancing the counter.
    ///
    /// Split out of [`Self::tick`] so a measurement that cannot be
    /// produced synchronously - the GPU gather, whose result only arrives
    /// a frame or two later - can ask the same question the CPU path
    /// asks, and get the same answer under the same
    /// `measure_interval_frames` and `force_remeasure` rules.
    pub(crate) fn measurement_due(&mut self) -> bool {
        // `frames_since_measure` is reset by the caller once it has
        // actually issued a measurement, not here: an async path that
        // fails to issue one (no textures bound yet) must stay due.
        self.frames_since_measure += 1;
        self.frames_since_measure >= self.interval
    }

    /// Mark a measurement as issued this frame - resets the interval
    /// countdown. Separate from [`Self::measurement_due`] because the
    /// asynchronous path can decide not to issue after asking.
    pub(crate) fn measurement_issued(&mut self) {
        self.frames_since_measure = 0;
    }

    /// Fold a fresh pair of band means into the smoothed correction.
    /// Shared by the synchronous CPU sampler and the asynchronous GPU
    /// gather, so both get the identical target/clamp/EMA behaviour and
    /// the same diagnostic line.
    pub(crate) fn apply_measurement(
        &mut self,
        left_mean: [f32; 3],
        right_mean: [f32; 3],
        params: &ColorMatchParams,
    ) {
        let target = [
            (left_mean[0] + right_mean[0]) * 0.5,
            (left_mean[1] + right_mean[1]) * 0.5,
            (left_mean[2] + right_mean[2]) * 0.5,
        ];
        let new_left = clamp_offset(sub3(target, left_mean), params);
        let new_right = clamp_offset(sub3(target, right_mean), params);
        ema_toward(&mut self.left_offset, new_left, params.ema_alpha);
        ema_toward(&mut self.right_offset, new_right, params.ema_alpha);
        log::debug!(
            "color_match measured: left_mean={left_mean:?} right_mean={right_mean:?} \
             target={target:?} new_left={new_left:?} new_right={new_right:?} \
             smoothed_left={:?} smoothed_right={:?}",
            self.left_offset,
            self.right_offset,
        );
    }

    fn tick(
        &mut self,
        params: &ColorMatchParams,
        measure: impl FnOnce() -> (Option<[f32; 3]>, Option<[f32; 3]>),
    ) -> ColorCorrection {
        self.interval = params.measure_interval_frames;
        if self.measurement_due() {
            self.measurement_issued();
            if let (Some(left_mean), Some(right_mean)) = measure() {
                self.apply_measurement(left_mean, right_mean, params);
            }
            // If measurement fails (band entirely out of FOV), keep
            // serving the last smoothed correction rather than snapping to
            // identity.
        }
        ColorCorrection {
            left_offset: self.left_offset,
            right_offset: self.right_offset,
        }
    }
}

/// Whether `is_right`'s plane is the one `renderer.rs` currently designates
/// as fading (`ground_tilt[3] = 1.0`) rather than fixed/opaque
/// (`ground_tilt[3] = 0.0`) - SYNC_WITH `left_uniforms.ground_tilt[3] = if
/// flip {1.0} else {0.0}` (and the mirrored right assignment). Default
/// (`blend_flip_direction = false`): right fades, left is fixed.
fn is_fading_plane(is_right: bool, blend_flip_direction: bool) -> bool {
    is_right != blend_flip_direction
}

/// Where, in plane UV space, the seam-adjacent measurement band sits for one
/// camera - the same edge and `seam_offset` shift `fisheye.wgsl`'s `fs_main`
/// uses for its alpha threshold (`u.flags.x == 1u` branch for `is_right`).
/// `seam_offset` must already be zeroed by the caller when this plane isn't
/// the fading one (see `is_fading_plane`) - the fixed/opaque plane's alpha
/// is hardcoded to 1.0 regardless of `seam_offset`, so its band must stay at
/// the unshifted edge. Kept separate from `measure_band_mean` so the bounds
/// math is unit-testable without needing synthetic distorted frames.
fn seam_band_bounds(is_right: bool, seam_offset: f64, band_width: f64) -> (f64, f64) {
    let (u0, u1) = if is_right {
        (seam_offset, seam_offset + band_width)
    } else {
        (1.0 - seam_offset - band_width, 1.0 - seam_offset)
    };
    (u0.clamp(0.0, 1.0), u1.clamp(0.0, 1.0))
}

fn sub3(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

fn clamp_offset(o: [f32; 3], params: &ColorMatchParams) -> [f32; 3] {
    [
        o[0].clamp(-params.max_y_offset, params.max_y_offset),
        o[1].clamp(-params.max_chroma_offset, params.max_chroma_offset),
        o[2].clamp(-params.max_chroma_offset, params.max_chroma_offset),
    ]
}

fn ema_toward(current: &mut [f32; 3], target: [f32; 3], alpha: f32) {
    for (c, t) in current.iter_mut().zip(target) {
        *c += alpha * (t - *c);
    }
}

/// Build a raw-byte sampler closure for tightly-packed YUV420P planes.
fn yuv420p_sampler<'a>(
    y: &'a [u8],
    u: &'a [u8],
    v: &'a [u8],
    width: u32,
) -> impl Fn(u32, u32) -> Option<(u8, u8, u8)> + 'a {
    let chroma_w = width / 2;
    move |x: u32, y_coord: u32| {
        let y_idx = (y_coord as usize) * (width as usize) + x as usize;
        let c_idx = ((y_coord / 2) as usize) * (chroma_w as usize) + (x / 2) as usize;
        Some((*y.get(y_idx)?, *u.get(c_idx)?, *v.get(c_idx)?))
    }
}

/// Build a raw-byte sampler closure for tightly-packed NV12 planes
/// (interleaved U,V at half resolution, `width` bytes per chroma row).
fn nv12_sampler<'a>(
    y: &'a [u8],
    uv: &'a [u8],
    width: u32,
) -> impl Fn(u32, u32) -> Option<(u8, u8, u8)> + 'a {
    move |x: u32, y_coord: u32| {
        let y_idx = (y_coord as usize) * (width as usize) + x as usize;
        let c_idx = ((y_coord / 2) as usize) * (width as usize) + ((x / 2) * 2) as usize;
        Some((*y.get(y_idx)?, *uv.get(c_idx)?, *uv.get(c_idx + 1)?))
    }
}

/// Texel positions, in the camera's *raw* (still distorted) frame, of the
/// coarse grid this module samples in the seam-adjacent band.
///
/// Extracted from [`measure_band_mean`] so the GPU gather can sample the
/// exact same points without duplicating the band geometry - the part
/// that has been got wrong twice already (see this crate's FRICTION.md on
/// `seam_offset` and `blend_flip_direction`). Positions depend only on
/// the lens and the band parameters, never on pixel content, so a caller
/// that uploads them to the GPU can cache them until the calibration
/// changes.
///
/// **Ground half only** (`v_frac` in `(0.5, 1.0]`, never `[0.0, 0.5]`) -
/// matches the sky/ground split `ground_tilt` already uses (`plane_y > 0`
/// in `fisheye.wgsl`'s `fs_main`; `v_frac` here is that same plane-local
/// `uv.y`). A seam-adjacent band that also samples the sky half measures
/// the wrong thing for two compounding reasons: sky content is optically
/// irrelevant to pitch/seam continuity, and on a tilted rig the two
/// cameras' skies can differ in brightness in the *opposite* direction
/// from their ground - averaging both into one mean lets a real,
/// visible ground-level exposure mismatch cancel against an unrelated
/// sky difference instead of being measured. Confirmed on a real 26.5deg
/// rig, real footage: the full-height band measured left/right as
/// near-identical (a visible seam went uncorrected) while the same band
/// restricted to `v_frac > 0.5` recovered most of the real, visually
/// obvious gap. See FRICTION.md's color-match band-geometry entry for
/// the full investigation (including why this - not a narrower
/// `band_width` - is the fix: near the seam-adjacent edge the KB4
/// corner-FOV coverage gap makes most of the *middle* of `v_frac` map
/// outside the raw frame regardless of `band_width`, splitting the
/// in-bounds points into a sky cluster and a ground cluster no matter
/// how the horizontal band is sized).
///
/// Points that map outside the raw frame are dropped rather than clamped:
/// a clamped point would silently feed the frame's edge pixel into the
/// mean as though it were band content.
pub(crate) fn band_sample_positions(
    width: u32,
    height: u32,
    params: &Lens,
    is_right: bool,
    match_params: &ColorMatchParams,
) -> Vec<[u32; 2]> {
    let is_fading = is_fading_plane(is_right, match_params.blend_flip_direction);
    let seam_offset = if is_fading {
        match_params.seam_offset as f64
    } else {
        0.0
    };
    let (u0, u1) = seam_band_bounds(is_right, seam_offset, match_params.band_width as f64);
    let grid_cols = match_params.grid_cols.max(1);
    let grid_rows = match_params.grid_rows.max(1);

    let mut out = Vec::with_capacity((grid_cols * grid_rows) as usize);
    for row in 0..grid_rows {
        // Ground half only - see this function's doc comment.
        let v_frac = 0.5 + (row as f64 + 0.5) / grid_rows as f64 * 0.5;
        for col in 0..grid_cols {
            let t = (col as f64 + 0.5) / grid_cols as f64;
            let u_frac = u0 + t * (u1 - u0);

            let (src_x, src_y) = undistorted_to_distorted(
                u_frac * width as f64,
                v_frac * height as f64,
                width,
                height,
                params,
            );
            if src_x < 0.0
                || src_y < 0.0
                || src_x >= (width - 1) as f64
                || src_y >= (height - 1) as f64
            {
                continue;
            }
            out.push([src_x as u32, src_y as u32]);
        }
    }
    out
}

/// Band mean from samples that were read somewhere else - the GPU gather
/// hands back normalized `(Y, U, V)` triples straight out of the texture,
/// and this runs the identical decode/gamma/average the CPU sampler runs
/// on its own bytes. Deliberately *not* a second implementation: the
/// whole point of the gather returning raw texels is that the color maths
/// stays defined once.
///
/// `None` when there is nothing to average, mirroring
/// [`measure_band_mean`]'s empty-band case.
pub(crate) fn mean_of_normalized_samples(
    samples: &[[f32; 4]],
    is_full_range: bool,
    gamma: f32,
) -> Option<[f32; 3]> {
    if samples.is_empty() {
        return None;
    }
    let inv_g = inv_gamma(gamma);
    let mut sum = [0f64; 3];
    for s in samples {
        let yuv = decode_transfer_yuv_normalized(s[0], s[1], s[2], is_full_range, inv_g);
        sum[0] += yuv[0] as f64;
        sum[1] += yuv[1] as f64;
        sum[2] += yuv[2] as f64;
    }
    let n = samples.len() as f64;
    Some([
        (sum[0] / n) as f32,
        (sum[1] / n) as f32,
        (sum[2] / n) as f32,
    ])
}

/// Mean color, in the shader's color-transfer YUV space, over a coarse grid
/// of points in one camera's seam-adjacent band.
///
/// `is_right` selects which edge of plane UV space the band sits against -
/// mirrors the alpha-blend convention documented on `fisheye.wgsl`'s
/// `fs_main` (right plane's seam-adjacent edge is at UV x=0, left plane's is
/// at UV x=1). Only shifted by `match_params.seam_offset` when this plane is
/// the one currently designated as fading (see `seam_band_bounds`) -
/// otherwise a manual seam drag decouples this band from the seam it's
/// meant to measure, or (for the fixed/opaque plane) shifts it away from
/// its actual, seam-offset-independent on-screen boundary. Returns `None`
/// if every sample point mapped outside the raw frame (band too close to
/// the edge of the lens' FOV, or the shift pushed it out of `[0, 1]`
/// entirely).
#[allow(clippy::too_many_arguments)]
fn measure_band_mean(
    width: u32,
    height: u32,
    params: &Lens,
    is_right: bool,
    is_full_range: bool,
    match_params: &ColorMatchParams,
    sample: impl Fn(u32, u32) -> Option<(u8, u8, u8)>,
) -> Option<[f32; 3]> {
    // This camera's manual gamma, applied per sample point *before* the
    // mean is taken - the shader applies it per pixel, and pow is not
    // linear, so gamma-ing the finished mean instead would measure a
    // different image than the one on screen.
    let inv_g = inv_gamma(if is_right {
        match_params.gamma_right
    } else {
        match_params.gamma_left
    });
    let mut sum = [0f64; 3];
    let mut count = 0u32;
    for [src_x, src_y] in band_sample_positions(width, height, params, is_right, match_params) {
        let Some((y_raw, u_raw, v_raw)) = sample(src_x, src_y) else {
            continue;
        };
        let yuv = decode_transfer_yuv(y_raw, u_raw, v_raw, is_full_range, inv_g);
        sum[0] += yuv[0] as f64;
        sum[1] += yuv[1] as f64;
        sum[2] += yuv[2] as f64;
        count += 1;
    }

    if count == 0 {
        return None;
    }
    Some([
        (sum[0] / count as f64) as f32,
        (sum[1] / count as f64) as f32,
        (sum[2] / count as f64) as f32,
    ])
}

/// Decode a raw YCbCr byte triple into the shader's color-transfer YUV
/// space: BT.709 YCbCr -> RGB range expansion (`sample_yuv`), then BT.709
/// full-range RGB -> YUV (`rgb_to_yuv`).
///
/// SYNC_WITH `shaders/fisheye.wgsl`'s `sample_yuv` and `rgb_to_yuv`. This
/// has to land in the exact same space `apply_color_transfer` applies the
/// offset in, or the measured correction will be systematically wrong.
fn decode_transfer_yuv(
    y_raw: u8,
    u_raw: u8,
    v_raw: u8,
    is_full_range: bool,
    inv_gamma: f32,
) -> [f32; 3] {
    decode_transfer_yuv_normalized(
        y_raw as f32 / 255.0,
        u_raw as f32 / 255.0,
        v_raw as f32 / 255.0,
        is_full_range,
        inv_gamma,
    )
}

/// The decode itself, on already-normalized `[0, 1]` channel values.
///
/// A GPU `textureLoad` of a Unorm plane hands back exactly this form, so
/// the gather path enters here and the byte path enters through the
/// wrapper above. A 10-bit (P010) plane read as R16Unorm normalizes
/// against 65535 rather than 1023<<6, a 0.1% scale error that is far
/// below the measurement's own noise and does not warrant a second code
/// path.
fn decode_transfer_yuv_normalized(
    y_n: f32,
    u_n: f32,
    v_n: f32,
    is_full_range: bool,
    inv_gamma: f32,
) -> [f32; 3] {
    let (y, cb, cr) = if is_full_range {
        (y_n, u_n - 0.5, v_n - 0.5)
    } else {
        (
            (y_n - 16.0 / 255.0) * (255.0 / 219.0),
            (u_n - 128.0 / 255.0) * (255.0 / 224.0),
            (v_n - 128.0 / 255.0) * (255.0 / 224.0),
        )
    };

    let r = (y + 1.5748 * cr).clamp(0.0, 1.0);
    let g = (y - 0.1873 * cb - 0.4681 * cr).clamp(0.0, 1.0);
    let b = (y + 1.8556 * cb).clamp(0.0, 1.0);

    // SYNC_WITH `fisheye.wgsl`'s `apply_gamma`: same curve, same place in
    // the chain (on RGB, before the YUV transfer). Identity short-circuits
    // for the same reason it does there.
    let (r, g, b) = if inv_gamma == 1.0 {
        (r, g, b)
    } else {
        (r.powf(inv_gamma), g.powf(inv_gamma), b.powf(inv_gamma))
    };

    [
        0.2126 * r + 0.7152 * g + 0.0722 * b,
        -0.1146 * r - 0.3854 * g + 0.5 * b,
        0.5 * r - 0.4542 * g - 0.0458 * b,
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_params() -> Lens {
        // GoPro HERO10-ish 4K KB4 coefficients, same as other reco-core tests.
        Lens {
            width: 640,
            height: 480,
            fx: 320.0,
            fy: 320.0,
            cx: 320.0,
            cy: 240.0,
            distortion: [0.0342, 0.0677, -0.0741, 0.0299],
            correction: 1.0,
        }
    }

    fn solid_yuv420p(width: u32, height: u32, y: u8, u: u8, v: u8) -> (Vec<u8>, Vec<u8>, Vec<u8>) {
        let y_plane = vec![y; (width * height) as usize];
        let c_plane = vec![u; ((width / 2) * (height / 2)) as usize];
        let v_plane = vec![v; ((width / 2) * (height / 2)) as usize];
        (y_plane, c_plane, v_plane)
    }

    #[test]
    fn measure_band_mean_solid_color_matches_decode() {
        let params = test_params();
        let (y, u, v) = solid_yuv420p(640, 480, 180, 140, 120);
        let sampler = yuv420p_sampler(&y, &u, &v, 640);

        let match_params = ColorMatchParams::default();
        let mean = measure_band_mean(640, 480, &params, true, false, &match_params, sampler)
            .expect("solid band should yield samples");

        let expected = decode_transfer_yuv(180, 140, 120, false, 1.0);
        for i in 0..3 {
            assert!(
                (mean[i] - expected[i]).abs() < 1e-4,
                "channel {i}: {mean:?} vs {expected:?}"
            );
        }
    }

    /// Y plane split top/bottom at `height / 2` (chroma flat, uninteresting
    /// for this test) - top half `sky_y`, bottom half `ground_y`.
    fn sky_ground_split_yuv420p(
        width: u32,
        height: u32,
        sky_y: u8,
        ground_y: u8,
    ) -> (Vec<u8>, Vec<u8>, Vec<u8>) {
        let mut y_plane = vec![0u8; (width * height) as usize];
        for row in 0..height {
            let v = if row < height / 2 { sky_y } else { ground_y };
            let start = (row * width) as usize;
            y_plane[start..start + width as usize].fill(v);
        }
        let c_plane = vec![128u8; ((width / 2) * (height / 2)) as usize];
        let v_plane = vec![128u8; ((width / 2) * (height / 2)) as usize];
        (y_plane, c_plane, v_plane)
    }

    #[test]
    fn band_sample_positions_ignores_the_sky_half() {
        // A wildly different sky half (Y=0, black) must not move the
        // measured mean at all - the band is ground-half only. If the
        // v-range restriction regressed back to spanning the whole plane
        // (v_frac 0..1), this solid-black sky would visibly darken the
        // result below `ground_y`'s own decoded value.
        let params = test_params();
        let (y, u, v) = sky_ground_split_yuv420p(640, 480, 0, 200);
        let sampler = yuv420p_sampler(&y, &u, &v, 640);

        let match_params = ColorMatchParams::default();
        let mean = measure_band_mean(640, 480, &params, true, false, &match_params, sampler)
            .expect("ground half should yield samples");

        let expected_ground = decode_transfer_yuv(200, 128, 128, false, 1.0);
        assert!(
            (mean[0] - expected_ground[0]).abs() < 1e-3,
            "measured {mean:?} should match the ground half {expected_ground:?} alone, \
             not be pulled toward the black sky half"
        );
    }

    #[test]
    fn manual_gamma_lifts_the_measured_band_mean() {
        // The measurement has to see the gamma the shader renders. A
        // gamma above 1.0 lifts mid-tones, so the measured luma of a
        // mid-grey band must rise; if the exponent were ignored (or
        // applied in the wrong direction) this is what would catch it.
        let params = test_params();
        let (y, u, v) = solid_yuv420p(640, 480, 128, 128, 128);

        let plain = ColorMatchParams {
            gamma_left: 1.0,
            ..Default::default()
        };
        let flat = measure_band_mean(640, 480, &params, false, false, &plain, {
            yuv420p_sampler(&y, &u, &v, 640)
        })
        .expect("solid band should yield samples");

        let lifted_params = ColorMatchParams {
            gamma_left: 2.0,
            ..Default::default()
        };
        let lifted = measure_band_mean(640, 480, &params, false, false, &lifted_params, {
            yuv420p_sampler(&y, &u, &v, 640)
        })
        .expect("solid band should yield samples");

        assert!(
            lifted[0] > flat[0] + 0.05,
            "gamma 2.0 should visibly lift mid-grey luma: {} -> {}",
            flat[0],
            lifted[0]
        );
    }

    #[test]
    fn manual_gamma_is_per_camera() {
        // Two identical cameras, gamma on one side only: the automatic
        // stage must now see a real difference and correct for it. This
        // is the whole point of the ordering - gamma first, offsets
        // measured on the gamma'd pixels.
        let params = test_params();
        let (y, u, v) = solid_yuv420p(640, 480, 128, 128, 128);
        let mut state = ColorMatchState::default();
        let match_params = ColorMatchParams {
            gamma_left: 2.0,
            ..Default::default()
        };

        let correction = state.update_yuv420p(
            (&y, &u, &v),
            (&y, &u, &v),
            640,
            480,
            &params,
            &params,
            false,
            &match_params,
        );

        // Left was brightened, so it gets pushed back down and right gets
        // pulled up by the same amount (both aim at the shared mean).
        assert!(
            correction.left_offset[0] < -1e-4,
            "left (gamma-lifted) should be corrected downwards: {:?}",
            correction.left_offset
        );
        assert!(
            correction.right_offset[0] > 1e-4,
            "right (untouched) should be corrected upwards: {:?}",
            correction.right_offset
        );
    }

    #[test]
    fn a_broken_gamma_value_falls_back_to_identity() {
        // Topology is a user-editable file. Zero, negative or NaN would
        // make `pow` produce a flat or NaN frame everywhere, so they must
        // land on identity instead.
        assert_eq!(inv_gamma(1.0), 1.0);
        assert_eq!(inv_gamma(0.0), 1.0);
        assert_eq!(inv_gamma(-2.0), 1.0);
        assert_eq!(inv_gamma(f32::NAN), 1.0);
        assert_eq!(inv_gamma(f32::INFINITY), 1.0);
        assert!((inv_gamma(2.0) - 0.5).abs() < 1e-6);
    }

    #[test]
    fn identical_cameras_converge_to_zero_offset() {
        let params = test_params();
        let (y, u, v) = solid_yuv420p(640, 480, 180, 128, 128);
        let mut state = ColorMatchState::default();
        let match_params = ColorMatchParams::default();

        let correction = state.update_yuv420p(
            (&y, &u, &v),
            (&y, &u, &v),
            640,
            480,
            &params,
            &params,
            false,
            &match_params,
        );

        for i in 0..3 {
            assert!(
                correction.left_offset[i].abs() < 1e-5,
                "left offset should stay ~0 when both cameras match: {:?}",
                correction.left_offset
            );
            assert!(
                correction.right_offset[i].abs() < 1e-5,
                "right offset should stay ~0 when both cameras match: {:?}",
                correction.right_offset
            );
        }
    }

    #[test]
    fn mismatched_cameras_converge_toward_shared_mean() {
        let params = test_params();
        // Left is darker (Y=100), right is brighter (Y=200); same chroma.
        let (ly, lu, lv) = solid_yuv420p(640, 480, 100, 128, 128);
        let (ry, ru, rv) = solid_yuv420p(640, 480, 200, 128, 128);
        let mut state = ColorMatchState::default();
        let match_params = ColorMatchParams::default();

        let mut correction = ColorCorrection::default();
        // Run several measurement cycles so the EMA settles.
        for _ in 0..50 {
            correction = state.update_yuv420p(
                (&ly, &lu, &lv),
                (&ry, &ru, &rv),
                640,
                480,
                &params,
                &params,
                false,
                &match_params,
            );
        }

        // Left (darker) should get a positive Y offset; right (brighter) a
        // negative one, moving both toward the shared mean.
        assert!(
            correction.left_offset[0] > 0.0,
            "left (darker) camera should get a brightening offset: {}",
            correction.left_offset[0]
        );
        assert!(
            correction.right_offset[0] < 0.0,
            "right (brighter) camera should get a darkening offset: {}",
            correction.right_offset[0]
        );
        // Safety clamp must hold even after many iterations.
        assert!(correction.left_offset[0] <= match_params.max_y_offset + 1e-6);
        assert!(correction.right_offset[0] >= -match_params.max_y_offset - 1e-6);
    }

    #[test]
    fn is_fading_plane_matches_renderer_convention() {
        // Default (blend_flip_direction = false): right fades, left is
        // fixed - matches `left_uniforms.ground_tilt[3] = if flip {1.0}
        // else {0.0}` in renderer.rs.
        assert!(is_fading_plane(true, false));
        assert!(!is_fading_plane(false, false));
        // Flipped: left fades, right is fixed - the reported bug's exact
        // scenario (`blend_flip_direction: true` in the user's
        // calibration), where the *right* plane's band must NOT shift by
        // seam_offset since right's alpha is hardcoded to 1.0 here.
        assert!(is_fading_plane(false, true));
        assert!(!is_fading_plane(true, true));
    }

    #[test]
    fn seam_band_bounds_tracks_seam_offset() {
        // No offset: matches the pre-`seam_offset` fixed-edge behavior.
        assert_eq!(seam_band_bounds(true, 0.0, 0.15), (0.0, 0.15));
        assert_eq!(seam_band_bounds(false, 0.0, 0.15), (0.85, 1.0));

        // A positive seam_offset must shift the band by exactly that much,
        // same as the shader's alpha threshold - this is the bug: before
        // this fix, the band never moved and could end up sampling content
        // nowhere near the actual (manually repositioned) seam.
        assert_eq!(seam_band_bounds(true, 0.1, 0.15), (0.1, 0.25));
        assert_eq!(seam_band_bounds(false, 0.1, 0.15), (0.75, 0.9));

        // seam_offset larger than band_width (the user's reported scenario)
        // must still land the band adjacent to the real seam, not clipped
        // back to the stale unshifted location.
        assert_eq!(seam_band_bounds(true, 0.2, 0.15), (0.2, 0.35));
    }

    #[test]
    fn seam_band_bounds_clamps_to_valid_uv_range() {
        // Large negative offset pushes the band fully out of [0, 1] - both
        // bounds clamp to 0.0 rather than going negative into undefined
        // distortion-mapping territory.
        assert_eq!(seam_band_bounds(true, -0.3, 0.15), (0.0, 0.0));
        // Large positive offset on the left edge clamps the top to 1.0.
        assert_eq!(seam_band_bounds(false, -0.3, 0.15), (1.0, 1.0));
    }

    #[test]
    fn measure_band_mean_returns_none_when_all_samples_out_of_bounds() {
        // Degenerate camera: zero focal length maps every ray to the same
        // undefined point: use a params set whose band maps entirely off
        // the (tiny) frame instead - a 2x2 frame with normal intrinsics has
        // almost all its KB4-mapped band fall outside bounds.
        let params = Lens {
            width: 2,
            height: 2,
            fx: 320.0,
            fy: 320.0,
            cx: 320.0,
            cy: 240.0,
            distortion: [0.0342, 0.0677, -0.0741, 0.0299],
            correction: 1.0,
        };
        let y = vec![0u8; 4];
        let u = vec![0u8; 1];
        let v = vec![0u8; 1];
        let sampler = yuv420p_sampler(&y, &u, &v, 2);
        let match_params = ColorMatchParams::default();

        let mean = measure_band_mean(2, 2, &params, true, false, &match_params, sampler);
        assert!(mean.is_none());
    }

    #[test]
    fn force_remeasure_applies_new_params_on_the_very_next_call() {
        // A large measure_interval_frames would normally mean many calls
        // pass before a param change is reflected - force_remeasure must
        // bypass that so a GUI slider drag shows up on the next frame.
        let params = test_params();
        let (ly, lu, lv) = solid_yuv420p(640, 480, 100, 128, 128);
        let (ry, ru, rv) = solid_yuv420p(640, 480, 200, 128, 128);
        let mut state = ColorMatchState::default();
        let mut match_params = ColorMatchParams {
            measure_interval_frames: 10_000,
            ..ColorMatchParams::default()
        };

        // First call measures immediately (fresh state is always due).
        let first = state.update_yuv420p(
            (&ly, &lu, &lv),
            (&ry, &ru, &rv),
            640,
            480,
            &params,
            &params,
            false,
            &match_params,
        );
        assert!(first.left_offset[0] > 0.0);

        // Tighten the clamp and force an immediate re-measure.
        match_params.max_y_offset = 0.01;
        state.force_remeasure();
        let second = state.update_yuv420p(
            (&ly, &lu, &lv),
            (&ry, &ru, &rv),
            640,
            480,
            &params,
            &params,
            false,
            &match_params,
        );

        assert!(
            second.left_offset[0] <= 0.01 + 1e-6,
            "tightened clamp should apply on the very next call: {}",
            second.left_offset[0]
        );
    }
}
