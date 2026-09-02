//! Panner trait — the camera-motion half of the tracker/panner split.
//!
//! A `Panner` consumes a clean `WorldState` (produced by one or
//! more `Tracker` instances from [`super::tracker`]) and returns a
//! `ViewportPosition` (from [`super::director`]) for the virtual
//! camera. It knows nothing
//! about raw detections, plausibility gates, or identity management —
//! those are tracker concerns. Its sole job is "given where things
//! *are*, where should the camera *look*?"
//!
//! Implementations (shipped in `reco-autocam`):
//! - `FieldPanner` - the production player+ball panner: trimmed-robust
//!   cluster centroid, ball-only follow when no cluster, dynamic FOV,
//!   ball-presence hysteresis, velocity-clamped chase. Lookahead is not
//!   a separate panner - the buffered run loop centered-smooths this
//!   panner's pose stream over past + future frames.
//! - `SweepPanner` - debug-only, ignores world state and slowly pans
//!   left-right within coverage bounds.
//! - `FilePanner` - replays a precomputed pose trajectory from CSV.

use super::director::MappedDetection;
use super::pipeline_event::{PipelineEvent, PipelineEventSink};
use super::tracker::{Tracker, WorldState};
use crate::calibration::Calibration;
use crate::geometry::ViewportPosition;

/// `ViewportPosition::fov_degrees`'s own documented pipeline default -
/// the fallback vertical FOV [`clamp_pitch_to_limits`] margins against
/// when a pose doesn't carry one (should be rare in practice; every
/// shipped panner sets a dynamic FOV every frame, but a pose is still
/// safe to clamp without one).
const DEFAULT_FOV_DEGREES: f32 = 75.0;

/// Clamp `pitch` to a manual AI-tracking safety margin - `(top, bottom)`
/// world-space pitch radians, either or both `None` for unrestricted -
/// so that the viewport's rendered TOP/BOTTOM EDGE stays within the
/// margin, not just its center pitch.
///
/// Margining by half the vertical FOV matters: the user sets this
/// margin by dragging a line to exactly where a visible defect (e.g.
/// the coverage-clamp's black-wedge corner leak - see
/// `project_coverage_clamp_corner_leak`) starts on the *rendered
/// preview*, i.e. they are marking an EDGE constraint. A wide dynamic
/// FOV moment (e.g. a "frame everyone" wide shot) can push the top
/// edge well above an unmargined center-pitch clamp even while the
/// center itself stays comfortably under the line - clamping only the
/// center silently let the edge (and the defect behind it) back into
/// frame on exactly the shots most likely to reach that far to begin
/// with. `fov_degrees` should be the pose's actual (already-dynamic)
/// FOV; `None` falls back to [`DEFAULT_FOV_DEGREES`].
///
/// Shared by [`dispatch`] (the live-frame path) and
/// `StitchCore::clamp_autocam_pitch` (the buffered/lookahead path,
/// which calls the panner directly and never goes through
/// [`DispatchContext`]) so the two can't drift out of sync.
///
/// Fails open toward a plain center clamp (not a full no-op) when the
/// margined range is inverted - i.e. the FOV is briefly too wide for
/// the margin to fully contain edge-to-edge. A misconfigured/degenerate
/// margin (bottom above top even before margining) still returns
/// `pitch` unchanged rather than pin the camera to a single pitch.
pub(crate) fn clamp_pitch_to_limits(
    pitch: f32,
    fov_degrees: Option<f32>,
    limits: (Option<f32>, Option<f32>),
) -> f32 {
    let half_vfov = (fov_degrees.unwrap_or(DEFAULT_FOV_DEGREES) * 0.5).to_radians();
    match limits {
        (Some(top), Some(bottom)) if bottom <= top => {
            let (lo, hi) = (bottom + half_vfov, top - half_vfov);
            if lo <= hi {
                pitch.clamp(lo, hi)
            } else {
                // FOV too wide for the margin to fit edge-to-edge this
                // frame - still clamp the center within the raw bounds
                // rather than give up entirely.
                pitch.clamp(bottom, top)
            }
        }
        (Some(top), None) => pitch.min(top - half_vfov),
        (None, Some(bottom)) => pitch.max(bottom + half_vfov),
        _ => pitch,
    }
}

/// Per-frame context a [`Panner`] receives alongside the world state.
///
/// The context carries timing plus a borrow of the current
/// calibration, so panners can project between camera and panorama
/// coordinates if needed (e.g. for coverage-aware edge handling).
/// It does NOT include raw detections — those never reach a panner.
#[derive(Debug)]
pub struct PanContext<'a> {
    /// Current frame index (0-based), monotonically increasing.
    pub frame_index: u64,
    /// Elapsed milliseconds since the start of processing.
    pub timestamp_ms: f64,
    /// The viewport position the session reported on the *previous*
    /// frame (after clamping and smoothing), or the session default
    /// if this is the first call. Panners use this to compute
    /// first-order motion deltas without needing their own state.
    pub previous_position: ViewportPosition,
    /// Shared calibration for optional camera↔panorama projection.
    /// Borrowed for the duration of the [`decide`](Panner::decide)
    /// call; panners must not retain it.
    pub calibration: &'a Calibration,
}

/// The contract implemented by every camera-motion policy.
///
/// Implementations must be **stateful over time** (to smooth motion,
/// apply dead-zones, anticipate trajectories) but **pure per call**
/// with respect to their inputs — i.e. `decide(&world, &ctx)` must
/// not mutate `world` or `ctx`, and repeated calls with identical
/// inputs may return different outputs only because of internal
/// state evolution.
///
/// # Invariants
///
/// - [`decide`](Self::decide) is called once per frame, in order.
/// - The returned [`ViewportPosition`]
///   is NOT yet clamped to the coverage boundary; the session applies
///   clamping after the panner returns. Panners should produce their
///   geometric preference and let the coverage math enforce reachability.
pub trait Panner: Send {
    /// Decide where the virtual camera should look this frame.
    fn decide(&mut self, world: &WorldState, ctx: &PanContext<'_>) -> ViewportPosition;

    /// Decide with access to future WorldStates from the lookahead buffer.
    ///
    /// `future` contains WorldStates for frames after the current one,
    /// ordered nearest-to-farthest. Empty when lookahead is disabled.
    /// Default delegates to [`decide`](Self::decide), ignoring the future.
    fn decide_with_lookahead(
        &mut self,
        world: &WorldState,
        future: &[WorldState],
        ctx: &PanContext<'_>,
    ) -> ViewportPosition {
        let _ = future;
        self.decide(world, ctx)
    }

    /// Optional debug snapshot from the last `decide()` call.
    fn debug_event(&self, _frame_index: u64) -> Option<PipelineEvent> {
        None
    }

    /// Clear any smoothing/momentum state carried across frames after a
    /// hard timeline discontinuity (e.g. a cut range excluded mid-export -
    /// see `reco_io::cut_range`). Default no-op: stateless panners and
    /// panners whose per-frame state is already safe to reuse across a
    /// jump (nothing accumulates) don't need to override this.
    ///
    /// Implementors that track velocity, an exponential moving average,
    /// or any other "last N frames" momentum should clear it here so the
    /// first post-cut frame reacts to fresh data immediately instead of
    /// smoothing across a gap that, from the panner's perspective, never
    /// happened. The current output position may be left as-is - only
    /// the *rate-of-change* state needs clearing, not the position
    /// itself.
    fn reset(&mut self) {}
}

/// Scalar inputs to [`dispatch`] bundled so the function stays under
/// `clippy::too_many_arguments`. The mutable pose + the three
/// `Option<&mut Box<dyn …>>` slots are the only moving parts per
/// caller; everything else fits here.
#[derive(Clone, Copy)]
pub(crate) struct DispatchContext<'a> {
    /// Raw mapped detections the trackers should consume this frame.
    pub detections: &'a [MappedDetection],
    /// Shared calibration handed to the panner via [`PanContext`].
    pub calibration: &'a Calibration,
    /// Current frame index (0-based, monotonically increasing).
    pub frame_index: u64,
    /// Elapsed milliseconds since session start.
    pub timestamp_ms: f64,
    /// Short label used only for the >1-ball warning so log output
    /// still says which caller ran the dispatch.
    pub caller: &'static str,
    /// Manual AI-tracking pitch safety margin - see
    /// [`clamp_pitch_to_limits`]. Unused by [`dispatch_detect_only`]
    /// (no panner runs there to clamp), but carried on the shared
    /// context struct so both call sites build it the same way.
    pub pitch_limit: (Option<f32>, Option<f32>),
}

/// Run the shared tracker → panner dispatch one frame's worth.
///
/// Both `StitchCore::resolve_current_pose` and
/// `StitchSession::fire_sink_and_update_director` used to inline this
/// ~50-line algorithm. They still differ on what they do around the
/// dispatch (clamp, fire sinks, return vs. store), but the dispatch
/// itself is identical:
///
/// 1. Build an empty [`WorldState`].
/// 2. Run the player tracker, store into `world.players`.
/// 3. Let the ball tracker `observe_world` (so it sees this frame's
///    players for anchor gating), then `update`; take the first
///    entity (warn if more than one) into `world.ball`.
/// 4. Build a [`PanContext`] carrying the caller's previous pose.
/// 5. Ask the panner to `decide`; update `previous_panner_pose` in
///    place; return the decided pose.
///
/// Returns `None` when no panner is attached.
///
/// When a [`PipelineEventSink`] is supplied via `event_sink`, emits a
/// [`PipelineEvent::WorldState`] right before `panner.decide` and a
/// [`PipelineEvent::PanDecision`] right after. Both sites are part
/// of the Step 6 trace vocabulary.
pub(crate) struct DispatchResult {
    pub pose: ViewportPosition,
    pub active_tracks: u32,
    pub ball_present: bool,
}

/// Run trackers only (no panner). Returns the WorldState for buffering.
pub(crate) fn dispatch_detect_only(
    player_tracker: Option<&mut Box<dyn Tracker>>,
    ball_tracker: Option<&mut Box<dyn Tracker>>,
    ctx: DispatchContext<'_>,
) -> WorldState {
    let mut world = WorldState::default();

    // Order matters: players first, then ball. The ball tracker's
    // `observe_world` sees the just-computed player positions so a
    // player-anchor gate can run against the current frame rather
    // than the previous one.
    if let Some(t) = player_tracker {
        world.players = t.update(ctx.detections, ctx.timestamp_ms);
    }
    if let Some(t) = ball_tracker {
        t.observe_world(&world);
        let ents = t.update(ctx.detections, ctx.timestamp_ms);
        if ents.len() > 1 {
            log::warn!(
                "{}: ball_tracker returned {} entities (expected <=1); taking first",
                ctx.caller,
                ents.len()
            );
        }
        world.ball = ents.into_iter().next();
    }

    world
}

pub(crate) fn dispatch(
    panner: Option<&mut Box<dyn Panner>>,
    player_tracker: Option<&mut Box<dyn Tracker>>,
    ball_tracker: Option<&mut Box<dyn Tracker>>,
    previous_panner_pose: &mut ViewportPosition,
    mut event_sink: Option<&mut (dyn PipelineEventSink + '_)>,
    ctx: DispatchContext<'_>,
) -> Option<DispatchResult> {
    let panner = panner?;
    // Build the WorldState via the shared tracker-run path (same as the
    // lookahead produce phase) so the two can never silently diverge.
    let world = dispatch_detect_only(player_tracker, ball_tracker, ctx);

    // Trace: WorldState (only pays for the clone when a sink exists).
    if let Some(sink) = event_sink.as_mut() {
        sink.emit(PipelineEvent::WorldState {
            frame_index: ctx.frame_index,
            timestamp_ms: ctx.timestamp_ms,
            players: world.players.clone(),
            ball: world.ball,
        });
    }

    let pan_ctx = PanContext {
        frame_index: ctx.frame_index,
        timestamp_ms: ctx.timestamp_ms,
        previous_position: *previous_panner_pose,
        calibration: ctx.calibration,
    };
    let active_tracks = world.players.len() as u32;
    let ball_present = world
        .ball
        .as_ref()
        .is_some_and(|b| !matches!(b.state, super::tracker::TrackState::Lost));

    // The lookahead-aware path does not come through here: the
    // buffered loop calls `StitchCore::decide_pose_with_lookahead`
    // with the real future window. This immediate-mode dispatch has
    // no future frames by construction.
    let mut pose = panner.decide_with_lookahead(&world, &[], &pan_ctx);
    pose.pitch = clamp_pitch_to_limits(pose.pitch, pose.fov_degrees, ctx.pitch_limit);
    *previous_panner_pose = pose;

    if let Some(sink) = event_sink.as_mut() {
        sink.emit(PipelineEvent::PanDecision {
            frame_index: ctx.frame_index,
            pose,
        });
        if let Some(debug) = panner.debug_event(ctx.frame_index) {
            sink.emit(debug);
        }
    }

    Some(DispatchResult {
        pose,
        active_tracks,
        ball_present,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::calibration::{Calibration, Framing, Lens, Topology};
    use crate::detect::tracker::{TrackState, TrackedEntity, WorldState};
    use crate::geometry::CameraId;

    #[test]
    fn clamp_pitch_to_limits_no_limit_passes_through() {
        assert_eq!(clamp_pitch_to_limits(0.5, Some(50.0), (None, None)), 0.5);
    }

    #[test]
    fn clamp_pitch_to_limits_clamps_both_bounds() {
        // Some(0.0) FOV -> zero margin, isolating the base center-clamp
        // behavior from the edge-margin behavior covered separately
        // below.
        assert_eq!(
            clamp_pitch_to_limits(0.9, Some(0.0), (Some(0.4), Some(-0.4))),
            0.4
        );
        assert_eq!(
            clamp_pitch_to_limits(-0.9, Some(0.0), (Some(0.4), Some(-0.4))),
            -0.4
        );
        assert_eq!(
            clamp_pitch_to_limits(0.1, Some(0.0), (Some(0.4), Some(-0.4))),
            0.1
        );
    }

    #[test]
    fn clamp_pitch_to_limits_one_sided_bounds() {
        // Top-only: never exceed the ceiling, floor is unrestricted.
        assert_eq!(
            clamp_pitch_to_limits(0.9, Some(0.0), (Some(0.4), None)),
            0.4
        );
        assert_eq!(
            clamp_pitch_to_limits(-0.9, Some(0.0), (Some(0.4), None)),
            -0.9
        );
        // Bottom-only: never go below the floor, ceiling is unrestricted.
        assert_eq!(
            clamp_pitch_to_limits(-0.9, Some(0.0), (None, Some(-0.4))),
            -0.4
        );
        assert_eq!(
            clamp_pitch_to_limits(0.9, Some(0.0), (None, Some(-0.4))),
            0.9
        );
    }

    #[test]
    fn clamp_pitch_to_limits_fails_open_on_inverted_bounds() {
        // A misconfigured margin (bottom above top, even before
        // margining) must not pin the camera to a single pitch - pass
        // the raw value through.
        assert_eq!(
            clamp_pitch_to_limits(0.5, Some(0.0), (Some(-0.4), Some(0.4))),
            0.5
        );
    }

    #[test]
    fn clamp_pitch_to_limits_margins_by_half_the_fov_not_just_center() {
        // The whole point: a wide dynamic FOV must not let the rendered
        // TOP/BOTTOM EDGE cross the line the user dragged, even while
        // the CENTER pitch alone would look comfortably clear of it.
        // 20deg FOV -> 10deg (0.1745rad) half-vfov margin on each side -
        // comfortably fits within the [-0.4, 0.4] band (2*half=0.349rad
        // < 0.8rad band width), so this isolates the margining behavior
        // from the too-wide-to-fit fallback covered separately below.
        let limits = (Some(0.4_f32), Some(-0.4_f32));
        let half_vfov = 10.0_f32.to_radians();

        // A center pitch that's fine unmargined (0.39 < 0.4) still gets
        // pulled in so the top edge (pitch + half_vfov) lands exactly
        // at the limit, not past it.
        let clamped = clamp_pitch_to_limits(0.39, Some(20.0), limits);
        assert!(
            (clamped + half_vfov - 0.4).abs() < 1e-5,
            "top edge should land exactly at the limit, got center={clamped} \
             (edge={})",
            clamped + half_vfov
        );

        // Mirror for the bottom edge.
        let clamped = clamp_pitch_to_limits(-0.39, Some(20.0), limits);
        assert!(
            (clamped - half_vfov - (-0.4)).abs() < 1e-5,
            "bottom edge should land exactly at the limit, got center={clamped} \
             (edge={})",
            clamped - half_vfov
        );

        // A pitch already well clear of both margined edges passes
        // through unchanged.
        assert_eq!(clamp_pitch_to_limits(0.0, Some(20.0), limits), 0.0);
    }

    #[test]
    fn clamp_pitch_to_limits_falls_back_to_center_clamp_when_fov_too_wide_for_margin() {
        // A 170deg FOV's half-vfov (85deg) exceeds the whole [-0.4,0.4]
        // margin band - there is no pitch whose full height fits inside
        // it. Falling back to a plain center clamp (not collapsing to
        // one pitch, and not failing open to unrestricted) is still a
        // real, useful restriction for this rare case.
        let clamped = clamp_pitch_to_limits(0.9, Some(170.0), (Some(0.4), Some(-0.4)));
        assert_eq!(clamped, 0.4);
    }

    /// A fixture calibration shaped like the v1 test JSON without
    /// needing disk access or real lens data.
    fn test_calibration() -> Calibration {
        let cam = || Lens::fisheye(1920, 1080, 900.0, 900.0, 960.0, 540.0, [0.0; 4]);
        Calibration::new(
            vec![cam(), cam()],
            Topology {
                intersect: 0.54,
                x_ty: 0.0,
                x_rz: 0.0,
                z_rx: 0.0,
                x_rx: 0.0,
                z_rz: 0.0,
                blend_width: 0.05,
                blend_flip_direction: false,
                seam_offset: 0.0,
                multiband_blend_enabled: false,
                color_match_enabled: true,
                color_match_band_width: 0.15,
                color_match_grid_cols: 8,
                color_match_grid_rows: 16,
                color_match_interval_frames: 15,
                color_match_ema_alpha: 0.15,
                color_match_max_y_offset: 0.06,
                color_match_max_chroma_offset: 0.04,
                color_gamma_left: 1.0,
                color_gamma_right: 1.0,
                color_match_auto_gamma: false,
                ground_tilt_x: 0.0,
                ground_tilt_z: 0.0,
                top_tilt_x: 0.0,
                top_tilt_z: 0.0,
                ground_tilt_band_width: 0.16,
                top_tilt_band_width: 0.16,
            },
            Framing {
                axis_offset: 0.24,
                tilt: 0.0,
                roll: 0.0,
            },
        )
    }

    /// Minimal panner that echoes the ball's yaw/pitch when present.
    struct EchoPanner;
    impl Panner for EchoPanner {
        fn decide(&mut self, world: &WorldState, _ctx: &PanContext<'_>) -> ViewportPosition {
            match world.ball {
                Some(b) => ViewportPosition {
                    yaw: b.yaw,
                    pitch: b.pitch,
                    fov_degrees: None,
                },
                None => ViewportPosition::default(),
            }
        }
    }

    #[test]
    fn echo_panner_follows_ball() {
        let cal = test_calibration();
        let mut p: Box<dyn Panner> = Box::new(EchoPanner);
        let world = WorldState {
            ball: Some(TrackedEntity {
                id: 0,
                class_id: 0,
                yaw: 0.3,
                pitch: -0.05,
                confidence: 0.9,
                state: TrackState::Tracking,
                age_frames: 5,
                origin: CameraId::Left,
            }),
            players: vec![],
        };
        let ctx = PanContext {
            frame_index: 0,
            timestamp_ms: 0.0,
            previous_position: ViewportPosition::default(),
            calibration: &cal,
        };
        let out = p.decide(&world, &ctx);
        assert_eq!(out.yaw, 0.3);
        assert_eq!(out.pitch, -0.05);
    }

    #[test]
    fn echo_panner_defaults_without_ball() {
        let cal = test_calibration();
        let mut p = EchoPanner;
        let world = WorldState::default();
        let ctx = PanContext {
            frame_index: 0,
            timestamp_ms: 0.0,
            previous_position: ViewportPosition::default(),
            calibration: &cal,
        };
        let out = p.decide(&world, &ctx);
        assert_eq!(out.yaw, 0.0);
        assert_eq!(out.pitch, 0.0);
    }
}
