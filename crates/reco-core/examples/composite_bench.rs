//! Standalone benchmark: how much does `LayeredOverlaySource`'s CPU
//! compositing cost at a 1920x1080 combined canvas (current behavior -
//! sized to the largest active layer) vs the old fixed 960x540 canvas?
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
        Ok(Some(self.frame.clone()))
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
        rgba: buf,
    }
}

fn main() {
    // Mirrors the real shapes: a 1920x1080 scoreboard layer (opaque
    // banner covering roughly a third of the frame) over a 960x540
    // PAUZE layer (the old fixed canvas size).
    let scoreboard = solid(1920, 1080, [10, 20, 30, 200]);
    let pauze = solid(960, 540, [0, 0, 0, 255]);

    let mut current = LayeredOverlaySource::new(vec![
        (
            Box::new(RepeatingSource {
                frame: pauze.clone(),
                remaining: ITERATIONS,
            }),
            OverlayPlacement::default(),
        ),
        (
            Box::new(RepeatingSource {
                frame: scoreboard.clone(),
                remaining: ITERATIONS,
            }),
            OverlayPlacement {
                offset: (0.0, 0.35),
                scale: 0.35,
            },
        ),
    ]);
    let t0 = Instant::now();
    let mut frames = 0u32;
    while let Ok(Some(frame)) = current.try_frame() {
        std::hint::black_box(&frame);
        frames += 1;
    }
    let elapsed = t0.elapsed();
    println!(
        "current (canvas grows to largest layer, 1920x1080 here): {frames} frames, \
         {:.2} ms total, {:.4} ms/frame",
        elapsed.as_secs_f64() * 1000.0,
        elapsed.as_secs_f64() * 1000.0 / f64::from(frames.max(1))
    );

    // Old behavior: canvas fixed at the PAUZE layer's own small
    // reference size regardless of what else is attached - reproduced
    // here by only ever feeding it frames no bigger than that, so the
    // canvas never grows past 960x540.
    let mut old = LayeredOverlaySource::new(vec![
        (
            Box::new(RepeatingSource {
                frame: pauze.clone(),
                remaining: ITERATIONS,
            }),
            OverlayPlacement::default(),
        ),
        (
            Box::new(RepeatingSource {
                frame: pauze,
                remaining: ITERATIONS,
            }),
            OverlayPlacement {
                offset: (0.0, 0.35),
                scale: 0.35,
            },
        ),
    ]);
    let t0 = Instant::now();
    let mut frames = 0u32;
    while let Ok(Some(frame)) = old.try_frame() {
        std::hint::black_box(&frame);
        frames += 1;
    }
    let elapsed = t0.elapsed();
    println!(
        "old-equivalent (canvas fixed at 960x540): {frames} frames, {:.2} ms total, \
         {:.4} ms/frame",
        elapsed.as_secs_f64() * 1000.0,
        elapsed.as_secs_f64() * 1000.0 / f64::from(frames.max(1))
    );
}
