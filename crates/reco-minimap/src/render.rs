//! Software renderer for the minimap (RGBA, no dependencies).
//! Output can go to OBS (`reco-obs`), a PiP via FFmpeg, or a PNG/PPM.

use crate::ball::BallState;
use crate::geom::Vec2;
use crate::sport::SportProfile;
use crate::team::Team;
use crate::{AgentOut, FrameState};

pub type Rgb = [u8; 3];

#[derive(Clone, Debug)]
pub struct Theme {
    pub px_per_m: f32,
    pub pad_m: f32,
    pub floor: Rgb,
    pub lines: Rgb,
    pub home: Rgb,
    pub away: Rgb,
    pub referee: Rgb,
    pub ball: Rgb,
    pub holder_ring: Rgb,
    pub background_alpha: u8,
    /// Lower bound for the opacity of dots whose player is currently not detected (coasting).
    /// A low value turns a white shirt into a grey dot on the dark floor.
    pub min_alpha: f32,
    /// Draw jersey numbers on the dots (and make the dots a bit larger so two digits fit).
    pub show_numbers: bool,
    /// Goalkeepers are drawn with diagonal stripes (set to false for plain dots).
    pub keeper_stripes: bool,
    /// Stripe colours of the home / away keeper; `None` = black or white, whichever contrasts with the team colour.
    pub home_keeper: Option<Rgb>,
    pub away_keeper: Option<Rgb>,
    /// Multiplies the size of player, referee and ball markers (1.0 = upstream size). Small
    /// overlays of a big field need larger markers to stay visible.
    pub marker_scale: f32,
}

impl Default for Theme {
    fn default() -> Self {
        Self {
            marker_scale: 1.0,
            px_per_m: 30.0,
            pad_m: 1.5,
            floor: [38, 44, 52],
            lines: [225, 228, 232],
            home: [220, 40, 40],
            away: [40, 110, 230],
            referee: [0, 0, 0],
            ball: [255, 140, 0],
            holder_ring: [255, 140, 0],
            background_alpha: 255,
            min_alpha: 0.8,
            show_numbers: false,
            keeper_stripes: true,
            home_keeper: None,
            away_keeper: None,
        }
    }
}

pub struct Canvas {
    pub w: usize,
    pub h: usize,
    /// RGBA, row-major, straight alpha.
    pub rgba: Vec<u8>,
}

impl Canvas {
    pub fn new(w: usize, h: usize, bg: Rgb, a: u8) -> Self {
        let mut rgba = vec![0u8; w * h * 4];
        for p in rgba.chunks_exact_mut(4) {
            p.copy_from_slice(&[bg[0], bg[1], bg[2], a]);
        }
        Self { w, h, rgba }
    }

    fn blend(&mut self, x: usize, y: usize, c: Rgb, cov: f32) {
        if cov <= 0.0 {
            return;
        }
        let i = (y * self.w + x) * 4;
        let a = cov.clamp(0.0, 1.0);
        let dst_a = self.rgba[i + 3] as f32 / 255.0;
        let out_a = a + dst_a * (1.0 - a);
        for (k, &ck) in c.iter().enumerate() {
            let d = self.rgba[i + k] as f32;
            let v = if out_a > 0.0 {
                (ck as f32 * a + d * dst_a * (1.0 - a)) / out_a
            } else {
                0.0
            };
            self.rgba[i + k] = v.round().clamp(0.0, 255.0) as u8;
        }
        self.rgba[i + 3] = (out_a * 255.0).round() as u8;
    }

    /// Filled disc (r_in = 0) or ring (r_in > 0), anti-aliased, `alpha` in 0..1.
    pub fn disc(&mut self, cx: f32, cy: f32, r_out: f32, r_in: f32, c: Rgb, alpha: f32) {
        let (x0, x1) = (
            (cx - r_out - 1.0).floor().max(0.0) as usize,
            ((cx + r_out + 1.0).ceil() as usize).min(self.w.saturating_sub(1)),
        );
        let (y0, y1) = (
            (cy - r_out - 1.0).floor().max(0.0) as usize,
            ((cy + r_out + 1.0).ceil() as usize).min(self.h.saturating_sub(1)),
        );
        for y in y0..=y1 {
            for x in x0..=x1 {
                let d = ((x as f32 + 0.5 - cx).powi(2) + (y as f32 + 0.5 - cy).powi(2)).sqrt();
                let outer = (r_out - d + 0.5).clamp(0.0, 1.0);
                let inner = if r_in > 0.0 {
                    (d - r_in + 0.5).clamp(0.0, 1.0)
                } else {
                    1.0
                };
                self.blend(x, y, c, outer * inner * alpha);
            }
        }
    }

    /// Diagonal stripes over a disc (alternating `period` px bands, anti-aliased at the rim).
    pub fn stripes(&mut self, cx: f32, cy: f32, r: f32, period: f32, c: Rgb, alpha: f32) {
        let (x0, x1) = (
            (cx - r - 1.0).floor().max(0.0) as usize,
            ((cx + r + 1.0).ceil() as usize).min(self.w.saturating_sub(1)),
        );
        let (y0, y1) = (
            (cy - r - 1.0).floor().max(0.0) as usize,
            ((cy + r + 1.0).ceil() as usize).min(self.h.saturating_sub(1)),
        );
        let period = period.max(1.5);
        for y in y0..=y1 {
            for x in x0..=x1 {
                let (px, py) = (x as f32 + 0.5, y as f32 + 0.5);
                let d = ((px - cx).powi(2) + (py - cy).powi(2)).sqrt();
                let inside = (r - d + 0.5).clamp(0.0, 1.0);
                if inside <= 0.0 {
                    continue;
                }
                // position across the stripes (diagonal), centred on the disc so every dot looks the same
                let u = ((px - cx) + (py - cy)) / std::f32::consts::SQRT_2 / period;
                if u.rem_euclid(2.0) >= 1.0 {
                    self.blend(x, y, c, inside * alpha);
                }
            }
        }
    }

    pub fn line(&mut self, a: (f32, f32), b: (f32, f32), thick: f32, c: Rgb, alpha: f32) {
        let r = thick / 2.0;
        let (x0, x1) = (
            (a.0.min(b.0) - r - 1.0).floor().max(0.0) as usize,
            ((a.0.max(b.0) + r + 1.0).ceil() as usize).min(self.w.saturating_sub(1)),
        );
        let (y0, y1) = (
            (a.1.min(b.1) - r - 1.0).floor().max(0.0) as usize,
            ((a.1.max(b.1) + r + 1.0).ceil() as usize).min(self.h.saturating_sub(1)),
        );
        let (dx, dy) = (b.0 - a.0, b.1 - a.1);
        let l2 = (dx * dx + dy * dy).max(1e-9);
        for y in y0..=y1 {
            for x in x0..=x1 {
                let (px, py) = (x as f32 + 0.5, y as f32 + 0.5);
                let t = (((px - a.0) * dx + (py - a.1) * dy) / l2).clamp(0.0, 1.0);
                let d = ((px - (a.0 + t * dx)).powi(2) + (py - (a.1 + t * dy)).powi(2)).sqrt();
                self.blend(x, y, c, (r - d + 0.5).clamp(0.0, 1.0) * alpha);
            }
        }
    }

    pub fn write_ppm(&self) -> Vec<u8> {
        let mut out = format!("P6\n{} {}\n255\n", self.w, self.h).into_bytes();
        for p in self.rgba.chunks_exact(4) {
            out.extend_from_slice(&p[..3]);
        }
        out
    }
}

/// 5x7 pixel digits (rows top to bottom, 5 bits each).
const FONT: [[u8; 7]; 10] = [
    [
        0b01110, 0b10001, 0b10011, 0b10101, 0b11001, 0b10001, 0b01110,
    ],
    [
        0b00100, 0b01100, 0b00100, 0b00100, 0b00100, 0b00100, 0b01110,
    ],
    [
        0b01110, 0b10001, 0b00001, 0b00010, 0b00100, 0b01000, 0b11111,
    ],
    [
        0b11110, 0b00001, 0b00001, 0b01110, 0b00001, 0b00001, 0b11110,
    ],
    [
        0b00010, 0b00110, 0b01010, 0b10010, 0b11111, 0b00010, 0b00010,
    ],
    [
        0b11111, 0b10000, 0b11110, 0b00001, 0b00001, 0b10001, 0b01110,
    ],
    [
        0b00110, 0b01000, 0b10000, 0b11110, 0b10001, 0b10001, 0b01110,
    ],
    [
        0b11111, 0b00001, 0b00010, 0b00100, 0b01000, 0b01000, 0b01000,
    ],
    [
        0b01110, 0b10001, 0b10001, 0b01110, 0b10001, 0b10001, 0b01110,
    ],
    [
        0b01110, 0b10001, 0b10001, 0b01111, 0b00001, 0b00010, 0b01100,
    ],
];

impl Canvas {
    /// Draw a 0-99 number centred at (`cx`,`cy`), `height` px tall, anti-aliased.
    pub fn number(&mut self, cx: f32, cy: f32, n: u8, height: f32, color: Rgb) {
        let digits: Vec<usize> = n.to_string().bytes().map(|b| (b - b'0') as usize).collect();
        let units_w = (digits.len() * 5 + digits.len().saturating_sub(1)) as f32; // 1 column gap
        let sc = height / 7.0;
        let (w, x0, y0) = (units_w * sc, cx - units_w * sc / 2.0, cy - height / 2.0);
        let sub = 3usize; // 3x3 samples per pixel
        for py in (y0.floor().max(0.0) as usize)
            ..=((y0 + height).ceil() as usize).min(self.h.saturating_sub(1))
        {
            for px in (x0.floor().max(0.0) as usize)
                ..=((x0 + w).ceil() as usize).min(self.w.saturating_sub(1))
            {
                let mut hit = 0usize;
                for sy in 0..sub {
                    for sx in 0..sub {
                        let u = ((px as f32 + (sx as f32 + 0.5) / sub as f32) - x0) / sc; // glyph units
                        let v = ((py as f32 + (sy as f32 + 0.5) / sub as f32) - y0) / sc;
                        if u < 0.0 || !(0.0..7.0).contains(&v) {
                            continue;
                        }
                        let cell = (u / 6.0).floor() as usize; // 5 columns + 1 gap per digit
                        let col = u - cell as f32 * 6.0;
                        if cell < digits.len()
                            && col < 5.0
                            && (FONT[digits[cell]][v as usize] >> (4 - col as usize)) & 1 == 1
                        {
                            hit += 1;
                        }
                    }
                }
                if hit > 0 {
                    self.blend(px, py, color, hit as f32 / (sub * sub) as f32);
                }
            }
        }
    }
}

pub struct Renderer {
    pub theme: Theme,
    pub profile: SportProfile,
}

impl Renderer {
    /// Basketball court (the original look).
    pub fn new(theme: Theme) -> Self {
        Self {
            theme,
            profile: SportProfile::basketball(),
        }
    }

    pub fn with_profile(theme: Theme, profile: SportProfile) -> Self {
        Self { theme, profile }
    }

    /// Marker scale: dots, lines and padding grow on bigger fields so they stay visible.
    fn k(&self) -> f32 {
        (self.profile.field.length / 28.0).sqrt().max(0.8)
    }

    /// A pixels-per-metre value that gives a ~900 px wide picture for this field.
    pub fn default_px_per_m(profile: &SportProfile) -> f32 {
        let k = (profile.field.length / 28.0).sqrt().max(0.8);
        900.0 / (profile.field.length + 3.0 * k)
    }

    pub fn size(&self) -> (usize, usize) {
        let (t, f, k) = (&self.theme, &self.profile.field, self.k());
        (
            ((f.length + 2.0 * t.pad_m * k) * t.px_per_m).round() as usize,
            ((f.width + 2.0 * t.pad_m * k) * t.px_per_m).round() as usize,
        )
    }

    fn px(&self, p: Vec2) -> (f32, f32) {
        let (t, k) = (&self.theme, self.k());
        (
            (p.x + t.pad_m * k) * t.px_per_m,
            (p.y + t.pad_m * k) * t.px_per_m,
        )
    }

    pub fn draw_court(&self, c: &mut Canvas) {
        let th = (0.08 * self.k() * self.theme.px_per_m).max(1.5);
        for pl in &self.profile.lines {
            for w in pl.windows(2) {
                c.line(self.px(w[0]), self.px(w[1]), th, self.theme.lines, 1.0);
            }
        }
    }

    fn agent(&self, c: &mut Canvas, a: &AgentOut, holder: bool) {
        let t = &self.theme;
        let (x, y) = self.px(a.pos);
        let r =
            0.45 * self.k() * t.px_per_m * t.marker_scale * if t.show_numbers { 1.35 } else { 1.0 };
        let fill = if a.referee {
            t.referee
        } else if a.team == Team::Home {
            t.home
        } else {
            t.away
        };
        // outline contrasts with the fill so dark shirts stay visible on the dark floor
        let lum = 0.2126 * fill[0] as f32 + 0.7152 * fill[1] as f32 + 0.0722 * fill[2] as f32;
        let edge = if lum < 90.0 {
            [235u8, 235, 235]
        } else {
            [15u8, 15, 15]
        };
        let alpha = a.alpha.max(t.min_alpha).min(1.0);
        c.disc(x, y, r + 1.5, 0.0, edge, alpha * 0.9);
        c.disc(x, y, r, 0.0, fill, alpha);
        let striped = a.keeper && !a.referee && t.keeper_stripes;
        if striped {
            let own = if a.team == Team::Home {
                t.home_keeper
            } else {
                t.away_keeper
            };
            let stripe = own.unwrap_or(if lum < 140.0 {
                [255u8, 255, 255]
            } else {
                [15u8, 15, 15]
            });
            c.stripes(x, y, r, r * 0.42, stripe, alpha * 0.9);
            if t.show_numbers && a.jersey.is_some() {
                c.disc(x, y, r * 0.66, 0.0, fill, alpha); // plain plate so the number stays readable
            }
        }
        if t.show_numbers
            && let Some(n) = a.jersey
        {
            let ink = if lum < 140.0 {
                [255u8, 255, 255]
            } else {
                [10u8, 10, 10]
            };
            c.number(x, y, n, (r * 1.05).max(7.0), ink);
        }
        if holder {
            c.disc(
                x,
                y,
                r * 1.75,
                r * 1.75 - (0.13 * self.k() * t.px_per_m).max(2.0),
                t.holder_ring,
                1.0,
            );
        }
    }

    pub fn render(&self, s: &FrameState) -> Canvas {
        let (w, h) = self.size();
        let mut c = Canvas::new(w, h, self.theme.floor, self.theme.background_alpha);
        self.draw_court(&mut c);
        let holder = if let BallState::Held { track } = s.ball.state {
            Some(track)
        } else {
            None
        };
        for a in s.referees.iter().chain(s.players.iter()) {
            self.agent(&mut c, a, holder == Some(a.id));
        }
        // ball only when it is free: small orange dot
        if matches!(s.ball.state, BallState::InFlight | BallState::Loose)
            && let Some(p) = s.ball.pos
        {
            let (x, y) = self.px(p);
            let r = 0.20 * self.k() * self.theme.px_per_m * self.theme.marker_scale;
            if s.ball.state == BallState::InFlight {
                c.disc(x + r * 0.5, y + r * 0.9, r * 0.9, 0.0, [0, 0, 0], 0.25); // soft shadow
            }
            c.disc(x, y, r + 1.0, 0.0, [30, 20, 0], 0.9);
            c.disc(x, y, r, 0.0, self.theme.ball, 1.0);
        }
        c
    }
}
