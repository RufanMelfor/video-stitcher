//! Composites multiple independent [`OverlayFrameSource`] layers into
//! one, bottom-to-top in registration order - so more than one overlay
//! producer (e.g. a scoreboard and the cut-range "PAUZE" transition)
//! can be attached to a session at once, when it only has a single
//! overlay slot (see `RgbaOverlayCompositor` - one texture, one
//! placement). Each layer keeps its own [`OverlayPlacement`],
//! replicated here with the exact same contain-fit math
//! `rgba_overlay.wgsl`'s `fs_main` uses, so a layer lands exactly
//! where it would if it were the only one attached directly.
//!
//! This module knows nothing about what any layer actually draws -
//! same spirit as `render::overlay`'s own doc comment.

use super::overlay::{OverlayFrame, OverlayFrameSource, OverlayPlacement};

/// Combines any number of overlay layers into one [`OverlayFrameSource`].
/// A layer with no frame yet (its own `try_frame` hasn't returned
/// `Some` yet) simply isn't drawn - not an error, just "nothing there
/// yet".
pub struct LayeredOverlaySource {
    layers: Vec<(Box<dyn OverlayFrameSource>, OverlayPlacement)>,
    cache: Vec<Option<OverlayFrame>>,
    output_size: (u32, u32),
}

impl LayeredOverlaySource {
    /// `output_size` is the session's real output resolution - the
    /// combined canvas is allocated at exactly that size, so every
    /// layer lands in it at precisely the size and position it would
    /// have had if it were the only overlay attached (see
    /// [`Self::try_frame`]).
    pub fn new(
        layers: Vec<(Box<dyn OverlayFrameSource>, OverlayPlacement)>,
        output_size: (u32, u32),
    ) -> Self {
        let cache = vec![None; layers.len()];
        Self {
            layers,
            cache,
            output_size: (output_size.0.max(1), output_size.1.max(1)),
        }
    }
}

impl OverlayFrameSource for LayeredOverlaySource {
    fn try_frame(&mut self) -> Result<Option<OverlayFrame>, String> {
        let mut any_changed = false;
        for (i, (source, _)) in self.layers.iter_mut().enumerate() {
            if let Some(frame) = source.try_frame()? {
                // A source is free to hand over a frame identical to
                // the one it last produced, and a real one does:
                // `reco_scoreboard`'s runtime re-captures its page at a
                // fixed rate (30fps), while what the page actually
                // *shows* changes about once a second when the clock
                // ticks. Recompositing for those is pure waste - and
                // far from free, since the canvas below is
                // output-resolution, so it turned a 22fps export into a
                // 7fps one when this was missing. Comparing the pixels
                // costs a memcmp of one already-in-cache buffer; the
                // composite it avoids is orders of magnitude more.
                if self.cache[i].as_ref().is_some_and(|cached| {
                    cached.width == frame.width
                        && cached.height == frame.height
                        && cached.design_size == frame.design_size
                        && cached.rgba == frame.rgba
                }) {
                    continue;
                }
                any_changed = true;
                // A fully-transparent frame (e.g. a `pause_overlay`
                // layer's one final "clear the compositor" frame once
                // its transition ends, see that module's `was_active`
                // handling) draws nothing - `composite_over` skips
                // every pixel via its `src_a <= 0.0` check regardless
                // of this frame's size. Drop it from the cache instead
                // of leaving a stale `Some(...)` sitting there forever
                // (this trait's `None` return only means "no update
                // this call", never "gone" - nothing else clears a
                // cache slot). A real bug found via this: once ANY
                // cut-range PAUZE transition had ever played, its
                // now-invisible layer's size (960x540, or worse before
                // that cap existed) stayed in the max-size fold below
                // for the *rest of the export*, forcing every
                // subsequent scoreboard frame through an unnecessary
                // resample long after the transition itself had ended
                // - not just during the transition, which is what every
                // prior investigation this session assumed the bug was
                // scoped to.
                if is_fully_transparent(&frame) {
                    self.cache[i] = None;
                } else {
                    self.cache[i] = Some(frame);
                }
            }
        }
        if !any_changed {
            return Ok(None);
        }

        // The canvas is allocated at the session's real output
        // resolution, with `design_size` set to match. Both halves of
        // that matter, and neither is arbitrary:
        //
        // - `design_size == output_size` makes the GPU compositor's
        //   own contain-fit math (`rgba_overlay.wgsl`) resolve to
        //   exactly 1:1 - the canvas covers the output frame with no
        //   scaling, and its texture is sampled texel-for-texel.
        // - `width`/`height == output_size` means `composite_over`
        //   below performs, for each layer, the *identical*
        //   contain-fit computation the shader would have performed
        //   had that layer been attached alone (same design size, same
        //   placement, same target resolution). So a layer lands at
        //   byte-identical position and size either way, and a
        //   producer that pre-scaled its own pixel buffer to match its
        //   real on-screen footprint (the scoreboard's Chrome capture,
        //   see `OverlayFrame::design_size`) is resampled by exactly
        //   nothing.
        //
        // Every earlier version of this derived the canvas size from
        // the layers themselves - a fixed 960x540 at first, then the
        // largest active layer's own pixel buffer. Both were wrong for
        // the same underlying reason, and the second one much less
        // obviously: a layer's own pixel buffer is sized to its
        // *on-screen footprint*, which for a typical scoreboard banner
        // (placement scale ~0.3) is a small fraction of the frame.
        // Sizing the shared canvas to that buffer and then placing the
        // layer within the canvas at its placement scale applied the
        // shrink a second time - a 768x432 capture squeezed into a
        // 230x130 footprint, then stretched back up by the GPU. The
        // result was a visibly soft scoreboard on every export where
        // any second layer existed at all, from the very first frame,
        // whether or not that second layer was drawing anything yet -
        // which is exactly why turning the PAUZE transition on made a
        // previously sharp scoreboard blurry for the entire export.
        // See SESSION_HANDOFF's 2026-08-25/26 entries for the full
        // investigation.
        let (width, height) = self.output_size;
        let mut canvas = OverlayFrame {
            width,
            height,
            design_size: self.output_size,
            rgba: vec![0u8; (width as usize) * (height as usize) * 4],
        };
        let mut canvas_is_empty = true;
        for (i, (_, placement)) in self.layers.iter().enumerate() {
            if let Some(frame) = &self.cache[i] {
                composite_over(&mut canvas, frame, *placement, canvas_is_empty);
                canvas_is_empty = false;
            }
        }
        Ok(Some(canvas))
    }
}

/// Whether every pixel in `frame` has zero alpha - such a frame draws
/// nothing (`composite_over` skips every pixel via its `src_a <= 0.0`
/// check) regardless of its size, so it's safe to drop from
/// `LayeredOverlaySource`'s cache entirely rather than let its size
/// linger in the max-size fold. Short-circuits on the first non-zero
/// alpha byte, so the common case (a normal, visible frame) is cheap.
fn is_fully_transparent(frame: &OverlayFrame) -> bool {
    frame.rgba.chunks_exact(4).all(|px| px[3] == 0)
}

/// Inclusive `(first, last)` row indices of `frame` containing any
/// non-zero alpha, or `None` when nothing in it is visible at all.
/// Used by [`composite_over`] to skip the parts of a layer's footprint
/// that cannot contribute anything - see its own comment for why that
/// matters at output resolution.
///
/// Deliberately rows only, not a full bounding box: each row stops at
/// its first visible pixel, so a fully opaque layer (the common case -
/// a scoreboard banner) costs one check per row rather than a scan of
/// every pixel, while a layer that is transparent except for a
/// horizontal band (the PAUZE caption) still gets that band found.
fn opaque_row_bounds(frame: &OverlayFrame) -> Option<(u32, u32)> {
    let row_has_alpha = |y: u32| {
        let row = (y * frame.width) as usize * 4;
        (0..frame.width as usize).any(|x| frame.rgba[row + x * 4 + 3] != 0)
    };
    let first = (0..frame.height).find(|&y| row_has_alpha(y))?;
    let last = (first..frame.height)
        .rev()
        .find(|&y| row_has_alpha(y))
        .unwrap_or(first);
    Some((first, last))
}

/// Alpha-composites `source` onto `canvas` using the exact placement
/// math `rgba_overlay.wgsl`'s `fs_main` uses (contain-fit within
/// `canvas`'s own size, shifted/scaled by `placement`). Bilinear
/// sampling - a cheaper nearest-neighbor filter was tried first (the
/// layers this combines update at most a few times a second, a
/// scoreboard clock tick or a cut-range fade step, so the *frequency*
/// of calls is low) but produced real, visible text degradation
/// whenever a scoreboard shared a canvas with a differently-sized
/// layer (see `try_frame`'s doc comment on canvas sizing, and
/// SESSION_HANDOFF's 2026-08-25 entry) - worth the extra per-call cost
/// at this call frequency. Straight-alpha "over" blend.
///
/// `canvas_is_empty` promises every pixel of `canvas` currently has
/// zero alpha - true for the first layer drawn onto a freshly allocated
/// canvas. Straight-alpha "over" onto a fully transparent destination
/// reduces exactly to `out = src` (`out_a = src_a + 0`, and the colour
/// term's `dst_a * (1 - src_a)` half vanishes, leaving `src_rgb *
/// src_a / src_a`), so the whole per-pixel blend - a destination read,
/// a reciprocal and three multiply-adds - collapses into a copy. Worth
/// special-casing because the layer this applies to is the PAUZE fade,
/// which covers the entire output frame.
fn composite_over(
    canvas: &mut OverlayFrame,
    source: &OverlayFrame,
    placement: OverlayPlacement,
    canvas_is_empty: bool,
) {
    let (out_w, out_h) = (canvas.width as f32, canvas.height as f32);
    // The footprint `source` is fit into uses its *design* size, not
    // its actual pixel buffer size - see `OverlayFrame::design_size`.
    // These differ when `source`'s producer pre-scaled its own pixel
    // buffer down for sharper rendering; the footprint math must stay
    // based on the stable design size regardless, or that shrink would
    // get (wrongly) re-applied here as an *additional* scale-down on
    // top of whatever the producer already did.
    let (ref_w, ref_h) = (source.design_size.0 as f32, source.design_size.1 as f32);
    if ref_w <= 0.0 || ref_h <= 0.0 {
        return;
    }

    let scale = (out_w / ref_w).min(out_h / ref_h) * placement.scale;
    let (fitted_w, fitted_h) = (ref_w * scale, ref_h * scale);
    let center_x = out_w * 0.5 + placement.offset.0 * out_w;
    let center_y = out_h * 0.5 + placement.offset.1 * out_h;
    let origin_x = center_x - fitted_w * 0.5;
    let origin_y = center_y - fitted_h * 0.5;
    if fitted_w <= 0.0 || fitted_h <= 0.0 {
        return;
    }

    let x0 = origin_x.max(0.0).floor() as u32;
    let mut y0 = origin_y.max(0.0).floor() as u32;
    let x1 = ((origin_x + fitted_w).min(out_w).ceil() as u32).min(canvas.width);
    let mut y1 = ((origin_y + fitted_h).min(out_h).ceil() as u32).min(canvas.height);

    // Narrow the scanned region to the part of the footprint that can
    // actually receive a non-transparent pixel. The canvas is
    // output-resolution (see `try_frame`), so a layer that covers the
    // whole frame while being almost entirely transparent - the PAUZE
    // caption is exactly that, a band of glyphs on empty space - would
    // otherwise interpolate an alpha value for every pixel of a 4K
    // frame just to discard nearly all of them. Scanning the (much
    // smaller) source's alpha channel to find that band first is a
    // rounding error by comparison. A fully opaque source's bounds are
    // unchanged by this, so it costs those nothing but the scan.
    match opaque_row_bounds(source) {
        None => return, // nothing visible anywhere in this layer
        Some((by0, by1)) => {
            // Source row range -> footprint fraction -> canvas row
            // range, widened by a row on each side to stay clear of the
            // half-texel offset the sampling below applies.
            let to_canvas_y = |sy: f32| origin_y + (sy / source.height as f32) * fitted_h;
            y0 = y0.max(to_canvas_y(by0 as f32).floor().max(0.0) as u32);
            y1 = y1.min((to_canvas_y((by1 + 2) as f32).ceil().max(0.0) as u32).min(canvas.height));
        }
    }

    // A single-texel source (the PAUZE fade card is one opaque black
    // pixel stretched over the whole frame - see `pause_overlay`) has
    // no detail to interpolate: every sample returns the same value, so
    // resolve it once here instead of running a four-tap bilinear per
    // output pixel.
    let flat = (source.width == 1 && source.height == 1).then(|| {
        [
            f32::from(source.rgba[0]) / 255.0,
            f32::from(source.rgba[1]) / 255.0,
            f32::from(source.rgba[2]) / 255.0,
            f32::from(source.rgba[3]) / 255.0,
        ]
    });
    if let Some(flat) = flat
        && flat[3] <= 0.0
    {
        return;
    }

    // Flat colour onto a still-empty canvas - the PAUZE fade, which
    // covers the entire output frame - reduces to writing the same four
    // bytes everywhere in the footprint (see this function's doc
    // comment for why "over" collapses to a copy here). Build one row
    // and clone it down the region: at 2560x1440 that turns ~3.7M
    // per-pixel blends into a handful of `copy_from_slice` calls, which
    // is the difference between this layer costing tens of milliseconds
    // and costing memory bandwidth. It is the single most expensive
    // layer in a transition precisely because it is full-frame, so it
    // is the one worth a special case.
    // Restricted to a footprint that covers the whole canvas, which is
    // exactly the PAUZE fade's case. Inside such a footprint every
    // pixel is guaranteed to satisfy the `u`/`v` bounds checks the
    // general loop applies, so skipping them changes nothing; on a
    // partial footprint the loop's rounded `x0`/`y0` can sit a pixel
    // outside the true fitted edge, and a blind fill would paint that
    // pixel where the general path correctly rejects it.
    let covers_canvas = origin_x <= 0.0
        && origin_y <= 0.0
        && origin_x + fitted_w >= out_w
        && origin_y + fitted_h >= out_h;
    if let Some(flat) = flat
        && canvas_is_empty
        && covers_canvas
        && x1 > x0
        && y1 > y0
    {
        let px = [
            (flat[0].clamp(0.0, 1.0) * 255.0).round() as u8,
            (flat[1].clamp(0.0, 1.0) * 255.0).round() as u8,
            (flat[2].clamp(0.0, 1.0) * 255.0).round() as u8,
            (flat[3].clamp(0.0, 1.0) * 255.0).round() as u8,
        ];
        let span = (x1 - x0) as usize;
        let mut row = Vec::with_capacity(span * 4);
        for _ in 0..span {
            row.extend_from_slice(&px);
        }
        for y in y0..y1 {
            let start = (y * canvas.width + x0) as usize * 4;
            canvas.rgba[start..start + span * 4].copy_from_slice(&row);
        }
        return;
    }

    // Hoisted out of the pixel loops: `try_frame` can call this a few
    // times a second at full canvas resolution (see its own doc
    // comment), and a division per pixel here was measured to matter -
    // ~3.5x the wall time at 1920x1080 vs. computing it once per call.
    let inv_fitted_w = 1.0 / fitted_w;
    let inv_fitted_h = 1.0 / fitted_h;

    for y in y0..y1 {
        let v = (y as f32 + 0.5 - origin_y) * inv_fitted_h;
        // Inclusive at 1.0 to match `rgba_overlay.wgsl`'s own bounds
        // check (`overlay_uv.y > 1.0` rejects, so `== 1.0` is kept) -
        // an exclusive `..1.0` range here would drop a sliver of
        // pixels right at an overlay's fitted edge that the shader
        // does draw.
        if !(0.0..=1.0).contains(&v) {
            continue;
        }
        // `v`/`u` are fractions (0..1) of the *footprint*, independent
        // of resolution - map them into `source`'s actual pixel buffer
        // (`source.width`/`height`), not the `ref_w`/`ref_h` design
        // size used only to compute that footprint above. These
        // coincide whenever a producer hasn't pre-scaled its own pixel
        // buffer; when it has, this is exactly what makes the smaller
        // buffer sample correctly across the still-full-size footprint
        // instead of only covering a corner of it.
        //
        // Bilinear, not nearest-neighbor: this canvas is shared between
        // layers (see the struct's own doc comment) and sized to the
        // *largest* layer's actual pixel buffer - any layer smaller
        // than that (e.g. `pause_overlay`'s 1x1 fade card, or a
        // scoreboard whose own pre-scaled capture happens to land
        // smaller than a co-active layer) still needs resampling up to
        // fill the shared canvas's footprint. Nearest-neighbor here was
        // real, reproducible visible text degradation whenever a
        // scoreboard shared a session with the PAUZE transition's old,
        // fixed-960x540 combined card - found via a real user export at
        // a realistic banner scale (~0.3), see SESSION_HANDOFF's
        // 2026-08-25 entry. `pause_overlay`'s fade/caption split
        // (2026-08-26) fixed the actual root cause of that particular
        // case, but this bilinear fix stays independently correct for
        // any other layer-size mismatch.
        let sy_f = (v * source.height as f32 - 0.5).clamp(0.0, (source.height.max(1) - 1) as f32);
        let sy0 = sy_f.floor() as u32;
        let sy1 = (sy0 + 1).min(source.height - 1);
        let fy = sy_f - sy0 as f32;
        for x in x0..x1 {
            let u = (x as f32 + 0.5 - origin_x) * inv_fitted_w;
            if !(0.0..=1.0).contains(&u) {
                continue;
            }
            let sx_f = (u * source.width as f32 - 0.5).clamp(0.0, (source.width.max(1) - 1) as f32);
            let sx0 = sx_f.floor() as u32;
            let sx1 = (sx0 + 1).min(source.width - 1);
            let fx = sx_f - sx0 as f32;

            let texel = |sx: u32, sy: u32, c: usize| -> f32 {
                f32::from(source.rgba[((sy * source.width + sx) as usize * 4) + c])
            };
            let bilinear = |c: usize| -> f32 {
                match flat {
                    Some(flat) => flat[c],
                    None => {
                        let top = texel(sx0, sy0, c) * (1.0 - fx) + texel(sx1, sy0, c) * fx;
                        let bottom = texel(sx0, sy1, c) * (1.0 - fx) + texel(sx1, sy1, c) * fx;
                        (top * (1.0 - fy) + bottom * fy) / 255.0
                    }
                }
            };
            // Alpha first, and bail before touching the color channels
            // at all when it's zero: this canvas is output-resolution
            // (see `try_frame`), and a layer whose footprint covers the
            // whole frame while being mostly transparent - the PAUZE
            // caption is exactly that, a few glyphs on empty space - is
            // otherwise paying for three wasted channel interpolations
            // on the overwhelming majority of its pixels.
            let src_a = bilinear(3);
            if src_a <= 0.0 {
                continue;
            }
            let src = [bilinear(0), bilinear(1), bilinear(2), src_a];
            let dst_idx = (y * canvas.width + x) as usize * 4;
            if canvas_is_empty {
                // See this function's doc comment: over a transparent
                // destination the blend below is exactly `out = src`.
                for (c, &s) in src.iter().enumerate() {
                    canvas.rgba[dst_idx + c] = (s.clamp(0.0, 1.0) * 255.0).round() as u8;
                }
                continue;
            }
            let dst_a = canvas.rgba[dst_idx + 3] as f32 / 255.0;
            let out_a = src_a + dst_a * (1.0 - src_a);
            if out_a <= 0.0001 {
                canvas.rgba[dst_idx] = 0;
                canvas.rgba[dst_idx + 1] = 0;
                canvas.rgba[dst_idx + 2] = 0;
                canvas.rgba[dst_idx + 3] = 0;
                continue;
            }
            let inv_out_a = 1.0 / out_a;
            for (c, &s) in src.iter().enumerate().take(3) {
                let d = canvas.rgba[dst_idx + c] as f32 / 255.0;
                let out = (s * src_a + d * dst_a * (1.0 - src_a)) * inv_out_a;
                canvas.rgba[dst_idx + c] = (out.clamp(0.0, 1.0) * 255.0).round() as u8;
            }
            canvas.rgba[dst_idx + 3] = (out_a.clamp(0.0, 1.0) * 255.0).round() as u8;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn solid_frame(width: u32, height: u32, rgba: [u8; 4]) -> OverlayFrame {
        let mut buf = vec![0u8; (width * height * 4) as usize];
        for px in buf.chunks_exact_mut(4) {
            px.copy_from_slice(&rgba);
        }
        OverlayFrame {
            width,
            height,
            design_size: (width, height),
            rgba: buf,
        }
    }

    /// Like `solid_frame`, but with a `design_size` different from the
    /// actual pixel buffer - stand-in for a producer that pre-scaled
    /// its own rendering down for sharper output (see
    /// `OverlayFrame::design_size`).
    fn solid_frame_pre_scaled(
        width: u32,
        height: u32,
        design_size: (u32, u32),
        rgba: [u8; 4],
    ) -> OverlayFrame {
        OverlayFrame {
            design_size,
            ..solid_frame(width, height, rgba)
        }
    }

    /// A fixed-content, single-shot source: returns `Some` exactly
    /// once, then `None` forever - enough to drive `LayeredOverlaySource`
    /// through its cache without needing real frame producers.
    struct OnceSource(Option<OverlayFrame>);
    impl OverlayFrameSource for OnceSource {
        fn try_frame(&mut self) -> Result<Option<OverlayFrame>, String> {
            Ok(self.0.take())
        }
    }

    #[test]
    fn composite_over_places_fully_opaque_source_at_default_center() {
        let mut canvas = solid_frame(100, 100, [0, 0, 0, 0]);
        let source = solid_frame(50, 50, [255, 0, 0, 255]);
        composite_over(&mut canvas, &source, OverlayPlacement::default(), false);

        let center_idx = (50 * 100 + 50) * 4;
        assert_eq!(&canvas.rgba[center_idx..center_idx + 4], &[255, 0, 0, 255]);
        assert_eq!(&canvas.rgba[0..4], &[255, 0, 0, 255]);
    }

    #[test]
    fn composite_over_respects_scale_and_offset() {
        let mut canvas = solid_frame(200, 100, [10, 10, 10, 255]);
        let source = solid_frame(20, 20, [0, 255, 0, 255]);
        let placement = OverlayPlacement {
            offset: (0.0, 0.4),
            scale: 0.5,
        };
        composite_over(&mut canvas, &source, placement, false);

        let bg_idx = (50 * 200 + 100) * 4;
        assert_eq!(&canvas.rgba[bg_idx..bg_idx + 4], &[10, 10, 10, 255]);

        let near_bottom_idx = (85 * 200 + 100) * 4;
        assert_eq!(
            &canvas.rgba[near_bottom_idx..near_bottom_idx + 4],
            &[0, 255, 0, 255]
        );
    }

    #[test]
    fn composite_over_blends_partial_alpha_source() {
        let mut canvas = solid_frame(10, 10, [0, 0, 0, 255]);
        let source = solid_frame(10, 10, [255, 255, 255, 128]);
        composite_over(&mut canvas, &source, OverlayPlacement::default(), false);

        let idx = (5 * 10 + 5) * 4;
        assert!(canvas.rgba[idx] > 100 && canvas.rgba[idx] < 155);
        assert_eq!(canvas.rgba[idx + 3], 255);
    }

    /// Regression test for the real quality bug found 2026-08-25: a
    /// scoreboard sharing a session with the PAUZE overlay had its
    /// already-sharp, pre-scaled text visibly degraded because this
    /// function resampled it with nearest-neighbor while squeezing it
    /// into the shared (differently-sized) canvas - see this fn's own
    /// doc comment. A striped black/white source, downscaled into a
    /// footprint smaller than its own pixel buffer (forcing a real
    /// resample, not a 1:1 copy), must land on some *intermediate* gray
    /// value somewhere - nearest-neighbor could only ever produce pure
    /// black or pure white pixels from this source, regardless of
    /// which exact texel any given output pixel happens to land on.
    #[test]
    fn composite_over_bilinear_interpolates_a_downscaled_striped_source() {
        // Background deliberately outside the (20, 235) range checked
        // below, so an untouched background pixel can never masquerade
        // as a genuine interpolated blend.
        let mut canvas = solid_frame(40, 40, [5, 5, 5, 255]);
        // 20x20 source: a 1px-pitch vertical stripe pattern (alternating
        // white/black every column) rather than one single hard edge -
        // an edge on literally every column makes it impossible for any
        // downscale ratio to get unlucky and only ever land exactly on
        // pixel centers, the way a single edge's exact position could.
        let mut buf = vec![0u8; 20 * 20 * 4];
        for y in 0..20usize {
            for x in 0..20usize {
                let value = if x % 2 == 0 { 255 } else { 0 };
                let idx = (y * 20 + x) * 4;
                buf[idx..idx + 4].copy_from_slice(&[value, value, value, 255]);
            }
        }
        let source = OverlayFrame {
            width: 20,
            height: 20,
            design_size: (20, 20),
            rgba: buf,
        };
        // Downscale into a footprint smaller than the source's own
        // pixel size, so this is a real resample, not a 1:1 copy.
        let placement = OverlayPlacement {
            offset: (0.0, 0.0),
            scale: 0.375,
        };
        composite_over(&mut canvas, &source, placement, false);

        // Scan every canvas pixel for at least one interpolated gray
        // value. Nearest-neighbor sampling could only ever produce pure
        // black (0) or pure white (255) from this source, regardless of
        // which pixel lands where.
        let has_interpolated_gray = canvas
            .rgba
            .chunks_exact(4)
            .any(|px| px[0] > 20 && px[0] < 235);
        assert!(
            has_interpolated_gray,
            "expected at least one interpolated gray pixel at the downscaled edge - \
             every sampled pixel was pure black or white, meaning nearest-neighbor \
             sampling is still in effect"
        );
    }

    #[test]
    fn layered_source_returns_none_until_any_layer_has_a_frame() {
        let mut layered = LayeredOverlaySource::new(
            vec![
                (Box::new(OnceSource(None)), OverlayPlacement::default()),
                (Box::new(OnceSource(None)), OverlayPlacement::default()),
            ],
            (100, 100),
        );
        assert!(layered.try_frame().unwrap().is_none());
    }

    #[test]
    fn layered_source_composites_bottom_layer_under_top_layer() {
        let bottom = solid_frame(10, 10, [0, 0, 0, 255]); // opaque black, full-frame
        let top = solid_frame(2, 2, [255, 255, 0, 255]); // opaque yellow, tiny corner-ish
        let mut layered = LayeredOverlaySource::new(
            vec![
                (
                    Box::new(OnceSource(Some(bottom))),
                    OverlayPlacement::default(),
                ),
                (
                    Box::new(OnceSource(Some(top))),
                    OverlayPlacement {
                        offset: (0.0, 0.0),
                        scale: 0.1, // tiny, stays near the center
                    },
                ),
            ],
            (10, 10),
        );
        let frame = layered.try_frame().unwrap().unwrap();
        // Corner: only the bottom (black) layer reaches there.
        assert_eq!(&frame.rgba[0..4], &[0, 0, 0, 255]);
        // Center: the top (yellow) layer should win.
        let center_idx = (5 * 10 + 5) * 4;
        assert_eq!(&frame.rgba[center_idx..center_idx + 4], &[255, 255, 0, 255]);
    }

    #[test]
    fn layered_source_keeps_showing_a_layer_once_cached_even_if_only_the_other_changes() {
        let base = solid_frame(10, 10, [1, 2, 3, 255]);
        let mut layered = LayeredOverlaySource::new(
            vec![
                (
                    Box::new(OnceSource(Some(base))),
                    OverlayPlacement::default(),
                ),
                (Box::new(OnceSource(None)), OverlayPlacement::default()), // never produces a frame
            ],
            (10, 10),
        );
        let frame = layered.try_frame().unwrap().unwrap();
        assert_eq!(&frame.rgba[0..4], &[1, 2, 3, 255]);
        // Second call: neither layer has anything new -> None (reuse).
        assert!(layered.try_frame().unwrap().is_none());
    }

    /// The `canvas_is_empty` fast path claims the straight-alpha blend
    /// collapses to a plain copy over a transparent destination - so it
    /// must produce byte-identical output to running the full blend,
    /// for partial alpha (where the reciprocal term actually matters)
    /// just as much as for opaque pixels.
    #[test]
    fn empty_canvas_fast_path_matches_the_full_blend_exactly() {
        for alpha in [255u8, 200, 128, 64, 1] {
            let source = solid_frame(9, 9, [200, 100, 50, alpha]);
            let placement = OverlayPlacement {
                offset: (0.1, -0.2),
                scale: 0.7,
            };
            let blended = {
                let mut canvas = solid_frame(32, 32, [0, 0, 0, 0]);
                composite_over(&mut canvas, &source, placement, false);
                canvas
            };
            let fast = {
                let mut canvas = solid_frame(32, 32, [0, 0, 0, 0]);
                composite_over(&mut canvas, &source, placement, true);
                canvas
            };
            assert_eq!(
                blended.rgba, fast.rgba,
                "fast path diverged from the full blend at source alpha {alpha}"
            );
        }
    }

    /// The full-canvas flat-colour row fill is an optimisation of the
    /// PAUZE fade specifically, so it must agree byte-for-byte with the
    /// general path for exactly that shape: a 1x1 source covering the
    /// whole canvas, at every alpha the fade ramps through.
    #[test]
    fn full_canvas_flat_fill_matches_the_general_path() {
        for alpha in [255u8, 200, 128, 64, 1] {
            let source = OverlayFrame {
                width: 1,
                height: 1,
                design_size: (32, 32),
                rgba: vec![0, 0, 0, alpha],
            };
            let general = {
                let mut canvas = solid_frame(32, 32, [0, 0, 0, 0]);
                composite_over(&mut canvas, &source, OverlayPlacement::default(), false);
                canvas
            };
            let filled = {
                let mut canvas = solid_frame(32, 32, [0, 0, 0, 0]);
                composite_over(&mut canvas, &source, OverlayPlacement::default(), true);
                canvas
            };
            assert_eq!(
                general.rgba, filled.rgba,
                "row fill diverged from the general path at alpha {alpha}"
            );
        }
    }

    /// A source handing over a frame identical to its previous one must
    /// not trigger a recomposite - the scoreboard runtime re-captures
    /// at a fixed rate while its content changes far more rarely, and
    /// compositing those duplicates at output resolution is what
    /// collapsed a real 2K export from 22fps to 7fps.
    #[test]
    fn an_unchanged_repeated_frame_does_not_trigger_a_recomposite() {
        let frame = solid_frame(8, 8, [1, 2, 3, 255]);
        let mut queue = std::collections::VecDeque::new();
        queue.push_back(Some(frame.clone()));
        queue.push_back(Some(frame.clone())); // byte-identical repeat
        queue.push_back(Some(solid_frame(8, 8, [9, 9, 9, 255]))); // genuinely different

        struct Queued(std::collections::VecDeque<Option<OverlayFrame>>);
        impl OverlayFrameSource for Queued {
            fn try_frame(&mut self) -> Result<Option<OverlayFrame>, String> {
                Ok(self.0.pop_front().flatten())
            }
        }

        let mut layered = LayeredOverlaySource::new(
            vec![(Box::new(Queued(queue)), OverlayPlacement::default())],
            (16, 16),
        );
        assert!(layered.try_frame().unwrap().is_some(), "first frame");
        assert!(
            layered.try_frame().unwrap().is_none(),
            "an identical repeat must be recognised as no change"
        );
        assert!(
            layered.try_frame().unwrap().is_some(),
            "a genuinely different frame must still come through"
        );
    }

    /// A fully-transparent incoming frame is dropped from the cache
    /// rather than stored - it draws nothing either way, so keeping it
    /// only costs a wasted full-footprint composite pass on every
    /// subsequent frame any *other* layer updates. (Before the canvas
    /// was pinned to the output resolution this was a correctness bug
    /// too: such a frame's size lingered in the canvas-sizing fold for
    /// the rest of the export.)
    #[test]
    fn fully_transparent_is_detected_only_when_every_pixel_has_zero_alpha() {
        assert!(is_fully_transparent(&solid_frame(4, 4, [9, 9, 9, 0])));
        assert!(!is_fully_transparent(&solid_frame(4, 4, [0, 0, 0, 255])));
        let mut one_visible_pixel = solid_frame(4, 4, [0, 0, 0, 0]);
        one_visible_pixel.rgba[3] = 1;
        assert!(!is_fully_transparent(&one_visible_pixel));
    }

    /// **The** regression test for this whole saga (SESSION_HANDOFF,
    /// 2026-08-25/26): a pre-scaled layer must be composited at exactly
    /// 1:1, with no resampling whatsoever, when the canvas is the
    /// output resolution - even when a second layer is present, and
    /// even when that layer is placed at a small `placement.scale`.
    ///
    /// The scenario, scaled down from the real 2K export that exposed
    /// it: a scoreboard whose design size is `200x100` is placed at
    /// scale `0.5` in a `200x100` output, so its real on-screen
    /// footprint is `100x50` and its producer pre-scaled its capture to
    /// exactly that. Sizing the canvas from the layers (every earlier
    /// version of this module) made the canvas `100x50` - the capture's
    /// own size - and then placed the layer *within* it at scale 0.5
    /// again, squeezing a crisp 100x50 capture into 50x25 before the
    /// GPU stretched it back out. Pinning the canvas to the output
    /// resolution is what makes the footprint and the capture agree.
    #[test]
    fn a_pre_scaled_layer_is_composited_pixel_for_pixel_alongside_another_layer() {
        // 100x50 actual pixels, 200x100 design - i.e. a producer that
        // pre-scaled its capture to its real on-screen footprint.
        let mut scoreboard = solid_frame_pre_scaled(100, 50, (200, 100), [10, 20, 30, 255]);
        // One distinct marker pixel: it can only survive byte-exact if
        // nothing resampled this layer.
        let marker_src = (5 * 100 + 10) * 4;
        scoreboard.rgba[marker_src..marker_src + 4].copy_from_slice(&[255, 0, 255, 255]);

        let mut layered = LayeredOverlaySource::new(
            vec![
                // A tiny co-active layer, standing in for the PAUZE
                // fade - its presence must not affect the scoreboard.
                (
                    Box::new(OnceSource(Some(solid_frame(1, 1, [0, 0, 0, 128])))),
                    OverlayPlacement::default(),
                ),
                (
                    Box::new(OnceSource(Some(scoreboard))),
                    OverlayPlacement {
                        offset: (0.0, 0.0),
                        scale: 0.5,
                    },
                ),
            ],
            (200, 100),
        );
        let frame = layered.try_frame().unwrap().unwrap();

        assert_eq!((frame.width, frame.height), (200, 100));
        assert_eq!(frame.design_size, (200, 100));

        // Footprint is 100x50 centered in 200x100, so it starts at
        // (50, 25) - source pixel (10, 5) must land exactly on canvas
        // pixel (60, 30), unblended and unresampled.
        let marker_dst = ((30 * 200) + 60) * 4;
        assert_eq!(
            &frame.rgba[marker_dst..marker_dst + 4],
            &[255, 0, 255, 255],
            "the pre-scaled layer was resampled instead of landing 1:1"
        );
        // ...and its immediate neighbour is the layer's own flat color,
        // not a blend of it with the marker - further proof no
        // interpolation happened.
        let neighbour_dst = ((30 * 200) + 61) * 4;
        assert_eq!(
            &frame.rgba[neighbour_dst..neighbour_dst + 4],
            &[10, 20, 30, 255]
        );
    }

    /// Regression test for the GetData-timeout crash traced to the
    /// mip-chain approach: a layer whose producer pre-scaled its own
    /// pixel buffer down (smaller `width`/`height` than `design_size`,
    /// e.g. the scoreboard renderer picking a smaller Chrome capture
    /// resolution for its current placement) must still land at the
    /// same footprint a full-size buffer would have, not get shrunk a
    /// second time by `composite_over` re-applying `placement.scale`
    /// against its already-reduced pixel size.
    #[test]
    fn composite_over_uses_design_size_not_actual_pixel_size_for_placement() {
        let full_size_canvas = {
            let mut canvas = solid_frame(100, 100, [0, 0, 0, 0]);
            let source = solid_frame(50, 50, [255, 0, 0, 255]);
            composite_over(&mut canvas, &source, OverlayPlacement::default(), false);
            canvas
        };
        let pre_scaled_canvas = {
            let mut canvas = solid_frame(100, 100, [0, 0, 0, 0]);
            // Actual pixel buffer is only 20x20 (already scaled down by
            // its producer), but its design_size is still the full
            // 50x50 the placement math should use - the composited
            // result must be pixel-identical to the full-size source
            // above, just sourced from fewer texels.
            let source = solid_frame_pre_scaled(20, 20, (50, 50), [255, 0, 0, 255]);
            composite_over(&mut canvas, &source, OverlayPlacement::default(), false);
            canvas
        };
        // Same footprint in both cases (a solid color, so nearest-
        // neighbor sampling the smaller buffer produces the exact same
        // output): center pixel and the fitted region's covered area
        // match exactly, not just visually.
        assert_eq!(full_size_canvas.rgba, pre_scaled_canvas.rgba);
    }

    /// The canvas is pinned to the output resolution, in both its
    /// actual pixels and its `design_size` - never derived from any
    /// layer's own size, however large or small that layer happens to
    /// be. `design_size == width`/`height` is what makes the GPU
    /// compositor's contain-fit resolve to 1:1 (see `try_frame`).
    #[test]
    fn canvas_is_always_the_output_size_regardless_of_layer_sizes() {
        for layer in [
            // Far smaller than the output...
            solid_frame_pre_scaled(20, 20, (1920, 1080), [255, 0, 255, 255]),
            // ...and far larger than it.
            solid_frame_pre_scaled(4000, 4000, (1920, 1080), [255, 0, 255, 255]),
        ] {
            let mut layered = LayeredOverlaySource::new(
                vec![(
                    Box::new(OnceSource(Some(layer))),
                    OverlayPlacement::default(),
                )],
                (640, 360),
            );
            let frame = layered.try_frame().unwrap().unwrap();
            assert_eq!((frame.width, frame.height), (640, 360));
            assert_eq!(frame.design_size, (640, 360));
        }
    }
}
