//! Basic geometry: 2D vectors, FIBA court dimensions, homography.

use std::ops::{Add, Mul, Sub};

/// FIBA court length (x axis) in metres.
pub const COURT_LENGTH: f32 = 28.0;
/// FIBA court width (y axis) in metres.
pub const COURT_WIDTH: f32 = 15.0;

#[derive(Clone, Copy, Debug, PartialEq, Default)]
pub struct Vec2 {
    pub x: f32,
    pub y: f32,
}

impl Vec2 {
    pub const fn new(x: f32, y: f32) -> Self {
        Self { x, y }
    }
    pub fn len(self) -> f32 {
        (self.x * self.x + self.y * self.y).sqrt()
    }
    pub fn dist(self, o: Vec2) -> f32 {
        (self - o).len()
    }
    pub fn lerp(self, o: Vec2, t: f32) -> Vec2 {
        self + (o - self) * t
    }
}

impl Add for Vec2 {
    type Output = Vec2;
    fn add(self, o: Vec2) -> Vec2 {
        Vec2::new(self.x + o.x, self.y + o.y)
    }
}
impl Sub for Vec2 {
    type Output = Vec2;
    fn sub(self, o: Vec2) -> Vec2 {
        Vec2::new(self.x - o.x, self.y - o.y)
    }
}
impl Mul<f32> for Vec2 {
    type Output = Vec2;
    fn mul(self, k: f32) -> Vec2 {
        Vec2::new(self.x * k, self.y * k)
    }
}

/// Playing field size in metres (x: length, left -> right; y: width, far -> near sideline).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Field {
    pub length: f32,
    pub width: f32,
}

impl Field {
    pub const fn new(length: f32, width: f32) -> Self {
        Self { length, width }
    }
    pub const fn basketball() -> Self {
        Self {
            length: COURT_LENGTH,
            width: COURT_WIDTH,
        }
    }
    /// True if `p` lies within the field grown by `margin` metres.
    pub fn contains(&self, p: Vec2, margin: f32) -> bool {
        p.x >= -margin
            && p.x <= self.length + margin
            && p.y >= -margin
            && p.y <= self.width + margin
    }
    pub fn clamp(&self, p: Vec2, margin: f32) -> Vec2 {
        Vec2::new(
            p.x.clamp(-margin, self.length + margin),
            p.y.clamp(-margin, self.width + margin),
        )
    }
    /// Distance to the nearest field edge in metres (negative outside).
    pub fn edge_dist(&self, p: Vec2) -> f32 {
        p.x.min(self.length - p.x).min(p.y).min(self.width - p.y)
    }
}

impl Default for Field {
    fn default() -> Self {
        Field::basketball()
    }
}

/// True if `p` lies within the (basketball) court rectangle grown by `margin` metres.
pub fn in_court(p: Vec2, margin: f32) -> bool {
    p.x >= -margin && p.x <= COURT_LENGTH + margin && p.y >= -margin && p.y <= COURT_WIDTH + margin
}

/// Clamp `p` into the court rectangle grown by `margin` metres.
pub fn clamp_to_court(p: Vec2, margin: f32) -> Vec2 {
    Vec2::new(
        p.x.clamp(-margin, COURT_LENGTH + margin),
        p.y.clamp(-margin, COURT_WIDTH + margin),
    )
}

/// Maps a point in the panorama (normalized `[0,1]` coords) to court metres.
///
/// Reco panoramas are *not* a pinhole image, so the real mapping is
/// "undo the panorama projection, then homography". Implement this trait
/// on top of reco-core's projection; [`HomographyProjector`] is the plain
/// planar variant and the calibration fallback.
///
/// `Send` so a [`crate::Minimap`] can live behind the stitch session's
/// event sink, which runs on the session thread.
pub trait Projector: Send {
    fn to_court(&self, u: f32, v: f32) -> Option<Vec2>;
}

/// Plain 3x3 homography (row-major), `h[8] == 1`.
#[derive(Clone, Copy, Debug)]
pub struct Homography {
    h: [f64; 9],
}

impl Homography {
    pub fn from_matrix(h: [f64; 9]) -> Self {
        Self { h }
    }

    /// Least-squares DLT from `n >= 4` point pairs (`src` -> `dst`).
    pub fn from_points(src: &[Vec2], dst: &[Vec2]) -> Option<Self> {
        if src.len() != dst.len() || src.len() < 4 {
            return None;
        }
        let mut ata = [[0f64; 8]; 8];
        let mut atb = [0f64; 8];
        for (s, d) in src.iter().zip(dst) {
            let (x, y, u, v) = (s.x as f64, s.y as f64, d.x as f64, d.y as f64);
            let rows = [
                ([x, y, 1.0, 0.0, 0.0, 0.0, -x * u, -y * u], u),
                ([0.0, 0.0, 0.0, x, y, 1.0, -x * v, -y * v], v),
            ];
            for (r, b) in rows.iter() {
                for i in 0..8 {
                    for j in 0..8 {
                        ata[i][j] += r[i] * r[j];
                    }
                    atb[i] += r[i] * b;
                }
            }
        }
        let s = solve8(ata, atb)?;
        Some(Self {
            h: [s[0], s[1], s[2], s[3], s[4], s[5], s[6], s[7], 1.0],
        })
    }

    pub fn apply(&self, p: Vec2) -> Option<Vec2> {
        let (x, y) = (p.x as f64, p.y as f64);
        let h = &self.h;
        let w = h[6] * x + h[7] * y + h[8];
        if w.abs() < 1e-12 {
            return None;
        }
        let u = (h[0] * x + h[1] * y + h[2]) / w;
        let v = (h[3] * x + h[4] * y + h[5]) / w;
        if !u.is_finite() || !v.is_finite() {
            return None;
        }
        Some(Vec2::new(u as f32, v as f32))
    }

    pub fn inverse(&self) -> Option<Self> {
        let m = &self.h;
        let (a, b, c, d, e, f, g, h, i) = (m[0], m[1], m[2], m[3], m[4], m[5], m[6], m[7], m[8]);
        let det = a * (e * i - f * h) - b * (d * i - f * g) + c * (d * h - e * g);
        if det.abs() < 1e-18 {
            return None;
        }
        let inv = [
            e * i - f * h,
            c * h - b * i,
            b * f - c * e,
            f * g - d * i,
            a * i - c * g,
            c * d - a * f,
            d * h - e * g,
            b * g - a * h,
            a * e - b * d,
        ];
        let s = 1.0 / det;
        let mut out = [0f64; 9];
        for k in 0..9 {
            out[k] = inv[k] * s;
        }
        Some(Self { h: out })
    }
}

fn solve8(mut a: [[f64; 8]; 8], mut b: [f64; 8]) -> Option<[f64; 8]> {
    for col in 0..8 {
        let mut piv = col;
        for r in col + 1..8 {
            if a[r][col].abs() > a[piv][col].abs() {
                piv = r;
            }
        }
        if a[piv][col].abs() < 1e-12 {
            return None;
        }
        a.swap(col, piv);
        b.swap(col, piv);
        for r in col + 1..8 {
            let f = a[r][col] / a[col][col];
            let pivot_row = a[col];
            for (dst, src) in a[r].iter_mut().zip(pivot_row.iter()).skip(col) {
                *dst -= f * src;
            }
            b[r] -= f * b[col];
        }
    }
    let mut x = [0f64; 8];
    for r in (0..8).rev() {
        let mut s = b[r];
        for c in r + 1..8 {
            s -= a[r][c] * x[c];
        }
        x[r] = s / a[r][r];
    }
    Some(x)
}

/// Planar calibration: panorama (normalized) -> court metres.
#[derive(Clone, Copy, Debug)]
pub struct HomographyProjector {
    pub pano_to_court: Homography,
}

impl HomographyProjector {
    /// `pano_pts`: normalized panorama coords of known court points,
    /// `court_pts`: the same points in court metres.
    pub fn calibrate(pano_pts: &[Vec2], court_pts: &[Vec2]) -> Option<Self> {
        Some(Self {
            pano_to_court: Homography::from_points(pano_pts, court_pts)?,
        })
    }
}

impl Projector for HomographyProjector {
    fn to_court(&self, u: f32, v: f32) -> Option<Vec2> {
        self.pano_to_court.apply(Vec2::new(u, v))
    }
}
