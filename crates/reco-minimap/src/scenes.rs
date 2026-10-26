//! Game situations ("scenes") recognised from tracked positions: restarts and set pieces whose
//! formation is easy to see - tip-off, free throw, throw-in / inbound, kick-off, corner, penalty,
//! 7-metre throw, and basketball shot attempts (2 / 3 points).
//!
//! Everything here is rule-based on the *tracked state* (positions in metres, speeds, ball state), so it
//! inherits the quality of detection and calibration. Dead-ball formations are recognised from where
//! people stand and that they stand still; they must hold for a moment before an event fires, and an
//! event fires once per formation. Shot attempts additionally need the ball: they fire when the ball
//! leaves a holder's hands towards a basket.
//!
//! Thresholds are first estimates validated on constructed scenes only - tune them on real footage.

use std::collections::{HashMap, HashSet};
mod basketball;
mod formations;

use crate::ball::BallState;
use crate::geom::Vec2;
use crate::sport::SportProfile;
use crate::sport::rules::{AnalysisRules, SceneRules};
use crate::team::Team;
use crate::tracker::TrackState;
use crate::{AgentOut, FrameState};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SceneKind {
    Kickoff,
    TipOff,
    FreeThrow,
    ThrowIn,
    Corner,
    Penalty,
    SevenMeter,
    ShotTwo,
    ShotThree,
}

impl SceneKind {
    pub fn key(self) -> &'static str {
        match self {
            SceneKind::Kickoff => "kickoff",
            SceneKind::TipOff => "tip_off",
            SceneKind::FreeThrow => "free_throw",
            SceneKind::ThrowIn => "throw_in",
            SceneKind::Corner => "corner",
            SceneKind::Penalty => "penalty",
            SceneKind::SevenMeter => "seven_meter",
            SceneKind::ShotTwo => "shot2",
            SceneKind::ShotThree => "shot3",
        }
    }

    /// Seconds the formation must hold before the event fires.
    fn hold(self) -> f64 {
        match self {
            SceneKind::TipOff => 1.0,
            SceneKind::ThrowIn => 1.2,
            SceneKind::Kickoff | SceneKind::Corner => 1.5,
            SceneKind::FreeThrow | SceneKind::Penalty | SceneKind::SevenMeter => 2.0,
            SceneKind::ShotTwo | SceneKind::ShotThree => 0.0,
        }
    }
}

#[derive(Clone, Debug)]
pub struct SceneEvent {
    /// First observation of this formation, not the delayed reporting time.
    pub started_at: f64,
    /// Heuristic evidence, not calibrated probabilities or referee decisions.
    pub evidence: Vec<&'static str>,
    /// Temporal context only: does not upgrade a possible substitution.
    pub related_substitutions: Vec<crate::SubEvent>,
    pub t: f64,
    pub kind: SceneKind,
    /// Team that takes the restart / shot (kicker, thrower, shooter).
    pub team: Option<Team>,
    pub pos: Vec2,
    /// `'l'` / `'r'` end of the field the scene belongs to, if any.
    pub side: Option<char>,
    pub confidence: f32,
    /// Shot attempts only: the release point is within the position error of the 3-point line.
    pub borderline: bool,
}

#[derive(Clone, Debug)]
pub struct SceneCfg {
    /// A player moving slower than this (m/s) counts as standing still.
    pub still_speed: f32,
    /// A formation may disappear this long (s) without resetting its hold timer.
    pub gap_tolerance: f64,
    /// Seconds without the formation before it can fire again.
    pub rearm_gap: f64,
    /// Minimum seconds between two events of the same kind and place.
    pub cooldown: f64,
}

impl Default for SceneCfg {
    fn default() -> Self {
        Self {
            still_speed: 0.9,
            gap_tolerance: 0.6,
            rearm_gap: 1.0,
            cooldown: 6.0,
        }
    }
}

struct Cand {
    kind: SceneKind,
    key: u8,
    team: Option<Team>,
    pos: Vec2,
    side: Option<char>,
    conf: f32,
}

struct Entry {
    since: f64,
    last_seen: f64,
    fired: bool,
}

struct PendingShot {
    t0: f64,
    release: Vec2,
    team: Team,
    goal: Vec2,
    start: f32,
    best: f32,
}

pub struct SceneDetector {
    rules: SceneRules,
    profile: SportProfile,
    pub cfg: SceneCfg,
    active: HashMap<(SceneKind, u8), Entry>,
    last_fire: HashMap<(SceneKind, u8), f64>,
    holder: Option<(Vec2, Team, f64)>,
    prev_state: Option<BallState>,
    pending: Option<PendingShot>,
    recent_substitutions: Vec<crate::SubEvent>,
    last_t: Option<f64>,
}

fn in_rect(p: Vec2, r: (f32, f32, f32, f32), pad: f32) -> bool {
    p.x >= r.0 - pad && p.x <= r.2 + pad && p.y >= r.1 - pad && p.y <= r.3 + pad
}

impl SceneDetector {
    pub fn new(profile: SportProfile) -> Self {
        let rules = AnalysisRules::for_sport(&profile.id).scenes;
        Self::with_rules(profile, rules)
    }

    /// Explicit policy allows custom field dimensions without guessing rules from
    /// geometry or a display name. Unsupported sports use `SceneRules::None`.
    pub fn with_rules(profile: SportProfile, rules: SceneRules) -> Self {
        Self {
            rules,
            profile,
            cfg: SceneCfg::default(),
            active: HashMap::new(),
            last_fire: HashMap::new(),
            holder: None,
            prev_state: None,
            pending: None,
            recent_substitutions: Vec::new(),
            last_t: None,
        }
    }

    pub fn profile(&self) -> &SportProfile {
        &self.profile
    }

    /// Feed the next frame; returns the scenes that fired in it.
    pub fn update(&mut self, s: &FrameState) -> Vec<SceneEvent> {
        let mut out = Vec::new();
        let t = s.t;
        if !t.is_finite() {
            return out;
        }
        // Seeking or sparse/missing input must not bridge unrelated sequences.
        if self
            .last_t
            .map(|last| t < last || t - last > 3.0)
            .unwrap_or(false)
        {
            self.active.clear();
            self.last_fire.clear();
            self.recent_substitutions.clear();
            self.holder = None;
            self.prev_state = None;
            self.pending = None;
        }
        self.last_t = Some(t);
        if self.rules == SceneRules::Basketball {
            self.recent_substitutions
                .retain(|e| t - e.t >= 0.0 && t - e.t <= 30.0);
            for e in &s.events {
                if e.t.is_finite()
                    && (0.0..=30.0).contains(&(t - e.t))
                    && !self
                        .recent_substitutions
                        .iter()
                        .any(|old| old.t == e.t && old.slot == e.slot && old.team == e.team)
                {
                    self.recent_substitutions.push(e.clone());
                }
            }
        }
        let players: Vec<&AgentOut> = s
            .players
            .iter()
            .filter(|p| {
                if self.rules == SceneRules::Basketball {
                    p.state == TrackState::Visible
                } else {
                    p.state != TrackState::Lost
                }
            })
            .collect();
        let mut cands: Vec<Cand> = Vec::new();
        if players.len() >= 4 {
            self.patterns(s, &players, &mut cands);
        }
        let mut present: HashSet<(SceneKind, u8)> = HashSet::new();
        for c in cands {
            let key = (c.kind, c.key);
            present.insert(key);
            let e = self.active.entry(key).or_insert(Entry {
                since: t,
                last_seen: t,
                fired: false,
            });
            if t - e.last_seen > self.cfg.gap_tolerance {
                *e = Entry {
                    since: t,
                    last_seen: t,
                    fired: false,
                };
            }
            e.last_seen = t;
            let last = self
                .last_fire
                .get(&key)
                .copied()
                .unwrap_or(f64::NEG_INFINITY);
            if !e.fired && t - e.since >= c.kind.hold() && t - last >= self.cfg.cooldown {
                e.fired = true;
                self.last_fire.insert(key, t);
                let mut evidence = vec!["persistent_formation"];
                if c.kind == SceneKind::FreeThrow {
                    evidence.extend([
                        "shooter_at_line",
                        "rebounders_both_lane_edges",
                        "lane_centre_clear",
                    ]);
                }
                if c.kind == SceneKind::FreeThrow && matches!(s.ball.state, BallState::Unknown) {
                    evidence.push("ball_unobserved");
                }
                let related_substitutions =
                    if self.rules == SceneRules::Basketball && c.kind == SceneKind::ThrowIn {
                        self.recent_substitutions
                            .iter()
                            .filter(|sub| sub.t <= t)
                            .cloned()
                            .collect::<Vec<_>>()
                    } else {
                        Vec::new()
                    };
                if !related_substitutions.is_empty() {
                    evidence.push("recent_substitution_candidate");
                }
                out.push(SceneEvent {
                    t,
                    started_at: e.since,
                    evidence,
                    related_substitutions,
                    kind: c.kind,
                    team: c.team,
                    pos: c.pos,
                    side: c.side,
                    confidence: c.conf,
                    borderline: false,
                });
            }
        }
        let rearm = self.cfg.rearm_gap;
        self.active
            .retain(|k, e| present.contains(k) || t - e.last_seen <= rearm);
        self.shots(s, &mut out);
        out
    }

    fn still(&self, p: &AgentOut) -> bool {
        p.vel.len() < self.cfg.still_speed
    }
}
