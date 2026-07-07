//! Manual field-line seam-continuity input.
//!
//! Every automatic near-field measurement tried so far (see
//! `FRICTION.md`) fails for the same underlying reason: AKAZE finds too
//! few confident keypoints in the deepest near-field rows (repetitive
//! grass texture), and generic photometric block-matching gets fooled by
//! that same texture. This module takes the near-field measurement out
//! of automatic detection entirely: the user manually identifies a real
//! field line's position in each camera's own GPU-undistorted frame by
//! clicking two points along it (not just one - a single point on a
//! straight line is ambiguous, since it can slide along the line and
//! still "match"; two points fix both position and slope).
//!
//! The two points don't need to be the same physical point in both
//! cameras - each line is clicked independently in its own camera's
//! frame. [`ClickedLine::extrapolate_to_seam`] extrapolates each camera's
//! line to that camera's own seam column (from [`geometry::seam_columns`],
//! derived from the current `intersect`), producing one point per camera.
//! Those two extrapolated points are then just a [`MatchedPoint`] -
//! identical in kind to what AKAZE already produces - so fitting against
//! them reuses the existing, tested `geometry::apply_transformations` /
//! `reprojection_error` machinery, including the already-built (but so
//! far unused, for lack of good near-field data) band-limited
//! `ground_tilt_x` / `ground_tilt_z` parameters from FRICTION.md points
//! 8-11.

use serde::{Deserialize, Serialize};

use crate::geometry;
use crate::types::MatchedPoint;

/// Two points along a real, straight field line, as manually identified in
/// one camera's own GPU-undistorted frame - stored in plane coordinates
/// (the same view/FOV-independent space `geometry::normalize_to_plane`
/// produces from pixels).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ClickedLine {
    p1_plane: [f64; 2],
    p2_plane: [f64; 2],
}

impl ClickedLine {
    /// Build from pixel coordinates in a camera's own GPU-undistorted debug
    /// frame (e.g. `reco calibrate --debug-dir`'s dumped PNGs), the way
    /// `tools/manual_line_picker.html` produces clicks.
    pub fn from_pixels(p1_px: (f64, f64), p2_px: (f64, f64), img_w: u32, img_h: u32) -> Self {
        Self {
            p1_plane: geometry::normalize_to_plane(p1_px.0, p1_px.1, img_w, img_h),
            p2_plane: geometry::normalize_to_plane(p2_px.0, p2_px.1, img_w, img_h),
        }
    }

    /// Build directly from already-canonical plane coordinates - what
    /// `resources/click_calib_v2.html`'s field-line mode exports (it does
    /// its own in-browser fisheye undistort, a port of `fisheye.wgsl`, so
    /// there's no separate pixel-space step or debug-dir dump needed).
    pub fn from_plane(p1_plane: [f64; 2], p2_plane: [f64; 2]) -> Self {
        Self { p1_plane, p2_plane }
    }

    /// Extrapolate this line to a given normalized-pixel-x seam column,
    /// returning the plane-space `[x, y]` position where the line would
    /// cross the seam.
    ///
    /// # Panics
    ///
    /// Panics if the two clicked points have the same plane-x coordinate
    /// (a vertical line in plane space) - the line then has no defined
    /// x-to-y slope to extrapolate with. This shouldn't happen for a real
    /// near-horizontal field line crossing the seam; a caller hitting this
    /// most likely swapped or duplicated a click.
    pub fn extrapolate_to_seam(&self, seam_nx: f64) -> [f64; 2] {
        let (p1, p2) = (self.p1_plane, self.p2_plane);
        let seam_x = (seam_nx - 0.5) * geometry::PLANE_WIDTH;

        let dx = p2[0] - p1[0];
        assert!(
            dx.abs() > 1e-12,
            "clicked line points must have distinct x (in plane space) to define a slope: \
             p1={p1:?} p2={p2:?}"
        );
        let t = (seam_x - p1[0]) / dx;
        let y = p1[1] + t * (p2[1] - p1[1]);
        [seam_x, y]
    }
}

/// Build a single [`MatchedPoint`] from a manually-clicked line pair - one
/// line per camera, in natural left/right order (the x-plane/z-plane swap
/// is handled internally, matching the convention `lib.rs` uses for
/// AKAZE-derived points).
///
/// Each line is extrapolated to its own camera's seam column (derived from
/// `intersect`), and the two extrapolated points become one high-trust
/// correspondence at exactly the seam - the one place automatic near-field
/// measurement has consistently failed on this project's real footage.
pub fn matched_point_from_lines(
    left_camera_line: &ClickedLine,
    right_camera_line: &ClickedLine,
    intersect: f64,
) -> MatchedPoint {
    let seams = geometry::seam_columns(intersect);
    let left_at_seam = left_camera_line.extrapolate_to_seam(seams.left_camera_seam_nx);
    let right_at_seam = right_camera_line.extrapolate_to_seam(seams.right_camera_seam_nx);

    MatchedPoint {
        left: right_at_seam,
        right: left_at_seam,
        left_pixel_nx: seams.right_camera_seam_nx,
        right_pixel_nx: seams.left_camera_seam_nx,
    }
}

/// Which coordinate space a [`ManualLinesFile`]'s clicked points are in.
///
/// `Pixel` (the default, for backward compatibility with
/// `tools/manual_line_picker.html`) needs `img_w`/`img_h` to convert via
/// `geometry::normalize_to_plane`. `Plane` points are already in canonical
/// plane space - what `resources/click_calib_v2.html`'s field-line mode
/// exports directly, since that tool already does its own in-browser
/// fisheye undistort - and are used as-is.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum CoordSpace {
    #[default]
    Pixel,
    Plane,
}

/// One clicked line, for serialized input files - in whichever
/// [`CoordSpace`] the containing [`ManualLinesFile`] declares.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct ClickedLineInput {
    pub p1: [f64; 2],
    pub p2: [f64; 2],
}

impl ClickedLineInput {
    fn into_clicked_line(self, img_w: u32, img_h: u32, coord_space: CoordSpace) -> ClickedLine {
        match coord_space {
            CoordSpace::Pixel => ClickedLine::from_pixels(
                (self.p1[0], self.p1[1]),
                (self.p2[0], self.p2[1]),
                img_w,
                img_h,
            ),
            CoordSpace::Plane => ClickedLine::from_plane(self.p1, self.p2),
        }
    }
}

/// A manually-clicked line pair for one real-world field line: one line as
/// seen in the left camera's own frame, one as seen in the right camera's.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct ManualLinePair {
    pub left: ClickedLineInput,
    pub right: ClickedLineInput,
}

/// Deserialized form of a manual-lines input file (see
/// `examples/fit_ground_tilt_manual.rs` for the CLI that consumes this).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ManualLinesFile {
    #[serde(default)]
    pub coord_space: CoordSpace,
    pub lines: Vec<ManualLinePair>,
}

impl ManualLinesFile {
    /// Convert every clicked line pair into a [`MatchedPoint`]. `img_w`/
    /// `img_h` are only used for `CoordSpace::Pixel` input; ignored (but
    /// still required, to keep one call site) for `CoordSpace::Plane`.
    /// The calibration's current `intersect` locates each camera's seam
    /// column either way.
    pub fn to_matched_points(
        &self,
        left_img_w: u32,
        left_img_h: u32,
        right_img_w: u32,
        right_img_h: u32,
        intersect: f64,
    ) -> Vec<MatchedPoint> {
        self.lines
            .iter()
            .map(|pair| {
                let left_line =
                    pair.left
                        .into_clicked_line(left_img_w, left_img_h, self.coord_space);
                let right_line =
                    pair.right
                        .into_clicked_line(right_img_w, right_img_h, self.coord_space);
                matched_point_from_lines(&left_line, &right_line, intersect)
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use approx::assert_abs_diff_eq;

    #[test]
    fn extrapolate_to_seam_recovers_known_line() {
        // A line through (100, 100) and (100, 300) in pixel space, both at
        // x=100 - i.e. constant plane-x, non-constant plane-y - extrapolated
        // to the seam column at that same plane-x should hit the same y
        // wherever we're extrapolating to, since the line is already
        // "vertical" in image space (constant x). Use a line with a real
        // slope instead so extrapolation is meaningful.
        let line = ClickedLine::from_pixels((100.0, 100.0), (300.0, 300.0), 1000, 1000);
        // plane coords: normalize_to_plane(100,100,1000,1000) = (-0.4, -0.4)
        //               normalize_to_plane(300,300,1000,1000) = (-0.2, -0.2)
        // slope dy/dx = 1.0, line is y = x (through origin).
        let p1 = geometry::normalize_to_plane(100.0, 100.0, 1000, 1000);
        let p2 = geometry::normalize_to_plane(300.0, 300.0, 1000, 1000);
        assert_abs_diff_eq!(p1[0], p1[1], epsilon = 1e-12);
        assert_abs_diff_eq!(p2[0], p2[1], epsilon = 1e-12);

        // Seam at nx=0.5 -> plane_x = 0.0 -> on the line y=x, expect y=0.0.
        let at_center = line.extrapolate_to_seam(0.5);
        assert_abs_diff_eq!(at_center[0], 0.0, epsilon = 1e-12);
        assert_abs_diff_eq!(at_center[1], 0.0, epsilon = 1e-10);

        // Seam at nx=0.75 -> plane_x = 0.25 -> expect y=0.25 (extrapolated
        // beyond the two clicked points, since both were at plane-x < 0).
        let beyond = line.extrapolate_to_seam(0.75);
        assert_abs_diff_eq!(beyond[0], 0.25, epsilon = 1e-12);
        assert_abs_diff_eq!(beyond[1], 0.25, epsilon = 1e-10);
    }

    #[test]
    #[should_panic(expected = "distinct x")]
    fn extrapolate_to_seam_rejects_degenerate_line() {
        // same x -> undefined slope in plane-x
        let line = ClickedLine::from_pixels((100.0, 100.0), (100.0, 300.0), 1000, 1000);
        line.extrapolate_to_seam(0.5);
    }

    #[test]
    fn matched_point_from_lines_zero_error_for_perfectly_continuous_line() {
        // Both cameras see the same real-world line y=x (in plane
        // coordinates), so a perfectly-calibrated seam should show zero
        // continuity error regardless of where the seam column falls.
        let left_line = ClickedLine::from_pixels((50.0, 50.0), (250.0, 250.0), 1000, 1000);
        let right_line = ClickedLine::from_pixels((700.0, 700.0), (900.0, 900.0), 1000, 1000);
        let intersect = 0.5;
        let mp = matched_point_from_lines(&left_line, &right_line, intersect);

        // Both extrapolated points should land on y=x at their respective
        // seam plane-x, so left/right y should match left/right x exactly.
        assert_abs_diff_eq!(mp.left[1], mp.left[0], epsilon = 1e-10);
        assert_abs_diff_eq!(mp.right[1], mp.right[0], epsilon = 1e-10);
    }

    #[test]
    fn matched_point_from_lines_uses_swap_convention() {
        // Sanity check on which camera maps to which MatchedPoint field:
        // left_camera_line -> mp.right (z-plane), right_camera_line -> mp.left (x-plane).
        let left_line = ClickedLine::from_pixels((50.0, 500.0), (250.0, 520.0), 1000, 1000);
        let right_line = ClickedLine::from_pixels((700.0, 900.0), (900.0, 920.0), 1000, 1000);
        let seams = geometry::seam_columns(0.5);
        let expected_left_at_seam = left_line.extrapolate_to_seam(seams.left_camera_seam_nx);
        let expected_right_at_seam = right_line.extrapolate_to_seam(seams.right_camera_seam_nx);

        let mp = matched_point_from_lines(&left_line, &right_line, 0.5);
        assert_eq!(mp.right, expected_left_at_seam);
        assert_eq!(mp.left, expected_right_at_seam);
    }

    #[test]
    fn manual_lines_file_round_trips_through_json() {
        let file = ManualLinesFile {
            coord_space: CoordSpace::Pixel,
            lines: vec![ManualLinePair {
                left: ClickedLineInput {
                    p1: [50.0, 500.0],
                    p2: [250.0, 520.0],
                },
                right: ClickedLineInput {
                    p1: [700.0, 900.0],
                    p2: [900.0, 920.0],
                },
            }],
        };
        let json = serde_json::to_string(&file).unwrap();
        let parsed: ManualLinesFile = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, file);
    }

    #[test]
    fn manual_lines_file_defaults_coord_space_to_pixel_when_absent() {
        // Backward compat with tools/manual_line_picker.html's existing
        // export, which predates the coord_space field entirely.
        let json = r#"{"lines": [{"left": {"p1": [50.0, 500.0], "p2": [250.0, 520.0]},
                                    "right": {"p1": [700.0, 900.0], "p2": [900.0, 920.0]}}]}"#;
        let parsed: ManualLinesFile = serde_json::from_str(json).unwrap();
        assert_eq!(parsed.coord_space, CoordSpace::Pixel);
    }

    #[test]
    fn to_matched_points_produces_one_point_per_line() {
        let file = ManualLinesFile {
            coord_space: CoordSpace::Pixel,
            lines: vec![
                ManualLinePair {
                    left: ClickedLineInput {
                        p1: [50.0, 500.0],
                        p2: [250.0, 520.0],
                    },
                    right: ClickedLineInput {
                        p1: [700.0, 900.0],
                        p2: [900.0, 920.0],
                    },
                },
                ManualLinePair {
                    left: ClickedLineInput {
                        p1: [60.0, 600.0],
                        p2: [260.0, 630.0],
                    },
                    right: ClickedLineInput {
                        p1: [710.0, 950.0],
                        p2: [910.0, 980.0],
                    },
                },
            ],
        };
        let points = file.to_matched_points(1000, 1000, 1000, 1000, 0.5);
        assert_eq!(points.len(), 2);
    }

    #[test]
    fn plane_coord_space_skips_pixel_normalization() {
        // Same line as `matched_point_from_lines_zero_error_for_perfectly_continuous_line`,
        // but expressed as CoordSpace::Plane input (what click_calib_v2.html's
        // field-line mode exports) instead of pixels - should agree exactly,
        // since from_pixels((50,50),(250,250),1000,1000) normalizes to the
        // same plane points used here directly.
        let pixel_line = ClickedLine::from_pixels((50.0, 50.0), (250.0, 250.0), 1000, 1000);
        let file = ManualLinesFile {
            coord_space: CoordSpace::Plane,
            lines: vec![ManualLinePair {
                left: ClickedLineInput {
                    p1: geometry::normalize_to_plane(50.0, 50.0, 1000, 1000),
                    p2: geometry::normalize_to_plane(250.0, 250.0, 1000, 1000),
                },
                right: ClickedLineInput {
                    p1: geometry::normalize_to_plane(700.0, 700.0, 1000, 1000),
                    p2: geometry::normalize_to_plane(900.0, 900.0, 1000, 1000),
                },
            }],
        };
        // img dims are irrelevant for Plane input - pass nonsense values to prove it.
        let points = file.to_matched_points(1, 1, 1, 1, 0.5);
        assert_eq!(points.len(), 1);
        let expected = matched_point_from_lines(
            &pixel_line,
            &ClickedLine::from_pixels((700.0, 700.0), (900.0, 900.0), 1000, 1000),
            0.5,
        );
        assert_abs_diff_eq!(points[0].left[0], expected.left[0], epsilon = 1e-12);
        assert_abs_diff_eq!(points[0].left[1], expected.left[1], epsilon = 1e-12);
        assert_abs_diff_eq!(points[0].right[0], expected.right[0], epsilon = 1e-12);
        assert_abs_diff_eq!(points[0].right[1], expected.right[1], epsilon = 1e-12);
    }
}
