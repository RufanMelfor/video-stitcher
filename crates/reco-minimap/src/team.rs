//! Team assignment from jersey colour (CIE Lab distance).

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Team {
    Home,
    Away,
    Unknown,
}

pub fn rgb_to_lab(rgb: [u8; 3]) -> [f32; 3] {
    let lin = |c: u8| {
        let c = c as f32 / 255.0;
        if c <= 0.04045 {
            c / 12.92
        } else {
            ((c + 0.055) / 1.055).powf(2.4)
        }
    };
    let (r, g, b) = (lin(rgb[0]), lin(rgb[1]), lin(rgb[2]));
    let x = (0.4124 * r + 0.3576 * g + 0.1805 * b) / 0.95047;
    let y = 0.2126 * r + 0.7152 * g + 0.0722 * b;
    let z = (0.0193 * r + 0.1192 * g + 0.9505 * b) / 1.08883;
    let f = |t: f32| {
        if t > 0.008856 {
            t.cbrt()
        } else {
            7.787 * t + 16.0 / 116.0
        }
    };
    let (fx, fy, fz) = (f(x), f(y), f(z));
    [116.0 * fy - 16.0, 500.0 * (fx - fy), 200.0 * (fy - fz)]
}

fn dist(a: [f32; 3], b: [f32; 3]) -> f32 {
    ((a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2) + (a[2] - b[2]).powi(2)).sqrt()
}

#[derive(Clone, Copy, Debug)]
pub struct TeamClassifier {
    home: [f32; 3],
    away: [f32; 3],
    /// Goalkeeper shirts (football, handball, hockey ...): they differ from the team colour but belong to the team.
    home_gk: Option<[f32; 3]>,
    away_gk: Option<[f32; 3]>,
    referee: Option<[f32; 3]>,
    /// Reject if farther than this from both teams.
    pub max_dist: f32,
    /// Reject if the two distances are closer than this.
    pub min_margin: f32,
}

impl TeamClassifier {
    pub fn new(home_rgb: [u8; 3], away_rgb: [u8; 3]) -> Self {
        Self {
            home: rgb_to_lab(home_rgb),
            away: rgb_to_lab(away_rgb),
            home_gk: None,
            away_gk: None,
            referee: None,
            max_dist: 45.0,
            min_margin: 6.0,
        }
    }

    /// Set the referee shirt colour (e.g. black or yellow) for 3-way matching.
    pub fn with_referee(mut self, rgb: [u8; 3]) -> Self {
        self.referee = Some(rgb_to_lab(rgb));
        self
    }

    /// Set the goalkeepers' shirt colours (either may be `None`, e.g. for a sport or a team without a distinct keeper kit).
    pub fn with_keepers(mut self, home_gk: Option<[u8; 3]>, away_gk: Option<[u8; 3]>) -> Self {
        self.home_gk = home_gk.map(rgb_to_lab);
        self.away_gk = away_gk.map(rgb_to_lab);
        self
    }

    /// Distance to a team: its outfield shirt or its goalkeeper shirt, whichever is closer; plus whether the keeper shirt won.
    fn team_dist(lab: [f32; 3], shirt: [f32; 3], gk: Option<[f32; 3]>) -> (f32, bool) {
        let d = dist(lab, shirt);
        match gk {
            Some(g) if dist(lab, g) < d => (dist(lab, g), true),
            _ => (d, false),
        }
    }

    fn nearest_team_dist(&self, lab: [f32; 3]) -> f32 {
        Self::team_dist(lab, self.home, self.home_gk)
            .0
            .min(Self::team_dist(lab, self.away, self.away_gk).0)
    }

    /// True if the colour is closer to the referee colour than to either team (keepers included) and close enough to it.
    pub fn is_referee_colored(&self, rgb: [u8; 3]) -> bool {
        match self.referee {
            Some(r) => {
                let lab = rgb_to_lab(rgb);
                let dr = dist(lab, r);
                dr <= self.max_dist && dr < self.nearest_team_dist(lab)
            }
            None => false,
        }
    }

    /// True if the colour is clearly neither team's nor a goalkeeper's (typical for referees: black / yellow).
    pub fn is_outlier(&self, rgb: [u8; 3]) -> bool {
        self.nearest_team_dist(rgb_to_lab(rgb)) > self.max_dist
    }

    pub fn classify(&self, rgb: Option<[u8; 3]>) -> Team {
        self.classify_role(rgb).0
    }

    /// Team and whether the colour is that team's goalkeeper shirt.
    pub fn classify_role(&self, rgb: Option<[u8; 3]>) -> (Team, bool) {
        let rgb = match rgb {
            Some(c) => c,
            None => return (Team::Unknown, false),
        };
        let lab = rgb_to_lab(rgb);
        let ((dh, gh), (da, ga)) = (
            Self::team_dist(lab, self.home, self.home_gk),
            Self::team_dist(lab, self.away, self.away_gk),
        );
        if dh.min(da) > self.max_dist || (dh - da).abs() < self.min_margin {
            return (Team::Unknown, false);
        }
        if dh < da {
            (Team::Home, gh)
        } else {
            (Team::Away, ga)
        }
    }
}
