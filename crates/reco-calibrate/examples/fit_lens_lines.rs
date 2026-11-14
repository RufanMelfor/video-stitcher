//! Plumb-line lens fit: refine ONE camera's KB4 lens parameters from a single
//! frame by making straight world lines straight, independent of the other
//! camera (so stereo parallax plays no part).
//!
//! Idea: a straight 3D line seen through a pinhole-like camera produces rays
//! that all lie in one plane through the optical centre. With the wrong lens
//! model the rays of a straight edge are not coplanar. So:
//!
//! 1. Detect edges (blur, Sobel, non-max suppression, hysteresis) and trace
//!    them into pixel chains.
//! 2. Cut chains into windows and keep the nearly straight ones (chord close
//!    to path length) - candidate straight world lines.
//! 3. For a lens, turn every chain point into a unit ray and measure the
//!    plane-fit residual (smallest eigenvalue of the ray covariance) in pixel
//!    equivalents.
//! 4. Fit lens parameters (k1..k4 first, then fx, cx, cy) with Nelder-Mead on a
//!    trimmed mean of squared residuals (non-lines such as branches are
//!    ignored by the trimming).
//! 5. Validate on chains that were NOT used for the fit (two-fold split by
//!    chain id). A fit that only improves its own training chains is reported
//!    as such. Earlier photometric lens fits in this repo looked good on their
//!    own score and did not generalise, so this check is the point.
//!
//! Usage:
//! ```text
//! cargo run --release -p reco-calibrate --example fit_lens_lines -- \
//!   <frame.png> <setup.json> <camera 0|1> <output_dir>
//! ```
//!
//! Writes `lens_fit_cam<N>.json` (fitted lens), `setup_lensfit_cam<N>.json`
//! (the setup with that camera's lens replaced) and `lines_cam<N>.png` (chains
//! coloured by residual after the fit). Never deletes anything.

use argmin::core::{CostFunction, Error as ArgminError, Executor, State};
use argmin::solver::neldermead::NelderMead;
use rayon::prelude::*;
use std::path::Path;

/// KB4 fisheye lens at the frame's resolution.
#[derive(Clone, Copy, Debug)]
struct Lens {
    fx: f64,
    fy: f64,
    cx: f64,
    cy: f64,
    d: [f64; 4],
}

fn theta_d(t: f64, d: &[f64; 4]) -> f64 {
    let t2 = t * t;
    t * (1.0 + t2 * (d[0] + t2 * (d[1] + t2 * (d[2] + t2 * d[3]))))
}

fn theta_d_prime(t: f64, d: &[f64; 4]) -> f64 {
    let t2 = t * t;
    1.0 + t2 * (3.0 * d[0] + t2 * (5.0 * d[1] + t2 * (7.0 * d[2] + t2 * 9.0 * d[3])))
}

/// Pixel to unit ray in camera space (z forward).
fn pixel_to_ray(l: &Lens, u: f64, v: f64) -> [f64; 3] {
    let x = (u - l.cx) / l.fx;
    let y = (v - l.cy) / l.fy;
    let td = (x * x + y * y).sqrt();
    if td < 1e-12 {
        return [0.0, 0.0, 1.0];
    }
    let mut t = td;
    for _ in 0..10 {
        let f = theta_d(t, &l.d) - td;
        let fp = theta_d_prime(t, &l.d);
        if fp.abs() < 1e-9 {
            break;
        }
        t -= f / fp;
        t = t.clamp(0.0, 2.6);
    }
    let s = t.sin() / td;
    [x * s, y * s, t.cos()]
}

/// One candidate straight line: subsampled subpixel points of a chain window.
#[derive(Clone)]
struct Seg {
    chain: usize,
    pts: Vec<[f32; 2]>,
}

/// RMS distance of the rays from their best-fit plane through the origin, in
/// pixels at the reference focal length.
fn seg_rms(l: &Lens, pts: &[[f32; 2]], fref: f64) -> f64 {
    let mut m = [[0.0f64; 3]; 3];
    for p in pts {
        let r = pixel_to_ray(l, p[0] as f64, p[1] as f64);
        for i in 0..3 {
            for j in 0..3 {
                m[i][j] += r[i] * r[j];
            }
        }
    }
    let mat = nalgebra::Matrix3::new(
        m[0][0], m[0][1], m[0][2], m[1][0], m[1][1], m[1][2], m[2][0], m[2][1], m[2][2],
    );
    let ev = nalgebra::SymmetricEigen::new(mat).eigenvalues;
    let lmin = ev.iter().cloned().fold(f64::INFINITY, f64::min).max(0.0);
    (lmin / pts.len() as f64).sqrt() * fref
}

fn trimmed_cost(l: &Lens, segs: &[&Seg], fref: f64, keep: f64) -> f64 {
    let mut r: Vec<f64> = segs
        .par_iter()
        .map(|s| {
            let e = seg_rms(l, &s.pts, fref);
            e * e
        })
        .collect();
    if r.is_empty() {
        return 0.0;
    }
    r.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let n = ((r.len() as f64 * keep).ceil() as usize).clamp(1, r.len());
    r[..n].iter().sum::<f64>() / n as f64
}

/// Segments whose rays are further than this from a plane under the starting
/// profile are not treated as straight lines (branches, cars, curved kerbs).
const INIT_MAX_PX: f64 = 6.0;

/// Parameter vector (scaled): `[dk1, dk2, dk3, dk4, rel_f, dcx, dcy]`.
const SCALES: [f64; 7] = [0.02, 0.02, 0.02, 0.01, 0.01, 8.0, 8.0];

fn lens_from(base: &Lens, p: &[f64]) -> Lens {
    let g = |i: usize| p.get(i).copied().unwrap_or(0.0) * SCALES[i];
    Lens {
        fx: base.fx * (1.0 + g(4)),
        fy: base.fy * (1.0 + g(4)),
        cx: base.cx + g(5),
        cy: base.cy + g(6),
        d: [
            base.d[0] + g(0),
            base.d[1] + g(1),
            base.d[2] + g(2),
            base.d[3] + g(3),
        ],
    }
}

#[derive(Clone)]
struct FitCost<'a> {
    base: Lens,
    segs: Vec<&'a Seg>,
    fref: f64,
    keep: f64,
}

impl CostFunction for FitCost<'_> {
    type Param = Vec<f64>;
    type Output = f64;
    fn cost(&self, p: &Self::Param) -> Result<Self::Output, ArgminError> {
        let l = lens_from(&self.base, p);
        // Gentle pull towards the profile so a flat landscape stays put.
        let reg: f64 = p.iter().map(|v| v * v).sum::<f64>() * 0.002;
        Ok(trimmed_cost(&l, &self.segs, self.fref, self.keep) + reg)
    }
}

fn fit(base: &Lens, segs: &[&Seg], fref: f64, keep: f64, start: &[f64], n_free: usize) -> Vec<f64> {
    let mut x0 = start[..n_free].to_vec();
    x0.resize(n_free, 0.0);
    let mut simplex = vec![x0.clone()];
    for i in 0..n_free {
        let mut v = x0.clone();
        v[i] += 0.5;
        simplex.push(v);
    }
    let cost = FitCost {
        base: *base,
        segs: segs.to_vec(),
        fref,
        keep,
    };
    let solver: NelderMead<Vec<f64>, f64> = NelderMead::new(simplex)
        .with_sd_tolerance(1e-9)
        .expect("tolerance");
    let res = Executor::new(cost, solver)
        .configure(|s| s.max_iters(500))
        .run()
        .expect("nelder-mead");
    let mut best = res.state().get_best_param().unwrap().clone();
    best.resize(7, 0.0);
    best
}

// ---------------------------------------------------------------- edges ----

fn gaussian_blur(src: &[f32], w: usize, h: usize, sigma: f32) -> Vec<f32> {
    let rad = (sigma * 3.0).ceil() as i32;
    let mut k: Vec<f32> = (-rad..=rad)
        .map(|i| (-((i * i) as f32) / (2.0 * sigma * sigma)).exp())
        .collect();
    let s: f32 = k.iter().sum();
    k.iter_mut().for_each(|v| *v /= s);
    let mut tmp = vec![0f32; w * h];
    tmp.par_chunks_mut(w).enumerate().for_each(|(y, row)| {
        for (x, cell) in row.iter_mut().enumerate() {
            let mut a = 0.0;
            for (i, kv) in k.iter().enumerate() {
                let xx = (x as i32 + i as i32 - rad).clamp(0, w as i32 - 1) as usize;
                a += src[y * w + xx] * kv;
            }
            *cell = a;
        }
    });
    let mut out = vec![0f32; w * h];
    out.par_chunks_mut(w).enumerate().for_each(|(y, row)| {
        for (x, cell) in row.iter_mut().enumerate() {
            let mut a = 0.0;
            for (i, kv) in k.iter().enumerate() {
                let yy = (y as i32 + i as i32 - rad).clamp(0, h as i32 - 1) as usize;
                a += tmp[yy * w + x] * kv;
            }
            *cell = a;
        }
    });
    out
}

const DIRS: [(i32, i32); 8] = [
    (1, 0),
    (1, 1),
    (0, 1),
    (-1, 1),
    (-1, 0),
    (-1, -1),
    (0, -1),
    (1, -1),
];

struct Edges {
    w: usize,
    h: usize,
    mag: Vec<f32>,
    sector: Vec<u8>,
    edge: Vec<bool>,
}

fn detect_edges(luma: &[f32], w: usize, h: usize) -> Edges {
    let blur = gaussian_blur(luma, w, h, 1.6);
    let mut mag = vec![0f32; w * h];
    let mut sector = vec![0u8; w * h];
    mag.par_chunks_mut(w)
        .zip(sector.par_chunks_mut(w))
        .enumerate()
        .for_each(|(y, (mrow, srow))| {
            if y == 0 || y + 1 >= h {
                return;
            }
            for x in 1..w - 1 {
                let p = |dx: i32, dy: i32| {
                    blur[((y as i32 + dy) as usize) * w + (x as i32 + dx) as usize]
                };
                let gx =
                    (p(1, -1) + 2.0 * p(1, 0) + p(1, 1)) - (p(-1, -1) + 2.0 * p(-1, 0) + p(-1, 1));
                let gy =
                    (p(-1, 1) + 2.0 * p(0, 1) + p(1, 1)) - (p(-1, -1) + 2.0 * p(0, -1) + p(1, -1));
                mrow[x] = (gx * gx + gy * gy).sqrt();
                let ang = gy.atan2(gx);
                let s = (ang / std::f32::consts::PI * 4.0).round() as i32;
                srow[x] = s.rem_euclid(4) as u8;
            }
        });
    // Non-maximum suppression along the gradient direction.
    let nms_dirs = [(1i32, 0i32), (1, 1), (0, 1), (-1, 1)];
    let mut peak = vec![false; w * h];
    for y in 2..h - 2 {
        for x in 2..w - 2 {
            let i = y * w + x;
            let m = mag[i];
            if m <= 0.0 {
                continue;
            }
            let (dx, dy) = nms_dirs[sector[i] as usize];
            let a = mag[((y as i32 + dy) as usize) * w + (x as i32 + dx) as usize];
            let b = mag[((y as i32 - dy) as usize) * w + (x as i32 - dx) as usize];
            if m > a && m >= b {
                peak[i] = true;
            }
        }
    }
    // Hysteresis with percentile thresholds taken over the peaks.
    let mut vals: Vec<f32> = (0..w * h).filter(|&i| peak[i]).map(|i| mag[i]).collect();
    vals.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let hi = vals[(vals.len() as f64 * 0.90) as usize];
    let lo = vals[(vals.len() as f64 * 0.65) as usize];
    let mut edge = vec![false; w * h];
    let mut stack: Vec<usize> = (0..w * h).filter(|&i| peak[i] && mag[i] >= hi).collect();
    for &i in &stack {
        edge[i] = true;
    }
    while let Some(i) = stack.pop() {
        let (x, y) = ((i % w) as i32, (i / w) as i32);
        for (dx, dy) in DIRS {
            let (nx, ny) = (x + dx, y + dy);
            if nx < 2 || ny < 2 || nx >= w as i32 - 2 || ny >= h as i32 - 2 {
                continue;
            }
            let j = ny as usize * w + nx as usize;
            if peak[j] && !edge[j] && mag[j] >= lo {
                edge[j] = true;
                stack.push(j);
            }
        }
    }
    Edges {
        w,
        h,
        mag,
        sector,
        edge,
    }
}

fn subpixel(e: &Edges, i: usize) -> [f32; 2] {
    let (x, y) = ((i % e.w) as i32, (i / e.w) as i32);
    let nms_dirs = [(1i32, 0i32), (1, 1), (0, 1), (-1, 1)];
    let (dx, dy) = nms_dirs[e.sector[i] as usize];
    let at = |xx: i32, yy: i32| e.mag[yy as usize * e.w + xx as usize];
    let m0 = at(x, y);
    let mp = at(x + dx, y + dy);
    let mm = at(x - dx, y - dy);
    let den = mm - 2.0 * m0 + mp;
    let off = if den < -1e-6 {
        (0.5 * (mm - mp) / den).clamp(-0.5, 0.5)
    } else {
        0.0
    };
    [x as f32 + off * dx as f32, y as f32 + off * dy as f32]
}

/// Trace edge pixels into chains (8-connected, greedy straight-ahead).
fn trace_chains(e: &Edges) -> Vec<Vec<usize>> {
    let (w, h) = (e.w as i32, e.h as i32);
    let mut visited = vec![false; e.w * e.h];
    let mut chains = Vec::new();
    let neighbours = |i: usize, visited: &Vec<bool>| -> Vec<(usize, (i32, i32))> {
        let (x, y) = ((i % e.w) as i32, (i / e.w) as i32);
        let mut out = Vec::new();
        for (dx, dy) in DIRS {
            let (nx, ny) = (x + dx, y + dy);
            if nx < 0 || ny < 0 || nx >= w || ny >= h {
                continue;
            }
            let j = ny as usize * e.w + nx as usize;
            if e.edge[j] && !visited[j] {
                out.push((j, (dx, dy)));
            }
        }
        out
    };
    let extend = |start: usize, visited: &mut Vec<bool>| -> Vec<usize> {
        let mut out = Vec::new();
        let mut cur = start;
        let mut prev: Option<(i32, i32)> = None;
        loop {
            let nb = neighbours(cur, visited);
            if nb.is_empty() {
                break;
            }
            let pick = match prev {
                None => nb[0],
                Some((px, py)) => *nb
                    .iter()
                    .max_by(|a, b| {
                        let da = (a.1.0 * px + a.1.1 * py) as f32
                            / ((a.1.0 * a.1.0 + a.1.1 * a.1.1) as f32).sqrt();
                        let db = (b.1.0 * px + b.1.1 * py) as f32
                            / ((b.1.0 * b.1.0 + b.1.1 * b.1.1) as f32).sqrt();
                        da.partial_cmp(&db).unwrap()
                    })
                    .unwrap(),
            };
            visited[pick.0] = true;
            out.push(pick.0);
            prev = Some(pick.1);
            cur = pick.0;
        }
        out
    };
    for i in 0..e.w * e.h {
        if e.edge[i] && !visited[i] {
            visited[i] = true;
            let fwd = extend(i, &mut visited);
            let mut back = extend(i, &mut visited);
            back.reverse();
            let mut chain = back;
            chain.push(i);
            chain.extend(fwd);
            if chain.len() >= 120 {
                chains.push(chain);
            }
        }
    }
    chains
}

fn windows(e: &Edges, chains: &[Vec<usize>]) -> Vec<Seg> {
    const WIN: usize = 240;
    const MIN_WIN: usize = 160;
    const STEP: usize = 4;
    const MARGIN: f32 = 12.0;
    let mut segs = Vec::new();
    for (ci, ch) in chains.iter().enumerate() {
        let pts: Vec<[f32; 2]> = ch.iter().map(|&i| subpixel(e, i)).collect();
        let mut start = 0;
        while start + MIN_WIN <= pts.len() {
            let end = (start + WIN).min(pts.len());
            let win = &pts[start..end];
            start = end;
            if win.len() < MIN_WIN {
                continue;
            }
            let len: f32 = win
                .windows(2)
                .map(|p| ((p[1][0] - p[0][0]).powi(2) + (p[1][1] - p[0][1]).powi(2)).sqrt())
                .sum();
            let a = win[0];
            let b = win[win.len() - 1];
            let chord = ((b[0] - a[0]).powi(2) + (b[1] - a[1]).powi(2)).sqrt();
            // Raw-pixel straightness must NOT be strict: in a fisheye a true
            // straight line is curved in the image, and only radial lines
            // stay straight (those carry no distortion information). The
            // real selection is the ray-plane residual under the profile.
            if len <= 0.0 || chord / len < 0.75 {
                continue;
            }
            if win.iter().any(|p| {
                p[0] < MARGIN
                    || p[1] < MARGIN
                    || p[0] > e.w as f32 - MARGIN
                    || p[1] > e.h as f32 - MARGIN
            }) {
                continue;
            }
            segs.push(Seg {
                chain: ci,
                pts: win.iter().step_by(STEP).cloned().collect(),
            });
        }
    }
    segs
}

// ----------------------------------------------------------------- main ----

fn rms_stats(l: &Lens, segs: &[&Seg], fref: f64, keep: f64) -> (f64, f64) {
    let mut r: Vec<f64> = segs.par_iter().map(|s| seg_rms(l, &s.pts, fref)).collect();
    r.sort_by(|a, b| a.partial_cmp(b).unwrap());
    if r.is_empty() {
        return (0.0, 0.0);
    }
    let med = r[r.len() / 2];
    let n = ((r.len() as f64 * keep).ceil() as usize).clamp(1, r.len());
    let tm = (r[..n].iter().map(|v| v * v).sum::<f64>() / n as f64).sqrt();
    (med, tm)
}

fn describe(p: &[f64]) -> String {
    let g = |i: usize| p[i] * SCALES[i];
    format!(
        "dk=[{:+.4},{:+.4},{:+.4},{:+.4}] fx{:+.3}% dcx={:+.1}px dcy={:+.1}px",
        g(0),
        g(1),
        g(2),
        g(3),
        g(4) * 100.0,
        g(5),
        g(6)
    )
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 5 {
        eprintln!(
            "Usage: {} <frame.png> <setup.json> <camera 0|1> <output_dir>",
            args[0]
        );
        std::process::exit(1);
    }
    let cam: usize = args[3].parse().expect("camera must be 0 or 1");
    let out_dir = Path::new(&args[4]);
    std::fs::create_dir_all(out_dir).unwrap();

    let setup: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&args[2]).unwrap()).unwrap();
    let lj = &setup["lenses"][cam];

    let img = image::open(&args[1]).expect("cannot open frame").to_luma8();
    let (w, h) = (img.width() as usize, img.height() as usize);
    let sx = w as f64 / lj["width"].as_f64().unwrap();
    let sy = h as f64 / lj["height"].as_f64().unwrap();
    let dj = lj["distortion"].as_array().unwrap();
    let base = Lens {
        fx: lj["fx"].as_f64().unwrap() * sx,
        fy: lj["fy"].as_f64().unwrap() * sy,
        cx: lj["cx"].as_f64().unwrap() * sx,
        cy: lj["cy"].as_f64().unwrap() * sy,
        d: [
            dj[0].as_f64().unwrap(),
            dj[1].as_f64().unwrap(),
            dj[2].as_f64().unwrap(),
            dj[3].as_f64().unwrap(),
        ],
    };
    let fref = base.fx;
    println!(
        "frame {w}x{h}, profile fx={:.1} cx={:.1} cy={:.1} d={:?}",
        base.fx, base.cx, base.cy, base.d
    );

    let luma: Vec<f32> = img.as_raw().iter().map(|&v| v as f32).collect();
    let edges = detect_edges(&luma, w, h);
    println!("edge pixels: {}", edges.edge.iter().filter(|&&b| b).count());
    let chains = trace_chains(&edges);
    println!("chains >=120 px: {}", chains.len());
    let all = windows(&edges, &chains);
    println!("straight windows: {}", all.len());

    // Pre-filter wild segments under the profile.
    let init: Vec<&Seg> = all.iter().collect();
    let keepable: Vec<&Seg> = init
        .iter()
        .cloned()
        .filter(|s| seg_rms(&base, &s.pts, fref) < INIT_MAX_PX)
        .collect();
    println!(
        "kept after initial residual filter (<{INIT_MAX_PX} px): {}",
        keepable.len()
    );
    if keepable.len() < 60 {
        eprintln!("too few straight segments to fit reliably");
        std::process::exit(2);
    }

    let keep = 0.7;
    let zero = [0.0f64; 7];
    let (med0, tm0) = rms_stats(&base, &keepable, fref, keep);
    println!("profile: median {med0:.3} px, trimmed rms {tm0:.3} px (all kept segments)");

    // Two-fold split by chain id: fit on one half, score on the other.
    let fold = |f: usize| -> (Vec<&Seg>, Vec<&Seg>) {
        let tr: Vec<&Seg> = keepable
            .iter()
            .cloned()
            .filter(|s| s.chain % 2 == f)
            .collect();
        let va: Vec<&Seg> = keepable
            .iter()
            .cloned()
            .filter(|s| s.chain % 2 != f)
            .collect();
        (tr, va)
    };
    for (label, n_free) in [("k1-k4", 4usize), ("k1-k4+fx+c", 7usize)] {
        for f in 0..2 {
            let (tr, va) = fold(f);
            let p1 = fit(&base, &tr, fref, keep, &zero, n_free.min(4));
            let p = if n_free > 4 {
                fit(&base, &tr, fref, keep, &p1, n_free)
            } else {
                p1
            };
            let l = lens_from(&base, &p);
            let (_, tr0) = rms_stats(&base, &tr, fref, keep);
            let (_, tr1) = rms_stats(&l, &tr, fref, keep);
            let (_, va0) = rms_stats(&base, &va, fref, keep);
            let (_, va1) = rms_stats(&l, &va, fref, keep);
            println!(
                "[{label}] fold {f}: train {tr0:.3}->{tr1:.3} px  VALIDATION {va0:.3}->{va1:.3} px  | {}",
                describe(&p)
            );
        }
    }

    // Final fit on everything (k only, then all).
    let p1 = fit(&base, &keepable, fref, keep, &zero, 4);
    // `konly` keeps fx/cx/cy at the profile: straight lines pin the distortion
    // shape but only weakly the focal length (it trades against k1), and a
    // wrong per-camera focal length shows up as a seam mismatch.
    let k_only = args.get(5).map(|s| s == "konly").unwrap_or(false);
    let p = if k_only {
        p1.clone()
    } else {
        fit(&base, &keepable, fref, keep, &p1, 7)
    };
    let fitted = lens_from(&base, &p);
    let (med1, tm1) = rms_stats(&fitted, &keepable, fref, keep);
    println!(
        "FINAL (all segments): median {med0:.3}->{med1:.3} px, trimmed rms {tm0:.3}->{tm1:.3} px"
    );
    println!("FINAL params: {}", describe(&p));
    println!(
        "fitted: fx={:.2} fy={:.2} cx={:.2} cy={:.2} d={:?}",
        fitted.fx, fitted.fy, fitted.cx, fitted.cy, fitted.d
    );

    // Residual by radial band, before and after (does it help towards the edges?).
    let (ccx, ccy) = (base.cx, base.cy);
    let rmax = ((w as f64 / 2.0).powi(2) + (h as f64 / 2.0).powi(2)).sqrt();
    for b in 0..4 {
        let (lo, hi) = (b as f64 * 0.25, (b + 1) as f64 * 0.25);
        let band: Vec<&Seg> = keepable
            .iter()
            .cloned()
            .filter(|s| {
                let m = s
                    .pts
                    .iter()
                    .fold([0.0f64; 2], |a, p| [a[0] + p[0] as f64, a[1] + p[1] as f64]);
                let n = s.pts.len() as f64;
                let r = (((m[0] / n) - ccx).powi(2) + ((m[1] / n) - ccy).powi(2)).sqrt() / rmax;
                r >= lo && r < hi
            })
            .collect();
        let (_, a0) = rms_stats(&base, &band, fref, keep);
        let (_, a1) = rms_stats(&fitted, &band, fref, keep);
        println!(
            "radius {lo:.2}-{hi:.2}: {} segs, trimmed rms {a0:.3} -> {a1:.3} px",
            band.len()
        );
    }

    // Outputs.
    let to_dist = |l: &Lens| l.d.to_vec();
    let unscale = |l: &Lens| {
        serde_json::json!({
            "width": lj["width"], "height": lj["height"],
            "fx": l.fx / sx, "fy": l.fy / sy, "cx": l.cx / sx, "cy": l.cy / sy,
            "distortion": to_dist(l), "correction": lj["correction"],
        })
    };
    std::fs::write(
        out_dir.join(format!("lens_fit_cam{cam}.json")),
        serde_json::to_string_pretty(&unscale(&fitted)).unwrap(),
    )
    .unwrap();
    let mut new_setup = setup.clone();
    new_setup["lenses"][cam] = unscale(&fitted);
    std::fs::write(
        out_dir.join(format!("setup_lensfit_cam{cam}.json")),
        serde_json::to_string_pretty(&new_setup).unwrap(),
    )
    .unwrap();

    // Overlay.
    let mut rgb = image::RgbImage::new(w as u32, h as u32);
    for (x, y, px) in img.enumerate_pixels() {
        let v = (px[0] as f32 * 0.6) as u8;
        rgb.put_pixel(x, y, image::Rgb([v, v, v]));
    }
    for s in &keepable {
        let e = seg_rms(&fitted, &s.pts, fref);
        let col = if e < 1.5 {
            image::Rgb([60, 255, 60])
        } else if e < 3.0 {
            image::Rgb([255, 230, 40])
        } else {
            image::Rgb([255, 50, 50])
        };
        for p in &s.pts {
            for dy in -2i32..=2 {
                for dx in -2i32..=2 {
                    let (x, y) = (p[0] as i32 + dx, p[1] as i32 + dy);
                    if x >= 0 && y >= 0 && (x as usize) < w && (y as usize) < h {
                        rgb.put_pixel(x as u32, y as u32, col);
                    }
                }
            }
        }
    }
    let small = image::imageops::resize(
        &rgb,
        1600,
        (1600.0 * h as f64 / w as f64) as u32,
        image::imageops::FilterType::Triangle,
    );
    small
        .save(out_dir.join(format!("lines_cam{cam}.png")))
        .unwrap();
    println!("wrote outputs to {}", out_dir.display());
}
