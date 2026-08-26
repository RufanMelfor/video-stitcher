//! Standalone benchmark: what does `LayeredOverlaySource`'s CPU
//! compositing cost now that the combined canvas is allocated at the
//! session's real output resolution (see that module's `try_frame`)?
//! Measures the two shapes that actually occur in an export with both
//! a scoreboard and the "PAUZE" cut-range transition attached:
//!
//! - **transition**: the full-frame PAUZE caption is visible, so a
//!   mostly-transparent full-frame layer is blended on every call. This
//!   is the expensive case, and it only runs while a fade is actually
//!   ramping (`pause_overlay`'s sources return `None` for the constant
//!   -alpha hold, so it is not paid per frame of the hold).
//! - **steady**: no transition in progress, only the scoreboard layer
//!   has content - the cost is dominated by allocating and zeroing the
//!   canvas, since the scoreboard's own footprint is a small fraction
//!   of the frame.
//!
//! Not part of the crate's test suite -
//! `cargo run -p reco-core --example composite_bench --release`.

use std::time::Instant;

use reco_core::render::overlay::{OverlayFrame, OverlayFrameSource, OverlayPlacement};
use reco_core::render::overlay_layers::LayeredOverlaySource;

const ITERATIONS: u32 = 500;

struct RepeatingSource {
    frame: OverlayFrame,
    remaining: u32,
}
impl OverlayFrameSource for RepeatingSource {
    fn try_frame(&mut self) -> Result<Option<OverlayFrame>, String> {
        if self.remaining == 0 {
            return Ok(None);
        }
        self.remaining -= 1;
        // Vary one pixel per call so every frame counts as genuinely
        // changed: `LayeredOverlaySource` skips recompositing a frame
        // byte-identical to the previous one, which is the right thing
        // in production but would leave this benchmark measuring a
        // single composite instead of the per-frame cost it exists to
        // report.
        let mut frame = self.frame.clone();
        frame.rgba[0] = self.remaining as u8;
        Ok(Some(frame))
    }
}

/// Never produces anything - stands in for a layer whose transition
/// has finished (or not started), which is the steady state.
struct SilentSource;
impl OverlayFrameSource for SilentSource {
    fn try_frame(&mut self) -> Result<Option<OverlayFrame>, String> {
        Ok(None)
    }
}

fn solid(width: u32, height: u32, rgba: [u8; 4]) -> OverlayFrame {
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

/// A 960x540 caption-shaped layer: transparent except for a horizontal
/// band, matching how little of `pause_overlay`'s caption mask is
/// actually opaque (the `src_a <= 0.0` early-out in `composite_over` is
/// what makes this cheap, so a fully-opaque stand-in would overstate
/// the cost badly).
fn caption_like() -> OverlayFrame {
    let (w, h) = (960u32, 540u32);
    let mut buf = vec![0u8; (w * h * 4) as usize];
    for y in (h / 2 - 40)..(h / 2 + 40) {
        for x in 0..w {
            let idx = ((y * w + x) * 4) as usize;
            buf[idx..idx + 4].copy_from_slice(&[255, 255, 255, 255]);
        }
    }
    OverlayFrame {
        width: w,
        height: h,
        design_size: (w, h),
        rgba: buf,
    }
}

fn bench(label: &str, output_size: (u32, u32), transition_active: bool) {
    // A scoreboard pre-scaled to its real on-screen footprint at
    // placement scale 0.35, the way the runtime actually sizes its
    // capture.
    let placement = OverlayPlacement {
        offset: (0.0, 0.35),
        scale: 0.35,
    };
    let design = (1920u32, 1080u32);
    let scale =
        reco_core::render::overlay::contain_fit_render_scale(design, output_size, placement);
    let capture_w = (design.0 as f32 * scale).round().max(1.0) as u32;
    let capture_h = (design.1 as f32 * scale).round().max(1.0) as u32;
    let mut scoreboard = solid(capture_w, capture_h, [10, 20, 30, 200]);
    scoreboard.design_size = design;

    let fade: Box<dyn OverlayFrameSource> = if transition_active {
        Box::new(RepeatingSource {
            frame: solid(1, 1, [0, 0, 0, 200]),
            remaining: ITERATIONS,
        })
    } else {
        Box::new(SilentSource)
    };
    let caption: Box<dyn OverlayFrameSource> = if transition_active {
        Box::new(RepeatingSource {
            frame: caption_like(),
            remaining: ITERATIONS,
        })
    } else {
        Box::new(SilentSource)
    };

    let mut layered = LayeredOverlaySource::new(
        vec![
            (fade, OverlayPlacement::default()),
            (caption, OverlayPlacement::default()),
            (
                Box::new(RepeatingSource {
                    frame: scoreboard,
                    remaining: ITERATIONS,
                }),
                placement,
            ),
        ],
        output_size,
    );

    let t0 = Instant::now();
    let mut frames = 0u32;
    while let Ok(Some(frame)) = layered.try_frame() {
        std::hint::black_box(&frame);
        frames += 1;
    }
    let elapsed = t0.elapsed();
    println!(
        "{label}: {frames} frames, {:.2} ms total, {:.4} ms/frame",
        elapsed.as_secs_f64() * 1000.0,
        elapsed.as_secs_f64() * 1000.0 / f64::from(frames.max(1))
    );
}

fn main() {
    bench("1080p, transition ramping", (1920, 1080), true);
    bench("1080p, steady (scoreboard only)", (1920, 1080), false);
    bench("2K, transition ramping", (2560, 1440), true);
    bench("2K, steady (scoreboard only)", (2560, 1440), false);
    bench("4K, transition ramping", (3840, 2160), true);
    bench("4K, steady (scoreboard only)", (3840, 2160), false);
}
