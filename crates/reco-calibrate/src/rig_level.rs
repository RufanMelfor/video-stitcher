//! Rig level (tilt and roll) from the scene: each camera's direction of
//! true "up", measured from vertical structures in its image (light
//! masts, goal posts, fence posts, building edges, standing people).
//!
//! The relative pose of the two cameras (what calibration fits) is the
//! same however the rig leans, so the lean has to come from the scene.
//! Vertical lines in the world all meet at one vanishing point, the
//! direction of up: for a straight image edge, the plane through the
//! camera centre and the edge has normal `n`, and up `u` satisfies
//! `n . u = 0`. Per camera:
//!
//! 1. Edge pixels with a strong, coherent orientation (structure tensor)
//!    that run roughly vertically in the image.
//! 2. Each one's local direction is undistorted through the lens (KB4,
//!    `reco_core::lens::distorted_to_undistorted`) into the normal `n` of
//!    its plane - so the fisheye curvature does not matter.
//! 3. Hough voting for `u` (parametrized as `(a, -1, b)`, a gnomonic chart
//!    around image-up) finds the direction most edge planes contain;
//!    least squares over the inliers refines it.
//!
//! `estimate_framing` turns the two up directions into the setup's
//! `Framing::tilt` / `roll` and the physical rig lean
//! (`reco_core::coverage`).

use nalgebra::{Matrix3, SymmetricEigen, Vector3};
use reco_core::calibration::{Calibration, Lens};
use reco_core::coverage::{RigLevel, framing_from_camera_up, physical_rig_level};
use reco_core::source::YuvFrame;

/// Frames are box-downscaled to at most this width before edge detection.
const WORK_WIDTH: u32 = 1280;
/// Structure-tensor window half-size (pixels, working resolution).
const TENSOR_RADIUS: usize = 2;
/// Minimum orientation coherence `((l1 - l2) / (l1 + l2))^2`: straight
/// edges, not grass or foliage texture.
const MIN_COHERENCE: f32 = 0.8;
/// Keep the strongest this fraction of coherent edge pixels.
const STRONG_FRACTION: f32 = 0.10;
/// Edge must run within this angle of image-vertical (verticals of a
/// tilted wide-angle camera lean a lot towards the image sides).
const MAX_LEAN_FROM_VERTICAL_DEG: f32 = 50.0;
/// Half-length (working pixels) of the edge segment undistorted per pixel.
const SEGMENT_HALF: f32 = 3.0;
/// Ignore pixels this far off-axis (KB4 profiles are least reliable at
/// the image corners, and rays near 90 deg are ill-conditioned).
const MAX_OFF_AXIS_DEG: f64 = 75.0;
/// Hough chart: `a` (sideways) and `b` (forward) ranges and cell size.
const A_RANGE: f64 = 0.6;
const B_MIN: f64 = -1.0;
const B_MAX: f64 = 0.6;
const CELL: f64 = 0.0025;
/// Inlier thresholds (degrees) for the two refinement passes.
const REFINE_THRESHOLDS_DEG: [f64; 2] = [1.0, 0.5];
/// Below this many inlier edge pixels a camera's up is not trusted.
pub const MIN_INLIERS: usize = 400;

/// Measured direction of true up for one camera.
#[derive(Debug, Clone, Copy)]
pub struct CameraUp {
    /// Unit vector in the camera frame: x = image right, y = image down,
    /// z = viewing direction.
    pub up: Vector3<f64>,
    /// Edge pixels consistent with it.
    pub inliers: usize,
    /// Edge pixels that voted.
    pub candidates: usize,
    /// RMS angle (degrees) between the inlier edge planes and `up`.
    pub residual_deg: f64,
}

impl CameraUp {
    /// The camera's own pitch (degrees, negative = looking down) and roll
    /// (degrees, positive = up leans to the image right).
    pub fn pitch_roll_deg(&self) -> (f64, f64) {
        (
            self.up.z.asin().to_degrees(),
            self.up.x.atan2(-self.up.y).to_degrees(),
        )
    }
}

/// Box-downscale the luma plane by an integer factor.
fn downscale(frame: &YuvFrame, factor: usize) -> (Vec<f32>, usize, usize) {
    let (w, h) = (
        frame.width as usize / factor,
        frame.height as usize / factor,
    );
    let src_w = frame.width as usize;
    let mut out = vec![0.0f32; w * h];
    let norm = 1.0 / (factor * factor) as f32;
    for y in 0..h {
        for x in 0..w {
            let mut sum = 0u32;
            for dy in 0..factor {
                let row = (y * factor + dy) * src_w + x * factor;
                for dx in 0..factor {
                    sum += u32::from(frame.y[row + dx]);
                }
            }
            out[y * w + x] = sum as f32 * norm;
        }
    }
    (out, w, h)
}

/// Box filter of radius `r` via an integral image.
fn box_blur(src: &[f32], w: usize, h: usize, r: usize) -> Vec<f32> {
    let mut integral = vec![0.0f64; (w + 1) * (h + 1)];
    for y in 0..h {
        let mut row = 0.0f64;
        for x in 0..w {
            row += f64::from(src[y * w + x]);
            integral[(y + 1) * (w + 1) + x + 1] = integral[y * (w + 1) + x + 1] + row;
        }
    }
    let mut out = vec![0.0f32; w * h];
    for y in 0..h {
        let (y0, y1) = (y.saturating_sub(r), (y + r + 1).min(h));
        for x in 0..w {
            let (x0, x1) = (x.saturating_sub(r), (x + r + 1).min(w));
            let s = integral[y1 * (w + 1) + x1]
                - integral[y0 * (w + 1) + x1]
                - integral[y1 * (w + 1) + x0]
                + integral[y0 * (w + 1) + x0];
            out[y * w + x] = (s / ((y1 - y0) * (x1 - x0)) as f64) as f32;
        }
    }
    out
}

/// Edge-plane normals `n` (camera frame) of the near-vertical straight
/// edge pixels of one frame.
fn edge_normals(frame: &YuvFrame, lens: &Lens) -> Vec<Vector3<f64>> {
    let factor = (frame.width.div_ceil(WORK_WIDTH)).max(1) as usize;
    let (img, w, h) = downscale(frame, factor);
    if w < 8 || h < 8 {
        return Vec::new();
    }
    // Sobel gradients.
    let mut gxx = vec![0.0f32; w * h];
    let mut gxy = vec![0.0f32; w * h];
    let mut gyy = vec![0.0f32; w * h];
    for y in 1..h - 1 {
        for x in 1..w - 1 {
            let p = |dx: isize, dy: isize| {
                img[(y as isize + dy) as usize * w + (x as isize + dx) as usize]
            };
            let gx = (p(1, -1) + 2.0 * p(1, 0) + p(1, 1)) - (p(-1, -1) + 2.0 * p(-1, 0) + p(-1, 1));
            let gy = (p(-1, 1) + 2.0 * p(0, 1) + p(1, 1)) - (p(-1, -1) + 2.0 * p(0, -1) + p(1, -1));
            gxx[y * w + x] = gx * gx;
            gxy[y * w + x] = gx * gy;
            gyy[y * w + x] = gy * gy;
        }
    }
    let (jxx, jxy, jyy) = (
        box_blur(&gxx, w, h, TENSOR_RADIUS),
        box_blur(&gxy, w, h, TENSOR_RADIUS),
        box_blur(&gyy, w, h, TENSOR_RADIUS),
    );

    // Coherent pixels with their strength and edge direction.
    let min_dir_y = MAX_LEAN_FROM_VERTICAL_DEG.to_radians().cos();
    let mut coherent: Vec<(f32, usize, (f32, f32))> = Vec::new();
    let margin = TENSOR_RADIUS + 2;
    for y in margin..h - margin {
        for x in margin..w - margin {
            let i = y * w + x;
            let (a, b, c) = (jxx[i], jxy[i], jyy[i]);
            let trace = a + c;
            if trace <= 1e-3 {
                continue;
            }
            let diff = ((a - c) * (a - c) + 4.0 * b * b).sqrt();
            let coherence = (diff / trace).powi(2);
            if coherence < MIN_COHERENCE {
                continue;
            }
            // Gradient orientation; the edge runs perpendicular to it.
            let phi = 0.5 * (2.0 * b).atan2(a - c);
            let (ex, ey) = (-phi.sin(), phi.cos());
            if ey.abs() < min_dir_y {
                continue;
            }
            coherent.push(((trace + diff) * 0.5, i, (ex, ey)));
        }
    }
    if coherent.is_empty() {
        return Vec::new();
    }
    let keep = ((coherent.len() as f32 * STRONG_FRACTION) as usize).max(1);
    coherent.select_nth_unstable_by(keep - 1, |p, q| q.0.total_cmp(&p.0));
    coherent.truncate(keep);

    // Undistort each pixel's short edge segment into its plane normal.
    let (fw, fh) = (frame.width, frame.height);
    let sx = f64::from(fw) / f64::from(lens.width);
    let sy = f64::from(fh) / f64::from(lens.height);
    // `distorted_to_undistorted`'s output intrinsics (plane-fitted FOV).
    let (out_fx, out_fy) = (lens.fx * sx / 2.0, lens.fy * sy / 2.0);
    let (out_cx, out_cy) = (
        (f64::from(fw) + 2.0 * lens.cx * sx) / 4.0,
        (f64::from(fh) + 2.0 * lens.cy * sy) / 4.0,
    );
    let max_tan = MAX_OFF_AXIS_DEG.to_radians().tan();
    let ray = |px: f64, py: f64| -> Option<Vector3<f64>> {
        let (ux, uy) = reco_core::lens::distorted_to_undistorted(px, py, fw, fh, lens)?;
        let v = Vector3::new((ux - out_cx) / out_fx, (uy - out_cy) / out_fy, 1.0);
        (v.x.hypot(v.y) <= max_tan).then_some(v)
    };
    let f = factor as f64;
    coherent
        .iter()
        .filter_map(|&(_, i, (ex, ey))| {
            let (x, y) = ((i % w) as f64, (i / w) as f64);
            let d = f64::from(SEGMENT_HALF);
            let to_full = |v: f64| (v + 0.5) * f;
            let r1 = ray(
                to_full(x - d * f64::from(ex)),
                to_full(y - d * f64::from(ey)),
            )?;
            let r2 = ray(
                to_full(x + d * f64::from(ex)),
                to_full(y + d * f64::from(ey)),
            )?;
            r1.cross(&r2).try_normalize(1e-12)
        })
        .collect()
}

/// Up direction most edge planes contain: Hough vote, then least squares.
fn vote_up(normals: &[Vector3<f64>]) -> Option<CameraUp> {
    let na = (2.0 * A_RANGE / CELL).round() as usize + 1;
    let nb = ((B_MAX - B_MIN) / CELL).round() as usize + 1;
    let mut acc = vec![0u32; na * nb];
    for n in normals {
        // n . (a, -1, b) = 0  ->  n.x a + n.z b = n.y
        if n.z.abs() >= n.x.abs() {
            for ia in 0..na {
                let a = -A_RANGE + ia as f64 * CELL;
                let b = (n.y - n.x * a) / n.z;
                let ib = ((b - B_MIN) / CELL).round();
                if ib >= 0.0 && (ib as usize) < nb {
                    acc[ib as usize * na + ia] += 1;
                }
            }
        } else {
            for ib in 0..nb {
                let b = B_MIN + ib as f64 * CELL;
                let a = (n.y - n.z * b) / n.x;
                let ia = ((a + A_RANGE) / CELL).round();
                if ia >= 0.0 && (ia as usize) < na {
                    acc[ib * na + ia as usize] += 1;
                }
            }
        }
    }
    // Peak of the 3x3-summed accumulator.
    let mut best = (0u32, 0usize, 0usize);
    for ib in 1..nb - 1 {
        for ia in 1..na - 1 {
            let mut s = 0;
            for db in 0..3 {
                for da in 0..3 {
                    s += acc[(ib + db - 1) * na + ia + da - 1];
                }
            }
            if s > best.0 {
                best = (s, ia, ib);
            }
        }
    }
    if best.0 == 0 {
        return None;
    }
    let mut up = Vector3::new(
        -A_RANGE + best.1 as f64 * CELL,
        -1.0,
        B_MIN + best.2 as f64 * CELL,
    )
    .normalize();

    let mut inliers: Vec<&Vector3<f64>> = Vec::new();
    for thr in REFINE_THRESHOLDS_DEG {
        let s = thr.to_radians().sin();
        inliers = normals.iter().filter(|n| n.dot(&up).abs() < s).collect();
        if inliers.len() < 3 {
            return None;
        }
        let m: Matrix3<f64> = inliers.iter().map(|n| *n * n.transpose()).sum();
        let eig = SymmetricEigen::new(m);
        let (k, _) = eig
            .eigenvalues
            .iter()
            .enumerate()
            .min_by(|p, q| p.1.total_cmp(q.1))?;
        let v = eig.eigenvectors.column(k).into_owned();
        up = if v.y > 0.0 { -v } else { v };
    }
    let residual = (inliers
        .iter()
        .map(|n| n.dot(&up).clamp(-1.0, 1.0).asin().powi(2))
        .sum::<f64>()
        / inliers.len() as f64)
        .sqrt()
        .to_degrees();
    Some(CameraUp {
        up,
        inliers: inliers.len(),
        candidates: normals.len(),
        residual_deg: residual,
    })
}

/// Measure one camera's up direction over several frames of it.
pub fn camera_up(frames: &[&YuvFrame], lens: &Lens) -> Option<CameraUp> {
    let normals: Vec<Vector3<f64>> = frames.iter().flat_map(|f| edge_normals(f, lens)).collect();
    vote_up(&normals)
}

/// Rig level measured from the scene.
#[derive(Debug, Clone, Copy)]
pub struct LevelEstimate {
    /// `Framing::tilt` for the setup (radians).
    pub framing_tilt: f64,
    /// `Framing::roll` for the setup (radians).
    pub framing_roll: f64,
    /// The physical rig lean those stand for.
    pub physical: Option<RigLevel>,
    /// Left camera's measurement.
    pub left: CameraUp,
    /// Right camera's measurement.
    pub right: CameraUp,
}

/// Measure the rig level on `pairs` (left, right frames) for setup `cal`
/// (its lenses and alignment). `None` when either camera has fewer than
/// [`MIN_INLIERS`] consistent vertical edges.
pub fn estimate_framing(
    cal: &Calibration,
    pairs: &[(YuvFrame, YuvFrame)],
) -> Option<LevelEstimate> {
    let (left_lens, right_lens) = (cal.lenses.first()?, cal.lenses.get(1)?);
    let lefts: Vec<&YuvFrame> = pairs.iter().map(|(l, _)| l).collect();
    let rights: Vec<&YuvFrame> = pairs.iter().map(|(_, r)| r).collect();
    let left = camera_up(&lefts, left_lens)?;
    let right = camera_up(&rights, right_lens)?;
    log::info!(
        "rig level: left up {:?} ({} / {} edges, {:.2} deg rms), right up {:?} ({} / {} edges, {:.2} deg rms)",
        left.up.as_slice(),
        left.inliers,
        left.candidates,
        left.residual_deg,
        right.up.as_slice(),
        right.inliers,
        right.candidates,
        right.residual_deg
    );
    if left.inliers < MIN_INLIERS || right.inliers < MIN_INLIERS {
        log::warn!("rig level: too few vertical edges, not trusted");
        return None;
    }
    let (framing_tilt, framing_roll) = framing_from_camera_up(cal, left.up, right.up)?;
    let mut leveled = cal.clone();
    leveled.framing.tilt = framing_tilt;
    leveled.framing.roll = framing_roll;
    Some(LevelEstimate {
        framing_tilt,
        framing_roll,
        physical: physical_rig_level(&leveled),
        left,
        right,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Synthetic edge normals: planes through the camera centre and
    /// lines parallel to `up`, at assorted positions.
    fn normals_for(up: Vector3<f64>, n: usize) -> Vec<Vector3<f64>> {
        (0..n)
            .map(|i| {
                let t = i as f64 * 0.37;
                let p = Vector3::new(t.sin() * 2.0, t.cos() * 0.5, 3.0 + (t * 1.3).cos());
                p.cross(&up).normalize()
            })
            .collect()
    }

    #[test]
    fn votes_recover_a_tilted_up() {
        let up = Vector3::new(0.05, -0.92, -0.36).normalize();
        let mut normals = normals_for(up, 2000);
        // Clutter: planes for random non-vertical lines.
        normals.extend(normals_for(Vector3::new(1.0, 0.1, 0.2).normalize(), 600));
        let est = vote_up(&normals).expect("estimate");
        assert!(
            est.up.angle(&up).to_degrees() < 0.05,
            "{:?} vs {up:?}",
            est.up
        );
        assert!(est.inliers >= 2000);
        let (pitch, roll) = est.pitch_roll_deg();
        assert!(
            pitch < -15.0 && roll.abs() < 5.0,
            "pitch {pitch} roll {roll}"
        );
    }

    #[test]
    fn no_edges_no_estimate() {
        assert!(vote_up(&[]).is_none());
    }

    #[test]
    fn box_blur_of_constant_is_constant() {
        let out = box_blur(&[2.0; 30], 6, 5, 2);
        assert!(out.iter().all(|v| (v - 2.0).abs() < 1e-6));
    }
}
