//! Local contrast boost (CLAHE) for the detector input tensor.
//!
//! In fog or haze the far half of the pitch loses most of its contrast and
//! the ball drops below the model's confidence threshold. Contrast Limited
//! Adaptive Histogram Equalization on the letterboxed input tensor brings it
//! back: on a misty match (8x8 tiles, clip 3) the share of sampled moments
//! with a ball detection at confidence 0.20 roughly doubled, with no balls
//! lost. Opt-in: a clear-weather image gains little and gets noisier.
//!
//! The algorithm works on BT.709 luma of the RGB `[0, 1]` CHW tensor, over
//! the letterbox content area only (the grey padding is left alone):
//!
//! - the content is split into [`TILES`] x [`TILES`] tiles; each tile gets a
//!   [`BINS`]-bin luma histogram,
//! - every bin is clipped at `clip_limit * tile_pixels / BINS` and the
//!   clipped excess is spread evenly over all bins,
//! - the tile's lookup table is the inclusive cumulative histogram divided
//!   by the tile pixel count; a luma value is mapped by interpolating
//!   linearly across its bin (from the previous bin's entry to its own),
//!   so smooth gradients stay smooth instead of turning into 1/256 steps,
//! - each pixel's new luma is bilinearly interpolated between the lookup
//!   tables of the four nearest tile centres, and the luma change is added
//!   to R, G and B (clamped to `[0, 1]`), which keeps the colour.
//!
//! [`apply_chw`] is the CPU reference. The wgpu preprocessor
//! (`crate::wgpu_preprocess`) runs the same algorithm on the GPU, and a test
//! there checks the two agree.

use crate::input_size::InputSize;

/// Tiles per axis of the content area.
pub const TILES: u32 = 8;
/// Luma histogram bins.
pub const BINS: u32 = 256;

/// Smallest clip limit that changes anything; below it the boost is off.
/// A clip limit of 1.0 already flattens each histogram to uniform.
pub const MIN_CLIP_LIMIT: f32 = 1.0;
/// Largest accepted clip limit. Higher values mainly amplify noise.
pub const MAX_CLIP_LIMIT: f32 = 8.0;

/// Validate a user-supplied clip limit: `0.0` means off, otherwise it must
/// be finite and within [`MIN_CLIP_LIMIT`]`..=`[`MAX_CLIP_LIMIT`].
pub fn validate_clip_limit(clip_limit: f32) -> Result<(), String> {
    if clip_limit == 0.0 {
        return Ok(());
    }
    if !clip_limit.is_finite() || !(MIN_CLIP_LIMIT..=MAX_CLIP_LIMIT).contains(&clip_limit) {
        return Err(format!(
            "AI contrast boost must be 0 (off) or between {MIN_CLIP_LIMIT} and {MAX_CLIP_LIMIT}, got {clip_limit}"
        ));
    }
    Ok(())
}

/// Content rectangle of a letterboxed tensor, in input pixels:
/// `(x, y, width, height)`. Matches the integer padding the wgpu
/// preprocessing shader uses.
pub fn content_rect(input: InputSize, frame_w: u32, frame_h: u32) -> (u32, u32, u32, u32) {
    let lb = input.letterbox(frame_w, frame_h);
    let (px, py) = (lb.pad_x as u32, lb.pad_y as u32);
    (
        px,
        py,
        input.width.saturating_sub(2 * px),
        input.height.saturating_sub(2 * py),
    )
}

/// BT.709 luma of an RGB pixel in `[0, 1]`, its histogram bin, and its
/// position within that bin (`0..=1`).
#[inline]
fn luma_bin(r: f32, g: f32, b: f32) -> (f32, usize, f32) {
    let y = 0.2126 * r + 0.7152 * g + 0.0722 * b;
    let scaled = y * BINS as f32;
    let bin = (scaled as i32).clamp(0, BINS as i32 - 1) as usize;
    (y, bin, (scaled - bin as f32).clamp(0.0, 1.0))
}

/// Map a luma value through one tile's lookup table, interpolating across
/// the bin.
#[inline]
fn map(lut: &[f32], bin: usize, frac: f32) -> f32 {
    let lower = if bin == 0 { 0.0 } else { lut[bin - 1] };
    lower + (lut[bin] - lower) * frac
}

/// Tile `t` spans `[t * len / TILES, (t + 1) * len / TILES)`.
#[inline]
fn tile_span(t: u32, len: u32) -> (u32, u32) {
    (t * len / TILES, (t + 1) * len / TILES)
}

/// Apply the local contrast boost in place to a CHW `[0, 1]` tensor of
/// `input` size whose content area is `rect` (see [`content_rect`]).
///
/// `clip_limit <= 0` is a no-op. A content area smaller than one pixel per
/// tile is left unchanged.
pub fn apply_chw(
    tensor: &mut [f32],
    input: InputSize,
    rect: (u32, u32, u32, u32),
    clip_limit: f32,
) {
    let (cx, cy, cw, ch) = rect;
    let (w, h) = (input.width as usize, input.height as usize);
    let plane = w * h;
    if clip_limit <= 0.0
        || cw < TILES
        || ch < TILES
        || tensor.len() < 3 * plane
        || cx + cw > input.width
        || cy + ch > input.height
    {
        return;
    }

    // Per-tile lookup tables.
    let mut luts = vec![0.0_f32; (TILES * TILES * BINS) as usize];
    for ty in 0..TILES {
        let (y0, y1) = tile_span(ty, ch);
        for tx in 0..TILES {
            let (x0, x1) = tile_span(tx, cw);
            let mut hist = [0.0_f32; BINS as usize];
            for y in y0..y1 {
                let row = (cy + y) as usize * w;
                for x in x0..x1 {
                    let i = row + (cx + x) as usize;
                    let (_, bin, _) = luma_bin(tensor[i], tensor[i + plane], tensor[i + 2 * plane]);
                    hist[bin] += 1.0;
                }
            }
            let n = ((y1 - y0) * (x1 - x0)) as f32;
            let lut = &mut luts[((ty * TILES + tx) * BINS) as usize..][..BINS as usize];
            build_lut(&hist, n, clip_limit, lut);
        }
    }

    // Bilinear between tile centres, luma delta added to RGB.
    let (tw, th) = (cw as f32 / TILES as f32, ch as f32 / TILES as f32);
    for y in 0..ch {
        let (ty0, ty1, fy) = interp_axis(y, th);
        let row = (cy + y) as usize * w;
        for x in 0..cw {
            let (tx0, tx1, fx) = interp_axis(x, tw);
            let i = row + (cx + x) as usize;
            let (r, g, b) = (tensor[i], tensor[i + plane], tensor[i + 2 * plane]);
            let (luma, bin, frac) = luma_bin(r, g, b);
            let at = |ty: u32, tx: u32| {
                map(
                    &luts[((ty * TILES + tx) * BINS) as usize..][..BINS as usize],
                    bin,
                    frac,
                )
            };
            let top = at(ty0, tx0) * (1.0 - fx) + at(ty0, tx1) * fx;
            let bottom = at(ty1, tx0) * (1.0 - fx) + at(ty1, tx1) * fx;
            let delta = top * (1.0 - fy) + bottom * fy - luma;
            tensor[i] = (r + delta).clamp(0.0, 1.0);
            tensor[i + plane] = (g + delta).clamp(0.0, 1.0);
            tensor[i + 2 * plane] = (b + delta).clamp(0.0, 1.0);
        }
    }
}

/// Clip, redistribute and integrate one tile histogram into `lut`.
fn build_lut(hist: &[f32; BINS as usize], n: f32, clip_limit: f32, lut: &mut [f32]) {
    let clip = (clip_limit * n / BINS as f32).max(1.0);
    let excess: f32 = hist.iter().map(|&c| (c - clip).max(0.0)).sum();
    let spread = excess / BINS as f32;
    let mut acc = 0.0_f32;
    for (out, &c) in lut.iter_mut().zip(hist.iter()) {
        acc += c.min(clip) + spread;
        *out = acc / n;
    }
}

/// Neighbouring tile indices and blend weight for pixel `p` along an axis
/// with tiles of `tile_len` pixels.
#[inline]
fn interp_axis(p: u32, tile_len: f32) -> (u32, u32, f32) {
    let g = (p as f32 + 0.5) / tile_len - 0.5;
    let t0 = (g.floor().max(0.0) as u32).min(TILES - 1);
    let t1 = (t0 + 1).min(TILES - 1);
    let f = (g - t0 as f32).clamp(0.0, 1.0);
    (t0, t1, f)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    fn tensor(input: InputSize, f: impl Fn(u32, u32) -> f32) -> Vec<f32> {
        let plane = (input.width * input.height) as usize;
        let mut t = vec![0.0; 3 * plane];
        for y in 0..input.height {
            for x in 0..input.width {
                let i = (y * input.width + x) as usize;
                let v = f(x, y);
                t[i] = v;
                t[i + plane] = v;
                t[i + 2 * plane] = v;
            }
        }
        t
    }

    #[test]
    fn zero_clip_limit_is_a_no_op() {
        let input = InputSize::new(64, 48);
        let orig = tensor(input, |x, y| ((x + y) % 7) as f32 / 10.0);
        let mut t = orig.clone();
        apply_chw(&mut t, input, (0, 0, 64, 48), 0.0);
        assert_eq!(t, orig);
    }

    #[test]
    fn smooth_ramp_widens_without_inverting() {
        // A hazy ramp from 0.45 to 0.55. The clip limit caps the gain per
        // tile, so it widens moderately, and must stay monotonic (no
        // inversion at tile borders).
        let input = InputSize::new(128, 96);
        let mut t = tensor(input, |x, _| 0.45 + 0.1 * x as f32 / 127.0);
        apply_chw(&mut t, input, (0, 0, 128, 96), 3.0);
        let row: Vec<f32> = t[..128].to_vec();
        let (lo, hi) = row
            .iter()
            .fold((1.0_f32, 0.0_f32), |(a, b), &v| (a.min(v), b.max(v)));
        assert!(hi - lo > 0.11, "range after boost {lo}..{hi}");
        assert!(row.windows(2).all(|w| w[1] >= w[0] - 1e-3), "{row:?}");
    }

    /// Mean absolute difference between horizontal neighbours of the R plane.
    pub(crate) fn local_detail(t: &[f32], input: InputSize, rows: std::ops::Range<u32>) -> f32 {
        let w = input.width as usize;
        let mut sum = 0.0;
        let mut n = 0;
        for y in rows {
            let row = &t[y as usize * w..][..w];
            for p in row.windows(2) {
                sum += (p[1] - p[0]).abs();
                n += 1;
            }
        }
        sum / n as f32
    }

    #[test]
    fn faint_texture_gets_more_local_contrast() {
        // Fog: fine detail of a few grey levels on a flat-ish background.
        let input = InputSize::new(128, 96);
        let orig = tensor(input, |x, y| 0.5 + ((x * 7 + y * 3) % 11) as f32 / 255.0);
        let mut t = orig.clone();
        apply_chw(&mut t, input, (0, 0, 128, 96), 3.0);
        let (before, after) = (
            local_detail(&orig, input, 0..96),
            local_detail(&t, input, 0..96),
        );
        assert!(after > 1.5 * before, "detail {before} -> {after}");
    }

    #[test]
    fn padding_outside_the_content_rect_is_untouched() {
        let input = InputSize::new(64, 64);
        let grey = 0.447_058_83;
        let mut t = tensor(input, |x, y| {
            if (8..56).contains(&y) {
                0.4 + 0.2 * x as f32 / 63.0
            } else {
                grey
            }
        });
        apply_chw(&mut t, input, (0, 8, 64, 48), 3.0);
        for y in (0..8).chain(56..64) {
            for x in 0..64 {
                assert_eq!(t[(y * 64 + x) as usize], grey);
            }
        }
    }

    #[test]
    fn colour_offsets_are_kept() {
        // The luma change is added to every channel, so R - G stays put
        // unless a channel clamps.
        let input = InputSize::new(64, 64);
        let plane = 64 * 64;
        let mut t = tensor(input, |x, _| 0.4 + 0.1 * x as f32 / 63.0);
        for v in &mut t[..plane] {
            *v += 0.05;
        }
        apply_chw(&mut t, input, (0, 0, 64, 64), 2.0);
        for i in 0..plane {
            if t[i] < 1.0 && t[i + plane] > 0.0 {
                assert!((t[i] - t[i + plane] - 0.05).abs() < 1e-4);
            }
        }
    }

    #[test]
    fn clip_limit_validation() {
        assert!(validate_clip_limit(0.0).is_ok());
        assert!(validate_clip_limit(3.0).is_ok());
        assert!(validate_clip_limit(0.5).is_err());
        assert!(validate_clip_limit(9.0).is_err());
        assert!(validate_clip_limit(f32::NAN).is_err());
    }

    #[test]
    fn content_rect_matches_letterbox() {
        let input = InputSize::new(1920, 1920);
        assert_eq!(content_rect(input, 3840, 2880), (0, 240, 1920, 1440));
    }
}
