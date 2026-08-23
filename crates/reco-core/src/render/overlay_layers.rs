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
    canvas_size: (u32, u32),
    layers: Vec<(Box<dyn OverlayFrameSource>, OverlayPlacement)>,
    cache: Vec<Option<OverlayFrame>>,
}

impl LayeredOverlaySource {
    /// `canvas_size` is the combined output's own reference-canvas
    /// size (letterboxed/scaled into the real output resolution by
    /// the compositor, same as any other [`OverlayFrame`]) -
    /// independent of any individual layer's own size, which is why
    /// each layer needs its own [`OverlayPlacement`] regardless.
    pub fn new(
        canvas_size: (u32, u32),
        layers: Vec<(Box<dyn OverlayFrameSource>, OverlayPlacement)>,
    ) -> Self {
        let cache = vec![None; layers.len()];
        Self {
            canvas_size,
            layers,
            cache,
        }
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

        let (width, height) = self.canvas_size;
        let mut canvas = OverlayFrame {
            width,
            height,
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
    let (ref_w, ref_h) = (source.width as f32, source.height as f32);
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

    for y in y0..y1 {
        let v = (y as f32 + 0.5 - origin_y) / fitted_h;
        // Inclusive at 1.0 to match `rgba_overlay.wgsl`'s own bounds
        // check (`overlay_uv.y > 1.0` rejects, so `== 1.0` is kept) -
        // an exclusive `..1.0` range here would drop a sliver of
        // pixels right at an overlay's fitted edge that the shader
        // does draw.
        if !(0.0..=1.0).contains(&v) {
            continue;
        }
        let sy = ((v * ref_h) as u32).min(source.height - 1);
        for x in x0..x1 {
            let u = (x as f32 + 0.5 - origin_x) / fitted_w;
            if !(0.0..=1.0).contains(&u) {
                continue;
            }
            let sx = ((u * ref_w) as u32).min(source.width - 1);
            let src_idx = (sy * source.width + sx) as usize * 4;
            let dst_idx = (y * canvas.width + x) as usize * 4;

            let src_a = source.rgba[src_idx + 3] as f32 / 255.0;
            if src_a <= 0.0 {
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
            for c in 0..3 {
                let s = source.rgba[src_idx + c] as f32 / 255.0;
                let d = canvas.rgba[dst_idx + c] as f32 / 255.0;
                let out = (s * src_a + d * dst_a * (1.0 - src_a)) / out_a;
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
            rgba: buf,
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
        let mut layered = LayeredOverlaySource::new(
            (10, 10),
            vec![
                (Box::new(OnceSource(None)), OverlayPlacement::default()),
                (Box::new(OnceSource(None)), OverlayPlacement::default()),
            ],
        );
        assert!(layered.try_frame().unwrap().is_none());
    }

    #[test]
    fn layered_source_composites_bottom_layer_under_top_layer() {
        let bottom = solid_frame(10, 10, [0, 0, 0, 255]); // opaque black, full-frame
        let top = solid_frame(2, 2, [255, 255, 0, 255]); // opaque yellow, tiny corner-ish
        let mut layered = LayeredOverlaySource::new(
            (10, 10),
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
            (10, 10),
            vec![
                (
                    Box::new(OnceSource(Some(base))),
                    OverlayPlacement::default(),
                ),
                (Box::new(OnceSource(None)), OverlayPlacement::default()), // never produces a frame
            ],
        );
        let frame = layered.try_frame().unwrap().unwrap();
        assert_eq!(&frame.rgba[0..4], &[1, 2, 3, 255]);
        // Second call: neither layer has anything new -> None (reuse).
        assert!(layered.try_frame().unwrap().is_none());
    }
}
