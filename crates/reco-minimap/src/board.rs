//! Tactics-board scene: one moment of a game as plain data, plus a vector (SVG) drawing of it.
//!
//! The scene is what a board app needs to start from: field lines (so no sport profile is needed on the
//! other side), players with team, number, keeper flag and position, an optional trail of the last seconds,
//! referees and the ball. All positions are metres: x along the field (left to right), y across (far to near
//! sideline), origin at the top-left corner of the field.

use crate::geom::{Field, Vec2};
use crate::team::Team;

pub const FORMAT: &str = "reco-minimap-board";
pub const VERSION: u32 = 1;

#[derive(Clone, Debug, PartialEq)]
pub struct BoardPlayer {
    pub id: u32,
    pub team: Team,
    pub number: Option<u8>,
    pub keeper: bool,
    pub pos: Vec2,
    /// Earlier positions, oldest first, ending just before `pos`.
    pub trail: Vec<Vec2>,
}

/// A coach's drawing on the board: an arrow (first to last point) or a freehand line.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum MarkKind {
    Arrow,
    Pen,
}

#[derive(Clone, Debug, PartialEq)]
pub struct BoardMark {
    pub kind: MarkKind,
    pub color: [u8; 3],
    pub points: Vec<Vec2>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct BoardBall {
    /// `held`, `flight`, `loose` or `unknown`.
    pub state: String,
    pub holder: Option<u32>,
    pub pos: Option<Vec2>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct BoardScene {
    pub sport: String,
    pub field: Field,
    /// Video time of the moment in seconds.
    pub t: f64,
    /// Drawing colours of the two teams and the stripe colour of their goalkeepers (`None` = black or white by contrast).
    pub home: [u8; 3],
    pub away: [u8; 3],
    pub home_keeper: Option<[u8; 3]>,
    pub away_keeper: Option<[u8; 3]>,
    pub players: Vec<BoardPlayer>,
    pub referees: Vec<Vec2>,
    pub ball: BoardBall,
    /// Arrows and freehand lines drawn on this moment.
    pub marks: Vec<BoardMark>,
    /// Field markings as polylines.
    pub lines: Vec<Vec<Vec2>>,
    /// Goal / basket centres.
    pub goals: Vec<Vec2>,
}

#[derive(Clone, Debug)]
pub struct SvgStyle {
    /// Width of the drawing in pixels (height follows); only a default size, the SVG scales freely.
    pub width_px: f32,
    pub pad_m: f32,
    pub floor: [u8; 3],
    pub lines: [u8; 3],
    pub numbers: bool,
    pub trails: bool,
    pub stripes: bool,
}

impl Default for SvgStyle {
    fn default() -> Self {
        Self {
            width_px: 900.0,
            pad_m: 1.5,
            floor: [38, 44, 52],
            lines: [225, 228, 232],
            numbers: true,
            trails: true,
            stripes: true,
        }
    }
}

fn hex(c: [u8; 3]) -> String {
    format!("#{:02x}{:02x}{:02x}", c[0], c[1], c[2])
}

fn lum(c: [u8; 3]) -> f32 {
    0.2126 * c[0] as f32 + 0.7152 * c[1] as f32 + 0.0722 * c[2] as f32
}

fn contrast(c: [u8; 3], limit: f32) -> [u8; 3] {
    if lum(c) < limit {
        [255, 255, 255]
    } else {
        [15, 15, 15]
    }
}

fn n(v: f32) -> String {
    let s = format!("{:.2}", v);
    let s = s.trim_end_matches('0').trim_end_matches('.');
    if s == "-0" || s.is_empty() {
        "0".into()
    } else {
        s.into()
    }
}

/// Draw the scene as a standalone SVG (viewBox in metres). Elements carry classes and `data-*` attributes
/// (`player home keeper`, `data-id`, `data-number`) so a web board can pick them up.
pub fn to_svg(s: &BoardScene, st: &SvgStyle) -> String {
    let pad = st.pad_m;
    let (vw, vh) = (s.field.length + 2.0 * pad, s.field.width + 2.0 * pad);
    let (w, h) = (st.width_px, st.width_px * vh / vw);
    let r = 0.45f32;
    let mut o = String::new();
    o.push_str(&format!(
        "<svg xmlns=\"http://www.w3.org/2000/svg\" viewBox=\"{} {} {} {}\" width=\"{}\" height=\"{}\" data-format=\"{}\" data-version=\"{}\" data-sport=\"{}\" data-t=\"{}\">\n",
        n(-pad), n(-pad), n(vw), n(vh), n(w), n(h), FORMAT, VERSION, s.sport.replace(|c: char| !(c.is_ascii_alphanumeric() || c == '_' || c == '-'), ""), n(s.t as f32)
    ));
    // keeper stripes: a diagonal two-colour pattern laid over the team colour
    let kp = |id: &str, team: [u8; 3], own: Option<[u8; 3]>| {
        let c = own.unwrap_or_else(|| contrast(team, 140.0));
        format!(
            "<pattern id=\"{id}\" patternUnits=\"userSpaceOnUse\" width=\"0.38\" height=\"0.38\" patternTransform=\"rotate(45)\"><rect width=\"0.19\" height=\"0.38\" fill=\"{}\"/></pattern>",
            hex(c)
        )
    };
    o.push_str("<defs>");
    o.push_str(&kp("stripes-home", s.home, s.home_keeper));
    o.push_str(&kp("stripes-away", s.away, s.away_keeper));
    o.push_str("</defs>\n");
    o.push_str(&format!(
        "<rect id=\"floor\" x=\"{}\" y=\"{}\" width=\"{}\" height=\"{}\" fill=\"{}\"/>\n",
        n(-pad),
        n(-pad),
        n(vw),
        n(vh),
        hex(st.floor)
    ));
    o.push_str(&format!("<g id=\"court\" fill=\"none\" stroke=\"{}\" stroke-width=\"0.08\" stroke-linecap=\"round\" stroke-linejoin=\"round\">\n", hex(st.lines)));
    for pl in &s.lines {
        let pts: Vec<String> = pl
            .iter()
            .map(|p| format!("{},{}", n(p.x), n(p.y)))
            .collect();
        o.push_str(&format!("<polyline points=\"{}\"/>\n", pts.join(" ")));
    }
    o.push_str("</g>\n");
    if st.trails {
        o.push_str("<g id=\"trails\" fill=\"none\" stroke-width=\"0.12\" stroke-linecap=\"round\" stroke-linejoin=\"round\" opacity=\"0.55\">\n");
        for p in s.players.iter().filter(|p| !p.trail.is_empty()) {
            let colour = if p.team == Team::Home { s.home } else { s.away };
            let pts: Vec<String> = p
                .trail
                .iter()
                .chain(std::iter::once(&p.pos))
                .map(|q| format!("{},{}", n(q.x), n(q.y)))
                .collect();
            o.push_str(&format!(
                "<polyline class=\"trail {}\" data-id=\"{}\" stroke=\"{}\" points=\"{}\"/>\n",
                team_class(p.team),
                p.id,
                hex(colour),
                pts.join(" ")
            ));
        }
        o.push_str("</g>\n");
    }
    o.push_str(
        "<g id=\"marks\" fill=\"none\" stroke-linecap=\"round\" stroke-linejoin=\"round\">\n",
    );
    for m in &s.marks {
        if m.points.len() < 2 {
            continue;
        }
        let pts: Vec<String> = m
            .points
            .iter()
            .map(|q| format!("{},{}", n(q.x), n(q.y)))
            .collect();
        let (kind, colour) = (
            if m.kind == MarkKind::Arrow {
                "arrow"
            } else {
                "pen"
            },
            hex(m.color),
        );
        if m.kind == MarkKind::Arrow {
            let (a, b) = (m.points[m.points.len() - 2], m.points[m.points.len() - 1]);
            let (dx, dy) = (b.x - a.x, b.y - a.y);
            let l = (dx * dx + dy * dy).sqrt().max(1e-3);
            let (ux, uy) = (dx / l, dy / l);
            let head = 0.9f32.min(l * 0.6);
            let (bx, by) = (b.x - ux * head, b.y - uy * head);
            let (hx, hy) = (-uy * head * 0.5, ux * head * 0.5);
            o.push_str(&format!("<g class=\"mark arrow\" stroke=\"{colour}\"><polyline stroke-width=\"0.2\" points=\"{} {},{}\"/><polygon fill=\"{colour}\" stroke=\"none\" points=\"{},{} {},{} {},{}\"/></g>\n",
                m.points[..m.points.len() - 1].iter().map(|q| format!("{},{}", n(q.x), n(q.y))).collect::<Vec<_>>().join(" "), n(bx), n(by),
                n(b.x), n(b.y), n(bx + hx), n(by + hy), n(bx - hx), n(by - hy)));
        } else {
            o.push_str(&format!("<polyline class=\"mark {kind}\" stroke=\"{colour}\" stroke-width=\"0.2\" points=\"{}\"/>\n", pts.join(" ")));
        }
    }
    o.push_str("</g>\n<g id=\"referees\">\n");
    for p in &s.referees {
        o.push_str(&format!("<circle class=\"referee\" cx=\"{}\" cy=\"{}\" r=\"{}\" fill=\"#000\" stroke=\"#ebebeb\" stroke-width=\"0.05\"/>\n", n(p.x), n(p.y), n(r)));
    }
    o.push_str("</g>\n<g id=\"players\">\n");
    for p in &s.players {
        let fill = if p.team == Team::Home { s.home } else { s.away };
        let edge = if lum(fill) < 90.0 {
            "#ebebeb"
        } else {
            "#0f0f0f"
        };
        let team = team_class(p.team);
        let number = p
            .number
            .map(|v| format!(" data-number=\"{v}\""))
            .unwrap_or_default();
        let keeper = if p.keeper { " keeper" } else { "" };
        o.push_str(&format!("<g class=\"player {team}{keeper}\" data-id=\"{}\"{number} transform=\"translate({} {})\">", p.id, n(p.pos.x), n(p.pos.y)));
        if s.ball.holder == Some(p.id) && s.ball.state == "held" {
            o.push_str(&format!("<circle class=\"holder\" r=\"{}\" fill=\"none\" stroke=\"#ff8c00\" stroke-width=\"0.13\"/>", n(r * 1.75)));
        }
        o.push_str(&format!(
            "<circle r=\"{}\" fill=\"{}\" stroke=\"{}\" stroke-width=\"0.06\"/>",
            n(r),
            hex(fill),
            edge
        ));
        let plate = p.keeper && st.stripes;
        if plate {
            o.push_str(&format!(
                "<circle r=\"{}\" fill=\"url(#stripes-{team})\"/>",
                n(r)
            ));
        }
        if st.numbers
            && let Some(v) = p.number
        {
            if plate {
                o.push_str(&format!(
                    "<circle r=\"{}\" fill=\"{}\"/>",
                    n(r * 0.66),
                    hex(fill)
                ));
            }
            let ink = hex(contrast(fill, 140.0));
            let size = if v >= 10 { 0.52 } else { 0.62 };
            o.push_str(&format!("<text text-anchor=\"middle\" dominant-baseline=\"central\" font-family=\"-apple-system, Helvetica, Arial, sans-serif\" font-weight=\"700\" font-size=\"{}\" fill=\"{}\">{v}</text>", n(size), ink));
        }
        o.push_str("</g>\n");
    }
    o.push_str("</g>\n");
    if matches!(s.ball.state.as_str(), "flight" | "loose")
        && let Some(p) = s.ball.pos
    {
        o.push_str(&format!("<circle id=\"ball\" cx=\"{}\" cy=\"{}\" r=\"0.2\" fill=\"#ff8c00\" stroke=\"#1e1400\" stroke-width=\"0.05\"/>\n", n(p.x), n(p.y)));
    }
    o.push_str("</svg>\n");
    o
}

fn team_class(t: Team) -> &'static str {
    if t == Team::Home { "home" } else { "away" }
}
