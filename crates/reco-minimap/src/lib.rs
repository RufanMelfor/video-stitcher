//! `reco-minimap`: top-down pitch minimap from reco's AI detections.
//!
//! ```text
//! detections (camera, normalized) -> Projector -> Tracker (+ BallTracker)
//!                                                  -> FrameState -> Renderer -> RGBA overlay
//! ```
//!
//! The tracking/rendering core (every module except [`reco`]) is vendored
//! from <https://github.com/wendibus/reco-minimap> (AGPL-3.0, commit
//! `5e96fa28`), without its `tools` layer (browser UI, ONNX inference,
//! ffmpeg I/O, OCR). Inference stays in `reco-detect`; [`reco`] wires the
//! core into a stitch session as a pipeline-event sink plus an overlay
//! layer. See this crate's `README.md` for the setup file and limits.

pub mod ball;
pub mod board;
pub mod detect;
pub mod geom;
pub mod identity;
pub mod jersey;
pub mod reco;
pub mod render;
pub mod scenes;
pub mod scheduler;
pub mod sport;
pub mod team;
pub mod tracker;

use ball::{BallCfg, BallObs, BallOut, BallTracker};
use detect::{Class, Det};
use geom::{Field, Projector};
use sport::SportProfile;
use team::{Team, TeamClassifier};
use tracker::{BBox, Obs, Track, TrackState, Tracker, TrackerCfg, TrackerEvent};

#[derive(Clone, Copy, Debug)]
pub struct AgentOut {
    /// Tracking-slot generation; pair with `id`, never use the slot as a person ID.
    pub segment: u32,
    pub identity: identity::IdentityAssignment,
    pub id: u32,
    pub referee: bool,
    pub team: Team,
    /// Wears the goalkeeper kit (only known when goalkeeper colours are set).
    pub keeper: bool,
    pub pos: geom::Vec2,
    /// Filter velocity in m/s (for forward prediction in live displays).
    pub vel: geom::Vec2,
    pub state: TrackState,
    /// 1.0 = seen now; fades towards 0.35 while the track coasts.
    pub alpha: f32,
    pub jersey: Option<u8>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SubKind {
    /// A player left and someone entered at the sideline, numbers not (yet) known.
    Possible,
    /// Fresh number evidence differs from the number before a re-entry.
    /// This is still a heuristic, not a manually verified substitution.
    Confirmed,
}

/// A substitution ("Wechsel") noticed this frame.
#[derive(Clone, Debug)]
pub struct SubEvent {
    pub t: f64,
    pub team: Team,
    pub slot: u32,
    pub out_number: Option<u8>,
    pub in_number: Option<u8>,
    pub kind: SubKind,
}

#[derive(Clone, Debug)]
pub struct FrameState {
    pub t: f64,
    pub players: Vec<AgentOut>,
    pub referees: Vec<AgentOut>,
    pub ball: BallOut,
    /// Substitutions detected in this frame (empty most of the time).
    pub events: Vec<SubEvent>,
}

fn lerp_agents(a: &[AgentOut], b: &[AgentOut], k: f32) -> Vec<AgentOut> {
    b.iter()
        .map(|nb| match a.iter().find(|x| x.id == nb.id) {
            Some(na) if na.segment == nb.segment => AgentOut {
                pos: na.pos.lerp(nb.pos, k),
                alpha: na.alpha + (nb.alpha - na.alpha) * k,
                ..if k < 0.5 { *na } else { *nb }
            },
            // Never animate a path between two occupants of a reused slot.
            Some(na) => {
                if k < 0.5 {
                    *na
                } else {
                    *nb
                }
            }
            None => *nb,
        })
        .collect()
}

impl FrameState {
    /// State between `self` (k = 0) and `next` (k = 1): lets a video be rendered at the full frame
    /// rate although the detector only runs every Nth frame. Agents are matched by track id.
    /// Discrete things (state, jersey) switch at the halfway point.
    pub fn lerp(&self, next: &FrameState, k: f32) -> FrameState {
        let k = k.clamp(0.0, 1.0);
        let ball_pos = match (self.ball.pos, next.ball.pos) {
            (Some(a), Some(b)) => Some(a.lerp(b, k)),
            (a, b) => {
                if k < 0.5 {
                    a
                } else {
                    b
                }
            }
        };
        let ball_state = if k < 0.5 {
            self.ball.state
        } else {
            next.ball.state
        };
        FrameState {
            t: self.t + (next.t - self.t) * k as f64,
            players: lerp_agents(&self.players, &next.players, k),
            referees: lerp_agents(&self.referees, &next.referees, k),
            ball: BallOut {
                state: ball_state,
                pos: ball_pos,
                vel: if k < 0.5 {
                    self.ball.vel
                } else {
                    next.ball.vel
                },
            },
            events: if k >= 1.0 {
                next.events.clone()
            } else {
                Vec::new()
            },
        }
    }

    /// Forward prediction by `dt` seconds (live display between detections).
    /// Velocity decays (people stop and turn), positions stay inside the court.
    pub fn extrapolate(&self, dt: f32) -> FrameState {
        self.extrapolate_in(dt, &Field::basketball())
    }

    /// Like [`FrameState::extrapolate`] for any field.
    pub fn extrapolate_in(&self, dt: f32, field: &Field) -> FrameState {
        let dt = dt.clamp(0.0, 1.5);
        let adv = |p: geom::Vec2, v: geom::Vec2, tau: f32| {
            let k = tau * (1.0 - (-dt / tau).exp());
            field.clamp(p + v * k, 1.0)
        };
        let agents = |v: &[AgentOut]| -> Vec<AgentOut> {
            v.iter()
                .map(|a| AgentOut {
                    pos: adv(a.pos, a.vel, 0.8),
                    ..*a
                })
                .collect()
        };
        FrameState {
            t: self.t + dt as f64,
            players: agents(&self.players),
            referees: agents(&self.referees),
            ball: BallOut {
                pos: self
                    .ball
                    .pos
                    .map(|p| adv(p, self.ball.vel.unwrap_or_default(), 0.4)),
                ..self.ball
            },
            events: self.events.clone(),
        }
    }
}

#[derive(Clone, Debug)]
pub struct Config {
    /// Playing field (metres). Use [`Config::for_sport`] to set field, team sizes and speeds together.
    pub field: Field,
    pub home_rgb: [u8; 3],
    pub away_rgb: [u8; 3],
    /// Goalkeeper shirt colours (optional; for sports where the keeper wears a different colour).
    pub home_gk_rgb: Option<[u8; 3]>,
    pub away_gk_rgb: Option<[u8; 3]>,
    /// Goalkeeper handling is on only for sports with a keeper kit (set by [`Config::for_sport`]; not basketball, volleyball ...).
    /// While false, keeper colours and roster marks are ignored.
    pub keepers_enabled: bool,
    pub tracker: TrackerCfg,
    pub ball: BallCfg,
    /// Roster numbers accepted from OCR (both teams merged); empty = any. Per-team rosters below take precedence.
    pub roster: Vec<u8>,
    pub roster_home: Vec<u8>,
    pub roster_away: Vec<u8>,
    /// Goalkeeper numbers per team (a subset of the rosters). With a goalkeeper kit colour set, the keeper can only
    /// be one of these and the outfield players cannot.
    pub keepers_home: Vec<u8>,
    pub keepers_away: Vec<u8>,
    /// Evidence needed to confirm a number, and the lead over the runner-up.
    pub num_min_score: f32,
    pub num_min_lead: f32,
    /// Evidence decay per processed frame (a stale reading fades; the confirmed number stays until replaced).
    pub vote_decay: f32,
    /// A slot that was lost and is re-taken counts as a possible substitution only if the player
    /// was last tracked within `sub_leave_m` of a sideline (the long sides, where the benches are),
    /// was gone for at least `sub_min_gone` seconds, and someone appears within `sub_near_edge_m`
    /// of a sideline. Players hidden in a crowd or under the basket do not qualify.
    pub sub_near_edge_m: f32,
    pub sub_leave_m: f32,
    pub sub_min_gone: f32,
    /// At most one possible substitution per slot in this many seconds.
    pub sub_cooldown: f32,
    /// The model rarely finds referees: treat on-court players whose torso colour
    /// is clearly neither team's (black, yellow ...) as referees.
    pub referee_from_color: bool,
    /// Referee shirt colour if known (enables nearest-colour matching: also works for black vs. a dark-navy team).
    pub referee_rgb: Option<[u8; 3]>,
    /// Ball candidates farther than this (m) outside the court are ignored.
    pub ball_margin: f32,
    /// Hysteresis: a new ball track needs at least this confidence ...
    pub ball_init_conf: f32,
    /// ... and a detection that is not near the tracked ball needs this one to take over.
    pub ball_reacquire_conf: f32,
    /// Max plausible ball speed (m/s) used to gate detections near the tracked ball.
    pub ball_max_speed: f32,
}

impl Config {
    /// Basketball defaults (the original configuration).
    pub fn new(home_rgb: [u8; 3], away_rgb: [u8; 3]) -> Self {
        Self::for_sport(&SportProfile::basketball(), home_rgb, away_rgb)
    }

    /// Field, slot counts and speed limits from a sport profile.
    pub fn for_sport(p: &SportProfile, home_rgb: [u8; 3], away_rgb: [u8; 3]) -> Self {
        Self::for_sport_with_rules(
            p,
            sport::rules::AnalysisRules::for_sport(&p.id),
            home_rgb,
            away_rgb,
        )
    }

    /// Override interpretation independently of geometry. Pass the same rules'
    /// `scenes` field to `SceneDetector::with_rules` when using a custom profile.
    pub fn for_sport_with_rules(
        p: &SportProfile,
        rules: sport::rules::AnalysisRules,
        home_rgb: [u8; 3],
        away_rgb: [u8; 3],
    ) -> Self {
        let tracker = TrackerCfg {
            field: p.field,
            vmax: p.player_vmax,
            max_per_team: p.team_size,
            max_referees: p.referees,
            ..TrackerCfg::default()
        };
        let ball = BallCfg {
            field: p.field,
            possession: rules.possession,
            ..BallCfg::default()
        };
        Self {
            field: p.field,
            home_rgb,
            away_rgb,
            home_gk_rgb: None,
            away_gk_rgb: None,
            keepers_enabled: sport::has_keepers(&p.id),
            tracker,
            ball,
            roster: Vec::new(),
            roster_home: Vec::new(),
            roster_away: Vec::new(),
            keepers_home: Vec::new(),
            keepers_away: Vec::new(),
            num_min_score: 3.0,
            num_min_lead: 1.5,
            vote_decay: 0.985,
            sub_near_edge_m: 2.0,
            sub_leave_m: 1.5,
            sub_min_gone: 3.0,
            sub_cooldown: 20.0,
            referee_from_color: true,
            referee_rgb: None,
            ball_margin: 3.0,
            ball_init_conf: 0.40,
            ball_reacquire_conf: 0.50,
            ball_max_speed: p.ball_max_speed,
        }
    }
}

pub struct Minimap {
    cfg: Config,
    projector: Box<dyn Projector>,
    classifier: TeamClassifier,
    tracker: Tracker,
    ball: BallTracker,
    last: Option<BallOut>,
    events_log: Vec<SubEvent>,
    /// Times of recent slot re-takes (several at once = the detector lost the scene, not a substitution).
    retakes: Vec<f64>,
    /// When each slot last raised a possible substitution.
    sub_raised: Vec<(u32, f64)>,
}

impl Minimap {
    pub fn new(cfg: Config, projector: Box<dyn Projector>) -> Self {
        let mut classifier = TeamClassifier::new(cfg.home_rgb, cfg.away_rgb);
        if let Some(r) = cfg.referee_rgb {
            classifier = classifier.with_referee(r);
        }
        if cfg.keepers_enabled && (cfg.home_gk_rgb.is_some() || cfg.away_gk_rgb.is_some()) {
            classifier = classifier.with_keepers(cfg.home_gk_rgb, cfg.away_gk_rgb);
        }
        let tracker = Tracker::new(cfg.tracker.clone());
        let ball = BallTracker::new(cfg.ball.clone());
        Self {
            cfg,
            projector,
            classifier,
            tracker,
            ball,
            last: None,
            events_log: Vec::new(),
            retakes: Vec::new(),
            sub_raised: Vec::new(),
        }
    }

    pub fn tracker(&self) -> &Tracker {
        &self.tracker
    }
    pub fn tracker_mut(&mut self) -> &mut Tracker {
        &mut self.tracker
    }

    /// Feed one frame of panorama detections (already NMS-merged across tiles).
    pub fn process(&mut self, t: f64, dets: &[Det]) -> FrameState {
        let mut obs: Vec<Obs> = Vec::new();
        let mut balls: Vec<(&Det, geom::Vec2)> = Vec::new();
        for d in dets {
            let (u, v) = d.foot();
            let court = match self.projector.to_court(u, v) {
                Some(p) => p,
                None => continue,
            };
            match d.class {
                Class::Ball => {
                    if self.cfg.field.contains(court, self.cfg.ball_margin) {
                        balls.push((d, court));
                    }
                }
                Class::Player | Class::Referee => {
                    let mut referee = d.class == Class::Referee;
                    let (mut team, mut keeper) = if referee {
                        (Team::Unknown, false)
                    } else {
                        self.classifier.classify_role(d.color)
                    };
                    if !referee
                        && self.cfg.referee_from_color
                        && let Some(c) = d.color
                        && (self.classifier.is_referee_colored(c)
                            || (team == Team::Unknown && self.classifier.is_outlier(c)))
                    {
                        referee = true;
                        team = Team::Unknown;
                        keeper = false;
                    }
                    let margin = if referee {
                        self.cfg.tracker.ref_margin
                    } else {
                        self.cfg.tracker.player_margin
                    };
                    if !self.cfg.field.contains(court, margin) {
                        continue; // bench, coaches, spectators
                    }
                    obs.push(Obs {
                        referee,
                        team: if referee { Team::Unknown } else { team },
                        keeper: keeper && !referee,
                        pos: court,
                        bbox: BBox {
                            cx: d.cx,
                            cy: d.cy,
                            w: d.w,
                            h: d.h,
                        },
                        conf: d.conf,
                    });
                }
            }
        }
        // spare balls and round things that never move are not the game ball
        balls.retain(|(_, court)| !self.ball.is_parked(t, *court));
        // ball with hysteresis: follow the tracked ball with low confidence, but need a
        // clearly confident detection to start or to jump somewhere else
        let reference = self.ball.reference(t);
        let age = self.ball.age(t);
        balls.retain(|(d, court)| match reference {
            Some(r) => {
                court.dist(r) <= self.cfg.ball_max_speed * age.min(1.0) + 1.5
                    || d.conf >= self.cfg.ball_reacquire_conf
            }
            None => d.conf >= self.cfg.ball_init_conf,
        });
        // among the rest: nearest to the tracked ball if confidence is comparable, else the strongest
        balls.sort_by(|a, b| {
            b.0.conf
                .partial_cmp(&a.0.conf)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        let mut best_ball = balls.first().copied();
        if let (Some(best), Some(r)) = (best_ball, reference)
            && best.1.dist(r) > 8.0
            && let Some(near) = balls
                .iter()
                .find(|c| c.1.dist(r) < 4.0 && c.0.conf >= best.0.conf * 0.5)
        {
            best_ball = Some(*near);
        }
        self.tracker.update(t, &obs);

        let ball_obs = best_ball.map(|(d, court)| BallObs {
            bbox: BBox {
                cx: d.cx,
                cy: d.cy,
                w: d.w,
                h: d.h,
            },
            court,
            conf: d.conf,
        });
        let ball = self.ball.update(t, ball_obs, self.tracker.tracks());
        self.last = Some(ball);

        let events = self.update_numbers(t);
        // build the output after the numbers are updated so `jersey` is current
        let (mut players, mut referees) = (Vec::new(), Vec::new());
        for tr in self.tracker.tracks() {
            if tr.hits < self.cfg.tracker.confirm_hits {
                continue;
            }
            let out = self.agent_out(tr, t);
            if tr.referee {
                referees.push(out)
            } else {
                players.push(out)
            }
        }
        identity::assign(&mut players);
        FrameState {
            t,
            players,
            referees,
            ball,
            events,
        }
    }

    /// Numbers a track may carry: the team roster, narrowed to the keepers (for a keeper kit) or without them
    /// (for outfield shirts) when keeper numbers are declared and the keeper kit tells the two apart.
    pub fn roster_for_track(&self, team: Team, keeper: bool) -> Vec<u8> {
        let base = self.roster_for(team).to_vec();
        let (kp, kit) = match team {
            Team::Home => (&self.cfg.keepers_home, self.cfg.home_gk_rgb.is_some()),
            Team::Away => (&self.cfg.keepers_away, self.cfg.away_gk_rgb.is_some()),
            Team::Unknown => return base,
        };
        if !self.cfg.keepers_enabled || kp.is_empty() || !kit {
            return base;
        }
        if keeper {
            kp.clone()
        } else {
            base.into_iter().filter(|n| !kp.contains(n)).collect()
        }
    }

    fn roster_for(&self, team: Team) -> &[u8] {
        let r = match team {
            Team::Home => &self.cfg.roster_home,
            Team::Away => &self.cfg.roster_away,
            Team::Unknown => &self.cfg.roster,
        };
        if r.is_empty() { &self.cfg.roster } else { r }
    }

    /// Re-takes of a slot near the field edge are possible substitutions; number evidence confirms or refutes them.
    fn update_numbers(&mut self, t: f64) -> Vec<SubEvent> {
        let mut ev = Vec::new();
        let field = self.cfg.field;
        let events = self.tracker.drain_events();
        self.retakes.retain(|x| t - x <= 1.0);
        self.retakes.extend(events.iter().map(|_| t));
        let dropout = self.retakes.len() >= 3;
        let side = |p: geom::Vec2| p.y.min(field.width - p.y); // distance to the nearer sideline, negative outside
        for e in events {
            let TrackerEvent::Retaken {
                id,
                team,
                last_pos,
                new_pos,
                unseen,
            } = e;
            let cooled = !self
                .sub_raised
                .iter()
                .any(|r| r.0 == id && t - r.1 < self.cfg.sub_cooldown as f64);
            if !dropout
                && cooled
                && team != Team::Unknown
                && unseen >= self.cfg.sub_min_gone
                && side(last_pos) <= self.cfg.sub_leave_m
                && side(new_pos) <= self.cfg.sub_near_edge_m
            {
                let out = self
                    .tracker
                    .tracks()
                    .iter()
                    .find(|x| x.id == id)
                    .and_then(|x| x.number_before);
                ev.push(SubEvent {
                    t,
                    team,
                    slot: id,
                    out_number: out,
                    in_number: None,
                    kind: SubKind::Possible,
                });
                self.sub_raised.retain(|r| r.0 != id);
                self.sub_raised.push((id, t));
            }
        }
        let (decay, min_s, min_l) = (
            self.cfg.vote_decay,
            self.cfg.num_min_score,
            self.cfg.num_min_lead,
        );
        let mut rosters: std::collections::HashMap<(Team, bool), Vec<u8>> = Default::default();
        for tm in [Team::Home, Team::Away] {
            for kp in [false, true] {
                rosters.insert((tm, kp), self.roster_for_track(tm, kp));
            }
        }
        for tr in self.tracker.tracks_mut().iter_mut().filter(|x| !x.referee) {
            tr.jersey.decay(decay);
            let roster = rosters
                .get(&(tr.team, tr.is_keeper()))
                .map(|r| r.as_slice())
                .unwrap_or(&[]);
            let best = match tr.jersey.best(roster, min_s, min_l) {
                Some(b) => b,
                None => continue,
            };
            if tr.confirmed == Some(best) {
                continue;
            }
            // A better OCR reading during an uninterrupted track (`confirmed` already
            // set) can simply correct a mistaken number. It is not evidence of substitution.
            if tr.confirmed.is_none()
                && let (Some(before), Some(at)) = (tr.number_before, tr.retaken_at)
                && before != best
                && t - at < 600.0
            {
                ev.push(SubEvent {
                    t,
                    team: tr.team,
                    slot: tr.id,
                    out_number: Some(before),
                    in_number: Some(best),
                    kind: SubKind::Confirmed,
                });
            }
            tr.confirmed = Some(best);
            tr.number_before = None;
        }
        self.events_log.extend(ev.iter().cloned());
        ev
    }

    /// All substitution events so far.
    pub fn events_log(&self) -> &[SubEvent] {
        &self.events_log
    }

    fn agent_out(&self, tr: &Track, t: f64) -> AgentOut {
        let unseen = (t - tr.last_seen) as f32;
        let alpha = match tr.state {
            TrackState::Visible if unseen < self.cfg.tracker.coast_start => 1.0,
            TrackState::Lost => 0.35,
            _ => (1.0 - 0.65 * unseen / (self.cfg.tracker.lost_after + 1.0)).clamp(0.35, 1.0),
        };
        AgentOut {
            segment: tr.segment,
            identity: identity::IdentityAssignment::default(),
            id: tr.id,
            referee: tr.referee,
            team: tr.team,
            keeper: tr.is_keeper(),
            pos: tr.pos,
            vel: tr.velocity(),
            state: tr.state,
            alpha,
            jersey: tr.confirmed,
        }
    }

    /// Feed the raw OCR output (`t` steps x `DIGIT_CLASSES` probabilities) for a track. With a roster the
    /// reading is decoded constrained to roster numbers; without one it is read freely.
    /// Returns the accepted (number, confidence), if any.
    pub fn add_number_probs(&mut self, track: u32, probs: &[f32], t: usize) -> Option<(u8, f32)> {
        use jersey::{ctc_greedy, ctc_roster, parse_number};
        let (team, keeper) = self
            .tracker
            .tracks()
            .iter()
            .find(|x| x.id == track)
            .map(|x| (x.team, x.is_keeper()))?;
        let roster = self.roster_for_track(team, keeper);
        let (n, conf) = if roster.is_empty() {
            let (s, c) = ctc_greedy(probs, t);
            (parse_number(&s)?, c)
        } else {
            let (n, post) = ctc_roster(probs, t, &roster)?;
            (n, post)
        };
        let min = if roster.is_empty() { 0.6 } else { 0.5 };
        if conf < min {
            return None;
        }
        self.add_jersey_reading(track, n, conf);
        Some((n, conf))
    }

    /// Feed an OCR reading for a track.
    pub fn add_jersey_reading(&mut self, track: u32, number: u8, conf: f32) {
        if let Some(tr) = self.tracker.tracks_mut().iter_mut().find(|t| t.id == track) {
            tr.jersey.add(number, conf);
        }
    }
}
