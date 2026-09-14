//! Virtual-camera matrices and clip constants (wgpu-free).
//!
//! The rasterization-side half of the geometry leaf: the view matrix the
//! render pose feeds, the projection-space correction, and the clip
//! planes - shared verbatim by the GPU pipeline and the CPU inverse maps
//! so the two executors agree by construction.

use nalgebra::{Matrix4, Point3, UnitQuaternion, Vector3};

/// Near clipping plane for the perspective projection.
pub const NEAR_PLANE: f32 = 0.01;
/// Far clipping plane for the perspective projection.
pub const FAR_PLANE: f32 = 5.0;

/// Build the view matrix for the virtual camera.
///
/// Camera sits at `position` and looks at the origin (corner where the two
/// planes meet) by default. This matches v1 Three.js where the OrbitControls
/// target is `[0, 0, 0]`. `yaw` rotates around Y (left/right from center),
/// `pitch` rotates around X (up/down).
pub fn view_matrix(
    position: &[f32; 3],
    yaw: f32,
    pitch: f32,
    rig_tilt: f32,
    rig_roll: f32,
) -> Matrix4<f32> {
    // One basis for the whole crate: the tilted+rolled reference frame
    // comes from the same rig_frame the pose inverse and the viewport-roll
    // margin computation use, so the three can never drift. The
    // world_to_render_pose round-trip test locks the pair together.
    let cam = super::virtual_camera::VirtualCamera::new(position);
    let eye = Point3::from(cam.eye);
    let (up_frame, rest_forward) = super::rig_correction::rig_frame(&cam, rig_tilt, rig_roll);

    // Yaw rotates around the (tilted+rolled) up axis; pitch around the
    // yaw-rotated right axis.
    let yaw_q = UnitQuaternion::from_axis_angle(&nalgebra::Unit::new_normalize(up_frame), yaw);
    let right = yaw_q * cam.base_right;
    let pitch_q = UnitQuaternion::from_axis_angle(&nalgebra::Unit::new_normalize(right), pitch);
    let rotation = pitch_q * yaw_q;
    let forward = rotation * rest_forward;
    let up = rotation * up_frame;
    let target = Point3::from(eye.coords + forward);
    nalgebra::Isometry3::look_at_rh(&eye, &target, &up).to_homogeneous()
}

/// A viewport's WORLD-space pose (camera basis, position, yaw/pitch/FOV,
/// rig tilt/roll) bundled for [`unproject_screen_to_world`] and
/// [`project_world_to_screen`] - keeps both under
/// `clippy::too_many_arguments`, and lets a caller build it once and
/// reuse it for several screen<->world conversions against the same
/// live pose (e.g. hit-testing both bounds of an on-screen drag
/// editor). Mirrors why `detect::panner::DispatchContext` exists.
pub struct ScreenProjection<'a> {
    /// The camera basis (`VirtualCamera::new(&position)` - callers
    /// typically already have both).
    pub cam: &'a super::virtual_camera::VirtualCamera,
    /// World-space camera position (`scene.camera_position`).
    pub position: [f32; 3],
    /// Viewport center world-space yaw (radians).
    pub world_yaw: f32,
    /// Viewport center world-space pitch (radians).
    pub world_pitch: f32,
    /// Vertical field of view (degrees).
    pub fov_v_deg: f32,
    /// Output aspect ratio (width / height).
    pub aspect: f32,
    /// Rig tilt (radians) - see [`view_matrix`].
    pub rig_tilt: f32,
    /// Rig roll (radians) - see [`view_matrix`].
    pub rig_roll: f32,
}

/// Reconstruct the world-space `(yaw, pitch)` a point on-screen
/// ray-projects to, given the viewport's WORLD-space pose (yaw, pitch,
/// vertical FOV) and aspect ratio it was rendered at.
///
/// `sx`/`sy` are normalized device coordinates in `[-1, 1]` - screen
/// center is `(0, 0)`, right/up positive. Translate a `[0,1]`
/// top-left-origin UI fraction `(u, v)` first: `sx = 2*u - 1`,
/// `sy = 1 - 2*v` (screen `v` grows downward, world `sy` grows upward).
///
/// The inverse of the forward corner-projection this same combination
/// (`view_matrix` + an FOV-derived ray + `VirtualCamera::direction_to_yaw_pitch`)
/// already performs elsewhere - see
/// `examples/verify_coverage_corner_gap.rs`'s own from-scratch
/// reimplementation of this exact math, written before this function
/// existed (and which needed two real fixes - a missing world-to-render
/// conversion, then a yaw-sign mixup - before its numbers were
/// trustworthy). Deliberately reuses the real [`view_matrix`] and
/// transposes its rotation block (orthonormal, so transpose is inverse)
/// rather than re-deriving the rotation independently, for the same
/// by-construction-correct reason. For interactive "click/drag a point
/// on the rendered preview to pick a world direction" UIs (e.g. the
/// manual AI-tracking pitch-limit editor).
pub fn unproject_screen_to_world(
    viewport: &ScreenProjection<'_>,
    sx: f32,
    sy: f32,
) -> super::types::ViewportPosition {
    let half_vfov = (viewport.fov_v_deg * 0.5).to_radians();
    let half_hfov = (viewport.aspect * half_vfov.tan()).atan();
    let view_dir = Vector3::new(sx * half_hfov.tan(), sy * half_vfov.tan(), -1.0_f32).normalize();

    let (render_yaw, render_pitch) = super::rig_correction::world_to_render_pose(
        viewport.cam,
        viewport.world_yaw,
        viewport.world_pitch,
        viewport.rig_tilt,
        viewport.rig_roll,
    );
    let view = view_matrix(
        &viewport.position,
        render_yaw,
        render_pitch,
        viewport.rig_tilt,
        viewport.rig_roll,
    );
    let r_t = view.fixed_view::<3, 3>(0, 0).into_owned().transpose();
    let world_dir = (r_t * view_dir).normalize();
    viewport.cam.direction_to_yaw_pitch(&world_dir)
}

/// Forward counterpart of [`unproject_screen_to_world`]: where on screen
/// (normalized device coordinates, `[-1, 1]`, same convention) does a
/// world-space `(target_yaw, target_pitch)` direction project to, given
/// the viewport's own WORLD-space pose and FOV/aspect?
///
/// Returns `None` when the target is behind the camera (dot product
/// with the forward axis is non-positive) - there is no finite screen
/// position for a point behind the eye. For interactive UIs that need
/// to draw a marker/line at a *known* world pitch/yaw over the live
/// preview (the display half of a drag-to-set-a-world-direction
/// editor; see [`unproject_screen_to_world`] for the input half).
///
/// `viewport.world_yaw`/`world_pitch` must be genuinely WORLD-frame
/// (rig-tilt/roll-independent) here - e.g. a panner's raw decision, or
/// live interactive pan/zoom state. A viewport pose that is already
/// RENDER-frame (rig-tilt/roll already baked in - notably
/// `StitchCore::safe_clamp`/`presented_clamped_pose`'s output, which is
/// exactly what `PipelineEvent::PosePresented` carries) must not be fed
/// here: it would double-apply the tilt/roll correction, scattering the
/// projected point upward/off by roughly twice the tilt angle. No
/// current caller needs a render-frame projection (the AI-debug overlay
/// that once did was removed - see `project_render_pose_to_screen_impl`'s
/// git history if a render-frame variant is needed again); reintroduce a
/// `project_render_pose_to_screen` sibling if one does.
pub fn project_world_to_screen(
    viewport: &ScreenProjection<'_>,
    target_world_yaw: f32,
    target_world_pitch: f32,
) -> Option<(f32, f32)> {
    let (render_yaw, render_pitch) = super::rig_correction::world_to_render_pose(
        viewport.cam,
        viewport.world_yaw,
        viewport.world_pitch,
        viewport.rig_tilt,
        viewport.rig_roll,
    );
    project_render_pose_to_screen_impl(
        viewport,
        render_yaw,
        render_pitch,
        target_world_yaw,
        target_world_pitch,
    )
}

/// Shared tail for [`project_world_to_screen`]: build the view matrix
/// from an already-resolved `(render_yaw, render_pitch)` and project a
/// world-space target direction through it. `target_world_yaw`/
/// `target_world_pitch` are always genuine world-space (a target
/// direction, not a viewport pose, is never itself subject to the
/// render-frame distinction above).
fn project_render_pose_to_screen_impl(
    viewport: &ScreenProjection<'_>,
    render_yaw: f32,
    render_pitch: f32,
    target_world_yaw: f32,
    target_world_pitch: f32,
) -> Option<(f32, f32)> {
    let view = view_matrix(
        &viewport.position,
        render_yaw,
        render_pitch,
        viewport.rig_tilt,
        viewport.rig_roll,
    );
    let r = view.fixed_view::<3, 3>(0, 0).into_owned();
    let target_dir = viewport
        .cam
        .yaw_pitch_to_direction(target_world_yaw, target_world_pitch);
    let view_dir = r * target_dir;
    if view_dir.z >= 0.0 {
        return None; // behind the camera (looks down -Z)
    }
    let half_vfov = (viewport.fov_v_deg * 0.5).to_radians();
    let half_hfov = (viewport.aspect * half_vfov.tan()).atan();
    let sx = (view_dir.x / -view_dir.z) / half_hfov.tan();
    let sy = (view_dir.y / -view_dir.z) / half_vfov.tan();
    Some((sx, sy))
}

/// Convert a nalgebra `Matrix4` to column-major `[[f32; 4]; 4]` for wgpu.
#[cfg(feature = "gpu")]
pub(crate) fn matrix4_to_columns(m: &Matrix4<f32>) -> [[f32; 4]; 4] {
    let s = m.as_slice();
    [
        [s[0], s[1], s[2], s[3]],
        [s[4], s[5], s[6], s[7]],
        [s[8], s[9], s[10], s[11]],
        [s[12], s[13], s[14], s[15]],
    ]
}

/// OpenGL to wgpu clip space correction: Z from \[-1,1\] to \[0,1\].
///
/// nalgebra's `Perspective3` uses OpenGL conventions. wgpu expects
/// clip space Z in [0, 1], so we apply this correction.
#[rustfmt::skip]
pub(crate) fn opengl_to_wgpu_matrix() -> Matrix4<f32> {
    Matrix4::new(
        1.0, 0.0, 0.0, 0.0,
        0.0, 1.0, 0.0, 0.0,
        0.0, 0.0, 0.5, 0.5,
        0.0, 0.0, 0.0, 1.0,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn opengl_to_wgpu_maps_z() {
        let m = opengl_to_wgpu_matrix();
        // Point at Z = -1 (OpenGL near) should map to Z = 0 (wgpu near)
        let p = m * nalgebra::Vector4::new(0.0, 0.0, -1.0, 1.0);
        assert!((p.z - (-0.5 + 0.5)).abs() < 1e-5); // -0.5 + 0.5 = 0
        // Point at Z = 1 (OpenGL far) should map to Z = 1 (wgpu far)
        let p = m * nalgebra::Vector4::new(0.0, 0.0, 1.0, 1.0);
        assert!((p.z - 1.0).abs() < 1e-5);
    }

    #[test]
    fn view_matrix_self_consistent_with_direction_to_yaw_pitch() {
        // Step 1e (un-ignored by Step 2's VirtualCamera basis fix):
        // directions synthesized at a known (yaw, pitch), run through
        // direction_to_yaw_pitch, then fed to view_matrix, must
        // transform a point on the dir ray to the camera's -Z axis
        // (the right-hand convention nalgebra::Isometry3::look_at_rh
        // uses).
        //
        // rig_tilt and rig_roll are both zero here: direction_to_yaw_pitch
        // does not take them (Model 4), so any non-zero tilt/roll
        // would break the round-trip by definition. Step 4 lands
        // RigCorrection and unblocks the full (yaw, pitch, tilt, roll)
        // version of this test.
        let camera_position = [0.24_f32, 0.0, 0.24];
        let yaw_steps = [-1.0_f32, -0.5, -0.1, 0.0, 0.1, 0.5, 1.0];
        let pitch_steps = [-0.6_f32, -0.2, 0.0, 0.2, 0.6];

        for &yaw in &yaw_steps {
            for &pitch in &pitch_steps {
                let dir = crate::projection::yaw_pitch_to_direction(yaw, pitch, &camera_position);
                let pos = crate::projection::direction_to_yaw_pitch(&dir, &camera_position);

                let view = view_matrix(&camera_position, pos.yaw, pos.pitch, 0.0, 0.0);

                // A point at eye + dir (unit step along the direction)
                // must land on camera-space -Z at distance 1.
                let target = nalgebra::Vector4::new(
                    camera_position[0] + dir.x,
                    camera_position[1] + dir.y,
                    camera_position[2] + dir.z,
                    1.0,
                );
                let cam = view * target;

                assert!(
                    cam.x.abs() < 1e-4,
                    "x should be zero (on camera forward axis), got {} at yaw={yaw} pitch={pitch}",
                    cam.x
                );
                assert!(
                    cam.y.abs() < 1e-4,
                    "y should be zero (on camera forward axis), got {} at yaw={yaw} pitch={pitch}",
                    cam.y
                );
                assert!(
                    (cam.z + 1.0).abs() < 1e-4,
                    "z should be -1 (camera looks down -Z), got {} at yaw={yaw} pitch={pitch}",
                    cam.z
                );
            }
        }
    }

    #[test]
    fn unproject_screen_to_world_center_ray_round_trips() {
        // sx=sy=0 is the viewport's own center ray - it must unproject
        // back to exactly the world (yaw, pitch) the viewport is
        // centered on, for any tilt/roll/FOV/aspect.
        use super::super::virtual_camera::VirtualCamera;
        let position = [0.24_f32, 0.0, 0.24];
        let cam = VirtualCamera::new(&position);
        for &(tilt, roll) in &[(0.0_f32, 0.0), (0.15, 0.0), (0.0, 0.12), (0.26, -0.1)] {
            for &(yaw, pitch) in &[(0.0_f32, 0.0), (0.3, -0.2), (-0.5, 0.4), (0.0, 0.6)] {
                for &fov in &[35.0_f32, 60.0, 90.0] {
                    let viewport = ScreenProjection {
                        cam: &cam,
                        position,
                        world_yaw: yaw,
                        world_pitch: pitch,
                        fov_v_deg: fov,
                        aspect: 16.0 / 9.0,
                        rig_tilt: tilt,
                        rig_roll: roll,
                    };
                    let pos = unproject_screen_to_world(&viewport, 0.0, 0.0);
                    assert!(
                        (pos.yaw - yaw).abs() < 1e-4 && (pos.pitch - pitch).abs() < 1e-4,
                        "center ray should round-trip at tilt={tilt} roll={roll} \
                         yaw={yaw} pitch={pitch} fov={fov}: got ({}, {})",
                        pos.yaw,
                        pos.pitch
                    );
                }
            }
        }
    }

    #[test]
    fn project_world_to_screen_round_trips_with_unproject() {
        // Locks the forward and inverse transforms together: project a
        // known world direction to screen, then unproject that screen
        // point back - must recover the original (yaw, pitch). Also
        // exercises the reverse order (unproject then re-project lands
        // back on the same screen point) so neither direction can drift
        // independently without this test catching it.
        use super::super::virtual_camera::VirtualCamera;
        let position = [0.24_f32, 0.0, 0.24];
        let cam = VirtualCamera::new(&position);
        for &(tilt, roll) in &[(0.0_f32, 0.0), (0.15, 0.0), (0.0, 0.12), (0.26, -0.1)] {
            for &(vp_yaw, vp_pitch) in &[(0.0_f32, 0.0), (0.3, -0.2), (-0.4, 0.3)] {
                let viewport = ScreenProjection {
                    cam: &cam,
                    position,
                    world_yaw: vp_yaw,
                    world_pitch: vp_pitch,
                    fov_v_deg: 55.0,
                    aspect: 16.0 / 9.0,
                    rig_tilt: tilt,
                    rig_roll: roll,
                };
                for &(target_yaw, target_pitch) in &[(0.05_f32, 0.05), (-0.2, 0.15), (0.1, -0.1)] {
                    let Some((sx, sy)) =
                        project_world_to_screen(&viewport, target_yaw, target_pitch)
                    else {
                        continue; // outside this sweep's FOV - not an error
                    };
                    let back = unproject_screen_to_world(&viewport, sx, sy);
                    assert!(
                        (back.yaw - target_yaw).abs() < 1e-4
                            && (back.pitch - target_pitch).abs() < 1e-4,
                        "project->unproject should round-trip at tilt={tilt} roll={roll} \
                         viewport=({vp_yaw},{vp_pitch}) target=({target_yaw},{target_pitch}): \
                         got ({}, {}) via screen ({sx},{sy})",
                        back.yaw,
                        back.pitch
                    );

                    let (sx2, sy2) = project_world_to_screen(&viewport, back.yaw, back.pitch)
                        .expect(
                            "re-projecting the round-tripped target must stay in front of the camera",
                        );
                    assert!(
                        (sx2 - sx).abs() < 1e-3 && (sy2 - sy).abs() < 1e-3,
                        "unproject->project should round-trip: ({sx},{sy}) vs ({sx2},{sy2})"
                    );
                }
            }
        }
    }

    #[test]
    fn unproject_screen_to_world_matches_hand_checkable_corners() {
        // Mirrors examples/verify_coverage_corner_gap.rs's own SELFTEST:
        // zero tilt/roll, centered at yaw=pitch=0, a 90deg *square* FOV
        // (aspect=1). Expected values are NOT the naive +-45/+-45 (a
        // corner ray's actual elevation off the horizontal plane is
        // shallower than its nominal half-FOV once both axes are
        // combined - the classic cube-corner angle atan(1/sqrt(2)) =
        // 35.26deg, confirmed against the real crate output by that
        // example script's own SELFTEST block before this function
        // existed). Yaw is mirror-signed (sx=-1 -> yaw=+45, not -45) -
        // this crate's intentional convention per `virtual_camera.rs`'s
        // left-handed-triple note, also confirmed there.
        use super::super::virtual_camera::VirtualCamera;
        let position = [1.0_f32, 0.0, 1.0];
        let cam = VirtualCamera::new(&position);
        for &(sx, sy, expect_yaw, expect_pitch) in &[
            (-1.0_f32, -1.0_f32, 45.0_f32, -35.26_f32),
            (1.0, -1.0, -45.0, -35.26),
            (-1.0, 1.0, 45.0, 35.26),
            (1.0, 1.0, -45.0, 35.26),
        ] {
            let viewport = ScreenProjection {
                cam: &cam,
                position,
                world_yaw: 0.0,
                world_pitch: 0.0,
                fov_v_deg: 90.0,
                aspect: 1.0,
                rig_tilt: 0.0,
                rig_roll: 0.0,
            };
            let pos = unproject_screen_to_world(&viewport, sx, sy);
            assert!(
                (pos.yaw.to_degrees() - expect_yaw).abs() < 0.5,
                "corner ({sx},{sy}): expected yaw~{expect_yaw}, got {}",
                pos.yaw.to_degrees()
            );
            assert!(
                (pos.pitch.to_degrees() - expect_pitch).abs() < 0.5,
                "corner ({sx},{sy}): expected pitch~{expect_pitch}, got {}",
                pos.pitch.to_degrees()
            );
        }
    }
}
