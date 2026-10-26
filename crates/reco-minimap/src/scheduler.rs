//! Opt-in live scheduling over the existing calibrated tiles.
//!
//! Crops are never resized/repositioned here: the model sees the same input as
//! in a full sweep. A greedy cover selects tiles around recent image-space
//! observations; regular full sweeps recover new/lost objects. This reduces
//! inference work, not a guarantee of continuous ball visibility.
use crate::{detect::Tile, tracker::BBox};

/// Independent rotating coverage, including tiles with no tracked person.
/// Sharing the existing model's worker pool avoids a second GPU model copy.
#[derive(Default)]
pub struct RecoverySweep {
    next: usize,
}
impl RecoverySweep {
    pub fn select(&mut self, total: usize, budget: usize) -> Vec<usize> {
        if total == 0 {
            return Vec::new();
        }
        let out = (0..budget.min(total))
            .map(|i| (self.next + i) % total)
            .collect();
        self.next = (self.next + budget.min(total)) % total;
        out
    }
}

pub struct TileScheduler {
    last_full: Option<f64>,
    interval: f64,
}

impl TileScheduler {
    pub fn new(interval: f64) -> Self {
        Self {
            last_full: None,
            interval: if interval.is_finite() {
                interval.clamp(0.1, 10.0)
            } else {
                2.0
            },
        }
    }

    /// Return tile indices plus whether a complete search is being performed.
    /// Empty evidence triggers recovery immediately rather than a blind wait.
    pub fn select(
        &mut self,
        t: f64,
        tiles: &[Tile],
        width: u32,
        height: u32,
        observations: &[BBox],
    ) -> (Vec<usize>, bool) {
        let full = self
            .last_full
            .map(|last| !t.is_finite() || t < last || t - last >= self.interval)
            .unwrap_or(true);
        let sweep = |this: &mut Self| {
            this.last_full = Some(t);
            ((0..tiles.len()).collect(), true)
        };
        if full || observations.is_empty() || width == 0 || height == 0 {
            return sweep(self);
        }
        // Five anchors include a motion margin around each box. Requiring all
        // anchors avoids merely looking at the previous centre after a turn.
        let mut points = Vec::new();
        for b in observations {
            if ![b.cx, b.cy, b.w, b.h].iter().all(|v| v.is_finite()) || b.w <= 0.0 || b.h <= 0.0 {
                return sweep(self);
            }
            let dx = b.w * 0.75;
            let dy = b.h * 0.65;
            for (x, y) in [
                (b.cx, b.cy),
                (b.cx - dx, b.cy - dy),
                (b.cx + dx, b.cy - dy),
                (b.cx - dx, b.cy + dy),
                (b.cx + dx, b.cy + dy),
            ] {
                points.push((
                    x.clamp(0.0, 1.0) * width as f32,
                    y.clamp(0.0, 1.0) * height as f32,
                ));
            }
        }
        let covers = |tile: &Tile, p: &(f32, f32)| {
            p.0 >= tile.x as f32
                && p.0 <= (tile.x + tile.w) as f32
                && p.1 >= tile.y as f32
                && p.1 <= (tile.y + tile.h) as f32
        };
        // Points outside the calibrated search area cannot be recovered by this
        // subset. Fall back to the full plan, never silently discard evidence.
        if points
            .iter()
            .any(|p| !tiles.iter().any(|tile| covers(tile, p)))
        {
            return sweep(self);
        }
        let mut selected = Vec::new();
        while !points.is_empty() {
            let (index, count) = tiles
                .iter()
                .enumerate()
                .map(|(i, tile)| (i, points.iter().filter(|p| covers(tile, p)).count()))
                .max_by_key(|(_, n)| *n)
                .unwrap();
            if count == 0 {
                return sweep(self);
            }
            selected.push(index);
            points.retain(|p| !covers(&tiles[index], p));
        }
        // Preserve original inference/NMS order for reproducible tie handling.
        selected.sort_unstable();
        if selected.len() >= tiles.len() {
            return sweep(self);
        }
        (selected, false)
    }
}
