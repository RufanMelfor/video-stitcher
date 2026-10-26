//! Court-space multi-object tracking with plausible motion.
//!
//! * constant-velocity Kalman filter per axis, speed/accel limited
//! * Hungarian association per group (home / away / referee), distance in metres
//! * fixed slots: at most 5 tracks per team, 2 referees; a returning player
//!   re-takes a lost slot instead of spawning a 6th dot
//! * lost players coast with damped velocity (never teleport, never leave the court)
//! * the *displayed* position follows the filter through a rate limiter

use crate::geom::{Field, Vec2};
use crate::jersey::JerseyVotes;
use crate::team::Team;

#[derive(Clone, Copy, Debug, Default)]
pub struct BBox {
    pub cx: f32,
    pub cy: f32,
    pub w: f32,
    pub h: f32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TrackState {
    Visible,
    Occluded,
    Lost,
}

#[derive(Clone, Debug)]
pub struct TrackerCfg {
    pub field: Field,
    pub vmax: f32,
    /// Process noise (acceleration spectral density).
    pub accel_noise: f32,
    /// Measurement std-dev in metres (ground projection error).
    pub meas_std: f32,
    pub coast_start: f32,
    pub occluded_after: f32,
    pub lost_after: f32,
    pub vel_damp_tau: f32,
    pub max_per_team: usize,
    pub max_referees: usize,
    pub confirm_hits: u32,
    pub player_margin: f32,
    pub ref_margin: f32,
    pub disp_tau: f32,
    pub reacq_tau: f32,
    pub reacq_secs: f32,
    /// A detection whose shirt colour says "other team" may still be this track if it is this
    /// close (m) to where the track is expected: one bad colour sample must not move a player
    /// to the other team's nearest slot.
    pub cross_team_gate: f32,
    /// Extra cost (m) of such a match, so that a same-team match is preferred when there is one.
    pub cross_team_penalty: f32,
    /// Weight of one colour sample in a track's running team evidence (-1 away .. +1 home).
    pub team_vote: f32,
    /// A track changes team when its evidence passes this value on the other side.
    pub team_flip: f32,
    /// Detections below this confidence only keep an existing track going (matched after the
    /// confident ones, and not to lost tracks); they never start a track or re-take a lost slot.
    /// A weak detector then still follows players through a crowd without its false positives
    /// becoming players. 0 = every detection counts fully.
    pub spawn_conf: f32,
}

impl Default for TrackerCfg {
    fn default() -> Self {
        Self {
            field: Field::basketball(),
            vmax: 9.0,
            accel_noise: 8.0,
            meas_std: 0.6,
            coast_start: 0.15,
            occluded_after: 0.25,
            lost_after: 2.0,
            vel_damp_tau: 0.8,
            max_per_team: 5,
            max_referees: 2,
            confirm_hits: 2,
            player_margin: 1.0,
            ref_margin: 2.5,
            disp_tau: 0.08,
            reacq_tau: 0.5,
            reacq_secs: 1.0,
            cross_team_gate: 0.8,
            cross_team_penalty: 1.0,
            team_vote: 0.06,
            team_flip: 0.3,
            spawn_conf: 0.0,
        }
    }
}

/// One projected observation fed to the tracker.
#[derive(Clone, Copy, Debug)]
pub struct Obs {
    pub referee: bool,
    pub team: Team,
    /// The shirt colour was the team's goalkeeper kit.
    pub keeper: bool,
    pub pos: Vec2,
    pub bbox: BBox,
    pub conf: f32,
}

#[derive(Clone, Copy, Debug)]
struct Kalman1D {
    p: f32,
    v: f32,
    pp: f32,
    pv: f32,
    vv: f32,
}

impl Kalman1D {
    fn new(p: f32, var: f32) -> Self {
        Self {
            p,
            v: 0.0,
            pp: var,
            pv: 0.0,
            vv: 4.0,
        }
    }
    fn predict(&mut self, dt: f32, q: f32) {
        self.p += self.v * dt;
        self.pp += dt * (2.0 * self.pv + dt * self.vv) + q * dt * dt * dt / 3.0;
        self.pv += dt * self.vv + q * dt * dt / 2.0;
        self.vv += q * dt;
    }
    fn update(&mut self, z: f32, r: f32) {
        let s = self.pp + r;
        let (kp, kv) = (self.pp / s, self.pv / s);
        let y = z - self.p;
        self.p += kp * y;
        self.v += kv * y;
        let (pp, pv, vv) = (self.pp, self.pv, self.vv);
        self.pp = (1.0 - kp) * pp;
        self.pv = (1.0 - kp) * pv;
        self.vv = vv - kv * pv;
    }
}

/// Something the tracker noticed that the layer above may want to interpret (e.g. a substitution).
#[derive(Clone, Copy, Debug)]
pub enum TrackerEvent {
    /// A lost slot was re-taken by a detection (possibly a different player).
    Retaken {
        id: u32,
        team: Team,
        last_pos: Vec2,
        new_pos: Vec2,
        unseen: f32,
    },
}

#[derive(Clone, Debug)]
pub struct Track {
    /// Increments whenever this reusable slot is re-acquired after being lost.
    /// `(id, segment)` identifies a continuous tracklet, not a person.
    pub segment: u32,
    pub id: u32,
    pub referee: bool,
    pub team: Team,
    kx: Kalman1D,
    ky: Kalman1D,
    pub state: TrackState,
    pub last_seen: f64,
    pub hits: u32,
    /// Displayed (rate-limited) position in court metres.
    pub pos: Vec2,
    pub bbox: Option<BBox>,
    pub conf: f32,
    reacq_until: f64,
    pub jersey: JerseyVotes,
    /// Number candidate for this continuous segment; cleared on re-acquisition.
    pub confirmed: Option<u8>,
    /// The confirmed number at the moment the slot was last re-taken.
    pub number_before: Option<u8>,
    pub retaken_at: Option<f64>,
    /// Running colour evidence for the team: +1 home .. -1 away.
    team_score: f32,
    /// Running evidence that this player wears the goalkeeper kit (> 0 = keeper).
    keeper_score: f32,
}

impl Track {
    /// True while the colour evidence says this is his team's goalkeeper.
    pub fn is_keeper(&self) -> bool {
        !self.referee && self.keeper_score > 0.0
    }
}

/// Counters about the team assignment (for run summaries).
#[derive(Clone, Copy, Debug, Default)]
pub struct TeamStats {
    /// Detections taken by a track although their colour said the other team.
    pub cross_matches: u64,
    /// Tracks that changed team because the colour evidence turned.
    pub flips: u64,
}

impl Track {
    pub fn velocity(&self) -> Vec2 {
        Vec2::new(self.kx.v, self.ky.v)
    }
    /// Raw filter position (un-rate-limited).
    pub fn filter_pos(&self) -> Vec2 {
        Vec2::new(self.kx.p, self.ky.p)
    }
    fn group(&self) -> usize {
        if self.referee {
            2
        } else if self.team == Team::Home {
            0
        } else {
            1
        }
    }
}

fn group_of_obs(o: &Obs) -> Option<usize> {
    if o.referee {
        Some(2)
    } else {
        match o.team {
            Team::Home => Some(0),
            Team::Away => Some(1),
            Team::Unknown => None,
        }
    }
}

/// Rectangular assignment (minimise total cost). Returns, per row, the column.
pub fn hungarian(cost: &[Vec<f64>]) -> Vec<Option<usize>> {
    let n = cost.len();
    if n == 0 {
        return Vec::new();
    }
    let m = cost[0].len();
    if m == 0 {
        return vec![None; n];
    }
    if n > m {
        let t: Vec<Vec<f64>> = (0..m)
            .map(|j| (0..n).map(|i| cost[i][j]).collect())
            .collect();
        let r = hungarian(&t);
        let mut out = vec![None; n];
        for (j, ri) in r.iter().enumerate() {
            if let Some(i) = ri {
                out[*i] = Some(j);
            }
        }
        return out;
    }
    let inf = f64::INFINITY;
    let (mut u, mut v) = (vec![0f64; n + 1], vec![0f64; m + 1]);
    let (mut p, mut way) = (vec![0usize; m + 1], vec![0usize; m + 1]);
    for i in 1..=n {
        p[0] = i;
        let mut j0 = 0usize;
        let mut minv = vec![inf; m + 1];
        let mut used = vec![false; m + 1];
        loop {
            used[j0] = true;
            let i0 = p[j0];
            let (mut delta, mut j1) = (inf, 0usize);
            for j in 1..=m {
                if !used[j] {
                    let cur = cost[i0 - 1][j - 1] - u[i0] - v[j];
                    if cur < minv[j] {
                        minv[j] = cur;
                        way[j] = j0;
                    }
                    if minv[j] < delta {
                        delta = minv[j];
                        j1 = j;
                    }
                }
            }
            for j in 0..=m {
                if used[j] {
                    u[p[j]] += delta;
                    v[j] -= delta;
                } else {
                    minv[j] -= delta;
                }
            }
            j0 = j1;
            if p[j0] == 0 {
                break;
            }
        }
        loop {
            let j1 = way[j0];
            p[j0] = p[j1];
            j0 = j1;
            if j0 == 0 {
                break;
            }
        }
    }
    let mut out = vec![None; n];
    for j in 1..=m {
        if p[j] != 0 {
            out[p[j] - 1] = Some(j - 1);
        }
    }
    out
}

const BIG: f64 = 1e6;

pub struct Tracker {
    pub cfg: TrackerCfg,
    tracks: Vec<Track>,
    events: Vec<TrackerEvent>,
    next_id: u32,
    t: Option<f64>,
    pub team_stats: TeamStats,
}

impl Tracker {
    pub fn new(cfg: TrackerCfg) -> Self {
        Self {
            cfg,
            tracks: Vec::new(),
            events: Vec::new(),
            next_id: 1,
            t: None,
            team_stats: TeamStats::default(),
        }
    }

    pub fn tracks(&self) -> &[Track] {
        &self.tracks
    }
    pub fn tracks_mut(&mut self) -> &mut [Track] {
        &mut self.tracks
    }

    pub fn drain_events(&mut self) -> Vec<TrackerEvent> {
        std::mem::take(&mut self.events)
    }

    fn gate(&self, unseen: f32) -> f32 {
        (2.0 + self.cfg.vmax * 0.7 * unseen.min(2.0)).min(14.0)
    }

    fn apply(&mut self, ti: usize, o: &Obs, t: f64) {
        let r = self.cfg.meas_std * self.cfg.meas_std;
        let vmax = self.cfg.vmax;
        let (occ, reacq) = (self.cfg.occluded_after, self.cfg.reacq_secs);
        let unseen = (t - self.tracks[ti].last_seen) as f32;
        if unseen > self.cfg.lost_after {
            // a slot that was lost for a while is picked up again: same player, or a substitute?
            let tr = &mut self.tracks[ti];
            let ev = TrackerEvent::Retaken {
                id: tr.id,
                team: tr.team,
                last_pos: Vec2::new(tr.kx.p, tr.ky.p),
                new_pos: o.pos,
                unseen,
            };
            tr.number_before = tr.confirmed;
            tr.retaken_at = Some(t);
            tr.segment += 1;
            // A reused slot is not evidence that the same person returned.
            // Keep the old number only for later substitution comparison.
            tr.jersey.clear();
            tr.confirmed = None;
            self.events.push(ev);
        }
        let tr = &mut self.tracks[ti];
        tr.kx.update(o.pos.x, r);
        tr.ky.update(o.pos.y, r);
        let sp = (tr.kx.v * tr.kx.v + tr.ky.v * tr.ky.v).sqrt();
        if sp > vmax {
            let k = vmax / sp;
            tr.kx.v *= k;
            tr.ky.v *= k;
        }
        tr.last_seen = t;
        tr.hits += 1;
        tr.state = TrackState::Visible;
        tr.conf = o.conf;
        tr.bbox = Some(o.bbox);
        if !tr.referee && !o.referee {
            let vote = match o.team {
                Team::Home => Some(1.0),
                Team::Away => Some(-1.0),
                Team::Unknown => None,
            };
            if let Some(v) = vote {
                let kv = if o.keeper { 1.0 } else { -1.0 };
                tr.keeper_score += (kv - tr.keeper_score) * self.cfg.team_vote;
                tr.team_score += (v - tr.team_score) * self.cfg.team_vote;
                if o.team != tr.team {
                    self.team_stats.cross_matches += 1;
                }
            }
        }
        if unseen > occ {
            tr.reacq_until = t + reacq as f64;
        }
    }

    fn spawn(&mut self, o: &Obs, t: f64) {
        let var = self.cfg.meas_std * self.cfg.meas_std;
        let tr = Track {
            segment: 0,
            id: self.next_id,
            referee: o.referee,
            team: if o.referee { Team::Unknown } else { o.team },
            kx: Kalman1D::new(o.pos.x, var),
            ky: Kalman1D::new(o.pos.y, var),
            state: TrackState::Visible,
            last_seen: t,
            hits: 1,
            pos: o.pos,
            bbox: Some(o.bbox),
            conf: o.conf,
            reacq_until: 0.0,
            jersey: JerseyVotes::default(),
            confirmed: None,
            number_before: None,
            retaken_at: None,
            keeper_score: if o.keeper { 0.6 } else { -0.6 },
            team_score: match (o.referee, o.team) {
                (false, Team::Home) => self.cfg.team_vote,
                (false, Team::Away) => -self.cfg.team_vote,
                _ => 0.0,
            },
        };
        self.next_id += 1;
        self.tracks.push(tr);
    }

    pub fn update(&mut self, t: f64, obs: &[Obs]) {
        let dt = match self.t {
            Some(p) => ((t - p) as f32).clamp(0.001, 0.5),
            None => 0.04,
        };
        self.t = Some(t);
        let cfg = self.cfg.clone();

        // 1. predict
        for tr in &mut self.tracks {
            let unseen = (t - tr.last_seen) as f32;
            tr.kx.predict(dt, cfg.accel_noise);
            tr.ky.predict(dt, cfg.accel_noise);
            if unseen > cfg.coast_start {
                let k = (-dt / cfg.vel_damp_tau).exp();
                tr.kx.v *= k;
                tr.ky.v *= k;
            }
            let sp = (tr.kx.v * tr.kx.v + tr.ky.v * tr.ky.v).sqrt();
            if sp > cfg.vmax {
                let k = cfg.vmax / sp;
                tr.kx.v *= k;
                tr.ky.v *= k;
            }
            let m = if tr.referee {
                cfg.ref_margin
            } else {
                cfg.player_margin
            };
            let c = cfg.field.clamp(Vec2::new(tr.kx.p, tr.ky.p), m);
            tr.kx.p = c.x;
            tr.ky.p = c.y;
        }

        // 2. associate per group
        let mut obs_used = vec![false; obs.len()];
        let mut trk_hit = vec![false; self.tracks.len()];
        // players of both teams in one assignment: the colour decides between candidates, but a
        // detection right where a track is expected belongs to it even if the colour disagrees
        // confident detections first; the weak ones then go to tracks that are still open and not lost
        for referee in [false, true] {
            let ti: Vec<usize> = (0..self.tracks.len())
                .filter(|&i| self.tracks[i].referee == referee)
                .collect();
            let oi: Vec<usize> = (0..obs.len())
                .filter(|&j| obs[j].referee == referee && obs[j].conf >= cfg.spawn_conf)
                .collect();
            self.match_sets(&ti, &oi, obs, &mut obs_used, &mut trk_hit, t);
            let ti: Vec<usize> = ti
                .into_iter()
                .filter(|&i| !trk_hit[i] && (t - self.tracks[i].last_seen) as f32 <= cfg.lost_after)
                .collect();
            let oi: Vec<usize> = (0..obs.len())
                .filter(|&j| obs[j].referee == referee && obs[j].conf < cfg.spawn_conf)
                .collect();
            self.match_sets(&ti, &oi, obs, &mut obs_used, &mut trk_hit, t);
        }
        self.settle_teams(&mut trk_hit);

        // 3. leftovers: re-take lost slots or spawn
        for j in 0..obs.len() {
            if obs_used[j] || obs[j].conf < cfg.spawn_conf {
                continue;
            }
            let g = match group_of_obs(&obs[j]) {
                Some(g) => g,
                None => continue,
            };
            let in_group: Vec<usize> = (0..self.tracks.len())
                .filter(|&i| self.tracks[i].group() == g)
                .collect();
            let cap = if g == 2 {
                cfg.max_referees
            } else {
                cfg.max_per_team
            };
            if in_group.len() < cap {
                self.spawn(&obs[j], t);
                trk_hit.push(true);
                obs_used[j] = true;
            } else {
                // full: only a Lost slot may be re-taken (nearest first)
                let best = in_group
                    .iter()
                    .copied()
                    .filter(|&i| !trk_hit[i] && self.tracks[i].state == TrackState::Lost)
                    .min_by(|&a, &b| {
                        let da = self.tracks[a].filter_pos().dist(obs[j].pos);
                        let db = self.tracks[b].filter_pos().dist(obs[j].pos);
                        da.partial_cmp(&db).unwrap_or(std::cmp::Ordering::Equal)
                    });
                if let Some(i) = best {
                    self.apply(i, &obs[j], t);
                    trk_hit[i] = true;
                    obs_used[j] = true;
                }
            }
        }

        // 4. state of unmatched tracks, drop tentative ghosts
        for (i, tr) in self.tracks.iter_mut().enumerate() {
            if trk_hit.get(i).copied().unwrap_or(false) {
                continue;
            }
            let unseen = (t - tr.last_seen) as f32;
            tr.state = if unseen > cfg.lost_after {
                TrackState::Lost
            } else if unseen > cfg.occluded_after {
                TrackState::Occluded
            } else {
                TrackState::Visible
            };
        }
        self.tracks
            .retain(|tr| tr.hits >= cfg.confirm_hits || (t - tr.last_seen) < 0.5);

        // 5. displayed position: smooth + rate limit (no teleporting)
        for tr in &mut self.tracks {
            let target = Vec2::new(tr.kx.p, tr.ky.p);
            let tau = if t < tr.reacq_until {
                cfg.reacq_tau
            } else {
                cfg.disp_tau
            };
            let a = 1.0 - (-dt / tau).exp();
            let mut step = (target - tr.pos) * a;
            let max_step = cfg.vmax * 1.3 * dt;
            let l = step.len();
            if l > max_step {
                step = step * (max_step / l);
            }
            tr.pos = tr.pos + step;
        }
    }

    /// Let a track follow its colour evidence to the other team. The slot count per team holds:
    /// if the other team is full, a lost slot there makes room (it was most likely this player's),
    /// otherwise the track stays.
    fn settle_teams(&mut self, hit: &mut Vec<bool>) {
        let (flip, cap) = (self.cfg.team_flip, self.cfg.max_per_team);
        let mut i = 0;
        while i < self.tracks.len() {
            let tr = &self.tracks[i];
            let to = match tr.team {
                Team::Home if tr.team_score < -flip => Team::Away,
                Team::Away if tr.team_score > flip => Team::Home,
                _ => {
                    i += 1;
                    continue;
                }
            };
            if tr.referee {
                i += 1;
                continue;
            }
            let there = |x: &Track| !x.referee && x.team == to;
            if self.tracks.iter().filter(|x| there(x)).count() >= cap {
                match self
                    .tracks
                    .iter()
                    .position(|x| there(x) && x.state == TrackState::Lost)
                {
                    Some(lost) => {
                        self.tracks.remove(lost);
                        hit.remove(lost);
                        if lost < i {
                            i -= 1;
                        }
                    }
                    None => {
                        let s = &mut self.tracks[i].team_score;
                        *s = s.clamp(-flip, flip);
                        i += 1;
                        continue;
                    }
                }
            }
            self.tracks[i].team = to;
            self.team_stats.flips += 1;
            i += 1;
        }
    }

    fn match_sets(
        &mut self,
        ti: &[usize],
        oi: &[usize],
        obs: &[Obs],
        used: &mut [bool],
        hit: &mut [bool],
        t: f64,
    ) {
        if ti.is_empty() || oi.is_empty() {
            return;
        }
        let mut cost = vec![vec![BIG; oi.len()]; ti.len()];
        for (r, &i) in ti.iter().enumerate() {
            let unseen = (t - self.tracks[i].last_seen) as f32;
            let gate = self.gate(unseen);
            let fp = self.tracks[i].filter_pos();
            let team = self.tracks[i].team;
            for (c, &j) in oi.iter().enumerate() {
                let d = fp.dist(obs[j].pos);
                let (limit, penalty) = if obs[j].referee || obs[j].team == team {
                    (gate, 0.0)
                } else if obs[j].team == Team::Unknown {
                    (gate, 0.5 * self.cfg.cross_team_penalty)
                } else {
                    (
                        gate.min(self.cfg.cross_team_gate),
                        self.cfg.cross_team_penalty,
                    )
                };
                if d <= limit {
                    cost[r][c] = (d + penalty) as f64;
                }
            }
        }
        let asg = hungarian(&cost);
        for (r, a) in asg.iter().enumerate() {
            if let Some(c) = a
                && cost[r][*c] < BIG
            {
                let (i, j) = (ti[r], oi[*c]);
                self.apply(i, &obs[j], t);
                hit[i] = true;
                used[j] = true;
            }
        }
    }
}
