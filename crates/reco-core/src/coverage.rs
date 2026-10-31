//! Angular coverage of a setup: each lens's field of view, and how many
//! degrees of the scene both cameras see.
//!
//! Pure math on a [`Calibration`] - no GPU, no I/O.
//!
//! - [`lens_fov`]: horizontal / vertical / diagonal field of view of one
//!   lens from its KB4 intrinsics (the physical lens, independent of the
//!   rig).
//! - [`horizon_coverage`]: sweeps viewing directions along the horizon
//!   from the stitch's virtual eye and tests, per direction, whether it
//!   lands inside each camera's real image - the same plane intersection
//!   and KB4 mapping `fisheye.wgsl` uses. The angle where both cameras
//!   have image is the overlap.
//!
//! The stitch models both cameras as seen from one virtual eye, while the
//! real cameras sit a few centimetres apart, so the rig angles are a
//! close guide (for comparing mounts, or "is there enough overlap"), not a
//! survey-grade measurement. The horizon row is outside the
//! `ground_tilt` / `top_tilt` bands, so those corrections do not change
//! the result.

use nalgebra::{Matrix4, Vector3, Vector4};

use crate::calibration::{Calibration, Lens};
use crate::geometry::VirtualCamera;
use crate::lens::kb4;
use crate::render::scene::SceneGeometry;

/// Angular step of the [`horizon_coverage`] sweep, in degrees.
pub const SWEEP_STEP_DEG: f64 = 0.05;
/// Half-range of the sweep either side of the view centre, in degrees.
const SWEEP_HALF_RANGE_DEG: f64 = 160.0;
const NEWTON_MAX_ITERS: usize = 50;
const NEWTON_EPS: f64 = 1e-12;

/// Field of view of one lens, in degrees.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LensFov {
    /// Across the full width, through the principal point.
    pub horizontal_deg: f64,
    /// Across the full height, through the principal point.
    pub vertical_deg: f64,
    /// Corner to opposite corner, through the principal point.
    pub diagonal_deg: f64,
}

/// Field of view of `lens` from its intrinsics and full KB4 distortion
/// (the physical lens, ignoring `Lens::correction`). `None` when the KB4
/// polynomial cannot be inverted at an image edge (degenerate profile).
pub fn lens_fov(lens: &Lens) -> Option<LensFov> {
    let (w, h) = (lens.width as f64, lens.height as f64);
    // Angle off the optical axis of a pixel at offset (dx, dy) from the
    // principal point: KB4 maps it to theta_d = |(dx/fx, dy/fy)|.
    let theta = |dx: f64, dy: f64| {
        let td = ((dx / lens.fx).powi(2) + (dy / lens.fy).powi(2)).sqrt();
        theta_from_theta_d(td, &lens.distortion)
    };
    let horizontal = theta(lens.cx, 0.0)? + theta(w - lens.cx, 0.0)?;
    let vertical = theta(0.0, lens.cy)? + theta(0.0, h - lens.cy)?;
    let diagonal = theta(lens.cx, lens.cy)? + theta(w - lens.cx, h - lens.cy)?;
    Some(LensFov {
        horizontal_deg: horizontal.to_degrees(),
        vertical_deg: vertical.to_degrees(),
        diagonal_deg: diagonal.to_degrees(),
    })
}

/// Invert `theta_d = kb4::theta_d(theta)` by Newton-Raphson. `None` when
/// it does not converge to an angle in `[0, pi)`.
fn theta_from_theta_d(theta_d: f64, d: &[f64; 4]) -> Option<f64> {
    if theta_d < NEWTON_EPS {
        return Some(0.0);
    }
    let mut theta = theta_d;
    for _ in 0..NEWTON_MAX_ITERS {
        let f_prime = kb4::theta_d_prime(theta, d);
        if f_prime.abs() < 1e-15 {
            return None;
        }
        let delta = (kb4::theta_d(theta, d) - theta_d) / f_prime;
        theta -= delta;
        if delta.abs() < NEWTON_EPS {
            return (0.0..std::f64::consts::PI)
                .contains(&theta)
                .then_some(theta);
        }
    }
    None
}

/// Horizontal angles (degrees) along the horizon as seen from the stitch's
/// virtual eye. 0 = straight at the seam corner, positive = to the
/// viewer's right.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct HorizonCoverage {
    /// Left camera's image: `[from, to]`. `None` if it covers nothing.
    pub left_deg: Option<[f64; 2]>,
    /// Right camera's image: `[from, to]`. `None` if it covers nothing.
    pub right_deg: Option<[f64; 2]>,
    /// Angle where both cameras have image.
    pub overlap_deg: f64,
    /// Angle where at least one camera has image (panorama width).
    pub panorama_deg: f64,
    /// Angle between the two cameras' optical axes.
    pub axis_angle_deg: Option<f64>,
    /// Where the export cuts (`Topology::seam_offset` on the right
    /// plane, default blend direction). `None` if outside the sweep.
    pub seam_deg: Option<f64>,
}

/// One camera plane: world -> plane-local transform plus the lens.
struct Plane<'a> {
    model: Matrix4<f64>,
    inverse: Matrix4<f64>,
    aspect: f64,
    lens: &'a Lens,
}

impl Plane<'_> {
    /// `fisheye.wgsl`'s extended plane UV (`in.uv * 2 - 0.5`) where the ray
    /// `eye + t * dir` (t > 0) hits this plane's quad, if it does.
    fn hit_uv(&self, eye: &Vector3<f64>, dir: &Vector3<f64>) -> Option<(f64, f64)> {
        let o = self.inverse * Vector4::new(eye.x, eye.y, eye.z, 1.0);
        let d = self.inverse * Vector4::new(dir.x, dir.y, dir.z, 0.0);
        if d.z.abs() < 1e-12 {
            return None;
        }
        let t = -o.z / d.z;
        if t <= 0.0 {
            return None;
        }
        // Quad vertex position -> vertex UV (see `quad_vertices`: x spans
        // -0.5..0.5, y spans -0.5/aspect..0.5/aspect top-down).
        let vx = o.x + t * d.x + 0.5;
        let vy = 0.5 - (o.y + t * d.y) * self.aspect;
        if !(0.0..=1.0).contains(&vx) || !(0.0..=1.0).contains(&vy) {
            return None;
        }
        Some((vx * 2.0 - 0.5, vy * 2.0 - 0.5))
    }

    /// Whether extended plane UV `(u, v)` samples inside this camera's
    /// frame (`fisheye.wgsl`'s KB4 lookup and bounds check).
    fn samples_image(&self, u: f64, v: f64) -> bool {
        let l = self.lens;
        let (w, h) = (l.width as f64, l.height as f64);
        let (fx, fy, cx, cy) = (l.fx / w, l.fy / h, l.cx / w, l.cy / h);
        let x = (u - cx) / fx;
        let y = (v - cy) / fy;
        let r = (x * x + y * y).sqrt();
        let scale = kb4::kb4_forward_scale_with_correction(r, &l.distortion, l.correction as f64);
        let du = fx * x * scale + cx;
        let dv = fy * y * scale + cy;
        (0.0..=1.0).contains(&du) && (0.0..=1.0).contains(&dv)
    }

    /// World position of extended plane UV `(u, v)`.
    fn world_at(&self, u: f64, v: f64) -> Vector3<f64> {
        let vx = (u + 0.5) / 2.0;
        let vy = (v + 0.5) / 2.0;
        let p = self.model * Vector4::new(vx - 0.5, (0.5 - vy) / self.aspect, 0.0, 1.0);
        Vector3::new(p.x, p.y, p.z)
    }
}

/// Sweep the horizon (see the module doc). Left = `lenses[0]`, right =
/// `lenses[1]`; a setup with fewer than two lenses covers nothing.
pub fn horizon_coverage(cal: &Calibration) -> HorizonCoverage {
    let empty = HorizonCoverage {
        left_deg: None,
        right_deg: None,
        overlap_deg: 0.0,
        panorama_deg: 0.0,
        axis_angle_deg: None,
        seam_deg: None,
    };
    let (Some(left_lens), Some(right_lens)) = (cal.lenses.first(), cal.lenses.get(1)) else {
        return empty;
    };
    let aspect = left_lens.width as f32 / left_lens.height.max(1) as f32;
    let scene = SceneGeometry::new(&cal.topology, &cal.framing, aspect);
    let plane = |model: Matrix4<f32>, lens| {
        let model = model.cast::<f64>();
        model.try_inverse().map(|inverse| Plane {
            model,
            inverse,
            aspect: scene.plane_aspect as f64,
            lens,
        })
    };
    let (Some(left), Some(right)) = (
        plane(scene.model_matrix_left(), left_lens),
        plane(scene.model_matrix_right(), right_lens),
    ) else {
        return empty;
    };

    let cam = VirtualCamera::new(&scene.camera_position);
    let eye = cam.eye.cast::<f64>();
    let up = VirtualCamera::world_up().cast::<f64>();
    let forward = cam.base_forward.cast::<f64>();
    // Horizontal, viewer's right (independent of base_right's handedness).
    let right_axis = forward.cross(&up).normalize();
    let angle_of = |p: Vector3<f64>| {
        let d = p - eye;
        d.dot(&right_axis).atan2(d.dot(&forward)).to_degrees()
    };

    let steps = (2.0 * SWEEP_HALF_RANGE_DEG / SWEEP_STEP_DEG).round() as usize;
    let mut left_range: Option<[f64; 2]> = None;
    let mut right_range: Option<[f64; 2]> = None;
    let (mut both, mut any) = (0usize, 0usize);
    let seam_u = cal.topology.seam_offset as f64;
    let mut seam: Option<(f64, f64)> = None;
    let extend = |range: &mut Option<[f64; 2]>, a: f64| {
        let r = range.get_or_insert([a, a]);
        r[0] = r[0].min(a);
        r[1] = r[1].max(a);
    };
    for i in 0..=steps {
        let a = -SWEEP_HALF_RANGE_DEG + i as f64 * SWEEP_STEP_DEG;
        let (s, c) = a.to_radians().sin_cos();
        let dir = forward * c + right_axis * s;
        let in_left = left
            .hit_uv(&eye, &dir)
            .is_some_and(|(u, v)| left.samples_image(u, v));
        let right_hit = right.hit_uv(&eye, &dir);
        let in_right = right_hit.is_some_and(|(u, v)| right.samples_image(u, v));
        if let Some((u, _)) = right_hit {
            let miss = (u - seam_u).abs();
            if seam.is_none_or(|(_, best)| miss < best) {
                seam = Some((a, miss));
            }
        }
        if in_left {
            extend(&mut left_range, a);
        }
        if in_right {
            extend(&mut right_range, a);
        }
        both += usize::from(in_left && in_right);
        any += usize::from(in_left || in_right);
    }

    // Optical axis: the principal point on each plane.
    let axis = |p: &Plane| {
        let (w, h) = (p.lens.width as f64, p.lens.height as f64);
        angle_of(p.world_at(p.lens.cx / w, p.lens.cy / h))
    };
    HorizonCoverage {
        left_deg: left_range,
        right_deg: right_range,
        overlap_deg: both as f64 * SWEEP_STEP_DEG,
        panorama_deg: any as f64 * SWEEP_STEP_DEG,
        axis_angle_deg: Some((axis(&right) - axis(&left)).abs()),
        // Only a true crossing counts, not the nearest edge of the quad.
        seam_deg: seam.filter(|(_, miss)| *miss < 0.01).map(|(a, _)| a),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::calibration::{Framing, Topology};

    fn test_calibration(distortion: [f64; 4]) -> Calibration {
        let cam = || Lens::fisheye(1920, 1080, 900.0, 900.0, 960.0, 540.0, distortion);
        Calibration::new(
            vec![cam(), cam()],
            Topology {
                intersect: 0.54,
                x_ty: 0.0,
                x_rz: 0.0,
                z_rx: 0.0,
                x_rx: 0.0,
                z_rz: 0.0,
                blend_width: 0.05,
                blend_flip_direction: false,
                seam_offset: 0.0,
                multiband_blend_enabled: false,
                color_match_enabled: true,
                color_match_band_width: 0.15,
                color_match_grid_cols: 8,
                color_match_grid_rows: 16,
                color_match_interval_frames: 15,
                color_match_ema_alpha: 0.15,
                color_match_max_y_offset: 0.06,
                color_match_max_chroma_offset: 0.04,
                color_gamma_left: 1.0,
                color_gamma_right: 1.0,
                color_match_auto_gamma: false,
                color_match_overlap_fit: false,
                ground_tilt_x: 0.0,
                ground_tilt_z: 0.0,
                top_tilt_x: 0.0,
                top_tilt_z: 0.0,
                ground_tilt_band_width: 0.16,
                top_tilt_band_width: 0.16,
                color_grade_brightness: 1.0,
                color_grade_saturation: 1.0,
                color_grade_gamma: 1.0,
                sharpen_amount: 0.0,
                sharpen_radius: 1.0,
            },
            Framing {
                axis_offset: 0.24,
                tilt: 0.0,
                roll: 0.0,
            },
        )
    }

    #[test]
    fn zero_distortion_is_equidistant_fisheye() {
        // KB4 with k = 0: theta = theta_d = offset / f.
        let lens = &test_calibration([0.0; 4]).lenses[0];
        let fov = lens_fov(lens).expect("invertible");
        let expect = |px: f64| (2.0 * px / 900.0).to_degrees();
        assert!((fov.horizontal_deg - expect(960.0)).abs() < 1e-6);
        assert!((fov.vertical_deg - expect(540.0)).abs() < 1e-6);
        let half_diag = (960.0f64.powi(2) + 540.0f64.powi(2)).sqrt();
        assert!((fov.diagonal_deg - expect(half_diag)).abs() < 1e-6);
    }

    #[test]
    fn theta_inversion_round_trips() {
        let d = [-0.02, 0.004, -0.001, 0.0002];
        for i in 1..20 {
            let theta = i as f64 * 0.07;
            let back = theta_from_theta_d(kb4::theta_d(theta, &d), &d).expect("converges");
            assert!((back - theta).abs() < 1e-9, "theta {theta} -> {back}");
        }
    }

    #[test]
    fn horizon_coverage_is_consistent() {
        let cov = horizon_coverage(&test_calibration([0.0; 4]));
        let l = cov.left_deg.expect("left covers");
        let r = cov.right_deg.expect("right covers");
        // Left camera on the viewer's left.
        assert!(l[0] + l[1] < r[0] + r[1], "left {l:?} right {r:?}");
        assert!(cov.overlap_deg > 0.0);
        assert!(cov.overlap_deg < (l[1] - l[0]).min(r[1] - r[0]));
        // Union = both spans minus the shared part (to sweep resolution).
        let union = (l[1] - l[0]) + (r[1] - r[0]) - cov.overlap_deg;
        assert!((cov.panorama_deg - union).abs() < 4.0 * SWEEP_STEP_DEG);
        let axis = cov.axis_angle_deg.expect("axes");
        assert!(axis > 0.0 && axis < 180.0, "axis angle {axis}");
        let seam = cov.seam_deg.expect("seam in view");
        assert!(
            seam >= r[0] - 1.0 && seam <= l[1] + 1.0,
            "seam {seam} outside overlap"
        );
    }

    #[test]
    fn moving_the_seam_moves_its_angle() {
        let mut cal = test_calibration([0.0; 4]);
        let a = horizon_coverage(&cal).seam_deg.expect("seam");
        cal.topology.seam_offset = 0.1;
        let b = horizon_coverage(&cal).seam_deg.expect("seam");
        assert!((a - b).abs() > 0.5, "seam {a} -> {b}");
    }

    #[test]
    fn single_lens_covers_nothing() {
        let mut cal = test_calibration([0.0; 4]);
        cal.lenses.truncate(1);
        let cov = horizon_coverage(&cal);
        assert_eq!(cov.overlap_deg, 0.0);
        assert!(cov.left_deg.is_none());
    }
}
