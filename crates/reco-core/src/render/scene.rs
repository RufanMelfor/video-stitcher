//! 3D scene model: two camera planes and a virtual camera.
//!
//! Replicates the v1 Three.js geometric model in Rust. Two textured
//! planes are arranged in an L-shape, and a virtual camera at the
//! corner renders the stitched panoramic view.
//!
//! ## Coordinate System
//!
//! ```text
//!          Z (up in plane space)
//!          │
//!          │  ┌──────────┐
//!          │  │  Left     │  (X-Z plane, faces +X direction)
//!          │  │  Camera   │
//!          └──┼──────────┐│──── X
//!             │  Right   ││
//!             │  Camera  ││
//!             └──────────┘│
//!                         │
//!   Camera at [d, 0, d] where d = framing.axis_offset
//! ```

use crate::calibration::{Framing, Topology};
use nalgebra::{Matrix4, Translation3, UnitQuaternion};

/// Computed 3D positions and rotations for the two camera planes.
///
/// Derived from a [`Topology`] by applying the intersection offset
/// and rotation corrections.
#[derive(Debug, Clone)]
pub struct SceneGeometry {
    /// Left plane position `[x, y, z]`.
    pub left_position: [f32; 3],
    /// Left plane rotation `[rx, ry, rz]` in radians.
    pub left_rotation: [f32; 3],
    /// Right plane position `[x, y, z]`.
    pub right_position: [f32; 3],
    /// Right plane rotation `[rx, ry, rz]` in radians.
    pub right_rotation: [f32; 3],
    /// Virtual camera position `[x, y, z]`.
    pub camera_position: [f32; 3],
    /// Plane width (normalized to 1.0).
    pub plane_width: f32,
    /// Plane aspect ratio (width / height), default 16:9.
    pub plane_aspect: f32,
    /// Local-space X of the left plane's seam-adjacent edge - where
    /// `z_rx`/`z_rz` pivot instead of the plane's own local origin, so a
    /// manual roll/tilt correction doesn't also drag the visible seam
    /// across the screen. See `model_matrix_left`'s doc for why this
    /// specific formula.
    left_seam_pivot_x: f32,
    /// Local-space X of the right plane's seam-adjacent edge - same
    /// purpose as `left_seam_pivot_x`, for `x_rx`/`x_rz`.
    right_seam_pivot_x: f32,
}

impl SceneGeometry {
    /// Derive the 3D scene geometry from the calibration's topology + framing.
    ///
    /// `aspect` is the source frame `width / height`. Mirrors the v1 plane
    /// positioning:
    /// - Left plane: `position = [0, 0, (w/2)(1 - intersect)]`, `rotation = [z_rx, π/2, z_rz]`
    /// - Right plane: `position = [(w/2)(1 - intersect), x_ty, 0]`, `rotation = [x_rx, 0, x_rz]`
    /// - Virtual camera at `[axis_offset, 0, axis_offset]`.
    pub fn new(topology: &Topology, framing: &Framing, aspect: f32) -> Self {
        let plane_width: f32 = 1.0;
        let half_offset = (plane_width / 2.0) * (1.0 - topology.intersect as f32);
        let axis = framing.axis_offset as f32;

        // Same formula as `renderer::seam_line_screen_points`'s `local_x`
        // (see its own comment for the derivation from the shader's
        // extended-uv seam test) - each plane's own mapping from
        // `seam_offset` to where its seam-adjacent edge sits in local
        // vertex space, independent of which plane currently fades.
        let seam_offset = topology.seam_offset;
        let left_seam_pivot_x = (0.5 - seam_offset) / 2.0;
        let right_seam_pivot_x = (seam_offset - 0.5) / 2.0;

        Self {
            left_position: [0.0, 0.0, half_offset],
            left_rotation: [
                topology.z_rx as f32,
                std::f32::consts::FRAC_PI_2,
                topology.z_rz as f32,
            ],
            right_position: [half_offset, topology.x_ty as f32, 0.0],
            right_rotation: [topology.x_rx as f32, 0.0, topology.x_rz as f32],
            camera_position: [axis, 0.0, axis],
            plane_width,
            plane_aspect: aspect,
            left_seam_pivot_x,
            right_seam_pivot_x,
        }
    }

    /// Model matrix for the left camera plane.
    ///
    /// The z-plane base rotation is π/2 around Y (faces sideways) - this
    /// is mandatory L-shape geometry, not a correction, so unlike `z_rx`/
    /// `z_rz` it always stays anchored at the plane's own local origin,
    /// applied innermost (closest to the raw vertex). `z_rx` is applied as
    /// a post-rotation around X so it acts as a roll around the plane's
    /// final normal. `z_rz` is applied as a pre-rotation (tilt correction).
    ///
    /// The `z_rx`/`z_rz` correction pivots around `left_seam_pivot_x` (at
    /// mid-height, y=0) instead of the plane's own local origin - pivoting
    /// at the origin would swing the seam-adjacent edge across the screen
    /// for any manual nudge, which is confusing when the whole point of
    /// those sliders is a small roll/tilt correction, not a seam
    /// reposition (that's what `seam_offset` is for). Pivoting here keeps
    /// the seam's mid-height point fixed; top/bottom still swing somewhat
    /// under a tilt (`z_rx`), same as tilting any real rigid plane around
    /// a point on it - a further Y-adjustable pivot would remove that too
    /// but isn't implemented yet. At `z_rx = z_rz = 0` the pivot sandwich
    /// is the identity, so this is exactly the pre-pivot geometry.
    pub fn model_matrix_left(&self) -> Matrix4<f32> {
        let t = Translation3::new(
            self.left_position[0],
            self.left_position[1],
            self.left_position[2],
        );
        // Mandatory orientation, never pivoted.
        let base = UnitQuaternion::from_euler_angles(0.0, self.left_rotation[1], 0.0);
        // z_rz tilt correction (applied here as a pre-rotation, same as
        // the original single combined-Euler construction).
        let tilt = UnitQuaternion::from_euler_angles(0.0, 0.0, self.left_rotation[2]);
        // z_rx roll correction (post-rotation, around the plane's final normal).
        let roll = UnitQuaternion::from_euler_angles(self.left_rotation[0], 0.0, 0.0);
        let correction = roll * tilt;
        let pivot = Translation3::new(self.left_seam_pivot_x, 0.0, 0.0);
        t.to_homogeneous()
            * pivot.to_homogeneous()
            * correction.to_homogeneous()
            * pivot.inverse().to_homogeneous()
            * base.to_homogeneous()
    }

    /// Model matrix for the right camera plane.
    ///
    /// See [`Self::model_matrix_left`]'s doc for why the rotation pivots
    /// around `right_seam_pivot_x` instead of the plane's local origin.
    pub fn model_matrix_right(&self) -> Matrix4<f32> {
        let t = Translation3::new(
            self.right_position[0],
            self.right_position[1],
            self.right_position[2],
        );
        let r = UnitQuaternion::from_euler_angles(
            self.right_rotation[0],
            self.right_rotation[1],
            self.right_rotation[2],
        );
        let pivot = Translation3::new(self.right_seam_pivot_x, 0.0, 0.0);
        t.to_homogeneous()
            * pivot.to_homogeneous()
            * r.to_homogeneous()
            * pivot.inverse().to_homogeneous()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::calibration::{Framing, Topology};

    fn topo(intersect: f64) -> Topology {
        Topology {
            intersect,
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
            ground_tilt_x: 0.0,
            ground_tilt_z: 0.0,
            top_tilt_x: 0.0,
            top_tilt_z: 0.0,
            ground_tilt_band_width: 0.16,
            top_tilt_band_width: 0.16,
        }
    }

    fn framing(axis_offset: f64) -> Framing {
        Framing {
            axis_offset,
            tilt: 0.0,
            roll: 0.0,
        }
    }

    #[test]
    fn geometry_from_default_layout() {
        let geom = SceneGeometry::new(&topo(0.5), &framing(0.25), 16.0 / 9.0);

        // Half offset = 0.5 * (1 - 0.5) = 0.25
        assert!((geom.left_position[2] - 0.25).abs() < 1e-5);
        assert!((geom.right_position[0] - 0.25).abs() < 1e-5);
        assert!((geom.camera_position[0] - 0.25).abs() < 1e-5);
        assert!((geom.camera_position[2] - 0.25).abs() < 1e-5);
        assert!((geom.plane_aspect - 16.0 / 9.0).abs() < 1e-5);
    }

    #[test]
    fn geometry_with_corrections() {
        let topology = Topology {
            intersect: 0.55,
            x_ty: 0.005,
            x_rz: 0.008,
            z_rx: -0.004,
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
            ground_tilt_x: 0.0,
            ground_tilt_z: 0.0,
            top_tilt_x: 0.0,
            top_tilt_z: 0.0,
            ground_tilt_band_width: 0.16,
            top_tilt_band_width: 0.16,
        };

        let geom = SceneGeometry::new(&topology, &framing(0.24), 16.0 / 9.0);

        // Right plane should have the x_ty correction
        assert!((geom.right_position[1] - 0.005).abs() < 1e-5);
        // Rotations should be applied
        assert!((geom.right_rotation[2] - 0.008).abs() < 1e-5);
        assert!((geom.left_rotation[0] - (-0.004)).abs() < 1e-5);
    }

    #[test]
    fn geometry_with_custom_aspect() {
        let aspect_4_3 = 4.0 / 3.0;
        let geom = SceneGeometry::new(&topo(0.5), &framing(0.25), aspect_4_3);
        assert!((geom.plane_aspect - aspect_4_3).abs() < 1e-5);
    }
}
