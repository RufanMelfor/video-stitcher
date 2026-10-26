//! Existing sport-specific formation heuristics, selected by explicit policy.
//! These are candidate generators, not ground truth or official referee decisions.
//! The parent module owns only shared event timing and latching.
use super::*;

impl SceneDetector {
    pub(super) fn patterns(&self, s: &FrameState, players: &[&AgentOut], out: &mut Vec<Cand>) {
        let sport = self.rules;
        if sport == SceneRules::None {
            return;
        }
        self.throw_ins(s, players, out);
        match sport {
            SceneRules::Basketball => {
                self.tip_off(players, out, 1.5, 2.5, 6);
                self.free_throws(s, players, out);
            }
            SceneRules::Lacrosse => self.tip_off(players, out, 1.2, 3.0, 4),
            _ => {}
        }
        if matches!(
            sport,
            SceneRules::Football
                | SceneRules::Futsal
                | SceneRules::Handball
                | SceneRules::Hockey
                | SceneRules::Rugby
        ) {
            self.kickoff(players, out);
        }
        if matches!(sport, SceneRules::Football | SceneRules::Futsal) {
            self.corners(players, out);
        }
        if matches!(
            sport,
            SceneRules::Football | SceneRules::Futsal | SceneRules::Handball | SceneRules::Hockey
        ) {
            self.penalties(players, out);
        }
    }

    /// Two opponents at the centre, everybody else standing around, nobody else close.
    fn tip_off(
        &self,
        players: &[&AgentOut],
        out: &mut Vec<Cand>,
        r_center: f32,
        r_pair: f32,
        min_others: usize,
    ) {
        let c = Vec2::new(
            self.profile.field.length / 2.0,
            self.profile.field.width / 2.0,
        );
        let mid: Vec<&&AgentOut> = players
            .iter()
            .filter(|p| p.pos.dist(c) <= r_center)
            .collect();
        if mid.len() != 2
            || mid[0].team == mid[1].team
            || mid.iter().any(|p| p.team == Team::Unknown)
        {
            return;
        }
        if mid[0].pos.dist(mid[1].pos) > r_pair || !mid.iter().all(|p| self.still(p)) {
            return;
        }
        let others: Vec<&&AgentOut> = players
            .iter()
            .filter(|p| p.pos.dist(c) > r_center)
            .collect();
        if others.len() < min_others || others.iter().any(|p| p.pos.dist(c) < r_center + 0.9) {
            return;
        }
        let calm = others
            .iter()
            .filter(|p| p.vel.len() < self.cfg.still_speed * 1.6)
            .count();
        if calm * 10 < others.len() * 7 {
            return;
        }
        out.push(Cand {
            kind: SceneKind::TipOff,
            key: 0,
            team: None,
            pos: c,
            side: None,
            conf: 0.7,
        });
    }

    /// A player standing still at the boundary, alone, with the game gathered around.
    fn throw_ins(&self, s: &FrameState, players: &[&AgentOut], out: &mut Vec<Cand>) {
        let (l, w) = (self.profile.field.length, self.profile.field.width);
        let sport = self.rules;
        for p in players {
            let ball_with = matches!(s.ball.state, BallState::Held { track } if track == p.id)
                || (!matches!(s.ball.state, BallState::Unknown | BallState::InFlight)
                    && s.ball.pos.map(|b| b.dist(p.pos) <= 1.5).unwrap_or(false));
            if self.profile.field.edge_dist(p.pos) > 0.8 || !self.still(p) {
                continue;
            }
            if players
                .iter()
                .filter(|q| q.id != p.id && q.pos.dist(p.pos) <= 9.0)
                .count()
                < 3
            {
                continue;
            }
            if players
                .iter()
                .any(|q| q.id != p.id && q.pos.dist(p.pos) < 1.2)
                && !(sport == SceneRules::Basketball && ball_with)
            {
                continue;
            }
            let (x, y) = (p.pos.x, p.pos.y);
            let in_corner = (x <= 2.5 || x >= l - 2.5) && (y <= 2.5 || y >= w - 2.5);
            let on_side = y <= 0.8 || y >= w - 0.8;
            let on_end = x <= 0.8 || x >= l - 0.8;
            let ok = match sport {
                SceneRules::Basketball => on_side || on_end,
                SceneRules::Football | SceneRules::Futsal => on_side && !in_corner,
                _ => on_side,
            };
            if !ok {
                continue;
            }
            out.push(Cand {
                kind: SceneKind::ThrowIn,
                key: (p.id % 250) as u8,
                team: Some(p.team),
                pos: p.pos,
                side: None,
                conf: if ball_with { 0.8 } else { 0.5 },
            });
        }
    }

    /// Both teams on their own halves, standing still; only the kicking team's taker(s) at the centre.
    fn kickoff(&self, players: &[&AgentOut], out: &mut Vec<Cand>) {
        let (l, w) = (self.profile.field.length, self.profile.field.width);
        if players.len() < 6.max((1.4 * self.profile.team_size as f32).ceil() as usize) {
            return;
        }
        let half = l / 2.0;
        let (home, away): (Vec<&&AgentOut>, Vec<&&AgentOut>) = {
            let h: Vec<_> = players.iter().filter(|p| p.team == Team::Home).collect();
            let a: Vec<_> = players.iter().filter(|p| p.team == Team::Away).collect();
            (h, a)
        };
        if home.len() < 2 || away.len() < 2 {
            return;
        }
        let frac = |v: &[&&AgentOut], left: bool| {
            v.iter()
                .filter(|p| {
                    if left {
                        p.pos.x <= half + 1.5
                    } else {
                        p.pos.x >= half - 1.5
                    }
                })
                .count() as f32
                / v.len() as f32
        };
        let split = (frac(&home, true) >= 0.85 && frac(&away, false) >= 0.85)
            || (frac(&home, false) >= 0.85 && frac(&away, true) >= 0.85);
        if !split {
            return;
        }
        let r_cc = match self.profile.id.as_str() {
            "football" => 9.15,
            "futsal" | "handball" => 3.0,
            "hockey" => 5.0,
            "rugby" => 10.0,
            _ => 4.0,
        };
        let c = Vec2::new(half, w / 2.0);
        let inside: Vec<&&AgentOut> = players.iter().filter(|p| p.pos.dist(c) <= r_cc).collect();
        if inside.is_empty()
            || inside.len() > 2
            || (inside.len() == 2 && inside[0].team != inside[1].team)
        {
            return;
        }
        let calm = players
            .iter()
            .filter(|p| p.vel.len() < self.cfg.still_speed * 1.6)
            .count();
        if calm * 10 < players.len() * 8 {
            return;
        }
        out.push(Cand {
            kind: SceneKind::Kickoff,
            key: 0,
            team: Some(inside[0].team),
            pos: c,
            side: None,
            conf: 0.7,
        });
    }

    /// A taker standing still at a corner, several players waiting in the area next to it.
    fn corners(&self, players: &[&AgentOut], out: &mut Vec<Cand>) {
        let (l, w) = (self.profile.field.length, self.profile.field.width);
        let min_box = if self.profile.id == "football" { 5 } else { 3 };
        for (i, (cx, cy, end)) in [
            (0.0f32, 0.0f32, 'l'),
            (0.0, w, 'l'),
            (l, 0.0, 'r'),
            (l, w, 'r'),
        ]
        .into_iter()
        .enumerate()
        {
            let corner = Vec2::new(cx, cy);
            let takers: Vec<&&AgentOut> = players
                .iter()
                .filter(|p| p.pos.dist(corner) <= 2.4 && self.still(p))
                .collect();
            if takers.len() != 1 {
                continue;
            }
            let k = takers[0];
            let rect = match self.profile.zone_rect(end, 1) {
                Some(r) => r,
                None => continue,
            };
            let inbox: Vec<&&AgentOut> = players
                .iter()
                .filter(|p| p.id != k.id && in_rect(p.pos, rect, 0.5))
                .collect();
            if inbox.len() < min_box {
                continue;
            }
            if inbox.iter().map(|p| p.vel.len()).sum::<f32>() / inbox.len() as f32 > 2.0 {
                continue;
            }
            let conf = (0.6 + 0.04 * inbox.len().min(8) as f32).min(0.9);
            out.push(Cand {
                kind: SceneKind::Corner,
                key: i as u8,
                team: Some(k.team),
                pos: k.pos,
                side: Some(end),
                conf,
            });
        }
    }

    /// Penalty / 7 m: the taker at the spot, nobody else near, at most the goalkeeper in the area.
    fn penalties(&self, players: &[&AgentOut], out: &mut Vec<Cand>) {
        let (clear_r, spot_r, kind) = match self.rules {
            SceneRules::Football => (8.5f32, 1.8f32, SceneKind::Penalty),
            SceneRules::Futsal | SceneRules::Hockey => (4.5, 1.5, SceneKind::Penalty),
            SceneRules::Handball => (1.9, 1.2, SceneKind::SevenMeter),
            _ => return,
        };
        if players.len() < 5.max((1.2 * self.profile.team_size as f32).ceil() as usize) {
            return;
        }
        for (end, key) in [('l', "spot_l"), ('r', "spot_r")] {
            let spot = match self.profile.landmark(key) {
                Some(s) => s,
                None => continue,
            };
            let takers: Vec<&&AgentOut> = players
                .iter()
                .filter(|p| p.pos.dist(spot) <= spot_r && self.still(p))
                .collect();
            if takers.len() != 1 {
                continue;
            }
            let k = takers[0];
            if players
                .iter()
                .any(|p| p.id != k.id && p.pos.dist(spot) < clear_r)
            {
                continue;
            }
            if let Some(z) = self.profile.zone_rect(end, 1)
                && players
                    .iter()
                    .filter(|p| p.id != k.id && in_rect(p.pos, z, 0.0))
                    .count()
                    > 1
            {
                continue; // only the goalkeeper may stand in the area
            }
            out.push(Cand {
                kind,
                key: end as u8,
                team: Some(k.team),
                pos: k.pos,
                side: Some(end),
                conf: 0.75,
            });
        }
    }
}
