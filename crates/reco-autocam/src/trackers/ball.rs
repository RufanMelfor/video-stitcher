//! Singleton ball tracker composing player-anchor
//! + nearest-to-last selection + [`Coaster`].
//!
//! Port of the Python POC at `/tmp/reco-ai-eval/build_tracker_video.py`
//! (see `build_trajectory`) into the `Tracker` contract from
//! [`reco_core::detect::tracker`].
//!
//! # Filter chain
//!
//! Each frame's detections pass through:
//!
//! 1. **Class filter** — only the tracker's `class_id` survives.
//! 2. **Position required** — detections whose
//!    [`MappedDetection::position`] is `None` (failed panorama
//!    projection) are dropped.
//! 3. **Player anchor** (optional) — if player anchors have been
//!    supplied via [`BallTracker::set_players`] and non-empty, a
//!    detection must be within a radius (radians) of at least one
//!    player in panorama yaw/pitch space to survive - see
//!    [`BallTracker::with_player_anchor_rad_near_far`] for why that
//!    radius ramps with the candidate's own pitch rather than being one
//!    flat number. When no players have been supplied (no player
//!    provider attached, or a ball-only model), the filter is a no-op.
//! 4. **Acquisition cluster gate** (optional, fresh acquisitions
//!    only) — when armed, a detection that would *start* a new track
//!    must also lie near the dominant player group, rejecting a stray
//!    ball from an adjacent pitch that step 3 waves through because
//!    the kids playing with it are tracked people too. Never applies
//!    to an established track. See
//!    [`BallTracker::with_acquire_cluster_gate`].
//! 5. **Nearest-to-last, with max-jump** — among survivors, pick the
//!    one whose panorama position is closest to the last accepted
//!    tracked position (breaking ties toward higher confidence).
//!    `max_jump_rad` always hard-rejects a candidate when player
//!    anchors are NOT active this frame (step 3 was a no-op). When
//!    anchors ARE active it rejects only *unconvincing* long jumps -
//!    a detection below
//!    [`BallTracker::with_jump_confidence`] - so the tracker can still
//!    follow a real ball that reappears far away without being free to
//!    teleport between two different balls on a weak flicker (see
//!    [`BallTracker::score`] and `with_jump_confidence` for the
//!    measured failure on both sides of that trade-off).
//!    Cross-camera yaw/pitch is
//!    meaningful because the projection already unifies the coordinate
//!    frame, so same-cam vs cross-cam are scored identically (unlike
//!    the Python POC which worked in pixels and had to special-case
//!    cross-cam).
//! 6. **Coaster** — if no candidate survived this frame, hold the
//!    last known position for up to `max_coast_frames` frames, then
//!    transition to `Lost`. With [`BallTracker::with_coast_carry`] the
//!    coasting position instead keeps drifting along the ball's recent
//!    velocity, easing to a stop.
//!
//! # Logging
//!
//! Following reco's explicit-decision principle, every state change
//! emits a log line at `info!` (acquisitions, losses) or `debug!`
//! (per-frame transitions). Rejection reasons for individual
//! detections log at `trace!` to keep the normal path quiet.
//!
//! [`MappedDetection::position`]: reco_core::detect::director::MappedDetection::position

use reco_core::detect::director::MappedDetection;
use reco_core::detect::tracker::{TrackState, TrackedEntity, Tracker};
use reco_core::geometry::CameraId;

use std::collections::VecDeque;

use crate::trackers::filters::{CoastStatus, Coaster};

/// Default angular gate on jumps between frames (radians).
///
/// ~20° — a ball can legitimately cross a significant chunk of the
/// panorama in one detection interval when a long pass is in flight,
/// especially at typical 5-frame tracker sample cadence. Tighter
/// gates (Python POC used ~500 px in camera frame) caused frequent
/// false losses during fast plays.
pub const DEFAULT_MAX_JUMP_RAD: f32 = 0.35;

/// Default coast budget in sample-frames (tracker calls). At the
/// POC's every-5-source-frames cadence on 30 fps footage, 20
/// sample-frames ≈ 3.3 seconds of held position — matches what the
/// POC's 4-minute DJI+GoPro evaluations settled on.
pub const DEFAULT_COAST_FRAMES: u32 = 20;

/// Default player-anchor radius in radians (~11°). Equivalent to
/// the POC's 250-500 px pixel threshold on a 3840-wide frame.
pub const DEFAULT_PLAYER_ANCHOR_RAD: f32 = 0.20;

/// How far the ball may appear to move per tracker tick, in radians
/// of panorama - see [`BallTracker::with_max_ball_speed`].
///
/// A tick is one `update` call, i.e. one processed frame, so at
/// `detection_interval` 3 on 30 fps footage one tick is ~33 ms of
/// video and a fresh detection arrives every third tick. Measured on
/// real 2026-09-12 footage: the flip between two *different* balls
/// covered ~1.5 rad across 6 ticks (0.25 rad/tick) while genuine ball
/// motion between consecutive detections stayed far below that.
/// `0.13 rad/tick` is ~4 rad/s at 30 fps, comfortably above real play
/// and well under a teleport.
pub const DEFAULT_MAX_BALL_SPEED_RAD_PER_TICK: f32 = 0.13;

/// Highest world pitch (radians) a ball detection may sit at and
/// still be considered part of this match - see
/// [`BallTracker::with_max_ball_pitch`].
///
/// `f32::INFINITY` (the default) accepts everything, preserving the
/// pre-2026-09-18 behaviour.
pub const DEFAULT_MAX_BALL_PITCH: f32 = f32::INFINITY;

/// Confidence a detection must carry to be believed across a jump
/// longer than [`DEFAULT_MAX_JUMP_RAD`] while player anchors are
/// active - see [`BallTracker::with_jump_confidence`].
///
/// `0.0` restores the pre-2026-09-18 behaviour (anchors active =>
/// no jump limit at all).
pub const DEFAULT_JUMP_CONFIDENCE: f32 = 0.5;

/// Neighborhood radius (radians) for the acquisition gate's density
/// peak - deliberately the same literal as
/// `FieldPannerConfig::cluster_bandwidth_rad`'s default so "the main
/// group of players" means the same thing to the tracker and to the
/// panner's aim. Duplicated rather than imported for the same reason
/// as [`DEFAULT_ANCHOR_PITCH_NEAR`]: `ball.rs` owns no dependency on
/// the `panners` module.
pub const DEFAULT_ACQUIRE_CLUSTER_BANDWIDTH_RAD: f32 = 0.30;

/// Consecutively tracked frames after which a track counts as
/// "established", arming the acquisition gate - see
/// [`BallTracker::with_acquire_cluster_gate`]. At `detection_interval`
/// 3 on 30 fps footage this is roughly 1.5 s of continuous tracking.
pub const DEFAULT_ACQUIRE_ESTABLISHED_FRAMES: u64 = 45;

/// World pitch (radians) marking the "near" end of the anchor-radius
/// ramp - see [`BallTracker::with_player_anchor_rad_near_far`].
/// Deliberately the same literal as
/// `reco_autocam::panners::FieldPannerConfig::pitch_near`'s default so
/// "near"/"far" mean the same real position everywhere in the autocam
/// stack; duplicated (not imported) because `ball.rs` has no
/// dependency on the `panners` module and shouldn't gain one just for
/// two constants.
pub const DEFAULT_ANCHOR_PITCH_NEAR: f32 = -0.05;

/// World pitch (radians) marking the "far" end of the anchor-radius
/// ramp - see [`DEFAULT_ANCHOR_PITCH_NEAR`].
pub const DEFAULT_ANCHOR_PITCH_FAR: f32 = 0.20;

/// Seconds of accepted measurements the coast-carry velocity is fitted
/// over - see [`BallTracker::with_coast_carry`]. Long enough to average
/// out detection jitter, short enough to follow a change of direction.
pub const COAST_CARRY_WINDOW_SECS: f32 = 1.0;

/// Furthest (radians) coast-carry may move a ball away from its last
/// measurement - see [`BallTracker::with_coast_carry`]. About a third of
/// a typical highlight frame width; a ball undetected longer than that
/// distance takes to cover is better held than guessed.
pub const COAST_CARRY_MAX_RAD: f32 = 0.4;

/// With coast-carry on and a velocity fitted, a detection below the
/// jump confidence farther than this (radians, plus a margin that grows
/// with the blind-carried distance) from the predicted position is
/// ignored - see the prediction gate in
/// [`BallTracker::update`](Tracker::update). Ten times the ~0.005 rad
/// the two cameras disagree on the same ball.
pub const COAST_CARRY_GATE_RAD: f32 = 0.05;

/// Median of `v` (reorders it); `None` when empty.
fn median(v: &mut [f32]) -> Option<f32> {
    if v.is_empty() {
        return None;
    }
    v.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let m = v.len() / 2;
    Some(if v.len().is_multiple_of(2) {
        0.5 * (v[m - 1] + v[m])
    } else {
        v[m]
    })
}

/// Singleton ball tracker emitting at most one
/// [`TrackedEntity`] per frame.
///
/// Internal state is the last accepted measurement, a coaster, and
/// the optional current-frame player anchors. Construct with
/// [`BallTracker::new`], optionally tune via the `with_*` builders,
/// and hand to the session as a `Box<dyn Tracker>`.
pub struct BallTracker {
    class_id: u16,
    coaster: Coaster,
    last: Option<LastKnown>,
    max_jump_rad: f32,
    /// Anchor radius (radians) applied at [`DEFAULT_ANCHOR_PITCH_NEAR`]
    /// or below - see [`with_player_anchor_rad_near_far`](Self::with_player_anchor_rad_near_far).
    player_anchor_rad_near: f32,
    /// Anchor radius (radians) applied at [`DEFAULT_ANCHOR_PITCH_FAR`]
    /// or above.
    player_anchor_rad_far: f32,
    /// Current-frame player anchors in panorama yaw/pitch. Populated each
    /// frame by [`observe_world`](Tracker::observe_world) (the session
    /// calls it after the player tracker runs). Empty when no player
    /// tracker is registered → the player-anchor filter is a no-op.
    current_players: Vec<(f32, f32)>,
    /// Persistent age counter; singleton ball so `id` is always 0
    /// but `age_frames` ticks every frame we're actively tracking.
    age_frames: u64,
    /// Max distance (radians) a *fresh* acquisition may sit from the
    /// dominant player group's centre. `None` disables the gate - see
    /// [`with_acquire_cluster_gate`](BallTracker::with_acquire_cluster_gate).
    acquire_max_dist_from_cluster: Option<f32>,
    /// Neighborhood radius for the acquisition gate's density peak.
    acquire_cluster_bandwidth_rad: f32,
    /// Consecutively tracked frames needed before the acquisition gate
    /// arms itself. 0 arms it immediately.
    acquire_established_frames: u64,
    /// Confidence a detection needs before it may be accepted across a
    /// jump longer than `max_jump_rad` while player anchors are active.
    jump_confidence: f32,
    /// Apparent ball speed limit, radians per tracker tick. `0`
    /// disables.
    max_ball_speed: f32,
    /// Reject ball detections above this world pitch. `INFINITY`
    /// disables.
    max_ball_pitch: f32,
    /// Monotonic tracker tick, incremented once per `update` call.
    /// Used instead of `timestamp_ms` because that carries wall-clock
    /// processing time, not video time: measured medians of 8 ms and
    /// outliers past 200 ms on a 33 ms/frame source would make the
    /// speed limit depend on how busy the machine happens to be.
    tick: u64,
    /// Longest `age_frames` reached by any track so far. Persists
    /// across losses so the gate stays armed once this session has
    /// proven it can hold a real ball; cleared only by
    /// [`Tracker::reset`].
    peak_age_frames: u64,
    /// Accepted measurements `(tick, yaw, pitch)` within the velocity
    /// window, newest last - the input to the coast-carry velocity fit.
    history: VecDeque<(u64, f32, f32)>,
    /// Coast-carry time constant in ticks (`0.0` = off) - see
    /// [`with_coast_carry`](Self::with_coast_carry).
    coast_carry_tau_ticks: f32,
    /// Ticks of history the velocity is fitted over.
    coast_carry_window_ticks: u64,
    /// Ceiling on how far (radians) a coasting position may drift from
    /// the last measurement.
    coast_carry_max_rad: f32,
}

#[derive(Debug, Clone, Copy)]
struct LastKnown {
    yaw: f32,
    pitch: f32,
    origin: CameraId,
    /// Tracker tick at which this position was measured, for the speed
    /// limit in [`BallTracker::max_plausible_jump`].
    tick: u64,
}

impl BallTracker {
    /// Build a new ball tracker tracking the given `class_id` with
    /// default parameters.
    pub fn new(class_id: u16) -> Self {
        Self {
            class_id,
            coaster: Coaster::new(DEFAULT_COAST_FRAMES),
            last: None,
            max_jump_rad: DEFAULT_MAX_JUMP_RAD,
            player_anchor_rad_near: DEFAULT_PLAYER_ANCHOR_RAD,
            player_anchor_rad_far: DEFAULT_PLAYER_ANCHOR_RAD,
            current_players: Vec::new(),
            age_frames: 0,
            acquire_max_dist_from_cluster: None,
            acquire_cluster_bandwidth_rad: DEFAULT_ACQUIRE_CLUSTER_BANDWIDTH_RAD,
            acquire_established_frames: DEFAULT_ACQUIRE_ESTABLISHED_FRAMES,
            jump_confidence: DEFAULT_JUMP_CONFIDENCE,
            max_ball_speed: DEFAULT_MAX_BALL_SPEED_RAD_PER_TICK,
            max_ball_pitch: DEFAULT_MAX_BALL_PITCH,
            tick: 0,
            peak_age_frames: 0,
            history: VecDeque::new(),
            coast_carry_tau_ticks: 0.0,
            coast_carry_window_ticks: 2,
            coast_carry_max_rad: 0.0,
        }
    }

    /// Let a coasting ball keep moving instead of freezing where it was
    /// last seen.
    ///
    /// The velocity is a least-squares fit over the measurements of the
    /// last `window_ticks` ticks. While coasting, the reported position
    /// is `last + v * tau * (1 - exp(-k / tau))` after `k` coast ticks:
    /// it starts at the ball's own speed and eases to a stop after
    /// roughly `2 * tau`, never drifting more than `max_rad` from the
    /// last measurement. `tau_ticks <= 0` turns it off (the default:
    /// hold the last position).
    ///
    /// Why: a ball the model loses mid-run (small, far, blurred) keeps
    /// rolling. Measured on a solo breakaway (2026-11-10): the ball ran
    /// at ~0.1 rad/s, was undetected for 2 s and reappeared 0.28 rad
    /// further on; a frozen coast gave the camera no reason to keep
    /// panning, so it stood still and then had to swing to catch up.
    /// A linear carry would have predicted -0.46 against the real -0.49.
    pub fn with_coast_carry(mut self, tau_ticks: f32, window_ticks: u64, max_rad: f32) -> Self {
        self.coast_carry_tau_ticks = if tau_ticks.is_finite() {
            tau_ticks.max(0.0)
        } else {
            0.0
        };
        self.coast_carry_window_ticks = window_ticks.max(2);
        self.coast_carry_max_rad = if max_rad.is_finite() {
            max_rad.max(0.0)
        } else {
            0.0
        };
        self
    }

    /// Robust straight-line fit of the history window: per axis the
    /// Theil-Sen estimate (median of all pairwise slopes, rad/tick) and
    /// the line's value at tick `at`. `None` with fewer than 3 samples
    /// or a span shorter than a third of the window (too little to tell
    /// motion from detection noise).
    ///
    /// Robust on purpose: the last detection before a loss is often the
    /// weakest and can be a different object. Measured on a breakaway
    /// (2026-11-10): the ball ran -0.17 -> -0.27 rad, then one 0.33
    /// confidence detection at -0.21 came last; a least-squares fit
    /// turned the carry around and moved the ball back.
    fn fitted_line(&self, at: u64) -> Option<((f32, f32), (f32, f32))> {
        if self.history.len() < 3 {
            return None;
        }
        let first = self.history.front()?.0;
        let newest = self.history.back()?.0;
        if ((newest - first) as f32) < self.coast_carry_window_ticks as f32 / 3.0 {
            return None;
        }
        let h: Vec<(f32, f32, f32)> = self
            .history
            .iter()
            .map(|&(t, y, p)| ((t - first) as f32, y, p))
            .collect();
        let mut sy = Vec::with_capacity(h.len() * (h.len() - 1) / 2);
        let mut sp = Vec::with_capacity(sy.capacity());
        for i in 0..h.len() {
            for j in i + 1..h.len() {
                let dt = h[j].0 - h[i].0;
                if dt > 0.0 {
                    sy.push((h[j].1 - h[i].1) / dt);
                    sp.push((h[j].2 - h[i].2) / dt);
                }
            }
        }
        let vy = median(&mut sy)?;
        let vp = median(&mut sp)?;
        let mut iy: Vec<f32> = h.iter().map(|s| s.1 - vy * s.0).collect();
        let mut ip: Vec<f32> = h.iter().map(|s| s.2 - vp * s.0).collect();
        let t_at = at.saturating_sub(first) as f32;
        let y_at = median(&mut iy)? + vy * t_at;
        let p_at = median(&mut ip)? + vp * t_at;
        Some(((vy, vp), (y_at, p_at)))
    }

    /// Where a coasting ball is reported: the last measurement, or with
    /// coast-carry on, the fitted line at the last measurement's tick
    /// carried along the fitted velocity.
    fn carried_position(&self, last: &LastKnown) -> (f32, f32) {
        let tau = self.coast_carry_tau_ticks;
        if tau <= 0.0 {
            return (last.yaw, last.pitch);
        }
        let Some(((vy, vp), (y0, p0))) = self.fitted_line(last.tick) else {
            return (last.yaw, last.pitch);
        };
        let k = self.tick.saturating_sub(last.tick) as f32;
        let travel = tau * (1.0 - (-k / tau).exp());
        let (mut x, mut z) = (y0 + vy * travel - last.yaw, p0 + vp * travel - last.pitch);
        let mag = (x * x + z * z).sqrt();
        if mag > self.coast_carry_max_rad && mag > 0.0 {
            let s = self.coast_carry_max_rad / mag;
            x *= s;
            z *= s;
        }
        (last.yaw + x, last.pitch + z)
    }

    /// Where coast-carry expects the ball this tick, or `None` when
    /// coast-carry is off, there is no track, or the history is too
    /// short for a velocity fit.
    ///
    /// Returns `(yaw, pitch, gate_rad)`: the gate starts at
    /// [`COAST_CARRY_GATE_RAD`] and widens by half the distance the
    /// fitted velocity covers since the last measurement, because the
    /// longer a ball is carried blind the less certain the guess.
    fn carry_prediction(&self) -> Option<(f32, f32, f32)> {
        if self.coast_carry_tau_ticks <= 0.0 {
            return None;
        }
        let last = self.last.as_ref()?;
        let ((vy, vp), _) = self.fitted_line(last.tick)?;
        let (y, p) = self.carried_position(last);
        let k = self.tick.saturating_sub(last.tick) as f32;
        let gate = COAST_CARRY_GATE_RAD + 0.5 * (vy * vy + vp * vp).sqrt() * k;
        Some((y, p, gate))
    }

    /// Record an accepted measurement for the velocity fit and drop
    /// samples older than the window. A detection re-fed unchanged on
    /// the frames between detection passes is stored once, so the fit
    /// weighs each real measurement equally.
    fn push_history(&mut self, yaw: f32, pitch: f32) {
        if self.coast_carry_tau_ticks <= 0.0 {
            return;
        }
        let repeat = self
            .history
            .back()
            .is_some_and(|&(_, y, p)| y == yaw && p == pitch);
        if !repeat {
            self.history.push_back((self.tick, yaw, pitch));
        }
        let cutoff = self.tick.saturating_sub(self.coast_carry_window_ticks);
        while self.history.front().is_some_and(|h| h.0 < cutoff) {
            self.history.pop_front();
        }
    }

    /// Override the per-frame max jump gate (radians).
    ///
    /// Detections whose panorama yaw/pitch is further than this from
    /// the last accepted position are rejected. Cross-camera and
    /// same-camera candidates use the same gate because the
    /// underlying [`ViewportPosition`](reco_core::geometry::ViewportPosition)
    /// yaw/pitch coordinate system is camera-agnostic.
    pub fn with_max_jump_rad(mut self, rad: f32) -> Self {
        self.max_jump_rad = rad.max(0.0);
        self
    }

    /// Override the coast budget.
    pub fn with_max_coast_frames(mut self, n: u32) -> Self {
        self.coaster = Coaster::new(n);
        self
    }

    /// Confidence needed to follow a jump longer than
    /// [`max_jump_rad`](Self::with_max_jump_rad) while player anchors
    /// are active.
    ///
    /// The jump limit used to be skipped entirely whenever anchors were
    /// active, because ANDing both gates could strand the tracker on a
    /// stale `last` forever (see [`score`](Self::score)). But dropping
    /// it altogether lets the tracker teleport between two different
    /// balls: measured on real footage with a neighbouring pitch in
    /// frame, it flipped 1.6 rad back and forth within 0.3 s, at one
    /// point abandoning a 0.90-confidence detection for a 0.28 one, and
    /// followed sub-0.35-confidence detections in 10% of tracked
    /// frames.
    ///
    /// Requiring confidence for the jump keeps both properties: a real
    /// ball reappearing far away (long pass, cross-camera handoff) is
    /// normally detected strongly and still gets through, so the
    /// tracker cannot strand itself; a weak flicker on the other side
    /// of the panorama no longer drags the camera along. `0.0`
    /// restores the old unconditional-jump behaviour.
    pub fn with_jump_confidence(mut self, conf: f32) -> Self {
        self.jump_confidence = conf.clamp(0.0, 1.0);
        self
    }

    /// Limit how far the ball may appear to move per tracker tick
    /// (one processed frame), in radians. `0.0` disables the limit.
    ///
    /// Confidence alone cannot separate two *different* balls that are
    /// both detected well - the measured case had 0.80 and 0.79
    /// confidence on opposite sides of the panorama, each the only
    /// candidate in its frame, and the tracker flipped between them.
    /// Physics can: 1.5 rad in 0.1 s is not a ball, it is a different
    /// ball. The allowance grows with the gap since the last accepted
    /// measurement, so a ball that was genuinely lost for a while can
    /// still be re-acquired anywhere, while a one-frame teleport is
    /// rejected.
    pub fn with_max_ball_speed(mut self, rad_per_tick: f32) -> Self {
        self.max_ball_speed = rad_per_tick.max(0.0);
        self
    }

    /// Reject ball detections sitting higher than this world pitch,
    /// i.e. further away up the frame than this match's play can be.
    ///
    /// On a rig pointed at one pitch, a neighbouring pitch's ball
    /// appears *above* the near touchline and, being further away,
    /// noticeably smaller. Measured on 2026-09-12 footage: every
    /// stray-ball track sat at pitch +0.17..+0.25 with a detection
    /// size around 0.005, while the match ball sat at -0.16..-0.31 at
    /// roughly twice that size. Pitch alone separates them cleanly,
    /// and unlike confidence, player proximity or motion - all three
    /// measured and rejected as unreliable here - it does not depend
    /// on what the other ball happens to be doing.
    ///
    /// This is deliberately a *detection* filter, not the existing
    /// `autocam_pitch_limit` (which only clamps the final camera pose
    /// and so still lets a stray ball drag the aim). `INFINITY`
    /// disables it.
    pub fn with_max_ball_pitch(mut self, pitch: f32) -> Self {
        self.max_ball_pitch = pitch;
        self
    }

    /// The furthest the ball could plausibly have travelled since the
    /// last accepted measurement. `None` when the limit is disabled or
    /// the elapsed time isn't usable (first frame, non-monotonic
    /// timestamps).
    fn max_plausible_jump(&self) -> Option<f32> {
        if self.max_ball_speed <= 0.0 {
            return None;
        }
        let last = self.last?;
        let ticks = self.tick.saturating_sub(last.tick).max(1);
        // `max_jump_rad` of slack on top, so the first sample after a
        // gap isn't judged against a near-zero budget.
        Some(self.max_ball_speed * ticks as f32 + self.max_jump_rad)
    }

    /// Gate *fresh acquisitions* on proximity to the dominant player
    /// group, to reject a stray ball that belongs to somebody else.
    ///
    /// [`passes_player_anchor`](Self::passes_player_anchor) only asks
    /// "is some tracked person near this ball", which a warm-up ball on
    /// an adjacent pitch passes trivially - the kids playing with it are
    /// tracked people too. Observed on real footage: with the match ball
    /// live on the right camera, a second ball entering the left
    /// camera's ROI was acquired and the aim swung to it. This gate adds
    /// the missing question: is the ball near *the match*, i.e. within
    /// `max_dist_rad` of the densest group's centre (the same density
    /// peak `FieldPanner` aims at, so tracker and panner agree on which
    /// players are "the match").
    ///
    /// Deliberately narrow, because a distance rule is wrong in exactly
    /// one important case - a genuinely isolated ball (long clearance,
    /// breakaway, goal kick) that the camera *should* follow:
    ///
    /// - It only runs on a **fresh acquisition** (`last` is `None`).
    ///   An established track keeps its existing behaviour: once the
    ///   right ball is held, nearest-to-last plus the anchor gate carry
    ///   it anywhere on the pitch, and a coasting ball far from the
    ///   cluster still holds the aim (the 2026-09-17 pendulum fix).
    /// - It stays disarmed until some track has reached
    ///   `established_frames` consecutive frames. A kickoff - always from
    ///   the centre, inside any sane radius - arms it; after that the
    ///   tracker has demonstrated it can hold the real ball, so a fresh
    ///   acquisition far from everyone is more likely the neighbouring
    ///   pitch than the match. Pass 0 to arm immediately.
    /// - `None` (the default) disables it entirely.
    ///
    /// `max_dist_rad` is clamped to be non-negative; `bandwidth_rad`
    /// takes [`DEFAULT_ACQUIRE_CLUSTER_BANDWIDTH_RAD`] when not
    /// positive.
    pub fn with_acquire_cluster_gate(
        mut self,
        max_dist_rad: Option<f32>,
        bandwidth_rad: f32,
        established_frames: u64,
    ) -> Self {
        self.acquire_max_dist_from_cluster = max_dist_rad.map(|r| r.max(0.0));
        self.acquire_cluster_bandwidth_rad = if bandwidth_rad > 0.0 {
            bandwidth_rad
        } else {
            DEFAULT_ACQUIRE_CLUSTER_BANDWIDTH_RAD
        };
        self.acquire_established_frames = established_frames;
        self
    }

    /// Override the player-anchor radius (radians), uniformly at every
    /// pitch. Set to a large value (e.g. `f32::INFINITY`) to
    /// effectively disable while keeping the code path active. A thin
    /// wrapper over [`with_player_anchor_rad_near_far`](Self::with_player_anchor_rad_near_far)
    /// with the same value on both ends - kept as the simple default
    /// entry point since most callers (and every test predating the
    /// near/far split) only need one number.
    pub fn with_player_anchor_rad(self, rad: f32) -> Self {
        self.with_player_anchor_rad_near_far(rad, rad)
    }

    /// Override the player-anchor radius (radians) as a ramp between a
    /// "near" value (applied at [`DEFAULT_ANCHOR_PITCH_NEAR`] or below)
    /// and a "far" value (applied at [`DEFAULT_ANCHOR_PITCH_FAR`] or
    /// above), linearly interpolated in between by the *candidate
    /// detection's own pitch* - see [`passes_player_anchor`](Self::passes_player_anchor).
    ///
    /// Exists because this gate's flat radius doesn't account for this
    /// kind of downward-tilted rig's own perspective compression: the
    /// same real-world "a teammate is right next to the ball" distance
    /// maps to a much *larger* panorama-space (yaw, pitch) gap near the
    /// camera (steep viewing angle) than it does far up the pitch
    /// (shallow viewing angle, near the horizon). A single flat radius
    /// tuned to work far up the pitch then rejects a real, anchored,
    /// high-confidence ball near the camera - observed on real XFT-UHTF
    /// footage: a 90%-confidence ball only ~1-2deg outside a 17deg flat
    /// gate near mid-pitch, and a real ball ~25deg from its nearest
    /// teammate (only 4.6deg apart in yaw, 24.5deg apart in pitch) close
    /// to the camera. Both values pass `rad.max(0.0)` independently -
    /// `near < far` is the expected/tuned direction but not enforced,
    /// so an inverted call degrades to "wider near, narrower far"
    /// rather than panicking.
    pub fn with_player_anchor_rad_near_far(mut self, near_rad: f32, far_rad: f32) -> Self {
        self.player_anchor_rad_near = near_rad.max(0.0);
        self.player_anchor_rad_far = far_rad.max(0.0);
        self
    }

    /// The anchor radius (radians) at a given world pitch: linear
    /// interpolation between [`player_anchor_rad_near`](Self::player_anchor_rad_near)
    /// at [`DEFAULT_ANCHOR_PITCH_NEAR`] and [`player_anchor_rad_far`](Self::player_anchor_rad_far)
    /// at [`DEFAULT_ANCHOR_PITCH_FAR`], clamped flat beyond either end
    /// (mirrors the `t_dist` ramp `FieldPannerConfig::target_fov` uses
    /// for its own near/far FOV bias, so the two "near vs far on the
    /// pitch" concepts in the autocam stack behave consistently).
    fn local_anchor_rad(&self, pitch: f32) -> f32 {
        let span = DEFAULT_ANCHOR_PITCH_FAR - DEFAULT_ANCHOR_PITCH_NEAR;
        let t = if span.abs() > 1e-6 {
            ((pitch - DEFAULT_ANCHOR_PITCH_NEAR) / span).clamp(0.0, 1.0)
        } else {
            0.0
        };
        self.player_anchor_rad_near + t * (self.player_anchor_rad_far - self.player_anchor_rad_near)
    }

    /// Supply the current frame's player anchors in panorama yaw/pitch.
    ///
    /// Called each frame by [`observe_world`](Tracker::observe_world) from
    /// the session's `WorldState`, after the player tracker has produced
    /// its output for this frame. Each call replaces the previous anchors -
    /// there is no accumulation. When no anchors are supplied, the
    /// player-anchor filter short-circuits to "accept" (no rejection).
    pub fn set_players(&mut self, players: &[TrackedEntity]) {
        self.current_players.clear();
        self.current_players
            .extend(players.iter().map(|p| (p.yaw, p.pitch)));
    }

    /// Score a candidate detection against the last known position.
    ///
    /// Lower = better. Returns `None` when the jump exceeds
    /// `max_jump_rad` and the candidate hasn't earned it: with no
    /// player anchors active that is always, with anchors active only
    /// below [`with_jump_confidence`](Self::with_jump_confidence). With
    /// no prior position, scoring is pure negative-confidence
    /// (highest-confidence detection wins).
    ///
    /// While player anchors are active the jump gate only rejects
    /// candidates below [`with_jump_confidence`](Self::with_jump_confidence);
    /// it used to be skipped entirely, on this reasoning: every
    /// candidate reaching `score()` already survived
    /// `passes_player_anchor` in [`Tracker::update`], which is itself a
    /// real plausibility check ("this is near an actual tracked
    /// player"), independent of - and no weaker than - "close to
    /// wherever the tracker last thought the ball was". ANDing both
    /// gates let a single missed/implausible detection permanently
    /// strand `last` on a stale or wrong point: every subsequent frame
    /// then re-rejects the genuine, anchor-passing, high-confidence
    /// ball forever (until a full coast-timeout Lost cycle resets
    /// `last` to `None`, restarting the same failure the next time the
    /// ball moves far in one detection interval - a real, observed
    /// symptom: sustained multi-second "ball not tracked" stretches
    /// even while the model detects it, right next to a player, every
    /// single frame). Distance from `last` still breaks ties toward
    /// continuity via the scoring formula below when multiple anchored
    /// candidates compete - it's just no longer a hard cutoff.
    fn score(&self, det: &MappedDetection) -> Option<f32> {
        let pos = det.position?;
        match self.last {
            None => Some(-det.confidence),
            Some(last) => {
                let dy = pos.yaw - last.yaw;
                let dp = pos.pitch - last.pitch;
                let dist = (dy * dy + dp * dp).sqrt();
                // Physics first: no confidence makes a 15 rad/s ball
                // real. Applies regardless of anchors, because this is
                // the one gate two equally-confident different balls
                // cannot both satisfy.
                if let Some(budget) = self.max_plausible_jump()
                    && dist > budget
                {
                    log::trace!(
                        "BallTracker: drop implausible jump - {dist:.2}rad in one step exceeds {budget:.2}rad budget (conf={:.2})",
                        det.confidence
                    );
                    return None;
                }
                // A jump beyond `max_jump_rad` has to earn it. Without
                // player anchors it is rejected outright (the original
                // rule). With anchors active it is allowed only for a
                // detection confident enough to be believed - see
                // `jump_confidence` for why neither "always reject" nor
                // "always allow" works here.
                if dist > self.max_jump_rad
                    && (self.current_players.is_empty() || det.confidence < self.jump_confidence)
                {
                    None
                } else {
                    // Balance proximity and confidence; the 0.1-rad
                    // weight on confidence picks the sharper detection
                    // when two candidates are within a pixel or two.
                    Some(dist - 0.1 * det.confidence)
                }
            }
        }
    }

    /// Decide whether this detection survives the player-anchor gate.
    ///
    /// The radius is evaluated at the *candidate's own pitch* (not each
    /// player's) via [`local_anchor_rad`](Self::local_anchor_rad) - see
    /// that method and [`with_player_anchor_rad_near_far`](Self::with_player_anchor_rad_near_far)
    /// for why a flat radius isn't good enough on a tilted rig.
    fn passes_player_anchor(&self, pos_yaw: f32, pos_pitch: f32) -> bool {
        if self.current_players.is_empty() {
            return true;
        }
        let radius = self.local_anchor_rad(pos_pitch);
        self.current_players.iter().any(|(py, pp)| {
            let dy = pos_yaw - *py;
            let dp = pos_pitch - *pp;
            (dy * dy + dp * dp).sqrt() <= radius
        })
    }

    /// Centre of the densest group among `current_players`: the player
    /// with the most neighbours within
    /// `acquire_cluster_bandwidth_rad`, averaged with those neighbours.
    ///
    /// Mirrors `FieldPanner::densest_cluster` (greedy density peak,
    /// O(n^2) over the tens of players in a frame) so both agree on
    /// which players are "the match"; kept as its own small routine
    /// because `ball.rs` has no dependency on the `panners` module.
    /// `None` when no players are known.
    fn dominant_cluster_centre(&self) -> Option<(f32, f32)> {
        let pts: Vec<(f32, f32)> = self
            .current_players
            .iter()
            .copied()
            .filter(|(y, p)| y.is_finite() && p.is_finite())
            .collect();
        if pts.is_empty() {
            return None;
        }
        let bw_sq = self.acquire_cluster_bandwidth_rad.powi(2);
        let within =
            |a: &(f32, f32), b: &(f32, f32)| (a.0 - b.0).powi(2) + (a.1 - b.1).powi(2) <= bw_sq;
        let centre = pts
            .iter()
            .max_by_key(|c| pts.iter().filter(|p| within(c, p)).count())?;
        let core: Vec<(f32, f32)> = pts.iter().filter(|p| within(centre, p)).copied().collect();
        let n = core.len() as f32;
        Some((
            core.iter().map(|p| p.0).sum::<f32>() / n,
            core.iter().map(|p| p.1).sum::<f32>() / n,
        ))
    }

    /// Whether a *fresh* acquisition at this position is plausible -
    /// see [`with_acquire_cluster_gate`](Self::with_acquire_cluster_gate)
    /// for the rationale and the cases deliberately left untouched.
    ///
    /// Accepts unconditionally when the gate is disabled, still
    /// disarmed, or no players are known (nothing to measure against).
    fn passes_acquire_cluster_gate(&self, pos_yaw: f32, pos_pitch: f32) -> bool {
        let Some(max_dist) = self.acquire_max_dist_from_cluster else {
            return true;
        };
        if self.peak_age_frames < self.acquire_established_frames {
            return true;
        }
        let Some((cy, cp)) = self.dominant_cluster_centre() else {
            return true;
        };
        let dist = ((pos_yaw - cy).powi(2) + (pos_pitch - cp).powi(2)).sqrt();
        if dist > max_dist {
            log::debug!(
                "BallTracker: reject acquisition — yaw={pos_yaw:.3} pitch={pos_pitch:.3} is {dist:.3}rad from the main group at yaw={cy:.3} pitch={cp:.3} (max {max_dist:.3})"
            );
            return false;
        }
        true
    }
}

impl Tracker for BallTracker {
    fn update(&mut self, detections: &[MappedDetection], timestamp_ms: f64) -> Vec<TrackedEntity> {
        let _ = timestamp_ms;
        self.tick = self.tick.saturating_add(1);
        // Step 1-4: filter candidates down to survivors.
        let mut survivors: Vec<&MappedDetection> = Vec::with_capacity(detections.len());
        let prediction = self.carry_prediction();
        for det in detections {
            if det.class_id != self.class_id {
                continue;
            }
            let Some(pos) = det.position else {
                log::trace!(
                    "BallTracker: drop — projection failed (class={} conf={:.2})",
                    det.class_id,
                    det.confidence
                );
                continue;
            };
            if pos.pitch > self.max_ball_pitch {
                log::trace!(
                    "BallTracker: drop above-pitch ball - pitch={:.3} exceeds {:.3} (conf={:.2})",
                    pos.pitch,
                    self.max_ball_pitch,
                    det.confidence
                );
                continue;
            }
            if !self.passes_player_anchor(pos.yaw, pos.pitch) {
                log::trace!(
                    "BallTracker: drop off-player — yaw={:.3} pitch={:.3} nearest player > {:.3}rad (local anchor radius at this pitch)",
                    pos.yaw,
                    pos.pitch,
                    self.local_anchor_rad(pos.pitch)
                );
                continue;
            }
            // Fresh acquisitions only: an established track is carried
            // by nearest-to-last and may legitimately roam far from the
            // group (breakaway, long clearance).
            if self.last.is_none() && !self.passes_acquire_cluster_gate(pos.yaw, pos.pitch) {
                continue;
            }
            // Coast-carry prediction gate: while the track has a fitted
            // velocity, a weak detection must lie near where the ball is
            // expected. Measured on a breakaway (2026-11-10): with the
            // real ball undetected, a static object 2-3x the ball's size
            // (conf 0.33-0.39) sat 0.07 rad behind the run; nearest-to-
            // last took it and froze the ball, so the camera stopped.
            // A confident detection always passes, so a real change of
            // direction is still followed.
            if let Some((py, pp, gate)) = prediction
                && det.confidence < self.jump_confidence
            {
                let d = ((pos.yaw - py).powi(2) + (pos.pitch - pp).powi(2)).sqrt();
                if d > gate {
                    log::trace!(
                        "BallTracker: drop off-course weak ball - yaw={:.3} pitch={:.3} conf={:.2} is {:.3}rad from the predicted {:.3}/{:.3}",
                        pos.yaw,
                        pos.pitch,
                        det.confidence,
                        d,
                        py,
                        pp
                    );
                    continue;
                }
            }
            survivors.push(det);
        }

        // Step 5: nearest-to-last selection.
        let best: Option<&MappedDetection> = survivors
            .iter()
            .filter_map(|d| self.score(d).map(|s| (s, *d)))
            .min_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal))
            .map(|(_, d)| d);

        // Step 6: lifecycle.
        if let Some(det) = best {
            let pos = det.position.expect("score() guarantees Some");
            let coasted = self.coaster.frames_coasting();
            let was_coasting = coasted > 0;
            let was_new_track = self.last.is_none();
            self.coaster.accept_fresh();
            // A reacquisition after a long gap may be a different ball
            // (or a jump the old samples cannot explain): fit fresh. A
            // short gap - one missed detection pass - keeps the history,
            // otherwise every flicker would reset the velocity.
            if was_new_track || u64::from(coasted) > self.coast_carry_window_ticks {
                self.history.clear();
            }
            self.push_history(pos.yaw, pos.pitch);
            self.last = Some(LastKnown {
                yaw: pos.yaw,
                pitch: pos.pitch,
                origin: det.camera,
                tick: self.tick,
            });
            self.age_frames = self.age_frames.saturating_add(1);
            self.peak_age_frames = self.peak_age_frames.max(self.age_frames);

            if was_new_track {
                log::info!(
                    "BallTracker: acquired yaw={:.3} pitch={:.3} cam={:?} conf={:.2}",
                    pos.yaw,
                    pos.pitch,
                    det.camera,
                    det.confidence
                );
            } else if was_coasting {
                log::debug!(
                    "BallTracker: reacquired after coast — yaw={:.3} pitch={:.3} cam={:?} conf={:.2}",
                    pos.yaw,
                    pos.pitch,
                    det.camera,
                    det.confidence
                );
            }

            return vec![TrackedEntity {
                id: 0,
                class_id: self.class_id,
                yaw: pos.yaw,
                pitch: pos.pitch,
                confidence: det.confidence,
                state: TrackState::Tracking,
                age_frames: self.age_frames,
                origin: det.camera,
            }];
        }

        // No fresh detection accepted this frame.
        match self.coaster.step_without_fresh() {
            CoastStatus::Coasting => {
                if let Some(last) = self.last {
                    let (yaw, pitch) = self.carried_position(&last);
                    log::trace!(
                        "BallTracker: coasting - yaw={:.3} pitch={:.3} (last seen {:.3}/{:.3}, {} frames)",
                        yaw,
                        pitch,
                        last.yaw,
                        last.pitch,
                        self.coaster.frames_coasting()
                    );
                    self.age_frames = self.age_frames.saturating_add(1);
                    self.peak_age_frames = self.peak_age_frames.max(self.age_frames);
                    vec![TrackedEntity {
                        id: 0,
                        class_id: self.class_id,
                        yaw,
                        pitch,
                        confidence: 0.0,
                        state: TrackState::Coasting,
                        age_frames: self.age_frames,
                        origin: last.origin,
                    }]
                } else {
                    // Coaster said Coasting but we have no last — only
                    // possible with a concurrent bug. Fail-soft to Lost.
                    log::warn!(
                        "BallTracker: coaster returned Coasting with no last — emitting Lost"
                    );
                    vec![]
                }
            }
            CoastStatus::Lost => {
                // Report the loss where the coast left the ball, not back
                // at the last measurement, so a camera holding on it does
                // not jump back.
                let carried = self.last.as_ref().map(|l| self.carried_position(l));
                self.history.clear();
                if let Some(last) = self.last.take() {
                    let (yaw, pitch) = carried.unwrap_or((last.yaw, last.pitch));
                    log::info!(
                        "BallTracker: track lost after {} coast frames (last yaw={:.3} pitch={:.3})",
                        self.coaster.frames_coasting(),
                        last.yaw,
                        last.pitch
                    );
                    // Age resets on full loss so the next acquisition
                    // starts a fresh count.
                    self.age_frames = 0;
                    vec![TrackedEntity {
                        id: 0,
                        class_id: self.class_id,
                        yaw,
                        pitch,
                        confidence: 0.0,
                        state: TrackState::Lost,
                        age_frames: 0,
                        origin: last.origin,
                    }]
                } else {
                    vec![]
                }
            }
            CoastStatus::Tracking => unreachable!("step_without_fresh never returns Tracking"),
        }
    }

    fn class_id(&self) -> u16 {
        self.class_id
    }

    /// Snapshot the current frame's players into the player-anchor
    /// filter. The session runs the player tracker before the ball
    /// tracker and hands the ball tracker a [`WorldState`] whose
    /// `players` field is already populated for this frame.
    ///
    /// A player tracker that is not registered leaves `world.players`
    /// empty, which `set_players` accepts — the downstream anchor
    /// filter short-circuits to "accept" when no players are known,
    /// preserving the Phase 2c behavior.
    ///
    /// [`WorldState`]: reco_core::detect::tracker::WorldState
    fn observe_world(&mut self, world: &reco_core::detect::tracker::WorldState) {
        self.set_players(&world.players);
    }

    /// Clear the coaster and last-known position so the first
    /// post-discontinuity frame starts from a clean "never tracked"
    /// state instead of coasting a now-stale position (or nearest-to-
    /// last-gating a fresh detection against a position from before
    /// the jump). `current_players`/`age_frames` are reset too since
    /// they're only meaningful relative to the same continuous frame
    /// sequence.
    fn reset(&mut self) {
        self.coaster.reset();
        self.last = None;
        self.history.clear();
        self.current_players.clear();
        self.age_frames = 0;
        self.tick = 0;
        // A discontinuity (seek/cut) invalidates "this session proved it
        // can hold a real ball", so the acquisition gate disarms and the
        // first post-jump acquisition is unconstrained again.
        self.peak_age_frames = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use reco_core::geometry::ViewportPosition;

    fn det(camera: CameraId, yaw: f32, pitch: f32, conf: f32, cx: f32, cy: f32) -> MappedDetection {
        MappedDetection {
            camera,
            class_id: 0,
            confidence: conf,
            camera_center: (cx, cy),
            camera_size: (0.05, 0.05),
            position: Some(ViewportPosition {
                yaw,
                pitch,
                fov_degrees: None,
            }),
            color: None,
        }
    }

    #[test]
    fn empty_detections_produce_nothing() {
        let mut t = BallTracker::new(0);
        let out = t.update(&[], 0.0);
        assert!(out.is_empty());
    }

    #[test]
    fn first_detection_emits_tracking() {
        let mut t = BallTracker::new(0);
        let d = det(CameraId::Left, 0.2, 0.1, 0.8, 0.5, 0.5);
        let out = t.update(&[d], 0.0);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].state, TrackState::Tracking);
        assert_eq!(out[0].yaw, 0.2);
        assert_eq!(out[0].pitch, 0.1);
        assert_eq!(out[0].origin, CameraId::Left);
    }

    #[test]
    fn non_matching_class_id_ignored() {
        let mut t = BallTracker::new(32); // sports ball
        let d = det(CameraId::Left, 0.2, 0.1, 0.8, 0.5, 0.5);
        // d has class_id=0, tracker wants 32 — should be ignored.
        let out = t.update(&[d], 0.0);
        assert!(out.is_empty());
    }

    #[test]
    fn missing_position_ignored() {
        let mut t = BallTracker::new(0);
        let mut d = det(CameraId::Left, 0.2, 0.1, 0.8, 0.5, 0.5);
        d.position = None;
        let out = t.update(&[d], 0.0);
        assert!(out.is_empty());
    }

    #[test]
    fn coast_then_reacquire() {
        let mut t = BallTracker::new(0).with_max_coast_frames(3);
        // Frame 1: acquire.
        let d1 = det(CameraId::Left, 0.2, 0.1, 0.8, 0.5, 0.5);
        let out1 = t.update(&[d1], 0.0);
        assert_eq!(out1[0].state, TrackState::Tracking);
        // Frames 2-3: no detection, coasting.
        let out2 = t.update(&[], 16.6);
        assert_eq!(out2[0].state, TrackState::Coasting);
        let out3 = t.update(&[], 33.3);
        assert_eq!(out3[0].state, TrackState::Coasting);
        // Frame 4: reacquire — state back to Tracking.
        let d4 = det(CameraId::Left, 0.21, 0.11, 0.7, 0.51, 0.51);
        let out4 = t.update(&[d4], 50.0);
        assert_eq!(out4[0].state, TrackState::Tracking);
    }

    #[test]
    fn coast_then_lost() {
        let mut t = BallTracker::new(0).with_max_coast_frames(2);
        let d = det(CameraId::Left, 0.2, 0.1, 0.8, 0.5, 0.5);
        t.update(&[d], 0.0);
        t.update(&[], 16.6); // coast 1
        t.update(&[], 33.3); // coast 2
        let out = t.update(&[], 50.0);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].state, TrackState::Lost);
        // One more — already lost, nothing to emit.
        let out2 = t.update(&[], 66.6);
        assert!(out2.is_empty());
    }

    #[test]
    fn max_jump_rejects_implausible_detection() {
        let mut t = BallTracker::new(0).with_max_jump_rad(0.1);
        // Acquire at yaw=0.
        let d1 = det(CameraId::Left, 0.0, 0.0, 0.9, 0.5, 0.5);
        t.update(&[d1], 0.0);
        // Big jump to yaw=1.0 — exceeds 0.1 gate.
        let d2 = det(CameraId::Left, 1.0, 0.0, 0.9, 0.5, 0.5);
        let out = t.update(&[d2], 16.6);
        // No fresh accepted — tracker coasts on the last known.
        assert_eq!(out[0].state, TrackState::Coasting);
        assert_eq!(out[0].yaw, 0.0);
    }

    #[test]
    fn player_anchor_active_bypasses_max_jump_gate() {
        // Reproduces a real observed failure: a ball genuinely detected
        // right next to a tracked player (high confidence, well inside
        // the anchor radius) kept getting rejected every frame because
        // it was farther than `max_jump_rad` from a stale `last` -
        // permanently stranding the tracker until a full coast-timeout
        // Lost cycle (multiple seconds later). Player-anchor presence
        // must let the anchored candidate through regardless of
        // distance from `last`.
        let mut t = BallTracker::new(0).with_max_jump_rad(0.1);
        // Acquire far away at yaw=-1.0 (no players yet - jump gate
        // active, matches an isolated/breakaway acquisition).
        let d0 = det(CameraId::Left, -1.0, -0.2, 0.6, 0.2, 0.2);
        t.update(&[d0], 0.0);

        // Next frame: a player appears right next to a NEW ball
        // position (yaw=0.5), far outside the 0.1 jump gate from
        // yaw=-1.0 - old code would coast/reject this forever.
        let player = TrackedEntity {
            id: 1,
            class_id: 0,
            yaw: 0.5,
            pitch: 0.28,
            confidence: 0.9,
            state: TrackState::Tracking,
            age_frames: 5,
            origin: CameraId::Left,
        };
        t.set_players(&[player]);
        // The real version of this failure is a ball re-detected after
        // a gap, not one that teleported between two consecutive
        // frames, so let it coast first and the speed limit has room.
        for _ in 0..12 {
            t.update(&[], 0.0);
        }
        t.set_players(&[player]);
        let anchored = det(CameraId::Left, 0.5, 0.28, 0.7, 0.5, 0.5);
        let out = t.update(&[anchored], 500.0);
        assert_eq!(
            out[0].state,
            TrackState::Tracking,
            "an anchored, high-confidence candidate must not be rejected \
             just for being far from a stale `last` position"
        );
        assert!((out[0].yaw - 0.5).abs() < 1e-6);
    }

    /// The 2026-09-18 flip-flop: with two balls in frame (a match ball
    /// and a neighbouring pitch's), a weak detection far away must not
    /// pull an established track across the panorama.
    #[test]
    fn weak_far_detection_cannot_steal_an_established_track() {
        let mut t = BallTracker::new(0)
            .with_player_anchor_rad(100.0)
            .with_max_jump_rad(0.35)
            .with_jump_confidence(0.5);
        t.set_players(&players_at(0.0, 0.0, 4));

        let strong = det(CameraId::Left, 0.40, 0.0, 0.90, 0.5, 0.5);
        assert_eq!(t.update(&[strong], 0.0)[0].state, TrackState::Tracking);

        // Measured shape of the real failure: 0.28-confidence candidate
        // 1.6rad away on the other camera.
        let weak_far = det(CameraId::Right, -1.20, 0.0, 0.28, 0.5, 0.5);
        let out = t.update(&[weak_far], 33.3);
        assert_eq!(
            out[0].state,
            TrackState::Coasting,
            "a weak far detection must be ignored, leaving the track coasting"
        );
        assert!(
            (out[0].yaw - 0.40).abs() < 1e-6,
            "the held position must stay on the real ball"
        );
    }

    /// The other side of that trade-off: a *confident* detection far
    /// away is still followed, so a long pass or a cross-camera handoff
    /// can't strand the tracker on a stale position.
    #[test]
    fn confident_far_detection_is_still_followed() {
        let mut t = BallTracker::new(0)
            .with_player_anchor_rad(100.0)
            .with_max_jump_rad(0.35)
            .with_jump_confidence(0.5);
        t.set_players(&players_at(0.0, 0.0, 4));

        let first = det(CameraId::Left, 0.40, 0.0, 0.90, 0.5, 0.5);
        t.update(&[first], 0.0);

        // A genuine long-range re-acquisition happens after a gap.
        for _ in 0..14 {
            t.update(&[], 100.0);
        }
        let strong_far = det(CameraId::Right, -1.20, 0.0, 0.85, 0.5, 0.5);
        let out = t.update(&[strong_far], 600.0);
        assert_eq!(out[0].state, TrackState::Tracking);
        assert!((out[0].yaw + 1.20).abs() < 1e-6);
        assert_eq!(out[0].origin, CameraId::Right);
    }

    /// When both are on offer the tracker must not abandon a strong
    /// nearby ball for a weak distant one.
    #[test]
    fn strong_near_candidate_beats_weak_far_one() {
        let mut t = BallTracker::new(0)
            .with_player_anchor_rad(100.0)
            .with_max_jump_rad(0.35)
            .with_jump_confidence(0.5);
        t.set_players(&players_at(0.0, 0.0, 4));
        t.update(&[det(CameraId::Left, 0.40, 0.0, 0.90, 0.5, 0.5)], 0.0);

        let near_strong = det(CameraId::Left, 0.45, 0.0, 0.90, 0.5, 0.5);
        let far_weak = det(CameraId::Right, -1.20, 0.0, 0.28, 0.5, 0.5);
        let out = t.update(&[far_weak, near_strong], 33.3);
        assert_eq!(out[0].state, TrackState::Tracking);
        assert!((out[0].yaw - 0.45).abs() < 1e-6);
    }

    /// `0.0` keeps the old "anchors disable the jump limit" behaviour,
    /// so the setting is a true opt-out.
    #[test]
    fn jump_confidence_zero_restores_unconditional_jumps() {
        let mut t = BallTracker::new(0)
            .with_player_anchor_rad(100.0)
            .with_max_jump_rad(0.35)
            .with_jump_confidence(0.0);
        t.set_players(&players_at(0.0, 0.0, 4));
        t.update(&[det(CameraId::Left, 0.40, 0.0, 0.90, 0.5, 0.5)], 0.0);

        for _ in 0..14 {
            t.update(&[], 100.0);
        }
        let weak_far = det(CameraId::Right, -1.20, 0.0, 0.28, 0.5, 0.5);
        let out = t.update(&[weak_far], 600.0);
        assert_eq!(out[0].state, TrackState::Tracking);
        assert!((out[0].yaw + 1.20).abs() < 1e-6);
    }

    /// The residual 2026-09-18 failure that confidence could not fix:
    /// two *equally confident* balls (0.80 vs 0.79 measured), each the
    /// only candidate in its own frame, with the tracker flipping
    /// ~1.5 rad between them in ~0.1 s. Only the speed limit separates
    /// these.
    #[test]
    fn equally_confident_second_ball_cannot_teleport_the_track() {
        let mut t = BallTracker::new(0)
            .with_player_anchor_rad(100.0)
            .with_max_jump_rad(0.35)
            .with_jump_confidence(0.5)
            .with_max_ball_speed(0.13);
        t.set_players(&players_at(0.0, 0.0, 4));

        t.update(&[det(CameraId::Left, 0.34, 0.0, 0.80, 0.5, 0.5)], 0.0);

        // 1.5 rad on the very next tick. Not a ball.
        let other_ball = det(CameraId::Right, -1.16, 0.0, 0.79, 0.5, 0.5);
        let out = t.update(&[other_ball], 100.0);
        assert_eq!(out[0].state, TrackState::Coasting);
        assert!((out[0].yaw - 0.34).abs() < 1e-6);
    }

    /// The allowance grows with the gap, so a ball that really was
    /// gone for a while can still be picked up far away.
    #[test]
    fn speed_limit_allows_a_far_jump_after_a_long_gap() {
        let mut t = BallTracker::new(0)
            .with_player_anchor_rad(100.0)
            .with_max_jump_rad(0.35)
            .with_jump_confidence(0.0)
            .with_max_ball_speed(0.13);
        t.set_players(&players_at(0.0, 0.0, 4));
        t.update(&[det(CameraId::Left, 0.34, 0.0, 0.80, 0.5, 0.5)], 0.0);

        // Let the track coast for a while; the budget grows per tick,
        // so the same 1.5 rad becomes ordinary rather than impossible.
        for i in 0..14 {
            t.update(&[], 100.0 + i as f64);
        }
        let far = det(CameraId::Right, -1.16, 0.0, 0.79, 0.5, 0.5);
        let out = t.update(&[far], 200.0);
        assert_eq!(out[0].state, TrackState::Tracking);
        assert!((out[0].yaw + 1.16).abs() < 1e-6);
    }

    /// Normal play must be untouched: a ball moving at a realistic
    /// speed between detection samples is still followed.
    #[test]
    fn speed_limit_does_not_disturb_normal_play() {
        let mut t = BallTracker::new(0)
            .with_player_anchor_rad(100.0)
            .with_max_ball_speed(0.13);
        t.set_players(&players_at(0.0, 0.0, 4));
        t.update(&[det(CameraId::Left, 0.0, 0.0, 0.80, 0.5, 0.5)], 0.0);
        // 0.3 rad on the next tick: within 0.13 + max_jump_rad slack.
        let moved = det(CameraId::Left, 0.30, 0.0, 0.80, 0.5, 0.5);
        let out = t.update(&[moved], 100.0);
        assert_eq!(out[0].state, TrackState::Tracking);
        assert!((out[0].yaw - 0.30).abs() < 1e-6);
    }

    /// `0.0` disables the limit entirely.
    #[test]
    fn speed_limit_zero_disables_the_check() {
        let mut t = BallTracker::new(0)
            .with_player_anchor_rad(100.0)
            .with_jump_confidence(0.0)
            .with_max_ball_speed(0.0);
        t.set_players(&players_at(0.0, 0.0, 4));
        t.update(&[det(CameraId::Left, 0.34, 0.0, 0.80, 0.5, 0.5)], 0.0);
        let teleport = det(CameraId::Right, -1.16, 0.0, 0.79, 0.5, 0.5);
        let out = t.update(&[teleport], 100.0);
        assert_eq!(out[0].state, TrackState::Tracking);
    }

    /// A ball on the neighbouring pitch sits higher in frame than this
    /// match's play can reach, and is rejected outright - the one
    /// signal that separated the two balls on real 2026-09-12 footage
    /// where confidence, player proximity and motion all failed.
    #[test]
    fn ball_above_the_pitch_ceiling_is_rejected() {
        let mut t = BallTracker::new(0).with_max_ball_pitch(0.15);
        // Measured stray-ball geometry: high in frame, confident.
        let stray = det(CameraId::Left, 0.34, 0.25, 0.80, 0.5, 0.5);
        assert!(
            t.update(&[stray], 0.0).is_empty(),
            "a ball above the ceiling must never start a track"
        );
        // Measured match-ball geometry: below the horizon line.
        let match_ball = det(CameraId::Right, -1.16, -0.16, 0.79, 0.5, 0.5);
        assert_eq!(t.update(&[match_ball], 33.3)[0].state, TrackState::Tracking);
    }

    /// The ceiling also protects an established track: a stray ball
    /// cannot steal one mid-play.
    #[test]
    fn pitch_ceiling_also_guards_an_established_track() {
        let mut t = BallTracker::new(0).with_max_ball_pitch(0.15);
        t.update(&[det(CameraId::Right, -1.16, -0.16, 0.80, 0.5, 0.5)], 0.0);
        let stray = det(CameraId::Left, 0.34, 0.25, 0.90, 0.5, 0.5);
        let out = t.update(&[stray], 33.3);
        assert_eq!(out[0].state, TrackState::Coasting);
        assert!((out[0].yaw + 1.16).abs() < 1e-6);
    }

    /// Default is off, so nothing changes until the user opts in.
    #[test]
    fn pitch_ceiling_off_by_default() {
        let mut t = BallTracker::new(0);
        let high = det(CameraId::Left, 0.34, 0.25, 0.80, 0.5, 0.5);
        assert_eq!(t.update(&[high], 0.0)[0].state, TrackState::Tracking);
    }

    /// Build `n` players clustered tightly around `(yaw, pitch)`.
    fn players_at(yaw: f32, pitch: f32, n: usize) -> Vec<TrackedEntity> {
        (0..n)
            .map(|i| TrackedEntity {
                id: i as u64,
                class_id: 0,
                yaw: yaw + (i as f32) * 0.01,
                pitch: pitch + (i as f32) * 0.01,
                confidence: 0.9,
                state: TrackState::Tracking,
                age_frames: 5,
                origin: CameraId::Left,
            })
            .collect()
    }

    /// The real 2026-09-18 failure: with the acquisition gate armed, a
    /// ball on a neighbouring pitch - flanked by its own (tracked) kids,
    /// so the player-anchor gate waves it through - must not start a
    /// track, because it is nowhere near the main group.
    #[test]
    fn acquire_gate_rejects_stray_ball_near_its_own_bystanders() {
        let mut t = BallTracker::new(0)
            .with_player_anchor_rad(0.30)
            .with_acquire_cluster_gate(Some(0.6), 0.30, 0);

        // The match: a dense group at yaw≈0.0, plus two bystanders far
        // away at yaw≈2.0 standing next to the stray ball.
        let mut world = players_at(0.0, 0.1, 6);
        world.extend(players_at(2.0, 0.1, 2));
        t.set_players(&world);

        let stray = det(CameraId::Left, 2.0, 0.1, 0.9, 0.5, 0.5);
        let out = t.update(&[stray], 0.0);
        assert!(
            out.is_empty(),
            "a ball {:.1}rad from the main group must not start a track",
            2.0_f32
        );

        // The match ball, inside the same frame's main group, still does.
        let real = det(CameraId::Left, 0.05, 0.1, 0.5, 0.5, 0.5);
        let out = t.update(&[real], 16.6);
        assert_eq!(out[0].state, TrackState::Tracking);
        assert!((out[0].yaw - 0.05).abs() < 1e-6);
    }

    /// Kickoff case: until a track has proven itself for
    /// `established_frames`, the gate stays disarmed so the very first
    /// acquisition of a session is never blocked.
    #[test]
    fn acquire_gate_disarmed_until_a_track_is_established() {
        let mut t = BallTracker::new(0)
            .with_player_anchor_rad(100.0)
            .with_acquire_cluster_gate(Some(0.5), 0.30, 3);
        t.set_players(&players_at(0.0, 0.1, 5));

        // Far from the group, but nothing has been tracked yet.
        let far = det(CameraId::Left, 2.0, 0.1, 0.9, 0.5, 0.5);
        let out = t.update(&[far], 0.0);
        assert_eq!(
            out[0].state,
            TrackState::Tracking,
            "the gate must stay disarmed before any track is established"
        );
    }

    /// Once armed, the gate must still never touch an *established*
    /// track - a breakaway ball racing away from the pack keeps being
    /// followed, since the gate only guards fresh acquisitions.
    #[test]
    fn acquire_gate_never_blocks_an_established_track() {
        let mut t = BallTracker::new(0)
            .with_player_anchor_rad(100.0)
            .with_max_jump_rad(f32::INFINITY)
            .with_acquire_cluster_gate(Some(0.5), 0.30, 2);
        t.set_players(&players_at(0.0, 0.1, 5));

        // Acquire near the group and hold it long enough to arm.
        for i in 0..4 {
            let d = det(CameraId::Left, 0.02, 0.1, 0.8, 0.5, 0.5);
            let out = t.update(&[d], i as f64 * 16.6);
            assert_eq!(out[0].state, TrackState::Tracking);
        }

        // Now the ball breaks away well beyond the gate's radius while
        // the players stay put. The track continues.
        let breakaway = det(CameraId::Left, 2.0, 0.1, 0.8, 0.5, 0.5);
        let out = t.update(&[breakaway], 100.0);
        assert_eq!(
            out[0].state,
            TrackState::Tracking,
            "an established track must follow the ball anywhere"
        );
        assert!((out[0].yaw - 2.0).abs() < 1e-6);
    }

    /// The gate is opt-in: the default tracker behaves exactly as
    /// before, accepting a far acquisition that passes the anchor gate.
    #[test]
    fn acquire_gate_off_by_default() {
        let mut t = BallTracker::new(0).with_player_anchor_rad(100.0);
        t.set_players(&players_at(0.0, 0.1, 5));
        let far = det(CameraId::Left, 3.0, 0.1, 0.9, 0.5, 0.5);
        let out = t.update(&[far], 0.0);
        assert_eq!(out[0].state, TrackState::Tracking);
    }

    /// With no players in the frame there is nothing to measure
    /// against, so the gate must not silently block every acquisition.
    #[test]
    fn acquire_gate_accepts_when_no_players_known() {
        let mut t = BallTracker::new(0).with_acquire_cluster_gate(Some(0.1), 0.30, 0);
        // No set_players() call at all.
        let d = det(CameraId::Left, 3.0, 0.1, 0.9, 0.5, 0.5);
        let out = t.update(&[d], 0.0);
        assert_eq!(out[0].state, TrackState::Tracking);
    }

    /// The density peak must follow the *bigger* group, not the mean of
    /// both, so a ball at the main group is accepted even when a
    /// distant knot of bystanders would drag a plain average away.
    #[test]
    fn acquire_gate_measures_against_the_densest_group() {
        let mut t = BallTracker::new(0)
            .with_player_anchor_rad(100.0)
            .with_acquire_cluster_gate(Some(0.4), 0.30, 0);
        let mut world = players_at(0.0, 0.0, 8);
        world.extend(players_at(3.0, 0.0, 3));
        t.set_players(&world);

        // Midway between the two groups - where a global mean would
        // land - must be rejected.
        let midway = det(CameraId::Left, 1.0, 0.0, 0.9, 0.5, 0.5);
        assert!(t.update(&[midway], 0.0).is_empty());

        // At the dominant group: accepted.
        let at_main = det(CameraId::Left, 0.05, 0.0, 0.9, 0.5, 0.5);
        assert_eq!(t.update(&[at_main], 16.6)[0].state, TrackState::Tracking);
    }

    #[test]
    fn cross_camera_handoff_tracks() {
        let mut t = BallTracker::new(0).with_max_jump_rad(0.3);
        // Acquire on left at yaw=0.15.
        let d1 = det(CameraId::Left, 0.15, 0.0, 0.8, 0.9, 0.5);
        let out1 = t.update(&[d1], 0.0);
        assert_eq!(out1[0].origin, CameraId::Left);
        // Next frame: right camera reports the same ball at close yaw.
        // Even though pixel coords are totally different (ball now at
        // left edge of right frame), the panorama yaw distance (0.05)
        // is within max_jump — tracker must switch cameras.
        let d2 = det(CameraId::Right, 0.20, 0.0, 0.75, 0.05, 0.5);
        let out2 = t.update(&[d2], 16.6);
        assert_eq!(out2[0].state, TrackState::Tracking);
        assert_eq!(out2[0].origin, CameraId::Right);
    }

    #[test]
    fn player_anchor_rejects_far_ball_when_players_present() {
        let mut t = BallTracker::new(0).with_player_anchor_rad(0.1);
        // Inject one player at yaw=1.0.
        let player = TrackedEntity {
            id: 1,
            class_id: 0,
            yaw: 1.0,
            pitch: 0.0,
            confidence: 0.9,
            state: TrackState::Tracking,
            age_frames: 5,
            origin: CameraId::Right,
        };
        t.set_players(&[player]);
        // Ball at yaw=0.2 is 0.8 rad from player — rejected.
        let d = det(CameraId::Left, 0.2, 0.0, 0.9, 0.5, 0.5);
        let out = t.update(&[d], 0.0);
        assert!(out.is_empty());
    }

    #[test]
    fn player_anchor_near_far_ramp_widens_close_to_the_camera() {
        // Same 0.3rad-away ball/player pair, evaluated at two different
        // pitches - a flat radius would reject both identically; the
        // near/far ramp must accept the near-pitch one and still reject
        // the far-pitch one, since only the near end was widened.
        let near_pitch = DEFAULT_ANCHOR_PITCH_NEAR;
        let far_pitch = DEFAULT_ANCHOR_PITCH_FAR;

        // near=0.35 (wide, matches this session's real-footage finding),
        // far=0.1 (tight, the flat default this test's sibling uses).
        let mut t_near = BallTracker::new(0).with_player_anchor_rad_near_far(0.35, 0.1);
        let player_near = TrackedEntity {
            id: 1,
            class_id: 0,
            yaw: 1.0,
            pitch: near_pitch,
            confidence: 0.9,
            state: TrackState::Tracking,
            age_frames: 5,
            origin: CameraId::Right,
        };
        t_near.set_players(&[player_near]);
        // 0.3rad away in yaw, same (near) pitch as the player - within
        // the 0.35 near radius.
        let d_near = det(CameraId::Left, 1.3, near_pitch, 0.9, 0.5, 0.5);
        assert_eq!(
            t_near.update(&[d_near], 0.0).len(),
            1,
            "a 0.3rad-away ball at the near pitch must be accepted by the widened near radius"
        );

        let mut t_far = BallTracker::new(0).with_player_anchor_rad_near_far(0.35, 0.1);
        let player_far = TrackedEntity {
            pitch: far_pitch,
            ..player_near
        };
        t_far.set_players(&[player_far]);
        // Same 0.3rad yaw offset, but at the far pitch - only the
        // (tight) far radius applies here, so this must still reject.
        let d_far = det(CameraId::Left, 1.3, far_pitch, 0.9, 0.5, 0.5);
        assert!(
            t_far.update(&[d_far], 0.0).is_empty(),
            "the same 0.3rad-away ball at the far pitch must still be rejected by the tight far radius"
        );
    }

    #[test]
    fn player_anchor_no_op_when_no_players_set() {
        let mut t = BallTracker::new(0).with_player_anchor_rad(0.1);
        // No set_players() call — filter should not reject.
        let d = det(CameraId::Left, 0.2, 0.0, 0.9, 0.5, 0.5);
        let out = t.update(&[d], 0.0);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].state, TrackState::Tracking);
    }

    #[test]
    fn class_id_accessor() {
        let t = BallTracker::new(32);
        assert_eq!(t.class_id(), 32);
    }

    #[test]
    fn observe_world_populates_player_anchors() {
        use reco_core::detect::tracker::WorldState;
        let mut t = BallTracker::new(0).with_player_anchor_rad(0.1);
        let player = TrackedEntity {
            id: 7,
            class_id: 0,
            yaw: 1.0,
            pitch: 0.0,
            confidence: 0.9,
            state: TrackState::Tracking,
            age_frames: 3,
            origin: CameraId::Right,
        };
        let world = WorldState {
            ball: None,
            players: vec![player],
        };
        t.observe_world(&world);
        // A ball far from the (only) player must be rejected by the
        // anchor filter — proves observe_world propagated players.
        let d = det(CameraId::Left, 0.2, 0.0, 0.9, 0.5, 0.5);
        let out = t.update(&[d], 0.0);
        assert!(out.is_empty(), "observe_world did not populate anchors");
    }

    #[test]
    fn observe_world_empty_players_leaves_filter_as_noop() {
        use reco_core::detect::tracker::WorldState;
        let mut t = BallTracker::new(0).with_player_anchor_rad(0.1);
        // Pre-seed with anchors, then observe an empty world: set_players
        // replaces (does not accumulate), so the filter becomes a no-op.
        let player = TrackedEntity {
            id: 1,
            class_id: 0,
            yaw: 1.0,
            pitch: 0.0,
            confidence: 0.9,
            state: TrackState::Tracking,
            age_frames: 1,
            origin: CameraId::Right,
        };
        t.set_players(&[player]);
        t.observe_world(&WorldState::default());
        let d = det(CameraId::Left, 0.2, 0.0, 0.9, 0.5, 0.5);
        let out = t.update(&[d], 0.0);
        assert_eq!(out.len(), 1, "empty world should reset anchors");
    }

    #[test]
    fn prefers_closer_candidate_over_higher_confidence() {
        let mut t = BallTracker::new(0).with_max_jump_rad(1.0);
        // Acquire at yaw=0.0.
        let d0 = det(CameraId::Left, 0.0, 0.0, 0.9, 0.5, 0.5);
        t.update(&[d0], 0.0);
        // Two candidates: one high-conf far (0.4 rad), one low-conf close (0.05 rad).
        // Score balances proximity against confidence (0.1-rad weight);
        // the close candidate wins because proximity dominates.
        let far = det(CameraId::Left, 0.40, 0.0, 0.95, 0.5, 0.5);
        let near = det(CameraId::Left, 0.05, 0.0, 0.55, 0.5, 0.5);
        let out = t.update(&[far, near], 16.6);
        assert_eq!(out.len(), 1);
        assert!(
            (out[0].yaw - 0.05).abs() < 1e-6,
            "expected near, got {}",
            out[0].yaw
        );
    }

    /// Feed a ball moving `step` rad/tick in yaw for `n` ticks through a
    /// coast-carry tracker (tau 30 ticks, window 30, cap 0.4).
    fn carry_tracker_after_run(step: f32, n: usize) -> (BallTracker, f32) {
        let mut t = BallTracker::new(0)
            .with_max_coast_frames(120)
            .with_coast_carry(30.0, 30, 0.4);
        let mut yaw = 0.0;
        for i in 0..n {
            yaw = step * i as f32;
            t.update(&[det(CameraId::Left, yaw, 0.1, 0.8, 0.5, 0.5)], 0.0);
        }
        (t, yaw)
    }

    #[test]
    fn coast_without_carry_holds_the_last_position() {
        let mut t = BallTracker::new(0).with_max_coast_frames(60);
        for i in 0..30 {
            t.update(
                &[det(CameraId::Left, -0.01 * i as f32, 0.1, 0.8, 0.5, 0.5)],
                0.0,
            );
        }
        for _ in 0..20 {
            let out = t.update(&[], 0.0);
            assert_eq!(out[0].state, TrackState::Coasting);
            assert!((out[0].yaw - (-0.29)).abs() < 1e-6);
        }
    }

    #[test]
    fn coast_carry_keeps_the_ball_moving_and_eases_out() {
        let (mut t, last) = carry_tracker_after_run(-0.005, 40);
        let mut prev = last;
        let mut first_step = 0.0;
        let mut last_step = 0.0;
        for k in 0..90 {
            let out = t.update(&[], 0.0);
            assert_eq!(out[0].state, TrackState::Coasting);
            let step = prev - out[0].yaw;
            assert!(
                step >= -1e-6,
                "carry must keep going the same way (tick {k})"
            );
            if k == 1 {
                first_step = step;
            }
            last_step = step;
            prev = out[0].yaw;
        }
        assert!(
            prev < last - 0.1,
            "carried {last} -> {prev}, expected well past the last fix"
        );
        assert!(
            last_step < 0.25 * first_step,
            "carry must slow down ({first_step} -> {last_step})"
        );
        assert!((last - prev) <= 0.4 + 1e-4, "carry is capped at 0.4 rad");
    }

    #[test]
    fn coast_carry_is_capped() {
        // A fast ball (0.03 rad/tick) carried for 3 s would travel ~0.9 rad.
        let (mut t, last) = carry_tracker_after_run(-0.03, 20);
        let mut out = Vec::new();
        for _ in 0..90 {
            out = t.update(&[], 0.0);
        }
        assert!(
            (last - out[0].yaw - 0.4).abs() < 1e-3,
            "drift {}",
            last - out[0].yaw
        );
    }

    #[test]
    fn coast_carry_ignores_a_weak_off_course_detection() {
        // The 2026-11-10 breakaway: the ball runs right, the model loses
        // it, and a weak static object behind the run is offered instead.
        let (mut t, last) = carry_tracker_after_run(-0.004, 40);
        let out = t.update(
            &[det(CameraId::Right, last + 0.07, 0.1, 0.35, 0.5, 0.5)],
            0.0,
        );
        assert_eq!(
            out[0].state,
            TrackState::Coasting,
            "weak off-course detection must be ignored"
        );
        assert!(
            out[0].yaw < last,
            "the carry continues past the last fix, got {}",
            out[0].yaw
        );
    }

    #[test]
    fn coast_carry_still_follows_a_confident_detection() {
        // A real change of direction is detected confidently and wins.
        let (mut t, last) = carry_tracker_after_run(-0.004, 40);
        let out = t.update(
            &[det(CameraId::Right, last + 0.07, 0.1, 0.8, 0.5, 0.5)],
            0.0,
        );
        assert_eq!(out[0].state, TrackState::Tracking);
        assert!((out[0].yaw - (last + 0.07)).abs() < 1e-6);
    }

    #[test]
    fn coast_carry_velocity_ignores_a_weak_outlier_last_fix() {
        // Robust fit: one off-line fix inside the gate (a jittery last
        // detection) must not turn the carry around.
        let (mut t, last) = carry_tracker_after_run(-0.004, 40);
        t.update(
            &[det(CameraId::Right, last + 0.02, 0.1, 0.8, 0.5, 0.5)],
            0.0,
        );
        let mut out = Vec::new();
        for _ in 0..30 {
            out = t.update(&[], 0.0);
        }
        assert!(
            out[0].yaw < last,
            "carry must keep the run's direction, got {} vs {last}",
            out[0].yaw
        );
    }
}
