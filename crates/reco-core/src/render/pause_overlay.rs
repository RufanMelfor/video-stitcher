//! "PAUZE" dip-to-black transition shown at cut-range boundaries.
//!
//! [`crate::render::overlay`] composites arbitrary RGBA graphics over the
//! stitched output without knowing anything about where they come from.
//! This module is one concrete producer: a black dip-to-black fade, with
//! a centered caption faded in alongside it, shown just before a
//! `reco_io::cut_range` boundary and faded back out just after, so the
//! otherwise-instant content jump reads as an intentional transition
//! instead of a glitch.
//!
//! The fade-in/fade-out portions are composited over frames that were
//! already going to be encoded (the last/first few seconds of the two
//! kept windows either side of the cut) - they add no extra output
//! duration. Only the fully-opaque "hold" portion in the middle has no
//! real content to draw on and is genuinely new screen time; see
//! `reco_io::cut_range::extend_for_pause_overlay` for how that is
//! carved out of the (real, just visually hidden) source footage
//! immediately following each cut.
//!
//! **Two independent [`OverlayFrameSource`]s, not one combined
//! texture.** [`PauseFadeSource`] (the black dip, covering the whole
//! frame) and [`PauseCaptionSource`] (the "PAUZE" text) share the exact
//! same alpha ramp but are rendered and uploaded separately, via
//! [`build_layers`]. Two consequences of splitting them this way:
//!
//! - The fade is a single flat color over the whole frame - a 1x1
//!   texture loses literally nothing at any resolution or placement,
//!   so it never needs more than that.
//! - The caption is the only part that benefits from real pixels, and
//!   [`build_layers`] sizes its actual pixel buffer using
//!   [`contain_fit_render_scale`] against the export's own output
//!   resolution - the same trick `reco_scoreboard::runtime` uses to
//!   size its Chrome capture - but **capped at the design canvas size**
//!   (960x540). Unlike the scoreboard's small banner, the caption
//!   covers most of the frame at the default placement, so letting it
//!   scale past that cap would re-render a multi-megapixel buffer every
//!   single frame of the transition (a real, measured export slowdown
//!   caught on a live 2K export - see this crate's own git history).
//!   The cap still lets it shrink *below* 960x540 for a smaller output,
//!   which is free (strictly fewer pixels than the capped case).
//!
//! Before this split, one combined black+caption card was rendered at a
//! fixed 960x540 regardless of the export's actual output size. That
//! was fine on its own (a single GPU bilinear sample either way), but
//! became a real bug once a scoreboard shared a single overlay texture
//! slot with it via `render::overlay_layers::LayeredOverlaySource`:
//! that combiner sizes its shared canvas to the *largest* active
//! layer's actual pixel buffer, so the fixed-960x540 card silently
//! capped the canvas there even when the export's real output
//! resolution (and the scoreboard's own, correctly pre-scaled capture)
//! was much higher - forcing the scoreboard's already-correctly-sized,
//! crisp capture through a lossy downscale-then-upscale round trip that
//! got worse the higher the export resolution. See SESSION_HANDOFF's
//! 2026-08-26 entry for the investigation that found this.

use ab_glyph::{Font, FontRef, GlyphId, PxScale, ScaleFont, point};

use super::overlay::{
    OverlayFrame, OverlayFrameSource, OverlayPlacement, contain_fit_render_scale,
};

/// Roboto (Google, OFL-1.1) - see `assets/fonts/Roboto-OFL.txt` and
/// `THIRD_PARTY_NOTICES.md`. A variable font; `ab_glyph` reads its
/// default (Regular) instance, which is all this module needs.
static FONT_BYTES: &[u8] = include_bytes!("../../assets/fonts/Roboto-Variable.ttf");

/// Reference (design) canvas the whole transition's layout is computed
/// against - drives *placement* only (via [`OverlayFrame::design_size`]),
/// not how many actual pixels either layer is rendered at. Both layers
/// cover the full frame at the default placement, same as before this
/// module's fade/caption split.
pub const CANVAS_WIDTH: u32 = 960;
pub const CANVAS_HEIGHT: u32 = 540;

/// Floor for the caption's actual rendered resolution, regardless of
/// how small [`contain_fit_render_scale`] computes - keeps "PAUZE"
/// legible instead of collapsing toward an unreadably coarse mask for
/// a pathologically small output size (e.g. a tiny live-preview
/// viewport).
const MIN_CAPTION_HEIGHT_PX: u32 = 96;

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

/// Alpha-ramp bookkeeping shared by [`PauseFadeSource`] and
/// [`PauseCaptionSource`] - each owns its own independent `Schedule`
/// (built from the same boundary list), not a handle to one shared
/// instance. That works without any locking because
/// `LayeredOverlaySource::try_frame` calls `try_frame` on every
/// registered layer exactly once per output frame, in order - two
/// independent counters advancing once per identical call cadence stay
/// perfectly in lockstep with each other for the life of the export,
/// same as two clocks ticking off the same metronome.
#[derive(Clone)]
struct Schedule {
    boundaries: Vec<PauseBoundary>,
    cursor: usize,
    frame_index: u64,
}

impl Schedule {
    fn new(boundaries: Vec<PauseBoundary>) -> Self {
        Self {
            boundaries,
            cursor: 0,
            frame_index: 0,
        }
    }

    /// Advance one frame and return the alpha at that frame.
    fn advance(&mut self) -> f32 {
        let frame_index = self.frame_index;
        self.frame_index += 1;
        self.alpha_at(frame_index)
    }

    fn alpha_at(&mut self, frame_index: u64) -> f32 {
        while self
            .boundaries
            .get(self.cursor)
            .is_some_and(|b| frame_index >= b.fade_in_end)
        {
            self.cursor += 1;
        }
        let Some(b) = self.boundaries.get(self.cursor) else {
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

/// Build the transition's two overlay layers, ready to register with
/// `reco_io::stitch_job::StitchJob` alongside any other layer (e.g. a
/// scoreboard) - always push the fade first, the caption second, so
/// the caption draws on top of the fade (and anything registered after
/// these two, like a scoreboard, draws on top of both).
///
/// `output_size` is the export's actual output resolution - used only
/// to pick the caption's actual pixel resolution (see this module's
/// doc comment); the fade never needs it, a flat color has no
/// resolution to get right.
pub fn build_layers(
    boundaries: Vec<PauseBoundary>,
    config: &PauseOverlayConfig,
    output_size: (u32, u32),
) -> (Box<dyn OverlayFrameSource>, Box<dyn OverlayFrameSource>) {
    let fade = Box::new(PauseFadeSource {
        schedule: Schedule::new(boundaries.clone()),
        was_active: false,
        last_alpha: None,
    });
    // Capped at 1.0: unlike the scoreboard's own small banner (where
    // `contain_fit_render_scale` picks a real target resolution to
    // pre-scale *down* to), the caption already covers most of the
    // frame at the default placement, so letting this scale go above
    // 1.0 would render it at up to the *full output resolution* (e.g.
    // 2560x1440 at 2K) - re-filling that many pixels every single
    // frame for the whole transition (including the entire, possibly
    // multi-second, constant-alpha hold) is real, synchronous per-frame
    // CPU work injected into the frame loop, not just a one-time cost.
    // Measured as a real, severe export slowdown on a live 2K export -
    // never repeat this without capping. 960x540 (the pre-split
    // fixed size) was already known to cost nothing worth noticing, so
    // capping there costs nothing while still shrinking proportionally
    // *below* that for a smaller output where it helps.
    let render_scale = contain_fit_render_scale(
        (CANVAS_WIDTH, CANVAS_HEIGHT),
        output_size,
        OverlayPlacement::default(),
    )
    .min(1.0);
    let caption_h = ((CANVAS_HEIGHT as f32 * render_scale).round() as u32)
        .max(MIN_CAPTION_HEIGHT_PX)
        .max(1);
    let caption_w = ((CANVAS_WIDTH as f32 * render_scale).round() as u32)
        .max(MIN_CAPTION_HEIGHT_PX * CANVAS_WIDTH / CANVAS_HEIGHT)
        .max(1);
    let mask = rasterize_text(&config.text, caption_w, caption_h, caption_h as f32 * 0.28);
    let caption = Box::new(PauseCaptionSource {
        schedule: Schedule::new(boundaries),
        mask,
        was_active: false,
        last_alpha: None,
    });
    (fade, caption)
}

/// The black dip-to-black fade, covering the whole frame. A single
/// flat color regardless of alpha - see this module's doc comment for
/// why its actual pixel buffer is always 1x1.
struct PauseFadeSource {
    schedule: Schedule,
    /// Set once alpha becomes 0 so the compositor gets one final
    /// fully-transparent frame instead of freezing on the last
    /// nonzero-alpha texture forever (`try_frame` returning `None`
    /// means "reuse the previous frame", not "clear it").
    was_active: bool,
    /// Alpha of the last frame actually emitted - see
    /// [`unchanged_alpha`].
    last_alpha: Option<f32>,
}

/// Whether this frame's alpha is identical to the previously emitted
/// one, meaning the frame it would produce is byte-identical to the one
/// already cached downstream and `try_frame` should return `None`
/// ("reuse the previous frame") instead.
///
/// This matters for more than avoiding a redundant buffer fill: a
/// `Some` return from *any* layer forces
/// `LayeredOverlaySource::try_frame` to recomposite every layer into a
/// freshly allocated, output-resolution canvas. Without this, the
/// fully-opaque hold portion of a transition - potentially several
/// seconds, where by definition nothing changes - would pay that cost
/// on every single frame. With it, the composite only runs when
/// something genuinely changed (the alpha ramping during a fade, or
/// another layer such as a scoreboard clock ticking over).
fn unchanged_alpha(last: Option<f32>, alpha: f32) -> bool {
    last == Some(alpha)
}

impl OverlayFrameSource for PauseFadeSource {
    fn try_frame(&mut self) -> Result<Option<OverlayFrame>, String> {
        let alpha = self.schedule.advance();
        if alpha <= 0.0 {
            if self.was_active {
                self.was_active = false;
                self.last_alpha = Some(0.0);
                return Ok(Some(render_fade(0.0)));
            }
            return Ok(None);
        }
        if unchanged_alpha(self.last_alpha, alpha) {
            return Ok(None);
        }
        self.was_active = true;
        self.last_alpha = Some(alpha);
        Ok(Some(render_fade(alpha)))
    }
}

fn render_fade(alpha: f32) -> OverlayFrame {
    let alpha_u8 = (alpha.clamp(0.0, 1.0) * 255.0).round() as u8;
    OverlayFrame {
        width: 1,
        height: 1,
        design_size: (CANVAS_WIDTH, CANVAS_HEIGHT),
        rgba: vec![0, 0, 0, alpha_u8],
    }
}

/// The "PAUZE" caption text, transparent everywhere outside the glyph
/// outlines - drawn on top of [`PauseFadeSource`]'s black, on the same
/// alpha ramp, but as an independent layer/texture. See this module's
/// doc comment for why its actual pixel resolution is pre-scaled to
/// the export's output resolution instead of a fixed constant.
struct PauseCaptionSource {
    schedule: Schedule,
    mask: TextMask,
    was_active: bool,
    /// See [`unchanged_alpha`].
    last_alpha: Option<f32>,
}

impl OverlayFrameSource for PauseCaptionSource {
    fn try_frame(&mut self) -> Result<Option<OverlayFrame>, String> {
        let alpha = self.schedule.advance();
        if alpha <= 0.0 {
            if self.was_active {
                self.was_active = false;
                self.last_alpha = Some(0.0);
                return Ok(Some(render_caption(&self.mask, 0.0)));
            }
            return Ok(None);
        }
        if unchanged_alpha(self.last_alpha, alpha) {
            return Ok(None);
        }
        self.was_active = true;
        self.last_alpha = Some(alpha);
        Ok(Some(render_caption(&self.mask, alpha)))
    }
}

fn render_caption(mask: &TextMask, alpha: f32) -> OverlayFrame {
    let alpha_u8 = (alpha.clamp(0.0, 1.0) * 255.0).round() as u8;
    let mut rgba = vec![0u8; mask.coverage.len() * 4];
    for (i, &coverage) in mask.coverage.iter().enumerate() {
        let base = i * 4;
        // White text; per-pixel alpha is the glyph coverage scaled by
        // the transition's current alpha - background pixels (coverage
        // 0.0) end up fully transparent instead of the flat black the
        // combined card used to paint there (that's `PauseFadeSource`'s
        // job now).
        rgba[base] = 255;
        rgba[base + 1] = 255;
        rgba[base + 2] = 255;
        rgba[base + 3] = ((coverage.clamp(0.0, 1.0) * alpha_u8 as f32).round()) as u8;
    }
    OverlayFrame {
        width: mask.width,
        height: mask.height,
        design_size: (CANVAS_WIDTH, CANVAS_HEIGHT),
        rgba,
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
        let mut schedule = Schedule::new(vec![boundary(100, 10, 20)]);
        assert_eq!(schedule.alpha_at(0), 0.0);
        assert_eq!(schedule.alpha_at(99), 0.0);
        assert_eq!(schedule.alpha_at(100), 0.0);
        assert!((schedule.alpha_at(105) - 0.5).abs() < 1e-6);
        assert_eq!(schedule.alpha_at(110), 1.0); // hold_start
        assert_eq!(schedule.alpha_at(129), 1.0); // last hold frame
        assert!((schedule.alpha_at(135) - 0.5).abs() < 1e-6);
        assert_eq!(schedule.alpha_at(140), 0.0); // fade_in_end
        assert_eq!(schedule.alpha_at(1000), 0.0);
    }

    #[test]
    fn two_independent_schedules_stay_in_lockstep_when_advanced_together() {
        // Simulates `LayeredOverlaySource` calling `try_frame` on both
        // layers once per output frame, in order - the whole premise
        // this module's split relies on (see `Schedule`'s doc comment).
        let boundaries = vec![boundary(2, 3, 4)];
        let mut fade = Schedule::new(boundaries.clone());
        let mut caption = Schedule::new(boundaries);
        for _ in 0..30 {
            assert_eq!(fade.advance(), caption.advance());
        }
    }

    #[test]
    fn try_frame_stays_none_until_active_then_sends_one_final_transparent_frame() {
        // fade_out_start=0, hold_start=2, hold_end=4, fade_in_end=6
        let (mut fade, mut caption) = build_layers(
            vec![boundary(0, 2, 2)],
            &PauseOverlayConfig::new(2.0, 2.0, "PAUZE").unwrap(),
            (1920, 1080),
        );
        for source in [&mut fade, &mut caption] {
            assert!(source.try_frame().unwrap().is_none()); // frame 0: alpha exactly 0.0
            assert!(source.try_frame().unwrap().is_some()); // frame 1: fading in (0.5)
            assert!(source.try_frame().unwrap().is_some()); // frame 2: hold start (1.0)
            // Frames 3 and 4 are still at alpha 1.0 - byte-identical to
            // frame 2, so `None` ("reuse the previous frame"). See
            // `unchanged_alpha` for why emitting these anyway would be
            // far more than a wasted buffer fill.
            assert!(source.try_frame().unwrap().is_none()); // frame 3: hold
            assert!(source.try_frame().unwrap().is_none()); // frame 4: hold end
            assert!(source.try_frame().unwrap().is_some()); // frame 5: fading out (0.5)
            // frame 6: alpha back to 0 - one final Some to clear the compositor's texture.
            assert!(source.try_frame().unwrap().is_some());
            // frame 7 onward: nothing left to do, reuse whatever's there (now transparent).
            assert!(source.try_frame().unwrap().is_none());
            assert!(source.try_frame().unwrap().is_none());
        }
    }

    /// The constant-alpha hold is the longest part of a transition and
    /// by definition never changes - it must not emit a single frame
    /// after the one that starts it, or every frame of it forces a
    /// full-canvas recomposite downstream (see `unchanged_alpha`).
    #[test]
    fn a_long_hold_emits_exactly_one_frame_not_one_per_frame() {
        // fade_out_start=0, hold_start=1, hold_end=61, fade_in_end=62:
        // a 60-frame hold between two single-frame fades.
        let (mut fade, _caption) = build_layers(
            vec![boundary(0, 1, 60)],
            &PauseOverlayConfig::new(1.0, 60.0, "PAUZE").unwrap(),
            (1920, 1080),
        );
        let mut emitted = 0;
        // Frames 0..=60 covers the ramp-in and the entire hold.
        for _ in 0..=60 {
            if fade.try_frame().unwrap().is_some() {
                emitted += 1;
            }
        }
        assert_eq!(
            emitted, 1,
            "expected exactly one emitted frame (the start of the hold) across a \
             60-frame constant-alpha hold, got {emitted}"
        );
    }

    #[test]
    fn fade_frame_is_always_a_single_pixel_regardless_of_output_size() {
        let (mut fade, _caption) = build_layers(
            vec![boundary(0, 1, 1)],
            &PauseOverlayConfig::new(1.0, 1.0, "PAUZE").unwrap(),
            (3840, 2160),
        );
        fade.try_frame().unwrap(); // frame 0, alpha 0.0 -> None
        let frame = fade.try_frame().unwrap().unwrap(); // frame 1, fading in
        assert_eq!((frame.width, frame.height), (1, 1));
        assert_eq!(frame.design_size, (CANVAS_WIDTH, CANVAS_HEIGHT));
        // Black, non-zero alpha.
        assert_eq!(&frame.rgba[0..3], &[0, 0, 0]);
        assert!(frame.rgba[3] > 0);
    }

    fn caption_dims(mut source: Box<dyn OverlayFrameSource>) -> (u32, u32) {
        // frame 0 is alpha 0.0 (None); force a real frame via frame 1.
        source.try_frame().unwrap();
        let frame = source.try_frame().unwrap().unwrap();
        (frame.width, frame.height)
    }

    /// Regression test for a real, measured export slowdown: an earlier
    /// version of this function let the caption's actual resolution
    /// grow past `CANVAS_WIDTH`/`HEIGHT` for any output resolution
    /// above 960x540 (i.e. almost every real export), up to the full
    /// output resolution - meaning `render_caption` re-filled a
    /// multi-megapixel buffer every single frame for the whole
    /// transition, including a possibly multi-second constant-alpha
    /// hold. 2K and 4K (both well above 960x540) must render the
    /// caption at exactly the pre-existing, known-cheap 960x540, not
    /// scale up with output resolution.
    #[test]
    fn caption_resolution_never_exceeds_the_design_canvas_size() {
        for output_size in [(1920, 1080), (2560, 1440), (3840, 2160)] {
            let (_fade, caption) = build_layers(
                vec![boundary(0, 1, 1)],
                &PauseOverlayConfig::new(1.0, 1.0, "PAUZE").unwrap(),
                output_size,
            );
            let (w, h) = caption_dims(caption);
            assert_eq!(
                (w, h),
                (CANVAS_WIDTH, CANVAS_HEIGHT),
                "at output {output_size:?}, expected the caption capped at the design \
                 canvas size ({CANVAS_WIDTH}x{CANVAS_HEIGHT}), got {w}x{h}"
            );
        }
    }

    /// Below the design canvas size, the caption still shrinks
    /// proportionally with output resolution (harmless - strictly
    /// fewer pixels than the capped case above, never more).
    #[test]
    fn caption_resolution_shrinks_below_the_cap_for_a_small_output() {
        let (_fade, caption) = build_layers(
            vec![boundary(0, 1, 1)],
            &PauseOverlayConfig::new(1.0, 1.0, "PAUZE").unwrap(),
            (480, 270),
        );
        let (w, h) = caption_dims(caption);
        assert!(
            w < CANVAS_WIDTH && h < CANVAS_HEIGHT,
            "expected a smaller-than-design caption at a small output, got {w}x{h}"
        );
    }
}
