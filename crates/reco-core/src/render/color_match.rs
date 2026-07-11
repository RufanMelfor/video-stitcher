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
use crate::calibration::CameraParams;
use crate::lens::undistorted_to_distorted;

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
        }
    }
}

/// Persistent per-pipeline state: the current smoothed correction plus the
/// countdown to the next re-measurement.
pub(crate) struct ColorMatchState {
    left_offset: [f32; 3],
    right_offset: [f32; 3],
    frames_since_measure: u32,
}

impl Default for ColorMatchState {
    fn default() -> Self {
        Self {
            left_offset: [0.0; 3],
            right_offset: [0.0; 3],
            // Due for a measurement on the very first frame regardless of
            // `measure_interval_frames`.
            frames_since_measure: u32::MAX - 1,
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
        left_params: &CameraParams,
        right_params: &CameraParams,
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
        left_params: &CameraParams,
        right_params: &CameraParams,
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

    fn tick(
        &mut self,
        params: &ColorMatchParams,
        measure: impl FnOnce() -> (Option<[f32; 3]>, Option<[f32; 3]>),
    ) -> ColorCorrection {
        self.frames_since_measure += 1;
        if self.frames_since_measure >= params.measure_interval_frames {
            self.frames_since_measure = 0;
            if let (Some(left_mean), Some(right_mean)) = measure() {
                let target = [
                    (left_mean[0] + right_mean[0]) * 0.5,
                    (left_mean[1] + right_mean[1]) * 0.5,
                    (left_mean[2] + right_mean[2]) * 0.5,
                ];
                let new_left = clamp_offset(sub3(target, left_mean), params);
                let new_right = clamp_offset(sub3(target, right_mean), params);
                ema_toward(&mut self.left_offset, new_left, params.ema_alpha);
                ema_toward(&mut self.right_offset, new_right, params.ema_alpha);
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

/// Mean color, in the shader's color-transfer YUV space, over a coarse grid
/// of points in one camera's seam-adjacent band.
///
/// `is_right` selects which edge of plane UV space the band sits against -
/// mirrors the alpha-blend convention documented on `fisheye.wgsl`'s
/// `fs_main` (right plane's seam-adjacent edge is at UV x=0, left plane's is
/// at UV x=1). Returns `None` if every sample point mapped outside the raw
/// frame (band too close to the edge of the lens' FOV).
fn measure_band_mean(
    width: u32,
    height: u32,
    params: &CameraParams,
    is_right: bool,
    is_full_range: bool,
    match_params: &ColorMatchParams,
    sample: impl Fn(u32, u32) -> Option<(u8, u8, u8)>,
) -> Option<[f32; 3]> {
    let band_width = match_params.band_width as f64;
    let (u0, u1) = if is_right {
        (0.0, band_width)
    } else {
        (1.0 - band_width, 1.0)
    };
    let grid_cols = match_params.grid_cols.max(1);
    let grid_rows = match_params.grid_rows.max(1);

    let mut sum = [0f64; 3];
    let mut count = 0u32;
    for row in 0..grid_rows {
        let v_frac = (row as f64 + 0.5) / grid_rows as f64;
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
            let Some((y_raw, u_raw, v_raw)) = sample(src_x as u32, src_y as u32) else {
                continue;
            };
            let yuv = decode_transfer_yuv(y_raw, u_raw, v_raw, is_full_range);
            sum[0] += yuv[0] as f64;
            sum[1] += yuv[1] as f64;
            sum[2] += yuv[2] as f64;
            count += 1;
        }
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
fn decode_transfer_yuv(y_raw: u8, u_raw: u8, v_raw: u8, is_full_range: bool) -> [f32; 3] {
    let y_n = y_raw as f32 / 255.0;
    let u_n = u_raw as f32 / 255.0;
    let v_n = v_raw as f32 / 255.0;

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

    [
        0.2126 * r + 0.7152 * g + 0.0722 * b,
        -0.1146 * r - 0.3854 * g + 0.5 * b,
        0.5 * r - 0.4542 * g - 0.0458 * b,
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_params() -> CameraParams {
        // GoPro HERO10-ish 4K KB4 coefficients, same as other reco-core tests.
        CameraParams {
            width: 640,
            height: 480,
            fx: 320.0,
            fy: 320.0,
            cx: 320.0,
            cy: 240.0,
            d: [0.0342, 0.0677, -0.0741, 0.0299],
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

        let expected = decode_transfer_yuv(180, 140, 120, false);
        for i in 0..3 {
            assert!(
                (mean[i] - expected[i]).abs() < 1e-4,
                "channel {i}: {mean:?} vs {expected:?}"
            );
        }
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
    fn measure_band_mean_returns_none_when_all_samples_out_of_bounds() {
        // Degenerate camera: zero focal length maps every ray to the same
        // undefined point: use a params set whose band maps entirely off
        // the (tiny) frame instead - a 2x2 frame with normal intrinsics has
        // almost all its KB4-mapped band fall outside bounds.
        let params = CameraParams {
            width: 2,
            height: 2,
            fx: 320.0,
            fy: 320.0,
            cx: 320.0,
            cy: 240.0,
            d: [0.0342, 0.0677, -0.0741, 0.0299],
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
