//! 3D geometric model and reprojection error objective.
//!
//! Ports the v1 Python position optimization math to Rust. Two camera
//! planes form an L-shape in 3D space with a virtual camera at the corner.
//! The optimizer adjusts 3-6 parameters (controlled by `lock_cam_d`,
//! `lock_z_rx`, and `enable_x_rx`) to minimize reprojection error.
//!
//! ## Reprojection Error
//!
//! For each matched point pair, shoots a ray from the camera through one
//! point on its plane and intersects with the other plane. The squared
//! distance between the intersection and the actual matched point is the
//! error. Both directions are computed for symmetry.
//!
//! ## Coordinate Convention
//!
//! - Left plane (x-plane): 2D `(x, y)` maps to 3D `(x, -y, 0)`
//! - Right plane (z-plane): 2D `(z, y)` maps to 3D `(0, -y, -z)`
//! - Camera sits at `[cam_d, 0, cam_d]` on the x=z bisector
//!
//! ## Left/Right Swap
//!
//! Following the v1 convention (processing.py:693), the *right camera*
//! points are placed on the x-plane (left in optimizer space) and the
//! *left camera* points on the z-plane. This swap is handled internally
//! by the public API so callers pass frames in natural left/right order.

use nalgebra::{Matrix3, Vector3};

use crate::types::MatchedPoint;

/// Plane width in the geometric model (normalized to 1.0).
pub const PLANE_WIDTH: f64 = 1.0;

/// Map a 2D point on the left plane (x-plane) to 3D.
///
/// `(x, y) -> (x, -y, 0)`
#[inline]
fn to_3d_x_plane(p: [f64; 2]) -> Vector3<f64> {
    Vector3::new(p[0], -p[1], 0.0)
}

/// Map a 2D point on the right plane (z-plane) to 3D.
///
/// `(z, y) -> (0, -y, -z)`
#[inline]
fn to_3d_z_plane(p: [f64; 2]) -> Vector3<f64> {
    Vector3::new(0.0, -p[1], -p[0])
}

/// Build a 3D rotation matrix from Euler angles (extrinsic ZYX order).
///
/// Equivalent to `Rz @ Ry @ Rx` matching the v1 Python implementation.
fn rotation_matrix(rx: f64, ry: f64, rz: f64) -> Matrix3<f64> {
    let (sx, cx) = rx.sin_cos();
    let (sy, cy) = ry.sin_cos();
    let (sz, cz) = rz.sin_cos();

    #[rustfmt::skip]
    let m = Matrix3::new(
        cz * cy,    cz * sy * sx - sz * cx,    cz * sy * cx + sz * sx,
        sz * cy,    sz * sy * sx + cz * cx,    sz * sy * cx - cz * sx,
        -sy,        cy * sx,                    cy * cx,
    );
    m
}

/// Parameters for the optimization objective function.
///
/// The core model has 5 parameters: `cam_d`, `intersect`, `x_ty`, `x_rz`,
/// `z_rx`. An optional 6th (`z_rz`) is available. Use `lock_cam_d = true`
/// to reduce to 4 parameters (derives `cam_d` from `intersect`).
#[derive(Debug, Clone, Copy)]
pub struct OptParams {
    /// Y-axis translation of the right plane (corrects vertical misalignment).
    pub x_ty: f64,
    /// Overlap ratio between the two planes `[0, 1]`.
    pub intersect: f64,
    /// Camera distance from origin along both X and Z axes.
    pub cam_d: f64,
    /// Z-axis rotation of the right plane (radians).
    pub x_rz: f64,
    /// X-axis rotation of the left plane (radians).
    pub z_rx: f64,
    /// Z-axis rotation of the left plane (radians) - the 6th parameter.
    /// `None` when running in 5-param mode.
    pub z_rz: Option<f64>,
    /// X-axis rotation of the right plane (radians).
    /// `None` unless `enable_x_rx` is set in the optimizer config.
    pub x_rx: Option<f64>,
    /// Band-limited ground-plane perspective correction for the x-plane
    /// (right camera's points, `MatchedPoint::left`) - see
    /// `band_limited_ground_warp`.
    ///
    /// `c = tan(theta)`, where `theta` is how much further down that
    /// camera really looks than the flat-plane model assumes, applied
    /// only within a near-field band (points closer to image center are
    /// mathematically untouched). `None` (or `Some(0.0)`) reproduces
    /// today's flat-plane behavior exactly. Independent from
    /// `ground_tilt_z` because the two planes need not share the same
    /// mount height/tilt deviation - see `crates/reco-calibrate/FRICTION.md`.
    /// Experimental - not yet wired into the production optimizer.
    pub ground_tilt_x: Option<f64>,
    /// Band-limited ground-plane perspective correction for the z-plane
    /// (left camera's points, `MatchedPoint::right`). See `ground_tilt_x`.
    pub ground_tilt_z: Option<f64>,
    /// Focal-scale constant for the x-plane's ground-tilt warp: `k_x =
    /// fy / (2 * width)` for the right camera (`MatchedPoint::left`),
    /// using that camera's own intrinsics. This is a **known constant**
    /// derived from calibrated intrinsics, not a fitted parameter -
    /// unlike every other field on this struct. Required to correctly
    /// convert a plane-y value to/from a true angle (see
    /// `warp_ground_y`'s doc comment for the derivation and why `k != 1`
    /// in general). Defaults to `1.0`, which reproduces the pre-2026-07-05
    /// (dimensionally incorrect) formula - only matters when
    /// `ground_tilt_x` is `Some`.
    pub k_x: f64,
    /// Focal-scale constant for the z-plane's ground-tilt warp (left
    /// camera, `MatchedPoint::right`). See `k_x`.
    pub k_z: f64,
}

impl OptParams {
    /// Unpack from a 5-element parameter vector.
    ///
    /// Order: `[x_ty, intersect, cam_d, x_rz, z_rx]`
    pub fn from_5param(x: &[f64]) -> Self {
        Self {
            x_ty: x[0],
            intersect: x[1],
            cam_d: x[2],
            x_rz: x[3],
            z_rx: x[4],
            z_rz: None,
            x_rx: None,
            ground_tilt_x: None,
            ground_tilt_z: None,
            k_x: 1.0,
            k_z: 1.0,
        }
    }

    /// Unpack from a 6-element parameter vector.
    ///
    /// Order: `[x_ty, intersect, cam_d, x_rz, z_rx, z_rz]`
    pub fn from_6param(x: &[f64]) -> Self {
        Self {
            x_ty: x[0],
            intersect: x[1],
            cam_d: x[2],
            x_rz: x[3],
            z_rx: x[4],
            z_rz: Some(x[5]),
            x_rx: None,
            ground_tilt_x: None,
            ground_tilt_z: None,
            k_x: 1.0,
            k_z: 1.0,
        }
    }

    /// Pack into a 5-element vector (ignoring `z_rz`).
    pub fn to_5param(&self) -> [f64; 5] {
        [self.x_ty, self.intersect, self.cam_d, self.x_rz, self.z_rx]
    }

    /// Pack into a 6-element vector.
    pub fn to_6param(&self) -> [f64; 6] {
        [
            self.x_ty,
            self.intersect,
            self.cam_d,
            self.x_rz,
            self.z_rx,
            self.z_rz.unwrap_or(0.0),
        ]
    }
}

/// Ground-plane perspective correction for a single plane-space coordinate.
///
/// `normalize_to_plane` produces `t`, a value proportional to (not equal
/// to) `tan(phi)` for the real vertical angle `phi` from the optical
/// axis. Tracing the exact pixel -> GPU-undistort -> `normalize_to_plane`
/// path (see `fisheye.wgsl`'s `fs_main` and its `uv * 2.0 - 0.5` remap):
///
/// ```text
/// t = tan(phi) * k,   where k = fy / (2 * image_width)
/// ```
///
/// (`fy`/`image_width` from that camera's own `CameraParams`, in pixels;
/// the factor of 2 comes directly from the shader's UV remap, confirmed
/// against `lens/mod.rs`'s CPU mirror which halves `fy`/`fx` for the same
/// reason). `k` is a fixed, known constant for a given camera and lens -
/// **not** a value to fit - it's usually far from `1.0` (~0.19 for this
/// project's DJI Osmo Action 4 rig at 3840px width), so treating `t` as
/// if it already equaled `tan(phi)` (this function's pre-2026-07-05
/// version) applies the tangent-addition identity at the wrong scale.
///
/// The correct derivation: `phi = atan(t / k)`, add the extra tilt
/// `theta` (`c = tan(theta)`), then convert back: `t' = k * tan(theta +
/// phi)`. Expanding via the tangent-addition identity gives:
///
/// ```text
/// warp(t, c, k) = k*tan(theta + phi) = (k*c + t) / (1 - c*t/k)
/// ```
///
/// `warp(t, 0.0, k) == t` exactly for any `k`, so `c = 0.0` reproduces
/// today's flat-plane behavior bit-for-bit. `k = 1.0` reduces to the
/// original (dimensionally-incorrect-for-real-cameras) formula - kept as
/// the default in `OptParams` so existing tests/callers that don't set a
/// real `k` are unaffected.
///
/// The denominator is guarded away from zero (the horizon-direction
/// singularity) so callers driving `c` via an unconstrained optimizer
/// can't wander into NaN/infinite territory.
#[inline]
pub fn warp_ground_y(t: f64, c: f64, k: f64) -> f64 {
    let denom = 1.0 - c * t / k;
    let denom = if denom.abs() < 1e-9 {
        1e-9_f64.copysign(denom)
    } else {
        denom
    };
    (k * c + t) / denom
}

/// Classic Hermite smoothstep, clamped to `[edge0, edge1]`.
#[inline]
fn smoothstep(edge0: f64, edge1: f64, x: f64) -> f64 {
    let t = ((x - edge0) / (edge1 - edge0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// `|t|` below which a point is fully outside the ground-tilt correction
/// band (0.08 matches this project's `sigma_y`/near-field-bucket
/// convention used elsewhere - see FRICTION.md, `fit_ground_tilt.rs`).
const GROUND_TILT_BAND_START: f64 = 0.08;
/// `|t|` at and beyond which the correction reaches full strength.
const GROUND_TILT_BAND_FULL: f64 = 0.16;

/// Band-limited ground-plane correction.
///
/// [`warp_ground_y`] is a *global* tilt reparametrization - `warp(0, c) =
/// c`, not `0`, so applying it uniformly shifts far-field points almost
/// as much as near-field ones (confirmed empirically and mathematically,
/// see FRICTION.md's "ground-plane perspective correction" entry: doing
/// this made far-field residual monotonically worse for every tested `c`,
/// and pushed `cam_d` to its bound). This wraps it in a smoothstep blend
/// against the identity so the correction is mathematically guaranteed to
/// leave points with `t <= GROUND_TILT_BAND_START` completely untouched,
/// ramping to the full `warp_ground_y` effect by `t >= GROUND_TILT_BAND_FULL`.
/// Same "correction ramps in, far field is provably unaffected" shape as
/// the reverted `ground_correction` shader ramp
/// (`smoothstep(0.75, 1.0, uv.y)`), but applied to the calibration fit
/// instead of a render-time pixel shift.
///
/// One-sided in `t`, not `|t|`: `t > 0` is near field (close to the
/// camera, bottom of frame - this project's `sigma_y`/plane-y convention),
/// which is what actually suffers from the flat-plane model's parallax
/// error; `t <= 0` is everything from the horizon up through the sky, and
/// is *always* identity regardless of magnitude. Every real matched point
/// this was ever fitted against (AKAZE, manual field-line clicks) is real
/// ground content with `t > 0`, so this was invisible during fitting - but
/// `fisheye.wgsl`'s render-time port applies this to *every pixel*,
/// including sky, where the old symmetric-in-`|t|` version would ramp to
/// full strength again once `|t| >= GROUND_TILT_BAND_FULL` in the negative
/// direction, visibly warping the skyline (caught by rendering a real
/// frame with `examples/verify_ground_tilt.rs`, not by any point-based
/// numeric check - see FRICTION.md point 20's wiring entry).
#[inline]
pub fn band_limited_ground_warp(t: f64, c: f64, k: f64) -> f64 {
    if t <= 0.0 {
        return t;
    }
    let weight = smoothstep(GROUND_TILT_BAND_START, GROUND_TILT_BAND_FULL, t);
    if weight == 0.0 {
        return t;
    }
    t + weight * (warp_ground_y(t, c, k) - t)
}

/// Apply geometric transformations to matched point pairs.
///
/// Converts 2D plane coordinates to 3D, applies rotations and translations
/// based on the optimization parameters, and returns the transformed 3D
/// positions for both planes.
///
/// The left/right swap is already baked into `MatchedPoint`: `.left` is
/// on the x-plane (right camera) and `.right` is on the z-plane (left camera).
pub fn apply_transformations(
    points: &[MatchedPoint],
    params: &OptParams,
) -> (Vec<Vector3<f64>>, Vec<Vector3<f64>>) {
    let half_offset = PLANE_WIDTH / 2.0 * (1.0 - params.intersect);

    // Left plane (x-plane): X-rotation by x_rx (right camera's independent
    // pitch - previously dead here: read by the renderer at
    // SceneGeometry::from_layout_with_aspect but never applied to this
    // cost function, so enabling it gave the optimizer zero gradient),
    // Z-rotation by x_rz, translated along X.
    let x_rx = params.x_rx.unwrap_or(0.0);
    let r_x_plane = rotation_matrix(x_rx, 0.0, params.x_rz);
    let t_x_plane = Vector3::new(half_offset, params.x_ty, 0.0);

    // Right plane (z-plane): X-rotation by z_rx, optionally Z-rotation by z_rz,
    // translated along Z
    let z_rz = params.z_rz.unwrap_or(0.0);
    let r_z_plane = rotation_matrix(params.z_rx, 0.0, z_rz);
    let t_z_plane = Vector3::new(0.0, 0.0, half_offset);

    let ground_tilt_x = params.ground_tilt_x.unwrap_or(0.0);
    let ground_tilt_z = params.ground_tilt_z.unwrap_or(0.0);

    let mut x_transformed = Vec::with_capacity(points.len());
    let mut z_transformed = Vec::with_capacity(points.len());

    for mp in points {
        let mut left = mp.left;
        let mut right = mp.right;
        if ground_tilt_x != 0.0 {
            left[1] = band_limited_ground_warp(left[1], ground_tilt_x, params.k_x);
        }
        if ground_tilt_z != 0.0 {
            right[1] = band_limited_ground_warp(right[1], ground_tilt_z, params.k_z);
        }
        let x_3d = to_3d_x_plane(left);
        let z_3d = to_3d_z_plane(right);

        // v1 uses `point @ R.T` (row-vector convention).
        // nalgebra uses column vectors, so `R * point` is equivalent.
        x_transformed.push(r_x_plane * x_3d + t_x_plane);
        z_transformed.push(r_z_plane * z_3d + t_z_plane);
    }

    (x_transformed, z_transformed)
}

/// Symmetric plane-to-plane reprojection error (sum of squared distances).
///
/// For each matched pair, shoots a ray from the camera through one point
/// and measures where it hits the other plane. The squared distance between
/// the intersection and the actual matched point is the error.
///
/// Both directions are computed (x-plane → z-plane and z-plane → x-plane)
/// for symmetry. This is the standard reprojection metric used in bundle
/// adjustment and has a proper global minimum in cam_d, unlike angular
/// error which is degenerate.
///
/// # Why this works
///
/// The x-plane stays at z=0 and the z-plane stays at x=0 even after
/// their respective rotations (Rz around Z-axis preserves z=0, Rx around
/// X-axis preserves x=0). So ray-plane intersection is a simple division.
pub fn reprojection_error(points: &[MatchedPoint], params: &OptParams) -> f64 {
    let camera = Vector3::new(params.cam_d, 0.0, params.cam_d);
    let (x_pts, z_pts) = apply_transformations(points, params);

    let mut total = 0.0;
    for (x_pt, z_pt) in x_pts.iter().zip(z_pts.iter()) {
        // Forward: ray from camera through x_pt, intersect z-plane (x=0)
        let dir_x = x_pt - camera;
        if dir_x.x.abs() > 1e-15 {
            let t = -camera.x / dir_x.x;
            if t > 0.0 {
                let hit = camera + t * dir_x;
                let dy = hit.y - z_pt.y;
                let dz = hit.z - z_pt.z;
                total += dy * dy + dz * dz;
            } else {
                total += 1e6;
            }
        }

        // Backward: ray from camera through z_pt, intersect x-plane (z=0)
        let dir_z = z_pt - camera;
        if dir_z.z.abs() > 1e-15 {
            let t = -camera.z / dir_z.z;
            if t > 0.0 {
                let hit = camera + t * dir_z;
                let dx = hit.x - x_pt.x;
                let dy = hit.y - x_pt.y;
                total += dx * dx + dy * dy;
            } else {
                total += 1e6;
            }
        }
    }
    total
}

/// Compute per-point symmetric reprojection errors.
///
/// Returns a vector of individual error values (sum of forward + backward
/// squared distances per pair). Used for outlier detection and trimmed
/// evaluation.
pub fn per_point_reprojection_error(points: &[MatchedPoint], params: &OptParams) -> Vec<f64> {
    let camera = Vector3::new(params.cam_d, 0.0, params.cam_d);
    let (x_pts, z_pts) = apply_transformations(points, params);

    x_pts
        .iter()
        .zip(z_pts.iter())
        .map(|(x_pt, z_pt)| {
            let mut err = 0.0;

            let dir_x = x_pt - camera;
            if dir_x.x.abs() > 1e-15 {
                let t = -camera.x / dir_x.x;
                if t > 0.0 {
                    let hit = camera + t * dir_x;
                    let dy = hit.y - z_pt.y;
                    let dz = hit.z - z_pt.z;
                    err += dy * dy + dz * dz;
                } else {
                    return 1e6;
                }
            }

            let dir_z = z_pt - camera;
            if dir_z.z.abs() > 1e-15 {
                let t = -camera.z / dir_z.z;
                if t > 0.0 {
                    let hit = camera + t * dir_z;
                    let dx = hit.x - x_pt.x;
                    let dy = hit.y - x_pt.y;
                    err += dx * dx + dy * dy;
                } else {
                    return 1e6;
                }
            }

            err
        })
        .collect()
}

/// Trimmed reprojection error for robust evaluation.
///
/// Computes per-point errors, sorts them, drops the worst `trim_fraction`
/// (e.g., 0.2 = drop worst 20%), and sums the remaining. This makes
/// the evaluation robust to outlier points that would otherwise steer
/// the optimizer toward large-rotation solutions.
pub fn trimmed_reprojection_error(
    points: &[MatchedPoint],
    params: &OptParams,
    trim_fraction: f64,
) -> f64 {
    if points.is_empty() {
        return 0.0;
    }
    let mut errors = per_point_reprojection_error(points, params);
    errors.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));

    let keep = ((1.0 - trim_fraction) * errors.len() as f64).ceil() as usize;
    let keep = keep.min(errors.len()).max(1);
    errors[..keep].iter().sum()
}

/// Compute the total angular error between matched point pairs.
///
/// Legacy objective from v1. Kept for diagnostic comparison.
/// For each pair, computes the angle between the direction vectors from
/// the virtual camera to each transformed 3D point.
pub fn angular_error(points: &[MatchedPoint], params: &OptParams) -> f64 {
    let camera = Vector3::new(params.cam_d, 0.0, params.cam_d);
    let (x_pts, z_pts) = apply_transformations(points, params);

    let mut total = 0.0;
    for (x_pt, z_pt) in x_pts.iter().zip(z_pts.iter()) {
        let dx = x_pt - camera;
        let dz = z_pt - camera;
        // Skip degenerate points at the camera position
        if dx.norm() < 1e-15 || dz.norm() < 1e-15 {
            continue;
        }
        let v_x = dx.normalize();
        let v_z = dz.normalize();
        let dot = v_x.dot(&v_z).clamp(-1.0, 1.0);
        total += dot.acos();
    }
    total
}

/// Pixel-space (normalized `[0, 1]`) column where each camera's own
/// contribution to the stitched output ends, derived from the current
/// `intersect` value.
///
/// Matches the left/right swap convention used throughout this module:
/// `MatchedPoint::left` / `left_pixel_nx` come from the *right* camera,
/// `MatchedPoint::right` / `right_pixel_nx` come from the *left* camera.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SeamColumns {
    /// Seam column in the right camera's own normalized pixel space -
    /// compare against `MatchedPoint::left_pixel_nx`.
    pub right_camera_seam_nx: f64,
    /// Seam column in the left camera's own normalized pixel space -
    /// compare against `MatchedPoint::right_pixel_nx`.
    pub left_camera_seam_nx: f64,
}

/// Compute both cameras' seam columns from the current `intersect` value.
pub fn seam_columns(intersect: f64) -> SeamColumns {
    SeamColumns {
        right_camera_seam_nx: intersect / 2.0,
        left_camera_seam_nx: 1.0 - intersect / 2.0,
    }
}

/// Configuration for seam-proximity weighting in the cost function.
#[derive(Debug, Clone, Copy)]
pub struct SeamWeightConfig {
    /// Horizontal Gaussian sigma (seam proximity weighting).
    pub sigma_x: f64,
    /// Vertical Gaussian sigma (center-band weighting).
    pub sigma_y: f64,
    /// Vertical center in plane coordinates. -0.05 = slightly above
    /// image center (horizon for pole-mounted cameras). 0.0 = image center.
    pub y_center: f64,
}

impl Default for SeamWeightConfig {
    fn default() -> Self {
        Self {
            sigma_x: 0.08,
            sigma_y: 0.08,
            y_center: -0.05,
        }
    }
}

impl SeamWeightConfig {
    /// Create from a single sigma value (backward compatible).
    pub fn from_sigma(sigma: f64) -> Self {
        Self {
            sigma_x: sigma,
            ..Default::default()
        }
    }
}

/// Compute per-point seam-weighted reprojection errors.
///
/// Each point's error is weighted by proximity to the stitch seam
/// (horizontal Gaussian) and image center (vertical Gaussian).
/// Used by [`SeamWeightedCost::per_point_cost`](crate::defaults::SeamWeightedCost).
pub fn per_point_seam_weighted_errors(
    points: &[MatchedPoint],
    params: &OptParams,
    sigma: f64,
    sigma_y: f64,
) -> Vec<f64> {
    per_point_seam_weighted_errors_full(
        points,
        params,
        &SeamWeightConfig {
            sigma_x: sigma,
            sigma_y,
            ..Default::default()
        },
    )
}

fn per_point_seam_weighted_errors_full(
    points: &[MatchedPoint],
    params: &OptParams,
    config: &SeamWeightConfig,
) -> Vec<f64> {
    let camera = Vector3::new(params.cam_d, 0.0, params.cam_d);
    let (x_pts, z_pts) = apply_transformations(points, params);

    let seams = seam_columns(params.intersect);
    // Clamp sigma to a minimum to prevent NaN from division by zero
    let sx = config.sigma_x.max(1e-6);
    let sy = config.sigma_y.max(1e-6);
    let inv_2sigma_sq = 1.0 / (2.0 * sx * sx);
    let inv_2sigma_y_sq = 1.0 / (2.0 * sy * sy);

    x_pts
        .iter()
        .zip(z_pts.iter())
        .enumerate()
        .map(|(idx, (x_pt, z_pt))| {
            let dl = points[idx].left_pixel_nx - seams.right_camera_seam_nx;
            let dr = points[idx].right_pixel_nx - seams.left_camera_seam_nx;
            let w_horiz =
                0.5 * ((-dl * dl * inv_2sigma_sq).exp() + (-dr * dr * inv_2sigma_sq).exp());

            let yl = points[idx].left[1] - config.y_center;
            let yr = points[idx].right[1] - config.y_center;
            let w_vert =
                0.5 * ((-yl * yl * inv_2sigma_y_sq).exp() + (-yr * yr * inv_2sigma_y_sq).exp());

            let w = w_horiz * w_vert;
            let mut err = 0.0;

            let dir_x = x_pt - camera;
            if dir_x.x.abs() > 1e-15 {
                let t = -camera.x / dir_x.x;
                if t > 0.0 {
                    let hit = camera + t * dir_x;
                    let dy = hit.y - z_pt.y;
                    let dz = hit.z - z_pt.z;
                    err += w * (dy * dy + dz * dz);
                } else {
                    err += w * 1e6;
                }
            }

            let dir_z = z_pt - camera;
            if dir_z.z.abs() > 1e-15 {
                let t = -camera.z / dir_z.z;
                if t > 0.0 {
                    let hit = camera + t * dir_z;
                    let dx = hit.x - x_pt.x;
                    let dy = hit.y - x_pt.y;
                    err += w * (dx * dx + dy * dy);
                } else {
                    err += w * 1e6;
                }
            }

            err
        })
        .collect()
}

/// Seam-weighted symmetric reprojection error (sum over all points).
///
/// Each point pair is weighted by a 2D Gaussian combining:
/// 1. **Horizontal**: proximity to the stitch seam (where alignment
///    matters most visually)
/// 2. **Vertical**: proximity to the image center (sky and close-up
///    ground features are less reliable)
///
/// The seam position updates dynamically with the current `intersect`
/// value. `sigma` controls the horizontal Gaussian width (seam
/// proximity); `sigma_y` controls the vertical Gaussian width (how much
/// weight near/far-field points get relative to the image center).
pub fn seam_weighted_reprojection_error(
    points: &[MatchedPoint],
    params: &OptParams,
    sigma: f64,
    sigma_y: f64,
) -> f64 {
    per_point_seam_weighted_errors(points, params, sigma, sigma_y)
        .iter()
        .sum()
}

/// Trimmed seam-weighted reprojection error.
///
/// Computes per-point seam-weighted errors, sorts them, drops the worst
/// `trim_fraction` (e.g., 0.2 = drop worst 20%), and sums the rest.
/// This makes the optimizer robust to outlier matches that survive RANSAC.
pub fn trimmed_seam_weighted_reprojection_error(
    points: &[MatchedPoint],
    params: &OptParams,
    sigma: f64,
    sigma_y: f64,
    trim_fraction: f64,
) -> f64 {
    if points.is_empty() {
        return 0.0;
    }
    let mut errors = per_point_seam_weighted_errors(points, params, sigma, sigma_y);
    errors.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));

    let keep = ((1.0 - trim_fraction) * errors.len() as f64).ceil() as usize;
    let keep = keep.min(errors.len()).max(1);
    errors[..keep].iter().sum()
}

/// Normalize a pixel coordinate to plane coordinates.
///
/// Matches v1's `_normalize_to_plane_coords`: x maps to `[-0.5, 0.5]`,
/// y maps to `[-h/(2w), h/(2w)]` preserving the image aspect ratio.
pub fn normalize_to_plane(px: f64, py: f64, img_w: u32, img_h: u32) -> [f64; 2] {
    debug_assert!(img_w > 0 && img_h > 0, "image dimensions must be nonzero");
    let w = img_w.max(1) as f64;
    let h = img_h.max(1) as f64;
    [
        (px / w - 0.5) * PLANE_WIDTH,
        (py / h - 0.5) * PLANE_WIDTH * (h / w),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use approx::assert_abs_diff_eq;

    #[test]
    fn to_3d_x_plane_maps_correctly() {
        let p = to_3d_x_plane([0.3, 0.1]);
        assert_abs_diff_eq!(p.x, 0.3, epsilon = 1e-10);
        assert_abs_diff_eq!(p.y, -0.1, epsilon = 1e-10);
        assert_abs_diff_eq!(p.z, 0.0, epsilon = 1e-10);
    }

    #[test]
    fn to_3d_z_plane_maps_correctly() {
        let p = to_3d_z_plane([0.2, 0.05]);
        assert_abs_diff_eq!(p.x, 0.0, epsilon = 1e-10);
        assert_abs_diff_eq!(p.y, -0.05, epsilon = 1e-10);
        assert_abs_diff_eq!(p.z, -0.2, epsilon = 1e-10);
    }

    #[test]
    fn identity_rotation_is_identity() {
        let r = rotation_matrix(0.0, 0.0, 0.0);
        let identity = Matrix3::identity();
        for i in 0..3 {
            for j in 0..3 {
                assert_abs_diff_eq!(r[(i, j)], identity[(i, j)], epsilon = 1e-10);
            }
        }
    }

    #[test]
    fn rotation_z_90_degrees() {
        let r = rotation_matrix(0.0, 0.0, std::f64::consts::FRAC_PI_2);
        let v = Vector3::new(1.0, 0.0, 0.0);
        let rotated = r * v;
        assert_abs_diff_eq!(rotated.x, 0.0, epsilon = 1e-10);
        assert_abs_diff_eq!(rotated.y, 1.0, epsilon = 1e-10);
        assert_abs_diff_eq!(rotated.z, 0.0, epsilon = 1e-10);
    }

    #[test]
    fn normalize_center_pixel_to_origin() {
        let [x, y] = normalize_to_plane(960.0, 540.0, 1920, 1080);
        assert_abs_diff_eq!(x, 0.0, epsilon = 1e-10);
        assert_abs_diff_eq!(y, 0.0, epsilon = 1e-10);
    }

    #[test]
    fn normalize_top_left_pixel() {
        let [x, y] = normalize_to_plane(0.0, 0.0, 1920, 1080);
        assert_abs_diff_eq!(x, -0.5, epsilon = 1e-10);
        // y = (0/1080 - 0.5) * (1080/1920) = -0.5 * 0.5625 = -0.28125
        assert_abs_diff_eq!(y, -0.28125, epsilon = 1e-10);
    }

    #[test]
    fn reprojection_error_zero_for_perfect_alignment() {
        // With full overlap (intersect=1), both planes meet at origin.
        // Points at (0,0) on both planes map to (0,0,0) in 3D.
        // Both rays from camera hit the same point, so reprojection error = 0.
        let points = vec![MatchedPoint::from_planes([0.0, 0.0], [0.0, 0.0])];
        let params = OptParams {
            x_ty: 0.0,
            intersect: 1.0,
            cam_d: 0.25,
            x_rz: 0.0,
            z_rx: 0.0,
            z_rz: None,
            x_rx: None,
            ground_tilt_x: None,
            ground_tilt_z: None,
            k_x: 1.0,
            k_z: 1.0,
        };
        let err = reprojection_error(&points, &params);
        assert_abs_diff_eq!(err, 0.0, epsilon = 1e-6);

        // Angular error should also be zero
        let ang = angular_error(&points, &params);
        assert_abs_diff_eq!(ang, 0.0, epsilon = 1e-6);
    }

    #[test]
    fn reprojection_error_increases_with_misalignment() {
        let points = vec![
            MatchedPoint::from_planes([0.1, 0.0], [0.1, 0.0]),
            MatchedPoint::from_planes([0.2, 0.05], [0.2, 0.05]),
        ];

        let good_params = OptParams {
            x_ty: 0.0,
            intersect: 0.5,
            cam_d: 0.25,
            x_rz: 0.0,
            z_rx: 0.0,
            z_rz: None,
            x_rx: None,
            ground_tilt_x: None,
            ground_tilt_z: None,
            k_x: 1.0,
            k_z: 1.0,
        };

        let bad_params = OptParams {
            x_ty: 0.3,
            intersect: 0.5,
            cam_d: 0.25,
            x_rz: 0.0,
            z_rx: 0.0,
            z_rz: None,
            x_rx: None,
            ground_tilt_x: None,
            ground_tilt_z: None,
            k_x: 1.0,
            k_z: 1.0,
        };

        let good_err = reprojection_error(&points, &good_params);
        let bad_err = reprojection_error(&points, &bad_params);
        assert!(
            bad_err > good_err,
            "misaligned params should have higher error: {bad_err} vs {good_err}"
        );
    }

    #[test]
    fn reprojection_error_and_angular_error_agree_on_ordering() {
        // Both metrics should agree that good params are better than bad.
        let points = vec![
            MatchedPoint::from_planes([0.1, 0.0], [0.1, 0.0]),
            MatchedPoint::from_planes([-0.1, 0.05], [-0.1, 0.05]),
        ];

        let good = OptParams {
            x_ty: 0.0,
            intersect: 0.5,
            cam_d: 0.25,
            x_rz: 0.0,
            z_rx: 0.0,
            z_rz: None,
            x_rx: None,
            ground_tilt_x: None,
            ground_tilt_z: None,
            k_x: 1.0,
            k_z: 1.0,
        };

        let bad = OptParams {
            x_ty: 0.3,
            intersect: 0.5,
            cam_d: 0.25,
            x_rz: 0.2,
            z_rx: 0.0,
            z_rz: None,
            x_rx: None,
            ground_tilt_x: None,
            ground_tilt_z: None,
            k_x: 1.0,
            k_z: 1.0,
        };

        let reproj_good = reprojection_error(&points, &good);
        let reproj_bad = reprojection_error(&points, &bad);
        let ang_good = angular_error(&points, &good);
        let ang_bad = angular_error(&points, &bad);

        assert!(reproj_bad > reproj_good);
        assert!(ang_bad > ang_good);
    }

    #[test]
    fn param_pack_unpack_roundtrip_5() {
        let params = OptParams {
            x_ty: 0.01,
            intersect: 0.55,
            cam_d: 0.24,
            x_rz: 0.008,
            z_rx: -0.004,
            z_rz: None,
            x_rx: None,
            ground_tilt_x: None,
            ground_tilt_z: None,
            k_x: 1.0,
            k_z: 1.0,
        };
        let packed = params.to_5param();
        let unpacked = OptParams::from_5param(&packed);
        assert_abs_diff_eq!(unpacked.x_ty, params.x_ty, epsilon = 1e-15);
        assert_abs_diff_eq!(unpacked.intersect, params.intersect, epsilon = 1e-15);
        assert_abs_diff_eq!(unpacked.cam_d, params.cam_d, epsilon = 1e-15);
    }

    #[test]
    fn param_pack_unpack_roundtrip_6() {
        let params = OptParams {
            x_ty: 0.01,
            intersect: 0.55,
            cam_d: 0.24,
            x_rz: 0.008,
            z_rx: -0.004,
            z_rz: Some(0.003),
            x_rx: None,
            ground_tilt_x: None,
            ground_tilt_z: None,
            k_x: 1.0,
            k_z: 1.0,
        };
        let packed = params.to_6param();
        let unpacked = OptParams::from_6param(&packed);
        assert_abs_diff_eq!(
            unpacked.z_rz.unwrap(),
            params.z_rz.unwrap(),
            epsilon = 1e-15
        );
    }

    #[test]
    fn seam_columns_matches_hand_derivation() {
        // Same values the seam-weighting test below already relies on via
        // hand-derived comments - now backed by the extracted helper.
        let seams = seam_columns(0.5);
        assert_abs_diff_eq!(seams.right_camera_seam_nx, 0.25, epsilon = 1e-12);
        assert_abs_diff_eq!(seams.left_camera_seam_nx, 0.75, epsilon = 1e-12);
    }

    #[test]
    fn seam_weighted_error_weights_near_seam_higher() {
        // Two identical points with different pixel positions: one near
        // the stitch seam, one far from it. The seam-weighted error
        // should give more weight to the near-seam point.
        let params = OptParams {
            x_ty: 0.0,
            intersect: 0.5,
            cam_d: 0.25,
            x_rz: 0.0,
            z_rx: 0.0,
            z_rz: None,
            x_rx: None,
            ground_tilt_x: None,
            ground_tilt_z: None,
            k_x: 1.0,
            k_z: 1.0,
        };

        // With intersect=0.5:
        //   right_cam_seam = 0.25 (compared to left_pixel_nx, which is RIGHT cam px)
        //   left_cam_seam  = 0.75 (compared to right_pixel_nx, which is LEFT cam px)
        //
        // Point near the seam: right cam pixel at 0.25, left cam pixel at 0.75
        let near_seam = MatchedPoint {
            left: [0.1, 0.0],
            right: [0.1, 0.0],
            left_pixel_nx: 0.25,  // right cam px, at right_cam_seam
            right_pixel_nx: 0.75, // left cam px, at left_cam_seam
        };

        // Same plane coords but pixel position far from seam
        let far_from_seam = MatchedPoint {
            left: [0.1, 0.0],
            right: [0.1, 0.0],
            left_pixel_nx: 0.8,  // right cam px, far from right_cam_seam (0.25)
            right_pixel_nx: 0.2, // left cam px, far from left_cam_seam (0.75)
        };

        let sigma = 0.08;

        // With any non-zero raw error (which these points have due to
        // geometry), the near-seam point should contribute more because
        // its Gaussian weight is ~1.0 vs ~0 for the far point.
        let err_near = seam_weighted_reprojection_error(&[near_seam], &params, sigma, sigma);
        let err_far = seam_weighted_reprojection_error(&[far_from_seam], &params, sigma, sigma);

        assert!(
            err_near > err_far,
            "near-seam point should contribute more error: {err_near} vs {err_far}"
        );
        // The far-from-seam point should be almost zero-weighted
        assert!(
            err_far < err_near * 1e-6,
            "far point should be near-zero weighted"
        );
    }

    #[test]
    fn warp_ground_y_identity_at_zero() {
        for t in [-0.4, -0.1, 0.0, 0.05, 0.2, 0.45] {
            assert_abs_diff_eq!(warp_ground_y(t, 0.0, 1.0), t, epsilon = 1e-15);
        }
    }

    #[test]
    fn warp_ground_y_monotonic_in_c() {
        // For a fixed near-field t, warp(t, c) should move monotonically
        // as c increases from 0 (this is what lets an optimizer actually
        // climb a gradient instead of hitting a flat or oscillating cost).
        let t = 0.2;
        let mut prev = warp_ground_y(t, 0.0, 1.0);
        for c in [0.05, 0.1, 0.15, 0.2] {
            let cur = warp_ground_y(t, c, 1.0);
            assert!(
                cur > prev,
                "warp should increase monotonically with c: c={c} gave {cur} <= prev {prev}"
            );
            prev = cur;
        }
    }

    #[test]
    fn warp_ground_y_guards_against_singularity() {
        // The horizon-direction singularity is at t = k/c (denominator
        // hits zero). Must return a large-but-finite value, never NaN/inf.
        let c = 2.0;
        let k = 1.0;
        let t = k / c;
        let warped = warp_ground_y(t, c, k);
        assert!(warped.is_finite(), "expected finite value, got {warped}");
    }

    #[test]
    fn warp_ground_y_matches_true_angle_roundtrip_for_nonunit_k() {
        // Direct check of the derivation in warp_ground_y's doc comment:
        // convert t to a true angle via t/k, add the extra tilt theta,
        // convert back by multiplying by k. This is the property that
        // distinguishes the corrected (2026-07-05) formula from the
        // original one, which only matched this derivation when k == 1.0
        // (this project's real DJI Osmo Action 4 rig has k ~= 0.19, not
        // 1.0 - see FRICTION.md's "k-scaling bug" entry).
        let k = 0.19; // representative of this project's real rig
        let theta = 0.1_f64;
        let c = theta.tan();
        for t in [-0.15_f64, -0.05, 0.0, 0.05, 0.15] {
            let phi = (t / k).atan();
            let expected = k * (theta + phi).tan();
            let actual = warp_ground_y(t, c, k);
            assert_abs_diff_eq!(actual, expected, epsilon = 1e-12);
        }
    }

    #[test]
    fn warp_ground_y_k_matters_reprojection_changes_with_k() {
        // Regression guard for the k-scaling bug itself: prove that using
        // the correct k (rather than silently defaulting to 1.0, as every
        // OptParams literal before this fix implicitly did) changes the
        // warped value for a realistic near-field t and nonzero c - i.e.
        // k is not a no-op default that happens to cancel out.
        let t = 0.12;
        let c = 0.1;
        let with_k1 = warp_ground_y(t, c, 1.0);
        let with_k_real = warp_ground_y(t, c, 0.19);
        assert!(
            (with_k1 - with_k_real).abs() > 1e-6,
            "k should change the warped value: k=1.0 -> {with_k1}, k=0.19 -> {with_k_real}"
        );
    }

    #[test]
    fn band_limited_warp_is_identity_below_band_start() {
        // The key property that fixes the global-shift failure mode found
        // in the unbanded ground_tilt experiment (FRICTION.md): points at
        // or below GROUND_TILT_BAND_START must be mathematically
        // untouched, for any c, not just approximately close.
        for t in [0.0, 0.02, -0.05, 0.079, -0.08] {
            for c in [-0.3, -0.1, 0.1, 0.3] {
                let warped = band_limited_ground_warp(t, c, 1.0);
                assert_abs_diff_eq!(warped, t, epsilon = 1e-12);
            }
        }
    }

    #[test]
    fn band_limited_warp_reaches_full_strength_beyond_band_full() {
        // At and beyond GROUND_TILT_BAND_FULL (positive t = near field),
        // the band-limited warp should match the raw (unbanded)
        // warp_ground_y exactly.
        let c = 0.15;
        for t in [0.16, 0.2, 0.25] {
            let banded = band_limited_ground_warp(t, c, 1.0);
            let raw = warp_ground_y(t, c, 1.0);
            assert_abs_diff_eq!(banded, raw, epsilon = 1e-12);
        }
    }

    #[test]
    fn band_limited_warp_is_one_sided_not_symmetric_in_abs_t() {
        // Regression guard for the bug caught by rendering a real frame
        // (examples/verify_ground_tilt.rs, FRICTION.md point 20's wiring
        // entry): t <= 0 (horizon and sky, in this project's plane-y
        // convention) must stay identity no matter how large |t| is or
        // what c is - never ramp back up to full strength the way a
        // naive `smoothstep(..., t.abs())` would beyond -BAND_FULL. Every
        // matched point this has ever been fitted against is real ground
        // content with t > 0, so this direction was never exercised by
        // fitting - only by rendering every pixel, including sky.
        for t in [-0.16, -0.2, -0.5, -1.0] {
            for c in [-0.3, -0.09, 0.09, 0.3, 1.18] {
                let warped = band_limited_ground_warp(t, c, 0.1897);
                assert_abs_diff_eq!(warped, t, epsilon = 1e-12);
            }
        }
    }

    #[test]
    fn band_limited_warp_identity_at_zero_c() {
        for t in [0.0, 0.05, 0.1, 0.15, 0.2, -0.18] {
            assert_abs_diff_eq!(band_limited_ground_warp(t, 0.0, 1.0), t, epsilon = 1e-15);
        }
    }

    #[test]
    fn band_limited_warp_transitions_smoothly() {
        // Inside the band, the result should lie strictly between the
        // identity and the full warp - no discontinuity at either edge.
        let c = 0.2;
        let t = 0.12; // midway between BAND_START (0.08) and BAND_FULL (0.16)
        let banded = band_limited_ground_warp(t, c, 1.0);
        let raw = warp_ground_y(t, c, 1.0);
        assert!(
            (banded - t).abs() < (raw - t).abs(),
            "mid-band value {banded} should be a partial blend, not the full warp {raw}"
        );
        assert!(
            (banded - t).abs() > 1e-9,
            "mid-band value {banded} should differ from identity {t}"
        );
    }

    // Step: evaluate band_limited_ground_warp on a real GPU via wgpu compute
    // dispatch and compare against this file's f64 Rust canonical. The
    // shader body below is a deliberate SYNC_WITH copy of
    // `reco-core/src/shaders/fisheye.wgsl`'s smoothstep_band/warp_ground_y/
    // band_limited_ground_warp - this is the production render's actual
    // ground-tilt implementation, not a reimplementation for testing.
    // Mirrors reco-core's `wgsl_kb4_matches_rust_kb4_on_theta_grid` (same
    // rationale: WGSL and Rust can't cross-language-link, so a real
    // GPU dispatch is the only way to lock the two numerically together).
    #[test]
    fn wgsl_ground_warp_matches_rust_on_grid() {
        use wgpu::util::DeviceExt;

        let gpu = match pollster::block_on(reco_core::gpu::GpuContext::new()) {
            Ok(ctx) => ctx,
            Err(
                reco_core::gpu::GpuError::NoAdapter | reco_core::gpu::GpuError::AdapterRequest(_),
            ) => {
                eprintln!("Skipping: no GPU adapter available");
                return;
            }
            Err(e) => panic!("Unexpected GPU error: {e}"),
        };
        let device = gpu.device();
        let queue = gpu.queue();

        // Grid covering: inside the untouched band, the ramp zone, fully
        // beyond the band, both signs of t, and c values spanning realistic
        // fitted values (FRICTION.md point 20: -0.09/-0.085) up to the
        // point-19 single-line overfit (-1.18) and its sign-flip, at two
        // different focal-scale constants (the real Werkplaats k, and 1.0).
        let ts: Vec<f32> = (-15..=15).map(|i| i as f32 * 0.02).collect(); // -0.30..=0.30
        let cs: [f32; 7] = [-1.18, -0.5, -0.09, 0.0, 0.085, 0.5, 1.18];
        let ks: [f32; 2] = [0.1897, 1.0];

        let mut t_in = Vec::new();
        let mut c_in = Vec::new();
        let mut k_in = Vec::new();
        for &k in &ks {
            for &c in &cs {
                for &t in &ts {
                    t_in.push(t);
                    c_in.push(c);
                    k_in.push(k);
                }
            }
        }
        let count = t_in.len() as u32;

        #[repr(C)]
        #[derive(Copy, Clone, bytemuck::Pod, bytemuck::Zeroable)]
        struct Uniforms {
            count: u32,
            _pad: [u32; 3],
        }
        let uniforms = Uniforms {
            count,
            _pad: [0; 3],
        };

        let shader_source = r#"
struct Uniforms { count: u32, _pad0: u32, _pad1: u32, _pad2: u32 }
@group(0) @binding(0) var<uniform> u: Uniforms;
@group(0) @binding(1) var<storage, read> t_in: array<f32>;
@group(0) @binding(2) var<storage, read> c_in: array<f32>;
@group(0) @binding(3) var<storage, read> k_in: array<f32>;
@group(0) @binding(4) var<storage, read_write> out: array<f32>;

const GROUND_TILT_BAND_START: f32 = 0.08;
const GROUND_TILT_BAND_FULL: f32 = 0.16;

fn smoothstep_band(edge0: f32, edge1: f32, x: f32) -> f32 {
    let t = clamp((x - edge0) / (edge1 - edge0), 0.0, 1.0);
    return t * t * (3.0 - 2.0 * t);
}

// SYNC_WITH shaders/fisheye.wgsl's warp_ground_y
fn warp_ground_y(t: f32, c: f32, k: f32) -> f32 {
    var denom = 1.0 - c * t / k;
    if abs(denom) < 1e-9 {
        denom = select(-1e-9, 1e-9, denom >= 0.0);
    }
    return (k * c + t) / denom;
}

// SYNC_WITH shaders/fisheye.wgsl's band_limited_ground_warp
fn band_limited_ground_warp(t: f32, c: f32, k: f32) -> f32 {
    if t <= 0.0 {
        return t;
    }
    let weight = smoothstep_band(GROUND_TILT_BAND_START, GROUND_TILT_BAND_FULL, t);
    if weight == 0.0 {
        return t;
    }
    return t + weight * (warp_ground_y(t, c, k) - t);
}

@compute @workgroup_size(64)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let i = gid.x;
    if (i >= u.count) { return; }
    out[i] = band_limited_ground_warp(t_in[i], c_in[i], k_in[i]);
}
"#;

        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("ground_warp_agreement_shader"),
            source: wgpu::ShaderSource::Wgsl(shader_source.into()),
        });

        let bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("ground_warp_agreement_bgl"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Storage { read_only: true },
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Storage { read_only: true },
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 3,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Storage { read_only: true },
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 4,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Storage { read_only: false },
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
            ],
        });

        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("ground_warp_agreement_pl"),
            bind_group_layouts: &[&bgl],
            immediate_size: 0,
        });

        let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("ground_warp_agreement_pipeline"),
            layout: Some(&pipeline_layout),
            module: &module,
            entry_point: Some("main"),
            compilation_options: Default::default(),
            cache: None,
        });

        let uniform_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("ground_warp_uniform"),
            contents: bytemuck::bytes_of(&uniforms),
            usage: wgpu::BufferUsages::UNIFORM,
        });
        let t_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("ground_warp_t_in"),
            contents: bytemuck::cast_slice(&t_in),
            usage: wgpu::BufferUsages::STORAGE,
        });
        let c_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("ground_warp_c_in"),
            contents: bytemuck::cast_slice(&c_in),
            usage: wgpu::BufferUsages::STORAGE,
        });
        let k_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("ground_warp_k_in"),
            contents: bytemuck::cast_slice(&k_in),
            usage: wgpu::BufferUsages::STORAGE,
        });

        let output_size = (count as u64) * std::mem::size_of::<f32>() as u64;
        let output_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("ground_warp_output"),
            size: output_size,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
            mapped_at_creation: false,
        });
        let staging_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("ground_warp_staging"),
            size: output_size,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });

        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("ground_warp_bind_group"),
            layout: &bgl,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: uniform_buffer.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: t_buffer.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: c_buffer.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: k_buffer.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 4,
                    resource: output_buffer.as_entire_binding(),
                },
            ],
        });

        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("ground_warp_encoder"),
        });
        {
            let mut cpass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("ground_warp_pass"),
                timestamp_writes: None,
            });
            cpass.set_pipeline(&pipeline);
            cpass.set_bind_group(0, &bind_group, &[]);
            let groups = count.div_ceil(64);
            cpass.dispatch_workgroups(groups, 1, 1);
        }
        encoder.copy_buffer_to_buffer(&output_buffer, 0, &staging_buffer, 0, output_size);
        queue.submit(std::iter::once(encoder.finish()));

        let buffer_slice = staging_buffer.slice(..);
        let (tx, rx) = std::sync::mpsc::channel();
        buffer_slice.map_async(wgpu::MapMode::Read, move |r| tx.send(r).unwrap());
        device
            .poll(wgpu::PollType::wait_indefinitely())
            .expect("poll should not fail on a well-formed dispatch");
        rx.recv().unwrap().expect("buffer should map successfully");

        let gpu_out: Vec<f32> = {
            let data = buffer_slice.get_mapped_range();
            bytemuck::cast_slice::<u8, f32>(&data).to_vec()
        };
        staging_buffer.unmap();

        let mut checked = 0;
        for i in 0..count as usize {
            let (t, c, k) = (t_in[i] as f64, c_in[i] as f64, k_in[i] as f64);
            let rust = band_limited_ground_warp(t, c, k);
            let diff = (gpu_out[i] as f64 - rust).abs();
            // warp_ground_y has a genuine pole (denom -> 0); near it, the
            // *output* magnitude grows large, so a fixed absolute tolerance
            // fails even though f32 vs f64 agree to full single-precision
            // accuracy. Relative-or-absolute (whichever is looser) is the
            // standard way to check a function whose output spans orders
            // of magnitude - tight near zero, appropriately loose near the
            // pole, without needing to special-case denom at all.
            let tol = 1e-4 + 1e-3 * rust.abs();
            assert!(
                diff < tol,
                "t={t} c={c} k={k}: GPU {} vs Rust {rust} (diff {diff}, tol {tol})",
                gpu_out[i]
            );
            checked += 1;
        }
        assert_eq!(checked, count as usize);
    }

    #[test]
    fn ground_tilt_x_and_z_change_reprojection_error_independently() {
        // Regression guard for the x_rx-style silent no-op bug: prove
        // ground_tilt_x/ground_tilt_z actually participate in the cost
        // function instead of being parameters the optimizer could set to
        // anything with zero effect on what it's minimizing - and that
        // each affects the result on its own, not just when combined.
        let points = vec![
            MatchedPoint::from_planes([0.1, 0.15], [0.12, 0.16]),
            MatchedPoint::from_planes([-0.15, 0.2], [-0.13, 0.22]),
            MatchedPoint::from_planes([0.05, -0.1], [0.06, -0.11]),
        ];
        let base = OptParams {
            x_ty: 0.0,
            intersect: 0.5,
            cam_d: 0.25,
            x_rz: 0.0,
            z_rx: 0.0,
            z_rz: None,
            x_rx: None,
            ground_tilt_x: None,
            ground_tilt_z: None,
            k_x: 1.0,
            k_z: 1.0,
        };
        let with_x = OptParams {
            ground_tilt_x: Some(0.3),
            ..base
        };
        let with_z = OptParams {
            ground_tilt_z: Some(0.3),
            k_x: 1.0,
            k_z: 1.0,
            ..base
        };
        let err_base = reprojection_error(&points, &base);
        let err_x = reprojection_error(&points, &with_x);
        let err_z = reprojection_error(&points, &with_z);
        assert!(
            (err_base - err_x).abs() > 1e-12,
            "ground_tilt_x should change reprojection error: base={err_base} x={err_x}"
        );
        assert!(
            (err_base - err_z).abs() > 1e-12,
            "ground_tilt_z should change reprojection error: base={err_base} z={err_z}"
        );
        // The two shouldn't happen to produce an identical effect - if
        // they did, that would suggest one of them isn't actually wired
        // to its own plane.
        assert!(
            (err_x - err_z).abs() > 1e-12,
            "ground_tilt_x and ground_tilt_z affect different planes and should not produce \
             identical error: x={err_x} z={err_z}"
        );
    }

    #[test]
    fn x_rx_changes_reprojection_error() {
        // Regression guard for the bug this change fixes: x_rx was
        // declared on OptParams and applied at render time
        // (SceneGeometry::from_layout_with_aspect) but never read inside
        // apply_transformations, so the CPU cost function had zero
        // gradient with respect to it even when enable_x_rx was set.
        let points = vec![
            MatchedPoint::from_planes([0.1, 0.15], [0.12, 0.16]),
            MatchedPoint::from_planes([-0.15, 0.2], [-0.13, 0.22]),
        ];
        let base = OptParams {
            x_ty: 0.0,
            intersect: 0.5,
            cam_d: 0.25,
            x_rz: 0.0,
            z_rx: 0.0,
            z_rz: None,
            x_rx: None,
            ground_tilt_x: None,
            ground_tilt_z: None,
            k_x: 1.0,
            k_z: 1.0,
        };
        let with_x_rx = OptParams {
            x_rx: Some(0.15),
            ..base
        };
        let err_base = reprojection_error(&points, &base);
        let err_x_rx = reprojection_error(&points, &with_x_rx);
        assert!(
            (err_base - err_x_rx).abs() > 1e-12,
            "x_rx should change reprojection error: base={err_base} with_x_rx={err_x_rx}"
        );
    }
}
