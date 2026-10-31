//! Angular coverage of a setup: each lens's field of view, the physical
//! angle between the two cameras, and how many degrees of the scene both
//! cameras see.
//!
//! Pure math on a [`Calibration`] - no GPU, no I/O.
//!
//! - [`lens_fov`]: horizontal / vertical / diagonal field of view of one
//!   lens from its KB4 intrinsics (the physical lens, independent of the
//!   rig).
//! - [`horizon_coverage`]: the rig along the horizon, in real camera
//!   angles.
//!
//! # Why not angles in the stitch model
//!
//! The stitch places the two undistorted images on planes that are always
//! 90 deg apart (the L-shape) and views them from one virtual eye; a real
//! rig angle other than 90 deg is absorbed by the eye position and
//! `intersect`. Angles measured from that eye are therefore model angles,
//! not the rig's (on a rig measured at 84.0 deg in CAD the eye sees ~80).
//!
//! Instead, [`horizon_coverage`] sweeps directions from the eye only to
//! find which pixels the calibration declares to be the *same* scene point
//! in both cameras (the same plane intersection and KB4 bounds test
//! `fisheye.wgsl` uses). Each camera's plane UV is its pinhole-normalized
//! image, so `atan((u - cx) / fx)` is the true horizontal angle of that
//! point off that camera's own optical axis. Differences of those angles
//! are physical: the camera angle is `angle_in_left - angle_in_right` for
//! a shared point, the overlap is the span of shared points in one
//! camera's angles. On the 84.0 deg rig this gives 84.4-84.5 deg for three
//! independent setups.
//!
//! It assumes the cameras share one projection centre (true to a few cm
//! on a two-camera rig; scene points are metres away) and uses the
//! horizon row, which lies outside the `ground_tilt` / `top_tilt` bands.
//!
//! # Rig level
//!
//! The camera angle says nothing about how the rig leans: tilting the whole
//! rig turns both cameras equally. The lean comes from the scene - each
//! camera's direction of true "up" (e.g. from vertical structures, see
//! `reco_calibrate::rig_level`). [`framing_from_camera_up`] turns those
//! into the setup's `Framing::tilt` / `roll` (the stitch model's rig
//! correction, see `geometry::rig_frame`); [`physical_rig_level`] turns a
//! setup's framing back into the physical rig tilt / roll, with the rig's
//! forward direction halfway between the two cameras.

use nalgebra::{Matrix3, Matrix4, Vector3, Vector4};

use crate::calibration::{Calibration, Lens};
use crate::geometry::{VirtualCamera, rig_frame};
use crate::lens::kb4;
use crate::render::scene::SceneGeometry;

/// Angular step of the [`horizon_coverage`] sweep, in degrees (of the
/// virtual eye; only sets the sampling density).
pub const SWEEP_STEP_DEG: f64 = 0.02;
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

/// The rig along the horizon, in physical degrees (see the module doc).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct HorizonCoverage {
    /// Angle between the two cameras' optical axes (median over the
    /// overlap). `None` without overlap.
    pub camera_angle_deg: Option<f64>,
    /// Horizontal angle the left camera contributes along the horizon.
    pub left_span_deg: f64,
    /// Horizontal angle the right camera contributes along the horizon.
    pub right_span_deg: f64,
    /// Angle seen by both cameras.
    pub overlap_deg: f64,
    /// Total horizontal angle of the panorama (`left + right - overlap`).
    pub panorama_deg: f64,
    /// Where the export cuts (`Topology::seam_offset` on the right plane,
    /// default blend direction) as `[degrees from the overlap's left edge,
    /// degrees from its right edge]`. `None` when the seam is not inside
    /// the overlap.
    pub seam_in_overlap_deg: Option<[f64; 2]>,
}

/// One camera plane: world -> plane-local transform plus the lens.
struct Plane<'a> {
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

    /// Horizontal angle (degrees, positive = right) of extended plane UV
    /// column `u` off this camera's own optical axis: plane UV is the
    /// pinhole-normalized image, so this is `atan(x)`.
    fn camera_angle(&self, u: f64) -> f64 {
        let l = self.lens;
        let (fx, cx) = (l.fx / l.width as f64, l.cx / l.width as f64);
        ((u - cx) / fx).atan().to_degrees()
    }
}

/// Running `[min, max]`.
fn extend(range: &mut Option<[f64; 2]>, a: f64) {
    let r = range.get_or_insert([a, a]);
    r[0] = r[0].min(a);
    r[1] = r[1].max(a);
}

fn span(range: Option<[f64; 2]>) -> f64 {
    range.map(|[a, b]| b - a).unwrap_or(0.0)
}

/// Measure the rig along the horizon (see the module doc). Left =
/// `lenses[0]`, right = `lenses[1]`; a setup with fewer than two lenses,
/// or degenerate planes, covers nothing.
pub fn horizon_coverage(cal: &Calibration) -> HorizonCoverage {
    let empty = HorizonCoverage {
        camera_angle_deg: None,
        left_span_deg: 0.0,
        right_span_deg: 0.0,
        overlap_deg: 0.0,
        panorama_deg: 0.0,
        seam_in_overlap_deg: None,
    };
    let (Some(left_lens), Some(right_lens)) = (cal.lenses.first(), cal.lenses.get(1)) else {
        return empty;
    };
    let aspect = left_lens.width as f32 / left_lens.height.max(1) as f32;
    let scene = SceneGeometry::new(&cal.topology, &cal.framing, aspect);
    let plane = |model: Matrix4<f32>, lens| {
        model.cast::<f64>().try_inverse().map(|inverse| Plane {
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
    let right_axis = forward.cross(&up).normalize();

    let steps = (2.0 * SWEEP_HALF_RANGE_DEG / SWEEP_STEP_DEG).round() as usize;
    // Ranges in each camera's own angles.
    let (mut left_range, mut right_range, mut overlap_range) = (None, None, None);
    let mut angle_diffs: Vec<f64> = Vec::new();
    let seam_u = cal.topology.seam_offset as f64;
    // (left-camera angle at the seam, |u - seam_u|) of the closest sample.
    let mut seam: Option<(f64, f64)> = None;
    for i in 0..=steps {
        let a = (-SWEEP_HALF_RANGE_DEG + i as f64 * SWEEP_STEP_DEG).to_radians();
        let dir = forward * a.cos() + right_axis * a.sin();
        let lh = left
            .hit_uv(&eye, &dir)
            .filter(|&(u, v)| left.samples_image(u, v));
        let rh = right
            .hit_uv(&eye, &dir)
            .filter(|&(u, v)| right.samples_image(u, v));
        if let Some((u, _)) = lh {
            extend(&mut left_range, left.camera_angle(u));
        }
        if let Some((u, _)) = rh {
            extend(&mut right_range, right.camera_angle(u));
        }
        if let (Some((ul, _)), Some((ur, _))) = (lh, rh) {
            // The same scene direction, as each camera sees it.
            let al = left.camera_angle(ul);
            angle_diffs.push(al - right.camera_angle(ur));
            extend(&mut overlap_range, al);
            let miss = (ur - seam_u).abs();
            if seam.is_none_or(|(_, best)| miss < best) {
                seam = Some((al, miss));
            }
        }
    }

    let (left_span, right_span, overlap) =
        (span(left_range), span(right_range), span(overlap_range));
    HorizonCoverage {
        camera_angle_deg: median(&mut angle_diffs),
        left_span_deg: left_span,
        right_span_deg: right_span,
        overlap_deg: overlap,
        panorama_deg: left_span + right_span - overlap,
        // Only a true crossing inside the overlap counts, not its nearest
        // edge. The overlap's left edge is where the right camera starts.
        seam_in_overlap_deg: match (seam, overlap_range) {
            (Some((al, miss)), Some([lo, hi])) if miss < 0.01 => Some([al - lo, hi - al]),
            _ => None,
        },
    }
}

/// Camera axes of one plane in the stitch model's world: columns are the
/// camera's x (image right), y (image down) and z (viewing direction).
/// The plane is the camera's image plane seen from the eye side: local +x
/// = image right, +y = image up, +z towards the eye.
fn model_camera_axes(model: &Matrix4<f32>) -> Matrix3<f64> {
    let r: Matrix3<f64> = model.fixed_view::<3, 3>(0, 0).into_owned().cast::<f64>();
    let x = (r * Vector3::x()).normalize();
    let y = (r * -Vector3::y()).normalize();
    let z = (r * -Vector3::z()).normalize();
    Matrix3::from_columns(&[x, y, z])
}

fn model_axes(cal: &Calibration) -> Option<(Matrix3<f64>, Matrix3<f64>, SceneGeometry)> {
    let lens = cal.lenses.first()?;
    let aspect = lens.width as f32 / lens.height.max(1) as f32;
    let scene = SceneGeometry::new(&cal.topology, &cal.framing, aspect);
    Some((
        model_camera_axes(&scene.model_matrix_left()),
        model_camera_axes(&scene.model_matrix_right()),
        scene,
    ))
}

/// `Framing::tilt` / `roll` (radians) that make the stitch's level
/// reference match the measured true-up directions of the two cameras
/// (unit vectors in each camera's own frame: x = image right, y = image
/// down, z = viewing direction). The two are averaged in the model's
/// world. `None` for a setup without lenses or a degenerate up.
pub fn framing_from_camera_up(
    cal: &Calibration,
    up_left: Vector3<f64>,
    up_right: Vector3<f64>,
) -> Option<(f64, f64)> {
    let (left, right, scene) = model_axes(cal)?;
    let u = (left * up_left.normalize() + right * up_right.normalize()).try_normalize(1e-9)?;
    let cam = VirtualCamera::new(&scene.camera_position);
    let fwd = cam.base_forward.cast::<f64>();
    let axis = cam.base_right.cast::<f64>().normalize();
    // rig_frame: tilt turns forward and up about base_right; roll then
    // turns up about the tilted forward f0, so the result stays
    // perpendicular to f0. f0(t) = fwd cos t + (axis x fwd) sin t
    // (fwd is horizontal, perpendicular to axis); solve u . f0 = 0.
    let side = axis.cross(&fwd);
    let tilt = (-(u.dot(&fwd))).atan2(u.dot(&side));
    let (up_t, f0) = rig_frame(&cam, tilt as f32, 0.0);
    let (up_t, f0) = (up_t.cast::<f64>(), f0.cast::<f64>());
    // u = R(f0, -roll) * up_t.
    let roll = -(up_t.cross(&u).dot(&f0)).atan2(up_t.dot(&u));
    Some((tilt, roll))
}

/// Physical lean of the rig, in degrees.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RigLevel {
    /// Forward lean: negative = pointing down (towards the pitch).
    pub tilt_deg: f64,
    /// Sideways lean: positive = the rig's top leans to the viewer's right.
    pub roll_deg: f64,
}

/// The physical rig tilt / roll that a setup's `Framing::tilt` / `roll`
/// stand for: the framing's level-up direction, expressed in each camera's
/// frame through the stitch model, then in the physical rig frame (the
/// cameras yawed by +- half of [`HorizonCoverage::camera_angle_deg`]
/// either side of the rig's forward direction), averaged over both
/// cameras. `None` without a camera angle (no overlap).
pub fn physical_rig_level(cal: &Calibration) -> Option<RigLevel> {
    let half = horizon_coverage(cal).camera_angle_deg?.to_radians() / 2.0;
    let (left, right, scene) = model_axes(cal)?;
    let cam = VirtualCamera::new(&scene.camera_position);
    let (u, _) = rig_frame(&cam, cal.framing.tilt as f32, cal.framing.roll as f32);
    let u = u.cast::<f64>();
    // Camera axes in the physical rig frame (x right, y down, z forward).
    let rig_axes = |yaw: f64| {
        let (s, c) = yaw.sin_cos();
        Matrix3::from_columns(&[
            Vector3::new(c, 0.0, -s),
            Vector3::y(),
            Vector3::new(s, 0.0, c),
        ])
    };
    let u_rig = (rig_axes(-half) * (left.transpose() * u)
        + rig_axes(half) * (right.transpose() * u))
        .try_normalize(1e-9)?;
    Some(RigLevel {
        // Pointing down by t puts up at -sin t along the viewing axis.
        tilt_deg: u_rig.z.asin().to_degrees(),
        roll_deg: u_rig.x.atan2(-u_rig.y).to_degrees(),
    })
}

fn median(v: &mut [f64]) -> Option<f64> {
    if v.is_empty() {
        return None;
    }
    v.sort_by(f64::total_cmp);
    Some(v[v.len() / 2])
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
        let cal = test_calibration([0.0; 4]);
        let cov = horizon_coverage(&cal);
        let hfov = lens_fov(&cal.lenses[0]).expect("fov").horizontal_deg;
        // Each camera contributes at most its lens's horizontal FOV.
        for s in [cov.left_span_deg, cov.right_span_deg] {
            assert!(s > 0.0 && s <= hfov + 0.1, "span {s} vs lens {hfov}");
        }
        assert!(cov.overlap_deg > 0.0);
        assert!(cov.overlap_deg < cov.left_span_deg.min(cov.right_span_deg));
        let angle = cov.camera_angle_deg.expect("overlap");
        assert!(angle > 0.0 && angle < hfov, "camera angle {angle}");
        // Co-located cameras: overlap = what both lenses see past the angle
        // between them, when the overlap reaches both image edges.
        assert!(
            cov.overlap_deg <= cov.left_span_deg / 2.0 + cov.right_span_deg / 2.0 - angle + 1.0,
            "overlap {} angle {angle}",
            cov.overlap_deg
        );
        let [from_left, from_right] = cov.seam_in_overlap_deg.expect("seam inside overlap");
        assert!((from_left + from_right - cov.overlap_deg).abs() < 1e-9);
        assert!(from_left >= 0.0 && from_right >= 0.0);
    }

    #[test]
    fn moving_the_seam_moves_it_within_the_overlap() {
        let mut cal = test_calibration([0.0; 4]);
        let a = horizon_coverage(&cal).seam_in_overlap_deg.expect("seam")[0];
        cal.topology.seam_offset = 0.1;
        let b = horizon_coverage(&cal).seam_in_overlap_deg.expect("seam")[0];
        assert!((a - b).abs() > 0.5, "seam {a} -> {b}");
    }

    #[test]
    fn model_camera_axes_look_from_the_eye_at_the_plane() {
        let cal = test_calibration([0.0; 4]);
        let (left, right, scene) = model_axes(&cal).expect("axes");
        let eye = VirtualCamera::new(&scene.camera_position).eye.cast::<f64>();
        for (axes, model) in [
            (left, scene.model_matrix_left()),
            (right, scene.model_matrix_right()),
        ] {
            let centre = (model.cast::<f64>() * Vector4::new(0.0, 0.0, 0.0, 1.0)).xyz();
            assert!(
                axes.column(2).dot(&(centre - eye)) > 0.0,
                "camera looks away from its plane"
            );
            // Right-handed: x cross y = z.
            let z = axes.column(0).cross(&axes.column(1));
            assert!((z - axes.column(2)).norm() < 1e-9);
        }
    }

    #[test]
    fn framing_round_trips_through_camera_up() {
        let mut cal = test_calibration([0.0; 4]);
        for (tilt, roll) in [(0.0, 0.0), (0.37, -0.02), (-0.2, 0.1)] {
            cal.framing.tilt = tilt;
            cal.framing.roll = roll;
            let (left, right, scene) = model_axes(&cal).expect("axes");
            let cam = VirtualCamera::new(&scene.camera_position);
            let u = rig_frame(&cam, tilt as f32, roll as f32).0.cast::<f64>();
            let (t, r) = framing_from_camera_up(&cal, left.transpose() * u, right.transpose() * u)
                .expect("framing");
            assert!(
                (t - tilt).abs() < 1e-5 && (r - roll).abs() < 1e-5,
                "{tilt},{roll} -> {t},{r}"
            );
        }
    }

    #[test]
    fn level_framing_is_a_level_rig() {
        let cal = test_calibration([0.0; 4]);
        let level = physical_rig_level(&cal).expect("level");
        assert!(
            level.tilt_deg.abs() < 1e-6 && level.roll_deg.abs() < 1e-6,
            "{level:?}"
        );
    }

    #[test]
    fn single_lens_covers_nothing() {
        let mut cal = test_calibration([0.0; 4]);
        cal.lenses.truncate(1);
        let cov = horizon_coverage(&cal);
        assert_eq!(cov.overlap_deg, 0.0);
        assert!(cov.camera_angle_deg.is_none());
    }
}
