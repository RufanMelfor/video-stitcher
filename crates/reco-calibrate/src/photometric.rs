//! Photometric (direct/ZNCC-based) alignment scoring - pure CPU math over
//! rendered RGBA buffers.
//!
//! This is an alternative objective function to the production
//! AKAZE-feature-based calibration: instead of minimizing reprojection
//! error between sparse matched keypoints, it directly compares rendered
//! pixels from the two cameras in their overlap region. See
//! `examples/fit_photometric.rs` for the harness that renders real
//! footage (via `reco_core::render::single_camera::SingleCameraRenderer`)
//! and drives an optimizer against this module's scoring functions. No
//! GPU or video I/O happens in this module - everything here is testable
//! with plain `cargo test -p reco-calibrate`.

/// Per-pixel overlap mask: true where BOTH cameras have real coverage.
pub struct OverlapMask {
    pub width: u32,
    pub height: u32,
    pub mask: Vec<bool>,
}

impl OverlapMask {
    /// Fraction of pixels where both cameras contribute (`0.0..=1.0`).
    pub fn coverage_fraction(&self) -> f64 {
        if self.mask.is_empty() {
            return 0.0;
        }
        let covered = self.mask.iter().filter(|&&b| b).count();
        covered as f64 / self.mask.len() as f64
    }
}

/// Build the overlap mask from two single-camera RGBA renders by ANDing
/// their alpha channels.
///
/// Requires both renders to come from a renderer that clears to
/// `wgpu::Color::TRANSPARENT` (not the production `BLACK`) - see
/// `reco_core::render::single_camera`'s module doc - so that alpha is a
/// clean per-pixel "this camera covers this pixel" signal, and ANDing
/// the two is a real overlap test rather than a false-positive OR (a
/// pixel outside both cameras' footprints reads `alpha=0` in both
/// buffers, so it's correctly excluded).
///
/// # Panics
///
/// Panics if either buffer's length doesn't match `width * height * 4`.
pub fn overlap_mask_from_alpha(
    left_rgba: &[u8],
    right_rgba: &[u8],
    width: u32,
    height: u32,
    alpha_threshold: u8,
) -> OverlapMask {
    let expected = (width as usize) * (height as usize) * 4;
    assert_eq!(left_rgba.len(), expected, "left_rgba size mismatch");
    assert_eq!(right_rgba.len(), expected, "right_rgba size mismatch");

    let mask: Vec<bool> = (0..(width as usize * height as usize))
        .map(|i| {
            let a_left = left_rgba[i * 4 + 3];
            let a_right = right_rgba[i * 4 + 3];
            a_left > alpha_threshold && a_right > alpha_threshold
        })
        .collect();

    OverlapMask {
        width,
        height,
        mask,
    }
}

/// BT.709 luma weights, matching `fisheye.wgsl`'s own `rgb_to_yuv` so
/// this metric reasons about brightness the same way the shader does.
const LUMA_R: f32 = 0.2126;
const LUMA_G: f32 = 0.7152;
const LUMA_B: f32 = 0.0722;

/// Convert an RGBA8 buffer to a luma-only `f32` buffer in `[0.0, 1.0]`.
///
/// # Panics
///
/// Panics if `rgba.len() != width * height * 4`.
pub fn to_luma(rgba: &[u8], width: u32, height: u32) -> Vec<f32> {
    let expected = (width as usize) * (height as usize) * 4;
    assert_eq!(rgba.len(), expected, "rgba size mismatch");

    (0..(width as usize * height as usize))
        .map(|i| {
            let r = rgba[i * 4] as f32 / 255.0;
            let g = rgba[i * 4 + 1] as f32 / 255.0;
            let b = rgba[i * 4 + 2] as f32 / 255.0;
            LUMA_R * r + LUMA_G * g + LUMA_B * b
        })
        .collect()
}

/// Zero-mean normalized cross-correlation between two equal-length
/// slices. Returns `1.0` for identical signals, `-1.0` for exactly
/// negated signals, and `None` if either slice's variance is below
/// `min_variance` (a flat/near-flat patch, e.g. a no-overlap region or a
/// textureless surface, can't produce a meaningful or numerically
/// stable score).
///
/// # Panics
///
/// Panics if `a.len() != b.len()`.
pub fn zncc(a: &[f32], b: &[f32], min_variance: f32) -> Option<f32> {
    assert_eq!(a.len(), b.len(), "zncc: slices must be equal length");
    let n = a.len();
    if n == 0 {
        return None;
    }

    let mean_a = a.iter().sum::<f32>() / n as f32;
    let mean_b = b.iter().sum::<f32>() / n as f32;

    let mut cov = 0.0f32;
    let mut var_a = 0.0f32;
    let mut var_b = 0.0f32;
    for i in 0..n {
        let da = a[i] - mean_a;
        let db = b[i] - mean_b;
        cov += da * db;
        var_a += da * da;
        var_b += db * db;
    }
    var_a /= n as f32;
    var_b /= n as f32;

    if var_a < min_variance || var_b < min_variance {
        return None;
    }

    let denom = (var_a * var_b).sqrt() * n as f32;
    Some((cov / denom).clamp(-1.0, 1.0))
}

/// Aggregate result of tiling the overlap region into patches and
/// scoring each with [`zncc`], skipping degenerate (near-flat) patches.
///
/// Patch-based rather than one global ZNCC number specifically so a
/// degenerate/aliased solution (e.g. the optimizer finds a placement
/// where the "overlap" region collapses onto a small, textureless, or
/// otherwise exploitable strip) shows up as a low `valid_patches` count
/// or a wide min/max spread, not hidden inside one deceptively good
/// scalar - the same "don't trust one aggregate number" lesson as
/// `FRICTION.md`'s homography/XFeat false positives.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ZnccReport {
    pub mean: f32,
    pub min: f32,
    pub max: f32,
    pub valid_patches: usize,
    pub skipped_patches: usize,
}

impl ZnccReport {
    /// A report representing "no usable signal at all" - used when the
    /// overlap mask has too few covered pixels to tile into any patch.
    pub fn empty() -> Self {
        Self {
            mean: -1.0,
            min: -1.0,
            max: -1.0,
            valid_patches: 0,
            skipped_patches: 0,
        }
    }
}

/// Tile `width x height` into `patch_size`-square blocks, score each
/// block that is *fully* inside the overlap mask with [`zncc`], and
/// aggregate. Blocks that aren't fully covered, or that are degenerate
/// (flat) in either image, are skipped and counted in
/// `skipped_patches`.
///
/// Equivalent to [`windowed_zncc_banded`] with `min_row_frac = 0.0`
/// (scores the whole overlap region, no row restriction).
pub fn windowed_zncc(
    left_luma: &[f32],
    right_luma: &[f32],
    mask: &OverlapMask,
    patch_size: u32,
    min_variance: f32,
) -> ZnccReport {
    windowed_zncc_banded(left_luma, right_luma, mask, patch_size, min_variance, 0.0)
}

/// Like [`windowed_zncc`], but only scores patches whose row range starts
/// at or beyond `min_row_frac * height` - i.e. restricts scoring to a
/// horizontal band starting `min_row_frac` of the way down the frame.
///
/// Exists because averaging ZNCC uniformly across the *whole* overlap
/// region lets the (much larger, already well-aligned) far-field area
/// dominate the mean, drowning out a thin near-field misalignment band -
/// exactly the failure mode observed when using [`windowed_zncc`] as an
/// optimizer objective (see `examples/fit_photometric.rs`'s FRICTION.md
/// writeup). Restricting to the near-field band forces the score to
/// actually reflect alignment quality there, rather than being diluted
/// by unrelated regions (e.g. background/skyline content, which may have
/// its own irreducible parallax mismatch unrelated to the near-field
/// seam this is meant to fix).
pub fn windowed_zncc_banded(
    left_luma: &[f32],
    right_luma: &[f32],
    mask: &OverlapMask,
    patch_size: u32,
    min_variance: f32,
    min_row_frac: f32,
) -> ZnccReport {
    let width = mask.width;
    let height = mask.height;
    assert_eq!(left_luma.len(), (width * height) as usize);
    assert_eq!(right_luma.len(), (width * height) as usize);

    let min_row = (min_row_frac.clamp(0.0, 1.0) * height as f32) as u32;

    let mut scores: Vec<f32> = Vec::new();
    let mut skipped = 0usize;

    let mut py = min_row;
    while py < height {
        let ph = patch_size.min(height - py);
        let mut px = 0u32;
        while px < width {
            let pw = patch_size.min(width - px);

            let mut fully_covered = true;
            let mut a = Vec::with_capacity((pw * ph) as usize);
            let mut b = Vec::with_capacity((pw * ph) as usize);
            'rows: for y in py..(py + ph) {
                for x in px..(px + pw) {
                    let idx = (y * width + x) as usize;
                    if !mask.mask[idx] {
                        fully_covered = false;
                        break 'rows;
                    }
                    a.push(left_luma[idx]);
                    b.push(right_luma[idx]);
                }
            }

            if fully_covered {
                match zncc(&a, &b, min_variance) {
                    Some(score) => scores.push(score),
                    None => skipped += 1,
                }
            } else {
                skipped += 1;
            }

            px += patch_size;
        }
        py += patch_size;
    }

    if scores.is_empty() {
        return ZnccReport {
            mean: ZnccReport::empty().mean,
            min: ZnccReport::empty().min,
            max: ZnccReport::empty().max,
            valid_patches: 0,
            skipped_patches: skipped,
        };
    }

    let mean = scores.iter().sum::<f32>() / scores.len() as f32;
    let min = scores.iter().cloned().fold(f32::INFINITY, f32::min);
    let max = scores.iter().cloned().fold(f32::NEG_INFINITY, f32::max);

    ZnccReport {
        mean,
        min,
        max,
        valid_patches: scores.len(),
        skipped_patches: skipped,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use approx::assert_abs_diff_eq;

    fn correlated_noise(n: usize, seed: u32) -> Vec<f32> {
        // Simple deterministic LCG, no external RNG dependency needed
        // beyond what's already a dev-dependency elsewhere in this crate.
        let mut state = seed.wrapping_add(12345);
        (0..n)
            .map(|_| {
                state = state.wrapping_mul(1664525).wrapping_add(1013904223);
                (state >> 8) as f32 / (1u32 << 24) as f32
            })
            .collect()
    }

    #[test]
    fn zncc_identical_signal_is_one() {
        let x = correlated_noise(64, 1);
        let score = zncc(&x, &x, 0.0).expect("non-degenerate signal");
        assert_abs_diff_eq!(score, 1.0, epsilon = 1e-4);
    }

    #[test]
    fn zncc_negated_signal_is_negative_one() {
        let x = correlated_noise(64, 2);
        let neg: Vec<f32> = x.iter().map(|v| -v).collect();
        let score = zncc(&x, &neg, 0.0).expect("non-degenerate signal");
        assert_abs_diff_eq!(score, -1.0, epsilon = 1e-4);
    }

    #[test]
    fn zncc_uncorrelated_signal_near_zero() {
        // Two independent pseudorandom streams: not exactly zero
        // correlation, but should be small in magnitude relative to the
        // identical/negated cases (which are exactly +-1). A large n
        // keeps sampling variance low enough for a tight-ish bound.
        let a = correlated_noise(4096, 42);
        let b = correlated_noise(4096, 999_331);
        let score = zncc(&a, &b, 0.0).expect("non-degenerate signal");
        assert!(
            score.abs() < 0.1,
            "expected near-zero correlation for independent streams, got {score}"
        );
    }

    #[test]
    fn zncc_flat_patch_returns_none() {
        let flat = vec![0.5_f32; 32];
        let other = correlated_noise(32, 3);
        assert!(zncc(&flat, &other, 1e-6).is_none());
        assert!(zncc(&other, &flat, 1e-6).is_none());
    }

    #[test]
    fn overlap_mask_from_alpha_is_and_not_or() {
        // 2x1 image: pixel 0 covered by left only, pixel 1 covered by right only.
        // Correct AND behavior: mask is all-false (no true overlap).
        let left_rgba = [0, 0, 0, 255, 0, 0, 0, 0];
        let right_rgba = [0, 0, 0, 0, 0, 0, 0, 255];
        let mask = overlap_mask_from_alpha(&left_rgba, &right_rgba, 2, 1, 10);
        assert_eq!(mask.mask, vec![false, false]);
        assert_abs_diff_eq!(mask.coverage_fraction(), 0.0, epsilon = 1e-12);
    }

    #[test]
    fn overlap_mask_from_alpha_true_overlap() {
        let left_rgba = [0, 0, 0, 255, 0, 0, 0, 0];
        let right_rgba = [0, 0, 0, 255, 0, 0, 0, 255];
        let mask = overlap_mask_from_alpha(&left_rgba, &right_rgba, 2, 1, 10);
        assert_eq!(mask.mask, vec![true, false]);
        assert_abs_diff_eq!(mask.coverage_fraction(), 0.5, epsilon = 1e-12);
    }

    #[test]
    fn windowed_zncc_skips_degenerate_patches_but_scores_real_ones() {
        // 4x2 image, patch_size=2: two 2x2 patches side by side.
        // Left patch: flat (degenerate). Right patch: correlated noise (real signal).
        let width = 4;
        let height = 2;
        let mask = OverlapMask {
            width,
            height,
            mask: vec![true; (width * height) as usize],
        };
        let mut left = vec![0.5_f32; (width * height) as usize];
        let mut right = vec![0.5_f32; (width * height) as usize];
        let noise_a = correlated_noise(4, 10);
        let noise_b = correlated_noise(4, 10); // identical seed -> identical signal
        for (i, &(x, y)) in [(2, 0), (3, 0), (2, 1), (3, 1)].iter().enumerate() {
            let idx = (y * width + x) as usize;
            left[idx] = noise_a[i];
            right[idx] = noise_b[i];
        }

        let report = windowed_zncc(&left, &right, &mask, 2, 1e-6);
        assert_eq!(report.valid_patches, 1);
        assert_eq!(report.skipped_patches, 1);
        assert!(
            report.mean > 0.9,
            "expected near-perfect correlation on the real patch"
        );
    }

    #[test]
    fn windowed_zncc_all_degenerate_returns_empty_report() {
        let width = 2;
        let height = 2;
        let mask = OverlapMask {
            width,
            height,
            mask: vec![true; 4],
        };
        let flat = vec![0.5_f32; 4];
        let report = windowed_zncc(&flat, &flat, &mask, 2, 1e-6);
        assert_eq!(report.valid_patches, 0);
        assert_eq!(report.skipped_patches, 1);
        assert_eq!(report.mean, ZnccReport::empty().mean);
    }

    #[test]
    fn windowed_zncc_banded_zero_frac_matches_unbanded() {
        // width=4, height=8, patch_size=4: two patches stacked vertically.
        let width = 4;
        let height = 8;
        let mask = OverlapMask {
            width,
            height,
            mask: vec![true; (width * height) as usize],
        };
        let left = correlated_noise((width * height) as usize, 20);
        let right = correlated_noise((width * height) as usize, 20);
        let unbanded = windowed_zncc(&left, &right, &mask, 4, 1e-6);
        let banded = windowed_zncc_banded(&left, &right, &mask, 4, 1e-6, 0.0);
        assert_eq!(unbanded, banded);
    }

    #[test]
    fn windowed_zncc_banded_excludes_patches_above_the_row_cutoff() {
        // width=4, height=8, patch_size=4: top patch (rows 0-3) is
        // degenerate/flat, bottom patch (rows 4-7) is real correlated
        // signal. Unbanded scoring sees both (1 valid, 1 skipped);
        // min_row_frac=0.5 should skip the top patch entirely (not just
        // score it as degenerate) and report only the bottom one.
        let width = 4;
        let height = 8;
        let mask = OverlapMask {
            width,
            height,
            mask: vec![true; (width * height) as usize],
        };
        let mut left = vec![0.5_f32; (width * height) as usize];
        let mut right = vec![0.5_f32; (width * height) as usize];
        let noise_a = correlated_noise(16, 30);
        let noise_b = correlated_noise(16, 30);
        for y in 4..8u32 {
            for x in 0..4u32 {
                let idx = (y * width + x) as usize;
                let local = ((y - 4) * width + x) as usize;
                left[idx] = noise_a[local];
                right[idx] = noise_b[local];
            }
        }

        let unbanded = windowed_zncc(&left, &right, &mask, 4, 1e-6);
        assert_eq!(unbanded.valid_patches, 1);
        assert_eq!(unbanded.skipped_patches, 1);

        let banded = windowed_zncc_banded(&left, &right, &mask, 4, 1e-6, 0.5);
        assert_eq!(banded.valid_patches, 1);
        assert_eq!(
            banded.skipped_patches, 0,
            "the excluded top patch should not even be attempted, not counted as skipped"
        );
        assert!(banded.mean > 0.9);
    }

    #[test]
    fn to_luma_matches_known_bt709_weights() {
        let pure_red = [255u8, 0, 0, 255];
        let pure_green = [0u8, 255, 0, 255];
        let pure_blue = [0u8, 0, 255, 255];
        assert_abs_diff_eq!(to_luma(&pure_red, 1, 1)[0], LUMA_R, epsilon = 1e-3);
        assert_abs_diff_eq!(to_luma(&pure_green, 1, 1)[0], LUMA_G, epsilon = 1e-3);
        assert_abs_diff_eq!(to_luma(&pure_blue, 1, 1)[0], LUMA_B, epsilon = 1e-3);
    }
}
