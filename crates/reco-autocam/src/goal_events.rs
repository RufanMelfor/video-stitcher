//! Goal-mouth polygon entry detection.
//!
//! Raw signal only: reports the frame a ball detection's center enters a
//! calibrated [`GoalGeometry`] polygon. This is deliberately NOT a
//! confirmed "goal scored" event - a ball's 2D projected position inside
//! a goal-mouth polygon in one camera's raw frame doesn't prove it
//! crossed the line in 3D (a corner-kick delivery, or a shot that flies
//! just over the crossbar, can pass through the same on-screen region
//! without the ball ever being behind the line). Confirming an entry as
//! a real goal needs a second signal - the ball returning to a kickoff/
//! center-circle position - which depends on kickoff detection that
//! doesn't exist yet in this codebase. Until that lands, this is the
//! honest, unconfirmed primitive to build the confirmed version on top
//! of, rather than a shortcut that pretends to be more certain than it is.

use reco_core::calibration::GoalGeometry;
use reco_core::detect::detector::Detection;
use reco_core::geometry::CameraId;
use reco_core::projection::point_in_polygon;

/// Tracks whether a point is currently inside a calibrated zone polygon,
/// reporting only the outside-to-inside transition so a caller sees one
/// event per entry rather than one per frame the point happens to stay
/// inside. Generic over any per-camera polygon - `GoalGeometry` is the
/// first consumer, but the same primitive would work for a future zone
/// (e.g. a penalty box) without changes here.
#[derive(Debug, Default, Clone, Copy)]
pub struct ZoneEntryTracker {
    inside: bool,
}

impl ZoneEntryTracker {
    /// New tracker, starting in the "outside" state.
    pub fn new() -> Self {
        Self::default()
    }

    /// Feed one frame's point. Returns `true` exactly on the frame the
    /// point transitions from outside to inside `polygon`.
    ///
    /// A fresh tracker has no prior frame to compare against, so it
    /// treats "not yet observed" as outside - a point already inside on
    /// the very first fed frame therefore reports an entry. This is a
    /// deliberate choice: it risks one spurious event if observation
    /// starts mid-play with the ball already sitting in the polygon, but
    /// that is far safer than the alternative of silently missing a real
    /// entry that happens to land on the very first observed frame.
    ///
    /// A polygon with fewer than 3 vertices (not calibrated) never
    /// reports entry - `point_in_polygon` itself already returns `false`
    /// for those, so there is nothing extra to special-case here.
    pub fn update(&mut self, point: [f64; 2], polygon: &[[f64; 2]]) -> bool {
        let now_inside = point_in_polygon(point, polygon);
        let entered = now_inside && !self.inside;
        self.inside = now_inside;
        entered
    }

    /// Whether the point was inside as of the last `update()` call.
    pub fn is_inside(&self) -> bool {
        self.inside
    }
}

/// Which calibrated goal (left- or right-camera polygon, per
/// [`GoalGeometry`]'s storage convention) a [`GoalEntryDetector`]
/// reported an entry for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GoalSide {
    Left,
    Right,
}

/// Detects a ball detection entering either calibrated goal-mouth
/// polygon. One [`ZoneEntryTracker`] per camera side, matching
/// `GoalGeometry`'s per-camera storage - entering the left goal does not
/// affect the right tracker's state and vice versa.
///
/// Feed it every ball detection as it arrives; frames with no ball
/// detected should simply not call `update()` at all, rather than
/// feeding some sentinel "outside" point - a brief miss while the ball
/// crosses the line (motion blur, partial occlusion by the net) would
/// otherwise reset the "was it just outside" state the transition check
/// relies on, and could cost the very entry event you're trying to catch.
#[derive(Debug, Default)]
pub struct GoalEntryDetector {
    left: ZoneEntryTracker,
    right: ZoneEntryTracker,
}

impl GoalEntryDetector {
    /// New detector, both sides starting in the "outside" state.
    pub fn new() -> Self {
        Self::default()
    }

    /// Feed one ball detection. Returns `Some(side)` on the frame it
    /// transitions into that side's goal polygon; `None` otherwise
    /// (still outside, still inside, or that side has no calibrated
    /// polygon).
    pub fn update(&mut self, ball: &Detection, goal: &GoalGeometry) -> Option<GoalSide> {
        let point = [ball.center_x as f64, ball.center_y as f64];
        match ball.camera {
            CameraId::Left => self
                .left
                .update(point, &goal.left)
                .then_some(GoalSide::Left),
            CameraId::Right => self
                .right
                .update(point, &goal.right)
                .then_some(GoalSide::Right),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn goal() -> GoalGeometry {
        GoalGeometry {
            left: vec![[0.05, 0.30], [0.05, 0.75], [0.22, 0.75], [0.22, 0.30]],
            right: vec![[0.78, 0.32], [0.78, 0.70], [0.95, 0.70], [0.95, 0.32]],
        }
    }

    fn ball_at(camera: CameraId, cx: f32, cy: f32) -> Detection {
        Detection {
            camera,
            class_id: 0,
            confidence: 0.9,
            center_x: cx,
            center_y: cy,
            width: 0.02,
            height: 0.02,
        }
    }

    #[test]
    fn zone_entry_tracker_fires_once_on_entry_not_every_frame_inside() {
        let mut t = ZoneEntryTracker::new();
        let polygon = [[0.2, 0.2], [0.8, 0.2], [0.8, 0.8], [0.2, 0.8]];
        assert!(!t.update([0.05, 0.05], &polygon), "starts outside");
        assert!(t.update([0.5, 0.5], &polygon), "enters -> fires once");
        assert!(!t.update([0.5, 0.5], &polygon), "stays inside -> no refire");
        assert!(!t.update([0.6, 0.6], &polygon), "moves within -> no refire");
        assert!(
            !t.update([0.05, 0.05], &polygon),
            "leaves -> no entry event"
        );
        assert!(t.update([0.5, 0.5], &polygon), "re-enters -> fires again");
    }

    #[test]
    fn zone_entry_tracker_degenerate_polygon_never_fires() {
        let mut t = ZoneEntryTracker::new();
        assert!(!t.update([0.5, 0.5], &[]));
        assert!(!t.update([0.5, 0.5], &[[0.0, 0.0], [1.0, 1.0]]));
    }

    #[test]
    fn zone_entry_tracker_fresh_tracker_starting_inside_reports_entry() {
        // Documented behavior: see `update`'s doc comment for why this
        // is the safer default.
        let mut t = ZoneEntryTracker::new();
        let polygon = [[0.2, 0.2], [0.8, 0.2], [0.8, 0.8], [0.2, 0.8]];
        assert!(t.update([0.5, 0.5], &polygon));
    }

    #[test]
    fn goal_entry_detector_reports_correct_side() {
        let mut d = GoalEntryDetector::new();
        let g = goal();
        assert_eq!(d.update(&ball_at(CameraId::Left, 0.5, 0.5), &g), None);
        assert_eq!(d.update(&ball_at(CameraId::Right, 0.5, 0.5), &g), None);
        assert_eq!(
            d.update(&ball_at(CameraId::Left, 0.1, 0.5), &g),
            Some(GoalSide::Left)
        );
        assert_eq!(
            d.update(&ball_at(CameraId::Right, 0.9, 0.5), &g),
            Some(GoalSide::Right)
        );
    }

    #[test]
    fn goal_entry_detector_does_not_refire_while_ball_stays_in_goal() {
        let mut d = GoalEntryDetector::new();
        let g = goal();
        assert_eq!(
            d.update(&ball_at(CameraId::Left, 0.1, 0.5), &g),
            Some(GoalSide::Left)
        );
        assert_eq!(d.update(&ball_at(CameraId::Left, 0.12, 0.52), &g), None);
    }

    #[test]
    fn goal_entry_detector_sides_are_independent() {
        let mut d = GoalEntryDetector::new();
        let g = goal();
        assert_eq!(
            d.update(&ball_at(CameraId::Left, 0.1, 0.5), &g),
            Some(GoalSide::Left)
        );
        // Left ball leaves the goal again - must not affect the right
        // tracker's independent state.
        assert_eq!(d.update(&ball_at(CameraId::Left, 0.5, 0.5), &g), None);
        assert_eq!(
            d.update(&ball_at(CameraId::Right, 0.9, 0.5), &g),
            Some(GoalSide::Right)
        );
    }

    #[test]
    fn goal_entry_detector_no_polygon_on_that_side_never_fires() {
        let mut d = GoalEntryDetector::new();
        let g = GoalGeometry::default();
        assert_eq!(d.update(&ball_at(CameraId::Left, 0.1, 0.5), &g), None);
        assert_eq!(d.update(&ball_at(CameraId::Right, 0.9, 0.5), &g), None);
    }
}
