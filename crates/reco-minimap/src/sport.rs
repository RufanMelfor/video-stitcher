//! Sport profiles: field size, line markings, team sizes and calibration landmarks.
//!
//! Geometry consumers (tracker limits, minimap drawing, calibration points, live overlay)
//! are driven by a [`SportProfile`]. Interpretation is selected separately in [`rules`];
//! adding a field profile does not automatically implement game understanding. Built-ins cover
//! the common standard fields; [`SportProfile::custom_rect`] makes a plain rectangle for any
//! other sport. Dimensions are the usual international standards; real halls often differ.

use crate::geom::{Field, Vec2};
pub mod rules;
use std::f32::consts::{FRAC_PI_2, PI, TAU};

#[derive(Clone, Debug)]
pub struct Landmark {
    /// Generic key, localised by the UI (e.g. `corner_fl`, `z1_l_far_base`).
    pub key: String,
    pub x: f32,
    pub y: f32,
}

#[derive(Clone, Debug)]
pub struct SportProfile {
    pub id: String,
    pub field: Field,
    /// Players per team on the field.
    pub team_size: usize,
    /// Referee / umpire slots.
    pub referees: usize,
    /// Plausible top speed of a player (m/s) and of the ball (m/s).
    pub player_vmax: f32,
    pub ball_max_speed: f32,
    /// Line markings as polylines in metres.
    pub lines: Vec<Vec<Vec2>>,
    pub landmarks: Vec<Landmark>,
    /// Goal / basket centres (metres), left end first.
    pub goals: Vec<Vec2>,
}

/// Same order as the sport list in Reco Trainer, then volleyball and the free rectangle.
pub const BUILTIN_IDS: &[&str] = &[
    "football",
    "futsal",
    "basketball",
    "handball",
    "hockey",
    "rugby",
    "lacrosse",
    "american_football",
    "volleyball",
    "custom",
];

/// Sports in which the goalkeeper wears a different colour from the team.
pub fn has_keepers(sport_id: &str) -> bool {
    matches!(
        sport_id,
        "football" | "futsal" | "handball" | "hockey" | "lacrosse"
    )
}

fn v(x: f32, y: f32) -> Vec2 {
    Vec2::new(x, y)
}
fn rect(x0: f32, y0: f32, x1: f32, y1: f32) -> Vec<Vec2> {
    vec![v(x0, y0), v(x1, y0), v(x1, y1), v(x0, y1), v(x0, y0)]
}
fn seg(a: Vec2, b: Vec2) -> Vec<Vec2> {
    vec![a, b]
}
fn arc(c: Vec2, r: f32, a0: f32, a1: f32) -> Vec<Vec2> {
    (0..=48)
        .map(|i| {
            let a = a0 + (a1 - a0) * i as f32 / 48.0;
            v(c.x + r * a.cos(), c.y + r * a.sin())
        })
        .collect()
}
fn circle(c: Vec2, r: f32) -> Vec<Vec2> {
    arc(c, r, 0.0, TAU)
}
fn lm(key: &str, x: f32, y: f32) -> Landmark {
    Landmark {
        key: key.to_string(),
        x,
        y,
    }
}

/// Corners, halfway-line ends and centre: the landmarks every field has.
fn base_landmarks(l: f32, w: f32) -> Vec<Landmark> {
    vec![
        lm("corner_fl", 0.0, 0.0),
        lm("corner_fr", l, 0.0),
        lm("corner_nr", l, w),
        lm("corner_nl", 0.0, w),
        lm("mid_far", l / 2.0, 0.0),
        lm("mid_near", l / 2.0, w),
        lm("center", l / 2.0, w / 2.0),
    ]
}

/// Rectangular area at both ends: key `z{n}_{l|r}_{far|near}_{base|inner}`.
fn zone_landmarks(out: &mut Vec<Landmark>, n: u8, l: f32, y_far: f32, y_near: f32, depth: f32) {
    for (side, sx, bx) in [("l", 1.0f32, 0.0f32), ("r", -1.0, l)] {
        for (pos, y) in [("far", y_far), ("near", y_near)] {
            out.push(lm(&format!("z{n}_{side}_{pos}_base"), bx, y));
            out.push(lm(&format!("z{n}_{side}_{pos}_inner"), bx + sx * depth, y));
        }
    }
}

/// Goal-area shape used by futsal / handball / hockey: quarter circles around the posts joined by a straight line.
fn goal_area(l: f32, w: f32, post_gap: f32, radius: f32) -> (Vec<Vec<Vec2>>, Vec<Landmark>) {
    let (p1, p2) = (w / 2.0 - post_gap / 2.0, w / 2.0 + post_gap / 2.0);
    let mut lines = Vec::new();
    let mut lms = Vec::new();
    for (side, mirror) in [("l", false), ("r", true)] {
        let mx = |x: f32| if mirror { l - x } else { x };
        let mut pl = Vec::new();
        for q in arc(v(0.0, p1), radius, -FRAC_PI_2, 0.0) {
            pl.push(v(mx(q.x), q.y));
        }
        for q in arc(v(0.0, p2), radius, 0.0, FRAC_PI_2) {
            pl.push(v(mx(q.x), q.y));
        }
        lines.push(pl);
        lms.push(lm(&format!("z1_{side}_far_base"), mx(0.0), p1 - radius));
        lms.push(lm(&format!("z1_{side}_near_base"), mx(0.0), p2 + radius));
        lms.push(lm(&format!("z1_{side}_far_inner"), mx(radius), p1));
        lms.push(lm(&format!("z1_{side}_near_inner"), mx(radius), p2));
    }
    (lines, lms)
}

impl SportProfile {
    pub fn builtin(id: &str) -> Option<SportProfile> {
        match id {
            "basketball" => Some(Self::basketball()),
            "football" => Some(Self::football()),
            "futsal" => Some(Self::futsal()),
            "handball" => Some(Self::handball()),
            "volleyball" => Some(Self::volleyball()),
            "hockey" => Some(Self::hockey()),
            "rugby" => Some(Self::rugby()),
            "lacrosse" => Some(Self::lacrosse()),
            "american_football" => Some(Self::american_football()),
            "custom" => Some(Self::custom_rect("custom", 40.0, 20.0, 5, 2)),
            _ => None,
        }
    }

    /// Plain rectangle with halfway line and centre circle - for any sport without a profile.
    pub fn custom_rect(
        id: &str,
        length: f32,
        width: f32,
        team_size: usize,
        referees: usize,
    ) -> SportProfile {
        let (l, w) = (length.max(2.0), width.max(2.0));
        let r = (w * 0.12).clamp(0.5, 9.15);
        SportProfile {
            id: id.to_string(),
            field: Field::new(l, w),
            team_size: team_size.clamp(1, 20),
            referees: referees.min(6),
            player_vmax: 9.0,
            ball_max_speed: 30.0,
            lines: vec![
                rect(0.0, 0.0, l, w),
                seg(v(l / 2.0, 0.0), v(l / 2.0, w)),
                circle(v(l / 2.0, w / 2.0), r),
            ],
            landmarks: base_landmarks(l, w),
            goals: Vec::new(),
        }
    }

    pub fn basketball() -> SportProfile {
        let (l, w) = (28.0f32, 15.0f32);
        let mut lines = vec![
            rect(0.0, 0.0, l, w),
            seg(v(l / 2.0, 0.0), v(l / 2.0, w)),
            circle(v(l / 2.0, w / 2.0), 1.8),
        ];
        let (tp, bk) = (6.75f32, 1.575f32);
        let ang = ((w / 2.0 - 0.9) / tp).asin();
        let ex = tp * ang.cos();
        for left in [true, false] {
            let at = |d: f32| if left { d } else { l - d };
            lines.push(rect(at(0.0).min(at(5.8)), 5.05, at(0.0).max(at(5.8)), 9.95)); // key
            lines.push(if left {
                arc(v(5.8, w / 2.0), 1.8, -FRAC_PI_2, FRAC_PI_2)
            } else {
                arc(v(l - 5.8, w / 2.0), 1.8, FRAC_PI_2, 3.0 * FRAC_PI_2)
            });
            lines.push(circle(v(at(bk), w / 2.0), 0.23));
            lines.push(seg(v(at(0.0), 0.9), v(at(bk + ex), 0.9)));
            lines.push(seg(v(at(0.0), w - 0.9), v(at(bk + ex), w - 0.9)));
            lines.push(if left {
                arc(v(bk, w / 2.0), tp, -ang, ang)
            } else {
                arc(v(l - bk, w / 2.0), tp, PI - ang, PI + ang)
            });
        }
        let mut lms = base_landmarks(l, w);
        zone_landmarks(&mut lms, 1, l, 5.05, 9.95, 5.8);
        SportProfile {
            id: "basketball".into(),
            field: Field::new(l, w),
            team_size: 5,
            referees: 2,
            player_vmax: 9.0,
            ball_max_speed: 20.0,
            lines,
            landmarks: lms,
            goals: vec![v(bk, w / 2.0), v(l - bk, w / 2.0)],
        }
    }

    pub fn football() -> SportProfile {
        let (l, w) = (105.0f32, 68.0f32);
        let mut lines = vec![
            rect(0.0, 0.0, l, w),
            seg(v(l / 2.0, 0.0), v(l / 2.0, w)),
            circle(v(l / 2.0, w / 2.0), 9.15),
        ];
        let (pa_y0, pa_y1, pa_d) = (13.84f32, 54.16f32, 16.5f32);
        let (ga_y0, ga_y1, ga_d) = (24.84f32, 43.16f32, 5.5f32);
        let a = (5.5f32 / 9.15).acos();
        lines.push(rect(0.0, pa_y0, pa_d, pa_y1));
        lines.push(rect(l - pa_d, pa_y0, l, pa_y1));
        lines.push(rect(0.0, ga_y0, ga_d, ga_y1));
        lines.push(rect(l - ga_d, ga_y0, l, ga_y1));
        lines.push(arc(v(11.0, w / 2.0), 9.15, -a, a));
        lines.push(arc(v(l - 11.0, w / 2.0), 9.15, PI - a, PI + a));
        let mut lms = base_landmarks(l, w);
        zone_landmarks(&mut lms, 1, l, pa_y0, pa_y1, pa_d);
        zone_landmarks(&mut lms, 2, l, ga_y0, ga_y1, ga_d);
        lms.push(lm("spot_l", 11.0, w / 2.0));
        lms.push(lm("spot_r", l - 11.0, w / 2.0));
        SportProfile {
            id: "football".into(),
            field: Field::new(l, w),
            team_size: 11,
            referees: 3,
            player_vmax: 10.0,
            ball_max_speed: 35.0,
            lines,
            landmarks: lms,
            goals: vec![v(0.0, w / 2.0), v(l, w / 2.0)],
        }
    }

    pub fn futsal() -> SportProfile {
        let (l, w) = (40.0f32, 20.0f32);
        let mut lines = vec![
            rect(0.0, 0.0, l, w),
            seg(v(l / 2.0, 0.0), v(l / 2.0, w)),
            circle(v(l / 2.0, w / 2.0), 3.0),
        ];
        let (ga, mut lms) = goal_area(l, w, 3.0, 6.0);
        lines.extend(ga);
        // the straight part in front of the goal
        lines.push(seg(v(6.0, 8.5), v(6.0, 11.5)));
        lines.push(seg(v(l - 6.0, 8.5), v(l - 6.0, 11.5)));
        let mut all = base_landmarks(l, w);
        all.append(&mut lms);
        all.push(lm("spot_l", 6.0, w / 2.0));
        all.push(lm("spot_r", l - 6.0, w / 2.0));
        SportProfile {
            id: "futsal".into(),
            field: Field::new(l, w),
            team_size: 5,
            referees: 2,
            player_vmax: 9.0,
            ball_max_speed: 25.0,
            lines,
            landmarks: all,
            goals: vec![v(0.0, w / 2.0), v(l, w / 2.0)],
        }
    }

    pub fn handball() -> SportProfile {
        let (l, w) = (40.0f32, 20.0f32);
        let mut lines = vec![rect(0.0, 0.0, l, w), seg(v(l / 2.0, 0.0), v(l / 2.0, w))];
        let (ga, mut lms) = goal_area(l, w, 3.0, 6.0);
        lines.extend(ga);
        lines.push(seg(v(6.0, 8.5), v(6.0, 11.5)));
        lines.push(seg(v(l - 6.0, 8.5), v(l - 6.0, 11.5)));
        lines.push(seg(v(7.0, 9.5), v(7.0, 10.5))); // 7 m mark
        lines.push(seg(v(l - 7.0, 9.5), v(l - 7.0, 10.5)));
        let mut all = base_landmarks(l, w);
        all.append(&mut lms);
        all.push(lm("spot_l", 7.0, w / 2.0));
        all.push(lm("spot_r", l - 7.0, w / 2.0));
        SportProfile {
            id: "handball".into(),
            field: Field::new(l, w),
            team_size: 7,
            referees: 2,
            player_vmax: 9.0,
            ball_max_speed: 30.0,
            lines,
            landmarks: all,
            goals: vec![v(0.0, w / 2.0), v(l, w / 2.0)],
        }
    }

    /// Rugby union: playing field 100 m + 2 x 10 m in-goal, 70 m wide (in-goal depth varies in real life).
    pub fn rugby() -> SportProfile {
        let (l, w, ig) = (120.0f32, 70.0f32, 10.0f32);
        let mut lines = vec![rect(0.0, 0.0, l, w)];
        for x in [ig, l / 2.0, l - ig, ig + 22.0, l - ig - 22.0] {
            lines.push(seg(v(x, 0.0), v(x, w)));
        }
        for x in [l / 2.0 - 10.0, l / 2.0 + 10.0] {
            lines.push(seg(v(x, 0.0), v(x, w))); // 10 m lines (dashed in reality)
        }
        let mut lms = base_landmarks(l, w);
        for (key, x) in [
            ("line_l", ig),
            ("line_r", l - ig),
            ("line2_l", ig + 22.0),
            ("line2_r", l - ig - 22.0),
        ] {
            lms.push(lm(&format!("{key}_far"), x, 0.0));
            lms.push(lm(&format!("{key}_near"), x, w));
        }
        SportProfile {
            id: "rugby".into(),
            field: Field::new(l, w),
            team_size: 15,
            referees: 3,
            player_vmax: 10.0,
            ball_max_speed: 30.0,
            lines,
            landmarks: lms,
            goals: Vec::new(),
        }
    }

    /// Men's field lacrosse: 110 x 60 yards, goal lines 15 yards from the end lines, 9 ft crease.
    pub fn lacrosse() -> SportProfile {
        let (l, w) = (100.58f32, 54.86f32);
        let gl = 13.716f32; // 15 yd
        let lines = vec![
            rect(0.0, 0.0, l, w),
            seg(v(l / 2.0, 0.0), v(l / 2.0, w)),
            seg(v(gl, 0.0), v(gl, w)),
            seg(v(l - gl, 0.0), v(l - gl, w)),
            circle(v(gl, w / 2.0), 2.743),
            circle(v(l - gl, w / 2.0), 2.743),
        ];
        let mut lms = base_landmarks(l, w);
        for (key, x) in [("line_l", gl), ("line_r", l - gl)] {
            lms.push(lm(&format!("{key}_far"), x, 0.0));
            lms.push(lm(&format!("{key}_near"), x, w));
        }
        SportProfile {
            id: "lacrosse".into(),
            field: Field::new(l, w),
            team_size: 10,
            referees: 3,
            player_vmax: 9.0,
            ball_max_speed: 40.0,
            lines,
            landmarks: lms,
            goals: Vec::new(),
        }
    }

    /// American football: 100 yd field + 2 x 10 yd end zones, 53 1/3 yd wide.
    pub fn american_football() -> SportProfile {
        let yd = 0.9144f32;
        let (l, w) = (120.0 * yd, 160.0 / 3.0 * yd);
        let mut lines = vec![rect(0.0, 0.0, l, w)];
        for k in 0..=10 {
            let x = (10.0 + 10.0 * k as f32) * yd; // goal line, then every 10 yards
            lines.push(seg(v(x, 0.0), v(x, w)));
        }
        let mut lms = base_landmarks(l, w);
        for (key, x) in [
            ("line_l", 10.0 * yd),
            ("line_r", l - 10.0 * yd),
            ("line2_l", 30.0 * yd),
            ("line2_r", l - 30.0 * yd),
        ] {
            lms.push(lm(&format!("{key}_far"), x, 0.0));
            lms.push(lm(&format!("{key}_near"), x, w));
        }
        SportProfile {
            id: "american_football".into(),
            field: Field::new(l, w),
            team_size: 11,
            referees: 7,
            player_vmax: 10.0,
            ball_max_speed: 35.0,
            lines,
            landmarks: lms,
            goals: Vec::new(),
        }
    }

    pub fn volleyball() -> SportProfile {
        let (l, w) = (18.0f32, 9.0f32);
        let lines = vec![
            rect(0.0, 0.0, l, w),
            seg(v(l / 2.0, 0.0), v(l / 2.0, w)),
            seg(v(6.0, 0.0), v(6.0, w)),
            seg(v(12.0, 0.0), v(12.0, w)),
        ];
        let mut lms = base_landmarks(l, w);
        lms.push(lm("line_l_far", 6.0, 0.0));
        lms.push(lm("line_l_near", 6.0, w));
        lms.push(lm("line_r_far", 12.0, 0.0));
        lms.push(lm("line_r_near", 12.0, w));
        SportProfile {
            id: "volleyball".into(),
            field: Field::new(l, w),
            team_size: 6,
            referees: 2,
            player_vmax: 7.0,
            ball_max_speed: 30.0,
            lines,
            landmarks: lms,
            goals: Vec::new(),
        }
    }

    pub fn hockey() -> SportProfile {
        let (l, w) = (91.4f32, 55.0f32);
        let mut lines = vec![
            rect(0.0, 0.0, l, w),
            seg(v(l / 2.0, 0.0), v(l / 2.0, w)),
            seg(v(22.9, 0.0), v(22.9, w)),
            seg(v(l - 22.9, 0.0), v(l - 22.9, w)),
        ];
        let (ga, mut lms) = goal_area(l, w, 3.66, 14.63);
        lines.extend(ga);
        lines.push(seg(v(14.63, w / 2.0 - 1.83), v(14.63, w / 2.0 + 1.83)));
        lines.push(seg(
            v(l - 14.63, w / 2.0 - 1.83),
            v(l - 14.63, w / 2.0 + 1.83),
        ));
        let mut all = base_landmarks(l, w);
        all.append(&mut lms);
        all.push(lm("line_l_far", 22.9, 0.0));
        all.push(lm("line_l_near", 22.9, w));
        all.push(lm("line_r_far", l - 22.9, 0.0));
        all.push(lm("line_r_near", l - 22.9, w));
        all.push(lm("spot_l", 6.4, w / 2.0));
        all.push(lm("spot_r", l - 6.4, w / 2.0));
        SportProfile {
            id: "hockey".into(),
            field: Field::new(l, w),
            team_size: 11,
            referees: 2,
            player_vmax: 10.0,
            ball_max_speed: 40.0,
            lines,
            landmarks: all,
            goals: vec![v(0.0, w / 2.0), v(l, w / 2.0)],
        }
    }

    pub fn landmark(&self, key: &str) -> Option<Vec2> {
        self.landmarks
            .iter()
            .find(|m| m.key == key)
            .map(|m| Vec2::new(m.x, m.y))
    }

    /// Rectangle (x0, y0, x1, y1) of zone `n` (1 = penalty area / key / goal area ...) at the end `'l'` or `'r'`.
    pub fn zone_rect(&self, end: char, n: u8) -> Option<(f32, f32, f32, f32)> {
        let k = |pos: &str, which: &str| self.landmark(&format!("z{n}_{end}_{pos}_{which}"));
        let (fb, fi, nb, ni) = (
            k("far", "base")?,
            k("far", "inner")?,
            k("near", "base")?,
            k("near", "inner")?,
        );
        let xs = [fb.x, fi.x, nb.x, ni.x];
        let (x0, x1) = (
            xs.iter().cloned().fold(f32::MAX, f32::min),
            xs.iter().cloned().fold(f32::MIN, f32::max),
        );
        Some((x0, fb.y.min(nb.y), x1, fb.y.max(nb.y)))
    }

    /// Insert extra points into long straight segments so a projective drawing stays accurate.
    pub fn densified_lines(&self, step_m: f32) -> Vec<Vec<Vec2>> {
        self.lines
            .iter()
            .map(|pl| {
                let mut out: Vec<Vec2> = Vec::new();
                for (i, p) in pl.iter().enumerate() {
                    if i > 0 {
                        let q = pl[i - 1];
                        let n = ((p.dist(q) / step_m).ceil() as usize).max(1);
                        for k in 1..n {
                            out.push(q.lerp(*p, k as f32 / n as f32));
                        }
                    }
                    out.push(*p);
                }
                out
            })
            .collect()
    }
}
