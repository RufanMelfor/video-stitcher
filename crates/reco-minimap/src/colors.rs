//! Dominant jersey colours via k-means in Lab space (from upstream `tools/colors.rs`,
//! without the goalkeeper picker).

use crate::team::rgb_to_lab;

pub fn hex(c: [u8; 3]) -> String {
    format!("#{:02x}{:02x}{:02x}", c[0], c[1], c[2])
}

pub fn parse_hex(s: &str) -> Option<[u8; 3]> {
    let s = s.trim().trim_start_matches('#');
    if s.len() != 6 {
        return None;
    }
    Some([
        u8::from_str_radix(&s[0..2], 16).ok()?,
        u8::from_str_radix(&s[2..4], 16).ok()?,
        u8::from_str_radix(&s[4..6], 16).ok()?,
    ])
}

#[derive(Clone, Debug)]
pub struct Cluster {
    pub rgb: [u8; 3],
    pub count: usize,
}

fn d2(a: [f32; 3], b: [f32; 3]) -> f32 {
    (a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2) + (a[2] - b[2]).powi(2)
}

/// k-means (k-means++ style deterministic seeding) over RGB samples; returns clusters by size.
pub fn kmeans(samples: &[[u8; 3]], k: usize) -> Vec<Cluster> {
    if samples.len() < k || k == 0 {
        return Vec::new();
    }
    let labs: Vec<[f32; 3]> = samples.iter().map(|&c| rgb_to_lab(c)).collect();
    let mut cent: Vec<[f32; 3]> = vec![labs[0]];
    while cent.len() < k {
        let far = (0..labs.len())
            .max_by(|&a, &b| {
                let da = cent
                    .iter()
                    .map(|c| d2(labs[a], *c))
                    .fold(f32::INFINITY, f32::min);
                let db = cent
                    .iter()
                    .map(|c| d2(labs[b], *c))
                    .fold(f32::INFINITY, f32::min);
                da.partial_cmp(&db).unwrap_or(std::cmp::Ordering::Equal)
            })
            .unwrap();
        cent.push(labs[far]);
    }
    let mut assign = vec![0usize; labs.len()];
    for _ in 0..25 {
        for (i, l) in labs.iter().enumerate() {
            assign[i] = (0..k)
                .min_by(|&a, &b| {
                    d2(*l, cent[a])
                        .partial_cmp(&d2(*l, cent[b]))
                        .unwrap_or(std::cmp::Ordering::Equal)
                })
                .unwrap();
        }
        let mut sum = vec![[0f32; 3]; k];
        let mut n = vec![0usize; k];
        for (i, l) in labs.iter().enumerate() {
            for c in 0..3 {
                sum[assign[i]][c] += l[c];
            }
            n[assign[i]] += 1;
        }
        for j in 0..k {
            if n[j] > 0 {
                for c in 0..3 {
                    cent[j][c] = sum[j][c] / n[j] as f32;
                }
            }
        }
    }
    // representative RGB = per-channel median of the members
    let mut out = Vec::new();
    for j in 0..k {
        let mut ch: [Vec<u8>; 3] = [vec![], vec![], vec![]];
        for (i, s) in samples.iter().enumerate() {
            if assign[i] == j {
                for c in 0..3 {
                    ch[c].push(s[c]);
                }
            }
        }
        if ch[0].is_empty() {
            continue;
        }
        let m = |v: &mut Vec<u8>| {
            v.sort_unstable();
            v[v.len() / 2]
        };
        out.push(Cluster {
            rgb: [m(&mut ch[0]), m(&mut ch[1]), m(&mut ch[2])],
            count: ch[0].len(),
        });
    }
    out.sort_by(|a, b| b.count.cmp(&a.count));
    out
}

/// Perceptual (Lab) distance between two RGB colours.
pub fn lab_dist(a: [u8; 3], b: [u8; 3]) -> f32 {
    d2(rgb_to_lab(a), rgb_to_lab(b)).sqrt()
}

/// Split the shirt-colour samples into two teams (2-means in Lab, so a team that appears in two
/// lighting variants stays one group). Returns the two groups (largest first) and their Lab distance.
pub fn pick_teams(samples: &[[u8; 3]]) -> Option<(Cluster, Cluster, f32)> {
    let c = kmeans(samples, 2);
    if c.len() < 2 {
        return None;
    }
    let d = lab_dist(c[0].rgb, c[1].rgb);
    Some((c[0].clone(), c[1].clone(), d))
}
