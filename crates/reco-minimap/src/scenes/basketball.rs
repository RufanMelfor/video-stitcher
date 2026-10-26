//! Basketball-only geometry and shot interpretation.
//! Shared timing, event storage and formation latching stay in the parent module.
use super::*;

impl SceneDetector {
    /// One shooter at the free-throw line, several players lined up in the lane, dead ball.
    pub(super) fn free_throws(&self, s: &FrameState, players: &[&AgentOut], out: &mut Vec<Cand>) {
        let (l, w) = (self.profile.field.length, self.profile.field.width);
        for (end, spot, lane_x) in [
            ('l', Vec2::new(5.8, w / 2.0), (-0.5f32, 6.1f32)),
            ('r', Vec2::new(l - 5.8, w / 2.0), (l - 6.1, l + 0.5)),
        ] {
            let shooters: Vec<&&AgentOut> =
                players.iter().filter(|p| p.pos.dist(spot) <= 1.1).collect();
            if shooters.len() != 1 || !self.still(shooters[0]) {
                continue;
            }
            let sh = shooters[0];
            // Rebounders wait ALONG both lane edges, not in an arbitrary crowd
            // under the basket. Tolerances accommodate imperfect calibration.
            let rebounders: Vec<_> = players
                .iter()
                .filter(|p| {
                    p.id != sh.id
                        && self.still(p)
                        && p.pos.x >= lane_x.0 + 1.0
                        && p.pos.x <= lane_x.1 - 1.0
                        && (1.7..=3.4).contains(&(p.pos.y - w / 2.0).abs())
                })
                .collect();
            let lane = rebounders.len();
            let both_sides = rebounders.iter().any(|p| p.pos.y < w / 2.0)
                && rebounders.iter().any(|p| p.pos.y > w / 2.0);
            let crowded_middle = players.iter().any(|p| {
                p.id != sh.id
                    && p.pos.x >= lane_x.0 + 1.0
                    && p.pos.x <= lane_x.1 - 1.0
                    && (p.pos.y - w / 2.0).abs() < 1.2
            });
            if lane < 3 || !both_sides || crowded_middle {
                continue;
            }
            // Missing ball is uncertainty; a reliable contradictory ball state
            // is evidence against a free-throw preparation.
            if matches!(s.ball.state, BallState::InFlight)
                || matches!(s.ball.state,BallState::Held{track} if track!=sh.id)
            {
                continue;
            }
            let moving = players
                .iter()
                .filter(|p| p.vel.len() > self.cfg.still_speed * 1.6)
                .count();
            if moving * 5 > players.len() {
                continue;
            }
            let ball_support = matches!(s.ball.state,BallState::Held{track} if track==sh.id);
            let conf =
                (0.5 + 0.04 * lane.min(6) as f32 + if ball_support { 0.15 } else { 0.0 }).min(0.9);
            out.push(Cand {
                kind: SceneKind::FreeThrow,
                key: end as u8,
                team: Some(sh.team),
                pos: sh.pos,
                side: Some(end),
                conf,
            });
        }
    }

    /// Basketball 2 / 3-point attempts: the ball leaves the holder's hands and travels towards a basket.
    pub(super) fn shots(&mut self, s: &FrameState, out: &mut Vec<SceneEvent>) {
        if self.rules != SceneRules::Basketball || self.profile.goals.len() < 2 {
            return;
        }
        let t = s.t;
        if let BallState::Held { track } = s.ball.state
            && let Some(p) = s.players.iter().find(|p| p.id == track)
        {
            self.holder = Some((p.pos, p.team, t));
        }
        let released = matches!(self.prev_state, Some(BallState::Held { .. }))
            && matches!(s.ball.state, BallState::InFlight);
        if released
            && let Some((pos, team, th)) = self.holder
            && t - th <= 1.5
        {
            let (goal, d) = self.profile.goals.iter().map(|g| (*g, pos.dist(*g))).fold(
                (self.profile.goals[0], f32::MAX),
                |a, b| {
                    if b.1 < a.1 { b } else { a }
                },
            );
            if d <= 13.0 {
                self.pending = Some(PendingShot {
                    t0: t,
                    release: pos,
                    team,
                    goal,
                    start: d,
                    best: d,
                });
            }
        }
        self.prev_state = Some(s.ball.state);
        let mut confirmed: Option<PendingShot> = None;
        if let Some(ps) = self.pending.as_mut() {
            if t - ps.t0 > 1.8 {
                self.pending = None;
            } else if let Some(bp) = s.ball.pos {
                ps.best = ps.best.min(bp.dist(ps.goal));
                if ps.best <= ps.start - 2.0 || ps.best <= 2.5 {
                    confirmed = self.pending.take();
                }
            }
        }
        if let Some(ps) = confirmed {
            let (l, w) = (self.profile.field.length, self.profile.field.width);
            // Location alone cannot distinguish an ordinary jump shot from a
            // free throw. Require a recently observed preparation at this end.
            let ft = [Vec2::new(5.8, w / 2.0), Vec2::new(l - 5.8, w / 2.0)];
            if ft.iter().zip(['l', 'r']).any(|(f, end)| {
                f.dist(ps.release) <= 1.3
                    && self
                        .last_fire
                        .get(&(SceneKind::FreeThrow, end as u8))
                        .map(|at| (0.0..=10.0).contains(&(ps.t0 - at)))
                        .unwrap_or(false)
            }) {
                out.push(SceneEvent {
                    t: ps.t0,
                    started_at: ps.t0,
                    kind: SceneKind::FreeThrow,
                    team: Some(ps.team),
                    pos: ps.release,
                    side: Some(if ps.goal.x < l / 2.0 { 'l' } else { 'r' }),
                    confidence: 0.75,
                    borderline: false,
                    evidence: vec!["recent_free_throw_formation", "ball_release_towards_basket"],
                    related_substitutions: Vec::new(),
                });
                return;
            }
            let dist = ps.release.dist(ps.goal);
            let from_base = if ps.goal.x < l / 2.0 {
                ps.release.x
            } else {
                l - ps.release.x
            };
            let strip = (ps.release.y <= 0.9 || ps.release.y >= w - 0.9) && from_base <= 3.0;
            let three = dist >= 6.8 || strip;
            let borderline = (dist - 6.75).abs() < 0.8
                || ((ps.release.y.min(w - ps.release.y) - 0.9).abs() < 0.6 && from_base <= 3.6);
            out.push(SceneEvent {
                t: ps.t0,
                kind: if three {
                    SceneKind::ShotThree
                } else {
                    SceneKind::ShotTwo
                },
                team: Some(ps.team),
                pos: ps.release,
                side: Some(if ps.goal.x < l / 2.0 { 'l' } else { 'r' }),
                confidence: if borderline { 0.45 } else { 0.6 },
                borderline,
                started_at: ps.t0,
                evidence: vec!["ball_release_towards_basket"],
                related_substitutions: Vec::new(),
            });
        }
    }
}
