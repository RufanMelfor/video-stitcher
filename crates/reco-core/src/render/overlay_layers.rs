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
}

impl LayeredOverlaySource {
    /// The combined canvas has no fixed size of its own - see
    /// [`Self::try_frame`] for why it's instead derived every frame
    /// from whichever registered layers currently have a cached
    /// frame, independent of any individual layer's own size, which
    /// is why each layer needs its own [`OverlayPlacement`] regardless.
    pub fn new(layers: Vec<(Box<dyn OverlayFrameSource>, OverlayPlacement)>) -> Self {
        let cache = vec![None; layers.len()];
        Self { layers, cache }
    }
}

impl OverlayFrameSource for LayeredOverlaySource {
    fn try_frame(&mut self) -> Result<Option<OverlayFrame>, String> {
        let mut any_changed = false;
        for (i, (source, _)) in self.layers.iter_mut().enumerate() {
            if let Some(frame) = source.try_frame()? {
                self.cache[i] = Some(frame);
                any_changed = true;
            }
        }
        if !any_changed {
            return Ok(None);
        }

        // Sized to the largest cached layer's actual pixel buffer, not
        // a fixed constant: a smaller layer (e.g. the PAUZE
        // transition's deliberately small 960x540 canvas, see
        // `pause_overlay`'s doc comment) then gets upscaled into this
        // canvas, while a larger layer (e.g. a scoreboard package's
        // full 1920x1080 canvas) draws pixel-for-pixel with no
        // resample at all instead of being downscaled into a smaller
        // shared canvas first - that previous fixed-small-canvas
        // behavior silently forced every scoreboard frame through a
        // lossy nearest-neighbor downscale-then-upscale round trip on
        // every export where a scoreboard and the PAUZE transition
        // were both active, visibly degrading its text.
        //
        // The canvas's own `design_size` (used below, drives where/
        // how large *this whole composited canvas* lands on the video
        // frame one level up) is instead the largest *design* size -
        // independent of a layer's actual pixel buffer, which may be
        // smaller when that layer's producer pre-scaled its own
        // rendering down for sharper output (see
        // `OverlayFrame::design_size`). Using actual pixel size there
        // instead would make the canvas's outer placement shrink right
        // along with an inner producer's quality optimization, which
        // has nothing to do with where the canvas itself should sit.
        let (width, height) = self
            .cache
            .iter()
            .flatten()
            .fold((1u32, 1u32), |(max_w, max_h), frame| {
                (max_w.max(frame.width), max_h.max(frame.height))
            });
        let (design_w, design_h) =
            self.cache
                .iter()
                .flatten()
                .fold((1u32, 1u32), |(max_w, max_h), frame| {
                    (
                        max_w.max(frame.design_size.0),
                        max_h.max(frame.design_size.1),
                    )
                });
        let mut canvas = OverlayFrame {
            width,
            height,
            design_size: (design_w, design_h),
            rgba: vec![0u8; (width as usize) * (height as usize) * 4],
        };
        for (i, (_, placement)) in self.layers.iter().enumerate() {
            if let Some(frame) = &self.cache[i] {
                composite_over(&mut canvas, frame, *placement);
            }
        }
        Ok(Some(canvas))
    }
}

/// Alpha-composites `source` onto `canvas` using the exact placement
/// math `rgba_overlay.wgsl`'s `fs_main` uses (contain-fit within
/// `canvas`'s own size, shifted/scaled by `placement`). Nearest-
/// neighbor sampling - the layers this combines update at most a few
/// times a second (a scoreboard clock tick, a cut-range fade step),
/// so a bilinear filter isn't worth the extra cost. Straight-alpha
/// "over" blend.
fn composite_over(canvas: &mut OverlayFrame, source: &OverlayFrame, placement: OverlayPlacement) {
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
    let y0 = origin_y.max(0.0).floor() as u32;
    let x1 = ((origin_x + fitted_w).min(out_w).ceil() as u32).min(canvas.width);
    let y1 = ((origin_y + fitted_h).min(out_h).ceil() as u32).min(canvas.height);

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
        let sy = ((v * source.height as f32) as u32).min(source.height - 1);
        for x in x0..x1 {
            let u = (x as f32 + 0.5 - origin_x) * inv_fitted_w;
            if !(0.0..=1.0).contains(&u) {
                continue;
            }
            let sx = ((u * source.width as f32) as u32).min(source.width - 1);
            let src_idx = (sy * source.width + sx) as usize * 4;
            let dst_idx = (y * canvas.width + x) as usize * 4;

            let src_alpha_byte = source.rgba[src_idx + 3];
            if src_alpha_byte == 0 {
                continue;
            }
            // Fast path for the overwhelmingly common case in practice
            // (a scoreboard banner, a PAUZE card - solid, fully opaque
            // graphics): `out = s` exactly when `src_a == 1.0`, since
            // `out_a` reduces to `1.0` regardless of `dst_a` - skips
            // reading the destination and the per-channel blend/divide
            // below entirely.
            if src_alpha_byte == 255 {
                canvas.rgba[dst_idx..dst_idx + 4]
                    .copy_from_slice(&source.rgba[src_idx..src_idx + 4]);
                continue;
            }
            let src_a = f32::from(src_alpha_byte) / 255.0;
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
            for c in 0..3 {
                let s = source.rgba[src_idx + c] as f32 / 255.0;
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
        composite_over(&mut canvas, &source, OverlayPlacement::default());

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
        composite_over(&mut canvas, &source, placement);

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
        composite_over(&mut canvas, &source, OverlayPlacement::default());

        let idx = (5 * 10 + 5) * 4;
        assert!(canvas.rgba[idx] > 100 && canvas.rgba[idx] < 155);
        assert_eq!(canvas.rgba[idx + 3], 255);
    }

    #[test]
    fn layered_source_returns_none_until_any_layer_has_a_frame() {
        let mut layered = LayeredOverlaySource::new(vec![
            (Box::new(OnceSource(None)), OverlayPlacement::default()),
            (Box::new(OnceSource(None)), OverlayPlacement::default()),
        ]);
        assert!(layered.try_frame().unwrap().is_none());
    }

    #[test]
    fn layered_source_composites_bottom_layer_under_top_layer() {
        let bottom = solid_frame(10, 10, [0, 0, 0, 255]); // opaque black, full-frame
        let top = solid_frame(2, 2, [255, 255, 0, 255]); // opaque yellow, tiny corner-ish
        let mut layered = LayeredOverlaySource::new(vec![
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
        ]);
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
        let mut layered = LayeredOverlaySource::new(vec![
            (
                Box::new(OnceSource(Some(base))),
                OverlayPlacement::default(),
            ),
            (Box::new(OnceSource(None)), OverlayPlacement::default()), // never produces a frame
        ]);
        let frame = layered.try_frame().unwrap().unwrap();
        assert_eq!(&frame.rgba[0..4], &[1, 2, 3, 255]);
        // Second call: neither layer has anything new -> None (reuse).
        assert!(layered.try_frame().unwrap().is_none());
    }

    /// Regression test for a real bug: the combined canvas used to be
    /// hardcoded to the PAUZE transition's own small 960x540 reference
    /// size, so a full-frame layer with a *larger* native resolution
    /// (e.g. a 1920x1080 scoreboard) got silently downscaled into it
    /// with nearest-neighbor sampling and then upscaled again by the
    /// GPU compositor - visibly degrading text. The canvas must now be
    /// sized to the largest active layer instead, so that layer draws
    /// pixel-for-pixel with no resample at all.
    #[test]
    fn layered_source_sizes_canvas_to_the_largest_layer_and_draws_it_unscaled() {
        let small = solid_frame(4, 4, [0, 0, 0, 255]); // stand-in for the PAUZE card
        let mut large = solid_frame(8, 8, [10, 20, 30, 255]); // stand-in for a scoreboard frame
        // A single distinct marker pixel: only survives untouched if the
        // large layer is composited without any resampling.
        let marker_idx = (3 * 8 + 5) * 4;
        large.rgba[marker_idx..marker_idx + 4].copy_from_slice(&[255, 0, 255, 255]);

        let mut layered = LayeredOverlaySource::new(vec![
            (
                Box::new(OnceSource(Some(small))),
                OverlayPlacement::default(),
            ),
            (
                Box::new(OnceSource(Some(large))),
                OverlayPlacement::default(),
            ),
        ]);
        let frame = layered.try_frame().unwrap().unwrap();

        // Canvas grew to the larger layer's own resolution, not the
        // smaller layer's.
        assert_eq!((frame.width, frame.height), (8, 8));
        // The large layer's marker pixel survived exactly, at the same
        // coordinates it was drawn at - proof it wasn't resampled.
        assert_eq!(&frame.rgba[marker_idx..marker_idx + 4], &[255, 0, 255, 255]);
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
            composite_over(&mut canvas, &source, OverlayPlacement::default());
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
            composite_over(&mut canvas, &source, OverlayPlacement::default());
            canvas
        };
        // Same footprint in both cases (a solid color, so nearest-
        // neighbor sampling the smaller buffer produces the exact same
        // output): center pixel and the fitted region's covered area
        // match exactly, not just visually.
        assert_eq!(full_size_canvas.rgba, pre_scaled_canvas.rgba);
    }

    /// The combined canvas's own `design_size` must track each layer's
    /// *design* size, not its actual (possibly pre-scaled) pixel
    /// buffer - otherwise a scoreboard layer that shrank its own pixel
    /// buffer for sharper rendering would also shrink where/how large
    /// the whole composited canvas lands on the video frame one level
    /// up, which has nothing to do with that producer-side optimization.
    #[test]
    fn canvas_design_size_tracks_largest_layer_design_size_not_pixel_size() {
        let pre_scaled = solid_frame_pre_scaled(20, 20, (1920, 1080), [255, 0, 255, 255]);
        let mut layered = LayeredOverlaySource::new(vec![(
            Box::new(OnceSource(Some(pre_scaled))),
            OverlayPlacement::default(),
        )]);
        let frame = layered.try_frame().unwrap().unwrap();
        // Actual pixel buffer stays small (the canvas doesn't need to
        // be any bigger than its one, already-small, layer)...
        assert_eq!((frame.width, frame.height), (20, 20));
        // ...but the canvas's design_size reports the layer's full
        // design resolution, for the outer compositor's own placement
        // math to use.
        assert_eq!(frame.design_size, (1920, 1080));
    }
}
