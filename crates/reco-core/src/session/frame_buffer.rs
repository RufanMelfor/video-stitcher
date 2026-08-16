//! Lookahead frame buffer for temporal-aware processing.
//!
//! Holds N decoded frames with their detection metadata so the
//! panner can see future WorldStates when deciding the current
//! frame's viewport position. Works for both CPU-resident frames
//! (`StereoFrame::Yuv420p` / `Nv12`) and GPU-resident frames, where
//! the pixels live in the VRAM pool and `vram_slot` indexes them.

use std::collections::VecDeque;

use crate::detect::director::MappedDetection;
use crate::detect::tracker::WorldState;
use crate::source::StereoFrame;

/// A [`BufferedFrame`]'s detection/tracking state - either resolved
/// (today's fully-synchronous behavior) or still in flight on an
/// [`crate::async_detect::AsyncDetectThread`], resolved lazily right
/// before the frame is actually consumed (see
/// `run_loop::run_panner_once`'s resolution step, and
/// `detection_dispatch::resolve_pending_world_state`).
#[derive(Clone)]
pub(crate) enum PendingWorldState {
    /// Detection already ran synchronously (async disabled, or this
    /// frame's detector call didn't split) - the value every session
    /// produced before async detect existed.
    Ready(WorldState),
    /// Detection submitted to the async thread under this produce
    /// index; not resolved yet.
    Pending(u64),
}

impl PendingWorldState {
    /// The resolved state if available, else `fallback` - used by
    /// [`FrameBuffer::future_world_states`]'s lookahead peek, which
    /// must not block waiting on results that aren't due to be
    /// consumed yet. Matches the existing stale-detection-reuse
    /// pattern already used elsewhere in the panner (last known state
    /// stands in until a fresher one arrives).
    fn resolved_or(&self, fallback: &WorldState) -> WorldState {
        match self {
            PendingWorldState::Ready(ws) => ws.clone(),
            PendingWorldState::Pending(_) => fallback.clone(),
        }
    }
}

/// A single buffered frame: decoded pixels + detection metadata.
pub(crate) struct BufferedFrame {
    pub frame: StereoFrame,
    pub world_state: PendingWorldState,
    pub detections: Vec<MappedDetection>,
    pub elapsed_ms: f64,
    pub decode_time: std::time::Duration,
    /// VRAM pool slot index (Some when GPU-resident, None for CPU frames).
    /// The pool slot is released after rendering.
    pub vram_slot: Option<usize>,
}

/// Fixed-capacity ring buffer of decoded frames.
///
/// The producer (decode + detect) pushes frames in. The consumer
/// (direct + render) pops from the front with access to all
/// remaining entries as the lookahead window.
pub(crate) struct FrameBuffer {
    frames: VecDeque<BufferedFrame>,
    capacity: usize,
    /// Most recently resolved `WorldState` - the lookahead fallback for
    /// any frame still `Pending` when `future_world_states()` peeks at
    /// it. Starts as `WorldState::default()` (no ball/players) before
    /// the first frame ever resolves, matching how a session with no
    /// detections yet already behaves.
    last_resolved: WorldState,
}

impl FrameBuffer {
    pub fn new(capacity: usize) -> Self {
        Self {
            frames: VecDeque::with_capacity(capacity),
            capacity,
            last_resolved: WorldState::default(),
        }
    }

    /// Record a freshly resolved `WorldState` as the new lookahead
    /// fallback. Call this whenever a `Pending` frame gets resolved
    /// (see `run_panner_once`) - FIFO submission order guarantees
    /// every frame still `Pending` in the buffer at that point is for
    /// a produce index *after* the one just resolved, so this is
    /// always the most current fallback available.
    pub fn set_last_resolved(&mut self, ws: WorldState) {
        self.last_resolved = ws;
    }

    pub fn len(&self) -> usize {
        self.frames.len()
    }

    pub fn is_full(&self) -> bool {
        self.frames.len() >= self.capacity
    }

    /// Push a new frame into the buffer. Panics if full.
    pub fn push(&mut self, frame: BufferedFrame) {
        debug_assert!(
            !self.is_full(),
            "FrameBuffer::push called on full buffer (cap={})",
            self.capacity
        );
        self.frames.push_back(frame);
    }

    /// Pop the oldest frame for rendering.
    pub fn pop(&mut self) -> Option<BufferedFrame> {
        self.frames.pop_front()
    }

    /// Collect future WorldStates from all frames currently in the
    /// buffer, ordered nearest-to-farthest. Used as the lookahead
    /// window for `Panner::decide_with_lookahead`. Entries still
    /// `Pending` (async detect hasn't resolved them yet) fall back to
    /// the last resolved `WorldState` rather than blocking - see
    /// [`PendingWorldState::resolved_or`].
    pub fn future_world_states(&self) -> Vec<WorldState> {
        self.frames
            .iter()
            .map(|f| f.world_state.resolved_or(&self.last_resolved))
            .collect()
    }

    pub fn is_empty(&self) -> bool {
        self.frames.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::detect::tracker::{TrackState, TrackedEntity, WorldState};

    fn make_frame(ball_yaw: f32) -> BufferedFrame {
        BufferedFrame {
            frame: StereoFrame::Nv12(crate::source::Nv12FramePair {
                left: crate::source::Nv12Data {
                    y: vec![128; 4],
                    uv: vec![128; 2],
                },
                right: crate::source::Nv12Data {
                    y: vec![128; 4],
                    uv: vec![128; 2],
                },
            }),
            world_state: PendingWorldState::Ready(WorldState {
                ball: Some(TrackedEntity {
                    id: 0,
                    class_id: 0,
                    yaw: ball_yaw,
                    pitch: 0.1,
                    confidence: 0.9,
                    state: TrackState::Tracking,
                    age_frames: 1,
                    origin: crate::geometry::CameraId::Left,
                }),
                players: vec![],
            }),
            detections: vec![],
            elapsed_ms: 0.0,
            decode_time: std::time::Duration::from_millis(3),
            vram_slot: None,
        }
    }

    fn ball_yaw(f: &BufferedFrame) -> f32 {
        let PendingWorldState::Ready(ws) = &f.world_state else {
            panic!("test frames are always Ready");
        };
        ws.ball.as_ref().unwrap().yaw
    }

    #[test]
    fn push_pop_fifo_order() {
        let mut buf = FrameBuffer::new(3);
        buf.push(make_frame(0.1));
        buf.push(make_frame(0.2));
        buf.push(make_frame(0.3));

        assert!((ball_yaw(&buf.pop().unwrap()) - 0.1).abs() < 1e-6);
        assert!((ball_yaw(&buf.pop().unwrap()) - 0.2).abs() < 1e-6);
        assert!((ball_yaw(&buf.pop().unwrap()) - 0.3).abs() < 1e-6);
        assert!(buf.pop().is_none());
    }

    #[test]
    fn fullness_tracking() {
        let mut buf = FrameBuffer::new(2);
        assert!(buf.is_empty());
        assert!(!buf.is_full());

        buf.push(make_frame(0.0));
        assert_eq!(buf.len(), 1);
        assert!(!buf.is_full());

        buf.push(make_frame(0.0));
        assert_eq!(buf.len(), 2);
        assert!(buf.is_full());
    }

    #[test]
    fn future_world_states_returns_remaining() {
        let mut buf = FrameBuffer::new(5);
        for i in 0..4 {
            buf.push(make_frame(i as f32 * 0.1));
        }
        buf.pop(); // remove frame 0

        let futures = buf.future_world_states();
        assert_eq!(futures.len(), 3);
        let yaws: Vec<f32> = futures
            .iter()
            .map(|ws| ws.ball.as_ref().unwrap().yaw)
            .collect();
        assert!((yaws[0] - 0.1).abs() < 1e-6);
        assert!((yaws[1] - 0.2).abs() < 1e-6);
        assert!((yaws[2] - 0.3).abs() < 1e-6);
    }

    #[test]
    #[should_panic(expected = "push called on full buffer")]
    fn push_on_full_panics() {
        let mut buf = FrameBuffer::new(1);
        buf.push(make_frame(0.0));
        buf.push(make_frame(0.0));
    }

    fn make_pending_frame(produce_index: u64) -> BufferedFrame {
        let mut f = make_frame(0.0);
        f.world_state = PendingWorldState::Pending(produce_index);
        f
    }

    #[test]
    fn pending_frame_falls_back_to_last_resolved_without_blocking() {
        let mut buf = FrameBuffer::new(5);
        // No frame has ever resolved yet - fallback is the default
        // (empty) WorldState.
        buf.push(make_pending_frame(0));
        let futures = buf.future_world_states();
        assert_eq!(futures.len(), 1);
        assert!(futures[0].ball.is_none());
    }

    #[test]
    fn pending_frame_uses_most_recent_resolved_fallback() {
        let mut buf = FrameBuffer::new(5);
        buf.push(make_pending_frame(0));
        buf.set_last_resolved(WorldState {
            ball: Some(TrackedEntity {
                id: 7,
                class_id: 0,
                yaw: 0.42,
                pitch: 0.0,
                confidence: 0.5,
                state: TrackState::Tracking,
                age_frames: 3,
                origin: crate::geometry::CameraId::Right,
            }),
            players: vec![],
        });
        let futures = buf.future_world_states();
        assert!((futures[0].ball.as_ref().unwrap().yaw - 0.42).abs() < 1e-6);
    }

    #[test]
    fn ready_and_pending_frames_mix_correctly() {
        let mut buf = FrameBuffer::new(5);
        buf.push(make_frame(0.1)); // Ready
        buf.push(make_pending_frame(1)); // Pending - falls back
        buf.set_last_resolved(WorldState {
            ball: Some(TrackedEntity {
                id: 0,
                class_id: 0,
                yaw: 0.99,
                pitch: 0.0,
                confidence: 0.5,
                state: TrackState::Tracking,
                age_frames: 1,
                origin: crate::geometry::CameraId::Left,
            }),
            players: vec![],
        });
        let futures = buf.future_world_states();
        assert!(
            (futures[0].ball.as_ref().unwrap().yaw - 0.1).abs() < 1e-6,
            "Ready frame keeps its own value"
        );
        assert!(
            (futures[1].ball.as_ref().unwrap().yaw - 0.99).abs() < 1e-6,
            "Pending frame uses the fallback"
        );
    }
}
