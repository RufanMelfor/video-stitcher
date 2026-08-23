//! "PAUZE" dip-to-black transition shown at cut-range boundaries.
//!
//! [`crate::render::overlay`] composites arbitrary RGBA graphics over the
//! stitched output without knowing anything about where they come from.
//! This module is one concrete producer: a short black card with a
//! centered caption, faded in just before a `reco_io::cut_range` boundary
//! and faded back out just after, so the otherwise-instant content jump
//! reads as an intentional transition instead of a glitch.
//!
//! The fade-in/fade-out portions are composited over frames that were
//! already going to be encoded (the last/first few seconds of the two
//! kept windows either side of the cut) - they add no extra output
//! duration. Only the fully-opaque "hold" portion in the middle has no
//! real content to draw on and is genuinely new screen time; see
//! `reco_io::cut_range::extend_for_pause_overlay` for how that is
//! carved out of the (real, just visually hidden) source footage
//! immediately following each cut.

use ab_glyph::{Font, FontRef, GlyphId, PxScale, ScaleFont, point};

use super::overlay::{OverlayFrame, OverlayFrameSource};

/// Roboto (Google, OFL-1.1) - see `assets/fonts/Roboto-OFL.txt` and
/// `THIRD_PARTY_NOTICES.md`. A variable font; `ab_glyph` reads its
/// default (Regular) instance, which is all this module needs.
static FONT_BYTES: &[u8] = include_bytes!("../../assets/fonts/Roboto-Variable.ttf");

/// Reference canvas the caption is rendered at. Small on purpose - the
/// compositor's `RgbaOverlayCompositor` scales/letterboxes this into
/// whatever the real output resolution is (same trick the scoreboard
/// overlay uses), and every active frame reallocates this buffer, so
/// keeping it modest matters for the handful of seconds it churns
/// around each cut boundary. `pub` so
/// `render::overlay_layers::LayeredOverlaySource` can size its own
/// combined canvas to match when this overlay is one of its layers.
pub const CANVAS_WIDTH: u32 = 960;
pub const CANVAS_HEIGHT: u32 = 540;

/// Settings for the transition. See the module doc for what `fade_secs`
/// and `hold_secs` each cover.
#[derive(Debug, Clone, PartialEq)]
pub struct PauseOverlayConfig {
    /// Seconds the overlay takes to fade in before a cut, and
    /// symmetrically to fade back out after it.
    pub fade_secs: f32,
    /// Seconds spent fully opaque between the two fades - the only
    /// part of the transition that lengthens the export.
    pub hold_secs: f32,
    /// Caption text, e.g. `"PAUZE"`.
    pub text: String,
}

impl PauseOverlayConfig {
    /// Build a config, rejecting non-finite/negative durations or a
    /// transition with no length at all.
    pub fn new(fade_secs: f32, hold_secs: f32, text: impl Into<String>) -> Result<Self, String> {
        if !fade_secs.is_finite() || fade_secs < 0.0 {
            return Err(format!(
                "pause overlay fade duration ({fade_secs:.2}s) must be finite and >= 0"
            ));
        }
        if !hold_secs.is_finite() || hold_secs < 0.0 {
            return Err(format!(
                "pause overlay hold duration ({hold_secs:.2}s) must be finite and >= 0"
            ));
        }
        if fade_secs + hold_secs <= 0.0 {
            return Err("pause overlay duration must be greater than 0".into());
        }
        Ok(Self {
            fade_secs,
            hold_secs,
            text: text.into(),
        })
    }

    /// Total on-screen length of one transition (both fades + the hold).
    pub fn total_secs(&self) -> f32 {
        self.fade_secs * 2.0 + self.hold_secs
    }
}

/// One cut-range boundary's fade schedule, in absolute output
/// frame-count space - the same counter `StitchSession::frame_count`
/// advances, which starts at 0 regardless of `--start-time` trimming
/// (see `reco_io::stitch_job`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PauseBoundary {
    /// First frame where alpha starts ramping 0% -> 100%.
    pub fade_out_start: u64,
    /// First frame at 100% opaque.
    pub hold_start: u64,
    /// First frame where alpha starts ramping 100% -> 0% again.
    pub hold_end: u64,
    /// First frame back at 0% (transition fully finished).
    pub fade_in_end: u64,
}

/// A single cached glyph-coverage bitmap: `coverage[y * width + x]` is
/// how much of that pixel the caption's antialiased outline covers,
/// `0.0` (background) to `1.0` (fully inside a glyph).
struct TextMask {
    width: u32,
    height: u32,
    coverage: Vec<f32>,
}

fn rasterize_text(text: &str, canvas_width: u32, canvas_height: u32, font_px: f32) -> TextMask {
    let font = FontRef::try_from_slice(FONT_BYTES).expect("embedded Roboto font must parse");
    let scale = PxScale::from(font_px);
    let scaled = font.as_scaled(scale);

    // Lay out left-to-right first to find the total advance width, so
    // the whole caption can be centered on the canvas.
    let mut ids: Vec<GlyphId> = Vec::with_capacity(text.chars().count());
    let mut cursor_x = 0.0f32;
    let mut last_id: Option<GlyphId> = None;
    for ch in text.chars() {
        let id = scaled.glyph_id(ch);
        if let Some(prev) = last_id {
            cursor_x += scaled.kern(prev, id);
        }
        cursor_x += scaled.h_advance(id);
        ids.push(id);
        last_id = Some(id);
    }
    let text_width = cursor_x;
    let text_height = scaled.ascent() - scaled.descent();
    let origin_x = (canvas_width as f32 - text_width) / 2.0;
    let origin_y = (canvas_height as f32 - text_height) / 2.0 + scaled.ascent();

    let mut coverage = vec![0.0f32; (canvas_width * canvas_height) as usize];
    let mut advance_x = 0.0f32;
    last_id = None;
    for id in ids {
        if let Some(prev) = last_id {
            advance_x += scaled.kern(prev, id);
        }
        let position = point(origin_x + advance_x, origin_y);
        if let Some(outlined) = font.outline_glyph(id.with_scale_and_position(scale, position)) {
            let bounds = outlined.px_bounds();
            outlined.draw(|x, y, c| {
                let px = bounds.min.x as i32 + x as i32;
                let py = bounds.min.y as i32 + y as i32;
                if px >= 0 && py >= 0 && (px as u32) < canvas_width && (py as u32) < canvas_height {
                    let idx = py as usize * canvas_width as usize + px as usize;
                    coverage[idx] = coverage[idx].max(c);
                }
            });
        }
        advance_x += scaled.h_advance(id);
        last_id = Some(id);
    }

    TextMask {
        width: canvas_width,
        height: canvas_height,
        coverage,
    }
}

/// [`OverlayFrameSource`] that plays the "PAUZE" transition at every
/// scheduled [`PauseBoundary`], driven purely by a self-incrementing
/// frame counter - this is called exactly once per encoded output
/// frame (see `session::frame_processing::refresh_overlay`), in the
/// same order `frame_count` advances, so the two stay in lockstep with
/// no external timestamp needed.
pub struct PauseOverlaySource {
    schedule: Vec<PauseBoundary>,
    /// Index of the earliest boundary that might still be relevant -
    /// advanced monotonically since `frame_index` only increases and
    /// `schedule` is sorted, so this never rescans from the start.
    cursor: usize,
    frame_index: u64,
    mask: TextMask,
    /// Set once alpha becomes 0 so the compositor gets one final
    /// fully-transparent frame instead of freezing on the last
    /// nonzero-alpha texture forever (`try_frame` returning `None`
    /// means "reuse the previous frame", not "clear it").
    was_active: bool,
}

impl PauseOverlaySource {
    /// `schedule` must be sorted by `fade_out_start` with no overlaps -
    /// guaranteed by construction in `reco_io::cut_range`, which builds
    /// it directly from validated, non-overlapping cut ranges.
    pub fn new(schedule: Vec<PauseBoundary>, config: &PauseOverlayConfig) -> Self {
        let mask = rasterize_text(
            &config.text,
            CANVAS_WIDTH,
            CANVAS_HEIGHT,
            CANVAS_HEIGHT as f32 * 0.28,
        );
        Self {
            schedule,
            cursor: 0,
            frame_index: 0,
            mask,
            was_active: false,
        }
    }

    /// Advance one frame and return the alpha at that frame, without
    /// building an [`OverlayFrame`] for it - for a caller compositing
    /// this overlay together with something else (e.g. reco-gui's
    /// scoreboard+pause combinator, which needs this overlay's current
    /// alpha on every frame the *scoreboard* changes too, not just the
    /// frames this overlay would call "changed" on its own) rather
    /// than using this source standalone via
    /// [`OverlayFrameSource::try_frame`].
    pub fn advance(&mut self) -> f32 {
        let frame_index = self.frame_index;
        self.frame_index += 1;
        self.alpha_at(frame_index)
    }

    /// Render this overlay's black+caption card at an arbitrary alpha
    /// (not necessarily whatever [`Self::advance`] last returned) -
    /// the non-advancing counterpart a compositing caller needs.
    pub fn render_at(&self, alpha: f32) -> OverlayFrame {
        self.render(alpha)
    }

    fn alpha_at(&mut self, frame_index: u64) -> f32 {
        while self
            .schedule
            .get(self.cursor)
            .is_some_and(|b| frame_index >= b.fade_in_end)
        {
            self.cursor += 1;
        }
        let Some(b) = self.schedule.get(self.cursor) else {
            return 0.0;
        };
        if b.fade_out_start == b.fade_in_end {
            // Degenerate (zero-length) boundary: no fade, no hold -
            // nothing to show. Kept in the schedule rather than
            // dropped so callers indexing it alongside a
            // per-boundary list (e.g. audio hold seconds) don't have
            // to special-case a shorter schedule.
            return 0.0;
        }
        if frame_index < b.fade_out_start {
            0.0
        } else if frame_index < b.hold_start {
            (frame_index - b.fade_out_start) as f32 / (b.hold_start - b.fade_out_start) as f32
        } else if frame_index < b.hold_end {
            1.0
        } else {
            1.0 - (frame_index - b.hold_end) as f32 / (b.fade_in_end - b.hold_end) as f32
        }
    }

    fn render(&self, alpha: f32) -> OverlayFrame {
        let alpha_u8 = (alpha.clamp(0.0, 1.0) * 255.0).round() as u8;
        let mut rgba = vec![0u8; self.mask.coverage.len() * 4];
        for (i, &coverage) in self.mask.coverage.iter().enumerate() {
            let v = (coverage.clamp(0.0, 1.0) * 255.0).round() as u8;
            let base = i * 4;
            rgba[base] = v;
            rgba[base + 1] = v;
            rgba[base + 2] = v;
            rgba[base + 3] = alpha_u8;
        }
        OverlayFrame {
            width: self.mask.width,
            height: self.mask.height,
            rgba,
        }
    }
}

impl OverlayFrameSource for PauseOverlaySource {
    fn try_frame(&mut self) -> Result<Option<OverlayFrame>, String> {
        let alpha = self.advance();
        if alpha <= 0.0 {
            if self.was_active {
                self.was_active = false;
                return Ok(Some(self.render(0.0)));
            }
            return Ok(None);
        }
        self.was_active = true;
        Ok(Some(self.render(alpha)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn boundary(fade_out_start: u64, fade_frames: u64, hold_frames: u64) -> PauseBoundary {
        PauseBoundary {
            fade_out_start,
            hold_start: fade_out_start + fade_frames,
            hold_end: fade_out_start + fade_frames + hold_frames,
            fade_in_end: fade_out_start + fade_frames * 2 + hold_frames,
        }
    }

    #[test]
    fn config_rejects_zero_duration() {
        assert!(PauseOverlayConfig::new(0.0, 0.0, "PAUZE").is_err());
    }

    #[test]
    fn config_rejects_negative_or_nan() {
        assert!(PauseOverlayConfig::new(-1.0, 4.0, "PAUZE").is_err());
        assert!(PauseOverlayConfig::new(3.0, f32::NAN, "PAUZE").is_err());
    }

    #[test]
    fn config_total_secs_sums_both_fades_and_hold() {
        let cfg = PauseOverlayConfig::new(3.0, 4.0, "PAUZE").unwrap();
        assert_eq!(cfg.total_secs(), 10.0);
    }

    #[test]
    fn rasterize_pauze_touches_pixels() {
        let mask = rasterize_text("PAUZE", CANVAS_WIDTH, CANVAS_HEIGHT, 150.0);
        assert!(
            mask.coverage.iter().any(|&c| c > 0.5),
            "expected at least some fully-covered glyph pixels"
        );
        // Roughly centered: the covered pixels' centroid should land near
        // the canvas center, not off in a corner.
        let mut sum_x = 0.0f64;
        let mut sum_y = 0.0f64;
        let mut total = 0.0f64;
        for y in 0..mask.height {
            for x in 0..mask.width {
                let c = mask.coverage[(y * mask.width + x) as usize] as f64;
                sum_x += c * x as f64;
                sum_y += c * y as f64;
                total += c;
            }
        }
        assert!(total > 0.0);
        let centroid_x = sum_x / total;
        let centroid_y = sum_y / total;
        assert!((centroid_x - mask.width as f64 / 2.0).abs() < mask.width as f64 * 0.15);
        assert!((centroid_y - mask.height as f64 / 2.0).abs() < mask.height as f64 * 0.15);
    }

    #[test]
    fn alpha_ramps_through_fade_hold_fade() {
        let mut source = PauseOverlaySource {
            schedule: vec![boundary(100, 10, 20)],
            cursor: 0,
            frame_index: 0,
            mask: TextMask {
                width: 1,
                height: 1,
                coverage: vec![0.0],
            },
            was_active: false,
        };
        assert_eq!(source.alpha_at(0), 0.0);
        assert_eq!(source.alpha_at(99), 0.0);
        assert_eq!(source.alpha_at(100), 0.0);
        assert!((source.alpha_at(105) - 0.5).abs() < 1e-6);
        assert_eq!(source.alpha_at(110), 1.0); // hold_start
        assert_eq!(source.alpha_at(129), 1.0); // last hold frame
        assert!((source.alpha_at(135) - 0.5).abs() < 1e-6);
        assert_eq!(source.alpha_at(140), 0.0); // fade_in_end
        assert_eq!(source.alpha_at(1000), 0.0);
    }

    #[test]
    fn try_frame_stays_none_until_active_then_sends_one_final_transparent_frame() {
        // fade_out_start=0, hold_start=2, hold_end=4, fade_in_end=6
        let mut source = PauseOverlaySource::new(
            vec![boundary(0, 2, 2)],
            &PauseOverlayConfig::new(2.0, 2.0, "PAUZE").unwrap(),
        );
        assert!(source.try_frame().unwrap().is_none()); // frame 0: alpha exactly 0.0
        assert!(source.try_frame().unwrap().is_some()); // frame 1: fading in
        assert!(source.try_frame().unwrap().is_some()); // frame 2: hold start
        assert!(source.try_frame().unwrap().is_some()); // frame 3: hold
        assert!(source.try_frame().unwrap().is_some()); // frame 4: hold end
        assert!(source.try_frame().unwrap().is_some()); // frame 5: fading out
        // frame 6: alpha back to 0 - one final Some to clear the compositor's texture.
        assert!(source.try_frame().unwrap().is_some());
        // frame 7 onward: nothing left to do, reuse whatever's there (now transparent).
        assert!(source.try_frame().unwrap().is_none());
        assert!(source.try_frame().unwrap().is_none());
    }
}
