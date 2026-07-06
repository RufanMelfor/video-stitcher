//! Row-varying local disparity correction - a spatially-varying
//! alternative to every global-parameter approach tried in
//! `examples/fit_photometric*.rs` (see `FRICTION.md` points 12-16).
//!
//! Every photometric experiment that searched for ONE set of global
//! parameters (placement, intrinsics, distortion) to fix the near-field
//! seam hit the same wall: real parallax means the correct correction at
//! the near field genuinely differs from the correct correction at the
//! far field, so any single global answer that helps one region hurts
//! another. This module instead measures the real vertical disparity
//! between the two cameras' rendered contributions *per horizontal band*
//! of the overlap region, and fits a smooth profile that varies by row -
//! so the near field and far field can each get the correction they
//! actually need, instead of being forced to share one number.
//!
//! Pipeline: [`measure_row_bands`] (per-frame, per-band 1D vertical
//! disparity via small-window ZNCC search) -> [`combine_across_frames`]
//! (reject bands whose measurement is unstable across frames - the
//! signature of a moving object, not real static geometry) ->
//! [`RowProfile::fit`] (smooth, band-limited interpolation across rows) ->
//! [`RowProfile::apply_vertical_shift`] (resample an image by the fitted
//! per-row vertical shift).
//!
//! Deliberately 1D (vertical shift only, not a full 2D field): every
//! visible seam defect observed in this investigation was a vertical
//! step in an otherwise-horizontal line (goal lines, sidelines, the
//! center-circle arc) - never a horizontal offset - so this is the
//! simplest correction that targets the actual observed failure mode.
//! A full 2D per-block field (closer to the thin-plate-spline elastic
//! warping literature) is a natural next step if this 1D version proves
//! out, not attempted here.

use crate::photometric::zncc;

/// One band's measured vertical disparity, in pixels: how far DOWN
/// (positive) or UP (negative) the right image's content must move to
/// align with the left image's content in this band.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RowBandDisparity {
    /// Row (pixel y) at the vertical center of this band.
    pub row_center: f32,
    /// Fitted vertical shift, in pixels.
    pub dy: f32,
    /// ZNCC score at the best shift - higher is a more confident match.
    pub confidence: f32,
}

/// Measure per-band vertical disparity between two single-camera luma
/// buffers, restricted to rows covered by `mask`.
///
/// Tiles the image into `band_height`-tall horizontal bands. For each
/// band with enough overlap coverage, searches `dy` in
/// `-max_dy..=max_dy` (pixels) for the shift that maximizes ZNCC between
/// the left band and the vertically-shifted right band, requiring both
/// the unshifted band and the compared region to lie within the overlap
/// mask. Returns `None` for a band that has too little overlap coverage,
/// or whose best-match ZNCC is below `min_confidence` (ambiguous/
/// textureless - not trustworthy).
pub fn measure_row_bands(
    left_luma: &[f32],
    right_luma: &[f32],
    mask: &crate::photometric::OverlapMask,
    band_height: u32,
    max_dy: i32,
    min_variance: f32,
    min_confidence: f32,
) -> Vec<Option<RowBandDisparity>> {
    let width = mask.width;
    let height = mask.height;
    assert_eq!(left_luma.len(), (width * height) as usize);
    assert_eq!(right_luma.len(), (width * height) as usize);

    let mut bands = Vec::new();
    let mut y0 = 0u32;
    while y0 < height {
        let bh = band_height.min(height - y0);
        bands.push(measure_one_band(
            left_luma,
            right_luma,
            mask,
            width,
            height,
            y0,
            bh,
            max_dy,
            min_variance,
            min_confidence,
        ));
        y0 += band_height;
    }
    bands
}

#[allow(clippy::too_many_arguments)]
fn measure_one_band(
    left_luma: &[f32],
    right_luma: &[f32],
    mask: &crate::photometric::OverlapMask,
    width: u32,
    height: u32,
    y0: u32,
    bh: u32,
    max_dy: i32,
    min_variance: f32,
    min_confidence: f32,
) -> Option<RowBandDisparity> {
    // Only use columns where the whole band (all rows y0..y0+bh) is
    // inside the overlap mask - keeps the compared region rectangular
    // and simple, at the cost of shrinking to the band's narrowest row.
    let mut cols: Vec<u32> = Vec::new();
    'col: for x in 0..width {
        for y in y0..(y0 + bh) {
            if !mask.mask[(y * width + x) as usize] {
                continue 'col;
            }
        }
        cols.push(x);
    }
    // Require a reasonable minimum width to keep the ZNCC statistically
    // meaningful (an arbitrary but generous floor).
    if cols.len() < 32 {
        return None;
    }

    let extract = |luma: &[f32], y0: i64| -> Option<Vec<f32>> {
        if y0 < 0 || y0 + bh as i64 > height as i64 {
            return None;
        }
        let mut out = Vec::with_capacity(cols.len() * bh as usize);
        for y in y0..(y0 + bh as i64) {
            for &x in &cols {
                out.push(luma[(y as u32 * width + x) as usize]);
            }
        }
        Some(out)
    };

    let left_band = extract(left_luma, y0 as i64)?;

    let mut best: Option<(i32, f32)> = None;
    for dy in -max_dy..=max_dy {
        let Some(right_band) = extract(right_luma, y0 as i64 + dy as i64) else {
            continue;
        };
        if let Some(score) = zncc(&left_band, &right_band, min_variance)
            && best.is_none_or(|(_, best_score)| score > best_score)
        {
            best = Some((dy, score));
        }
    }

    let (dy, confidence) = best?;
    if confidence < min_confidence {
        return None;
    }
    Some(RowBandDisparity {
        row_center: y0 as f32 + bh as f32 / 2.0,
        dy: dy as f32,
        confidence,
    })
}

/// Combine per-frame band measurements into one set, rejecting bands
/// whose `dy` is unstable across frames (frame-to-frame spread above
/// `max_dy_spread`) - static ground/background geometry should agree
/// across frames; a moving object (e.g. a player) will not.
///
/// # Panics
///
/// Panics if `per_frame` is empty, or if the per-frame band vectors
/// don't all have the same length.
pub fn combine_across_frames(
    per_frame: &[Vec<Option<RowBandDisparity>>],
    max_dy_spread: f32,
) -> Vec<Option<RowBandDisparity>> {
    assert!(!per_frame.is_empty(), "need at least one frame");
    let n_bands = per_frame[0].len();
    for frames in per_frame {
        assert_eq!(
            frames.len(),
            n_bands,
            "all frames must have the same band count"
        );
    }

    (0..n_bands)
        .map(|band_idx| {
            let present: Vec<RowBandDisparity> = per_frame
                .iter()
                .filter_map(|frames| frames[band_idx])
                .collect();
            if present.is_empty() {
                return None;
            }
            let dys: Vec<f32> = present.iter().map(|b| b.dy).collect();
            let min_dy = dys.iter().cloned().fold(f32::INFINITY, f32::min);
            let max_dy = dys.iter().cloned().fold(f32::NEG_INFINITY, f32::max);
            if max_dy - min_dy > max_dy_spread {
                return None; // unstable across frames - likely a mover
            }
            let mean_dy = dys.iter().sum::<f32>() / dys.len() as f32;
            let mean_confidence =
                present.iter().map(|b| b.confidence).sum::<f32>() / present.len() as f32;
            Some(RowBandDisparity {
                row_center: present[0].row_center,
                dy: mean_dy,
                confidence: mean_confidence,
            })
        })
        .collect()
}

/// A smooth, per-row vertical-shift profile built from sparse band
/// measurements - linearly interpolated between measured bands, held
/// constant beyond the first/last measured band, and tapered to exactly
/// zero within `taper_rows` of the outermost measured bands so the
/// correction can never apply outside where it was actually measured
/// (the same "band-limited, mathematically guaranteed zero beyond the
/// band" discipline as `geometry::band_limited_ground_warp`).
#[derive(Debug, Clone)]
pub struct RowProfile {
    /// `dy` shift for every row `0..height`.
    dy_by_row: Vec<f32>,
}

impl RowProfile {
    /// Build a profile from combined band measurements (as returned by
    /// [`combine_across_frames`]). Bands that are `None` are treated as
    /// gaps - interpolated across like missing data, not zeros.
    pub fn fit(bands: &[Option<RowBandDisparity>], height: u32, taper_rows: f32) -> Self {
        let known: Vec<(f32, f32)> = bands
            .iter()
            .filter_map(|b| b.map(|b| (b.row_center, b.dy)))
            .collect();

        let mut dy_by_row = vec![0.0f32; height as usize];
        if known.is_empty() {
            return Self { dy_by_row };
        }

        let first_row = known.first().unwrap().0;
        let last_row = known.last().unwrap().0;

        for (row, slot) in dy_by_row.iter_mut().enumerate() {
            let row = row as f32;
            let raw = interpolate(&known, row);
            let taper = if row < first_row {
                smoothstep(first_row - taper_rows, first_row, row)
            } else if row > last_row {
                1.0 - smoothstep(last_row, last_row + taper_rows, row)
            } else {
                1.0
            };
            *slot = raw * taper;
        }

        Self { dy_by_row }
    }

    /// Vertical shift, in pixels, for the given output row.
    pub fn dy_at(&self, row: u32) -> f32 {
        self.dy_by_row.get(row as usize).copied().unwrap_or(0.0)
    }

    /// Resample an RGBA image by this profile's per-row vertical shift:
    /// `output(x, y) = input(x, y - dy_at(y))`, bilinearly interpolated,
    /// transparent (alpha 0) where the sample falls outside the source.
    pub fn apply_vertical_shift(&self, rgba: &[u8], width: u32, height: u32) -> Vec<u8> {
        assert_eq!(rgba.len(), (width as usize) * (height as usize) * 4);
        let mut out = vec![0u8; rgba.len()];
        for y in 0..height {
            let src_y = y as f32 - self.dy_at(y);
            for x in 0..width {
                let sample = bilinear_sample_rgba(rgba, width, height, x as f32, src_y);
                let idx = ((y * width + x) * 4) as usize;
                out[idx..idx + 4].copy_from_slice(&sample);
            }
        }
        out
    }
}

fn interpolate(known: &[(f32, f32)], row: f32) -> f32 {
    if row <= known[0].0 {
        return known[0].1;
    }
    if row >= known[known.len() - 1].0 {
        return known[known.len() - 1].1;
    }
    for w in known.windows(2) {
        let (r0, v0) = w[0];
        let (r1, v1) = w[1];
        if row >= r0 && row <= r1 {
            let t = if r1 > r0 { (row - r0) / (r1 - r0) } else { 0.0 };
            return v0 + t * (v1 - v0);
        }
    }
    unreachable!("row must fall within [known[0], known[last]] here")
}

fn smoothstep(edge0: f32, edge1: f32, x: f32) -> f32 {
    if (edge1 - edge0).abs() < 1e-6 {
        return if x < edge0 { 0.0 } else { 1.0 };
    }
    let t = ((x - edge0) / (edge1 - edge0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

fn bilinear_sample_rgba(rgba: &[u8], width: u32, height: u32, x: f32, y: f32) -> [u8; 4] {
    if x < 0.0 || y < 0.0 || x > (width as f32 - 1.0) || y > (height as f32 - 1.0) {
        return [0, 0, 0, 0];
    }
    let x0 = x.floor() as u32;
    let y0 = y.floor() as u32;
    let x1 = (x0 + 1).min(width - 1);
    let y1 = (y0 + 1).min(height - 1);
    let fx = x - x0 as f32;
    let fy = y - y0 as f32;

    let px =
        |xi: u32, yi: u32, c: usize| -> f32 { rgba[((yi * width + xi) * 4) as usize + c] as f32 };

    let mut out = [0u8; 4];
    for (c, slot) in out.iter_mut().enumerate() {
        let top = px(x0, y0, c) * (1.0 - fx) + px(x1, y0, c) * fx;
        let bottom = px(x0, y1, c) * (1.0 - fx) + px(x1, y1, c) * fx;
        let v = top * (1.0 - fy) + bottom * fy;
        *slot = v.round().clamp(0.0, 255.0) as u8;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::photometric::OverlapMask;

    fn solid_mask(width: u32, height: u32) -> OverlapMask {
        OverlapMask {
            width,
            height,
            mask: vec![true; (width * height) as usize],
        }
    }

    /// Deterministic pseudo-random luma pattern (so bands have real
    /// texture, not flat/degenerate content).
    fn textured_luma(width: u32, height: u32, seed: u32) -> Vec<f32> {
        let mut state = seed.wrapping_add(12345);
        (0..(width * height))
            .map(|_| {
                state = state.wrapping_mul(1664525).wrapping_add(1013904223);
                (state >> 8) as f32 / (1u32 << 24) as f32
            })
            .collect()
    }

    /// Shift a luma buffer vertically by `dy` (positive = content moves
    /// down), leaving vacated rows as a copy of the nearest edge row (so
    /// there's no artificial flat/degenerate band at the border).
    fn shift_luma_vertically(src: &[f32], width: u32, height: u32, dy: i32) -> Vec<f32> {
        let mut out = vec![0.0f32; src.len()];
        for y in 0..height as i32 {
            let src_y = (y - dy).clamp(0, height as i32 - 1) as u32;
            for x in 0..width {
                out[(y as u32 * width + x) as usize] = src[(src_y * width + x) as usize];
            }
        }
        out
    }

    #[test]
    fn measure_row_bands_recovers_known_uniform_shift() {
        let (width, height) = (64, 64);
        let left = textured_luma(width, height, 1);
        let right = shift_luma_vertically(&left, width, height, 5); // right = left shifted down by 5
        let mask = solid_mask(width, height);

        let bands = measure_row_bands(&left, &right, &mask, 16, 10, 1e-6, 0.3);
        assert!(
            bands.iter().any(|b| b.is_some()),
            "expected at least one confident band"
        );
        for b in bands.into_iter().flatten() {
            // right is shifted +5 relative to left, so left must shift by
            // +5 (not -5) to match right: measure_one_band searches
            // shifting the RIGHT band by `dy` to match left, so the
            // recovered dy should be +5 (right's content is 5 rows lower,
            // so sampling right at y+5 finds what was at left's row y).
            assert!((b.dy - 5.0).abs() < 1e-3, "expected dy=5.0, got {}", b.dy);
        }
    }

    #[test]
    fn measure_row_bands_none_when_insufficient_coverage() {
        let (width, height) = (64, 64);
        let left = textured_luma(width, height, 2);
        let right = textured_luma(width, height, 2);
        let mut mask = solid_mask(width, height);
        // Cover fewer than 32 columns everywhere - every band should be unmeasurable.
        for m in mask.mask.iter_mut() {
            *m = false;
        }
        for y in 0..height {
            for x in 0..10 {
                mask.mask[(y * width + x) as usize] = true;
            }
        }
        let bands = measure_row_bands(&left, &right, &mask, 16, 5, 1e-6, 0.0);
        assert!(bands.iter().all(|b| b.is_none()));
    }

    #[test]
    fn combine_across_frames_rejects_unstable_bands() {
        let stable_a = RowBandDisparity {
            row_center: 10.0,
            dy: 3.0,
            confidence: 0.9,
        };
        let stable_b = RowBandDisparity {
            row_center: 10.0,
            dy: 3.2,
            confidence: 0.9,
        };
        let unstable_a = RowBandDisparity {
            row_center: 30.0,
            dy: -8.0,
            confidence: 0.9,
        };
        let unstable_b = RowBandDisparity {
            row_center: 30.0,
            dy: 9.0,
            confidence: 0.9,
        };

        let frame1 = vec![Some(stable_a), Some(unstable_a)];
        let frame2 = vec![Some(stable_b), Some(unstable_b)];

        let combined = combine_across_frames(&[frame1, frame2], 2.0);
        assert!(combined[0].is_some(), "stable band should survive");
        assert!(
            combined[1].is_none(),
            "unstable (mover) band should be rejected"
        );
    }

    #[test]
    fn combine_across_frames_none_stays_none() {
        let frame1 = vec![
            None,
            Some(RowBandDisparity {
                row_center: 10.0,
                dy: 1.0,
                confidence: 0.5,
            }),
        ];
        let frame2 = vec![
            None,
            Some(RowBandDisparity {
                row_center: 10.0,
                dy: 1.1,
                confidence: 0.5,
            }),
        ];
        let combined = combine_across_frames(&[frame1, frame2], 5.0);
        assert!(combined[0].is_none());
        assert!(combined[1].is_some());
    }

    #[test]
    fn row_profile_is_zero_with_no_measurements() {
        let profile = RowProfile::fit(&[None, None, None], 100, 10.0);
        for row in 0..100 {
            assert_eq!(profile.dy_at(row), 0.0);
        }
    }

    #[test]
    fn row_profile_interpolates_between_bands() {
        let bands = vec![
            Some(RowBandDisparity {
                row_center: 10.0,
                dy: 0.0,
                confidence: 1.0,
            }),
            Some(RowBandDisparity {
                row_center: 50.0,
                dy: 10.0,
                confidence: 1.0,
            }),
        ];
        let profile = RowProfile::fit(&bands, 100, 0.0);
        assert!((profile.dy_at(10) - 0.0).abs() < 1e-3);
        assert!((profile.dy_at(50) - 10.0).abs() < 1e-3);
        assert!(
            (profile.dy_at(30) - 5.0).abs() < 0.5,
            "expected ~midpoint, got {}",
            profile.dy_at(30)
        );
    }

    #[test]
    fn row_profile_tapers_to_zero_beyond_measured_range() {
        let bands = vec![
            Some(RowBandDisparity {
                row_center: 40.0,
                dy: 8.0,
                confidence: 1.0,
            }),
            Some(RowBandDisparity {
                row_center: 60.0,
                dy: 8.0,
                confidence: 1.0,
            }),
        ];
        let profile = RowProfile::fit(&bands, 100, 10.0);
        // Well beyond the taper zone in both directions: must be exactly 0.
        assert_eq!(profile.dy_at(0), 0.0);
        assert_eq!(profile.dy_at(99), 0.0);
        // At the measured band itself: full value.
        assert!((profile.dy_at(50) - 8.0).abs() < 1e-3);
        // Inside the taper zone: strictly between 0 and the full value.
        let tapering = profile.dy_at(35);
        assert!(
            tapering > 0.0 && tapering < 8.0,
            "expected partial taper, got {tapering}"
        );
    }

    #[test]
    fn apply_vertical_shift_moves_content_correctly() {
        let (width, height) = (8, 20);
        let mut rgba = vec![0u8; (width * height * 4) as usize];
        // Put a distinct marker row at y=10.
        for x in 0..width {
            let idx = ((10 * width + x) * 4) as usize;
            rgba[idx..idx + 4].copy_from_slice(&[200, 100, 50, 255]);
        }
        // Two bands spanning rows 5..15 with the same dy give a flat
        // plateau covering both the marker's source row (10) and the
        // expected destination row (13), avoiding the taper falloff
        // that a single-point profile would apply right at its edge.
        let bands = vec![
            Some(RowBandDisparity {
                row_center: 5.0,
                dy: 3.0,
                confidence: 1.0,
            }),
            Some(RowBandDisparity {
                row_center: 15.0,
                dy: 3.0,
                confidence: 1.0,
            }),
        ];
        let profile = RowProfile::fit(&bands, height, 0.0);
        let shifted = profile.apply_vertical_shift(&rgba, width, height);
        // dy=3 means output(x,y) = input(x, y-3), so the marker (at input
        // row 10) should now appear at output row 13.
        let idx13 = ((13 * width) * 4) as usize;
        assert_eq!(shifted[idx13], 200);
        assert_eq!(shifted[idx13 + 1], 100);
        assert_eq!(shifted[idx13 + 2], 50);
    }
}
