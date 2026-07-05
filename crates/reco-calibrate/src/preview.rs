//! Annotated detection preview: renders where AKAZE detected and matched
//! keypoints onto an RGBA image, for live diagnostic display while
//! calibration runs (see [`crate::calibrate_with_reporting`]'s
//! `on_frame_progress` callback).
//!
//! No GUI toolkit dependency here - callers convert the raw RGBA buffer
//! to whatever image type their UI needs (e.g. `slint::Image`).

use crate::features::{DetectRegion, KeyPoint};

/// Blue: detected but did not survive the spatial-overlap filter.
const COLOR_DETECTED: [u8; 4] = [80, 140, 255, 255];
/// Green: matched and inside the expected overlap band.
const COLOR_SURVIVED: [u8; 4] = [60, 230, 90, 255];
/// Yellow: outline of the expected stitch-overlap region.
const COLOR_REGION: [u8; 4] = [232, 192, 32, 255];

/// A side-by-side annotated detection image (left camera | right camera),
/// with detected keypoints and the expected overlap region drawn on top.
#[derive(Debug, Clone)]
pub struct DetectionPreview {
    /// Combined RGBA pixel data, `width * height * 4` bytes.
    pub rgba: Vec<u8>,
    pub width: u32,
    pub height: u32,
}

/// Render an annotated detection preview for one frame pair.
///
/// `survived_left`/`survived_right` are keypoint indices (into `kp_left`/
/// `kp_right`) that matched and passed the spatial-overlap filter - drawn
/// in green with a larger radius; everything else is drawn in blue.
#[allow(clippy::too_many_arguments)]
pub fn render_detection_preview(
    left_rgba: &[u8],
    lw: u32,
    lh: u32,
    kp_left: &[KeyPoint],
    survived_left: &[usize],
    left_region: &DetectRegion,
    right_rgba: &[u8],
    rw: u32,
    rh: u32,
    kp_right: &[KeyPoint],
    survived_right: &[usize],
    right_region: &DetectRegion,
) -> DetectionPreview {
    let mut left = left_rgba.to_vec();
    let mut right = right_rgba.to_vec();

    draw_region_box(&mut left, lw, lh, left_region);
    draw_region_box(&mut right, rw, rh, right_region);
    draw_keypoints(&mut left, lw, lh, kp_left, survived_left);
    draw_keypoints(&mut right, rw, rh, kp_right, survived_right);

    let (rgba, width, height) = side_by_side(&left, lw, lh, &right, rw, rh);
    DetectionPreview {
        rgba,
        width,
        height,
    }
}

/// Dot/line sizes below are chosen relative to image width, not fixed
/// pixel counts: this preview is typically rendered at full undistorted
/// camera resolution (often 3840px+ wide per camera) and then squeezed
/// down to fit a much smaller GUI panel. A handful of fixed-size pixels
/// survives that downscale as a barely-visible smear at best - sized
/// relative to width, dots and lines stay legible after shrinking.
fn draw_keypoints(rgba: &mut [u8], w: u32, h: u32, kps: &[KeyPoint], survived: &[usize]) {
    let survived: std::collections::HashSet<usize> = survived.iter().copied().collect();
    let detected_radius = (w as f32 / 400.0).max(4.0) as i32;
    let survived_radius = (w as f32 / 200.0).max(8.0) as i32;
    for (i, kp) in kps.iter().enumerate() {
        let hit = survived.contains(&i);
        let color = if hit { COLOR_SURVIVED } else { COLOR_DETECTED };
        let radius = if hit {
            survived_radius
        } else {
            detected_radius
        };
        draw_dot(rgba, w, h, kp.x as i32, kp.y as i32, radius, color);
    }
}

fn draw_dot(rgba: &mut [u8], w: u32, h: u32, cx: i32, cy: i32, r: i32, color: [u8; 4]) {
    for dy in -r..=r {
        for dx in -r..=r {
            if dx * dx + dy * dy > r * r {
                continue;
            }
            let x = cx + dx;
            let y = cy + dy;
            if x < 0 || y < 0 || x as u32 >= w || y as u32 >= h {
                continue;
            }
            let idx = (y as u32 * w + x as u32) as usize * 4;
            if idx + 4 <= rgba.len() {
                rgba[idx..idx + 4].copy_from_slice(&color);
            }
        }
    }
}

fn draw_region_box(rgba: &mut [u8], w: u32, h: u32, region: &DetectRegion) {
    let x0 = (region.x_min * w as f32) as i32;
    let x1 = ((region.x_max * w as f32) as i32).saturating_sub(1);
    let y0 = (region.y_min * h as f32) as i32;
    let y1 = ((region.y_max * h as f32) as i32).saturating_sub(1);
    let thickness = (w as f32 / 600.0).max(3.0) as i32;
    draw_hline(rgba, w, h, x0, x1, y0, thickness, COLOR_REGION);
    draw_hline(rgba, w, h, x0, x1, y1, thickness, COLOR_REGION);
    draw_vline(rgba, w, h, y0, y1, x0, thickness, COLOR_REGION);
    draw_vline(rgba, w, h, y0, y1, x1, thickness, COLOR_REGION);
}

#[allow(clippy::too_many_arguments)]
fn draw_hline(
    rgba: &mut [u8],
    w: u32,
    h: u32,
    x0: i32,
    x1: i32,
    y: i32,
    thickness: i32,
    color: [u8; 4],
) {
    let half = thickness / 2;
    for yy in (y - half)..=(y + half) {
        if yy < 0 || yy as u32 >= h {
            continue;
        }
        for x in x0.max(0)..=x1.min(w as i32 - 1) {
            let idx = (yy as u32 * w + x as u32) as usize * 4;
            if idx + 4 <= rgba.len() {
                rgba[idx..idx + 4].copy_from_slice(&color);
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn draw_vline(
    rgba: &mut [u8],
    w: u32,
    h: u32,
    y0: i32,
    y1: i32,
    x: i32,
    thickness: i32,
    color: [u8; 4],
) {
    let half = thickness / 2;
    for xx in (x - half)..=(x + half) {
        if xx < 0 || xx as u32 >= w {
            continue;
        }
        for y in y0.max(0)..=y1.min(h as i32 - 1) {
            let idx = (y as u32 * w + xx as u32) as usize * 4;
            if idx + 4 <= rgba.len() {
                rgba[idx..idx + 4].copy_from_slice(&color);
            }
        }
    }
}

/// Concatenate two RGBA buffers left/right into one wider image (letting
/// the shorter side sit at the top, black-filled below).
fn side_by_side(
    left: &[u8],
    lw: u32,
    lh: u32,
    right: &[u8],
    rw: u32,
    rh: u32,
) -> (Vec<u8>, u32, u32) {
    let h = lh.max(rh);
    let w = lw + rw;
    let mut out = vec![0u8; (w * h * 4) as usize];
    for y in 0..lh {
        let src_start = (y * lw * 4) as usize;
        let src_end = src_start + (lw * 4) as usize;
        let dst_start = (y * w * 4) as usize;
        let dst_end = dst_start + (lw * 4) as usize;
        out[dst_start..dst_end].copy_from_slice(&left[src_start..src_end]);
    }
    for y in 0..rh {
        let src_start = (y * rw * 4) as usize;
        let src_end = src_start + (rw * 4) as usize;
        let dst_start = (y * w * 4) as usize + (lw * 4) as usize;
        let dst_end = dst_start + (rw * 4) as usize;
        out[dst_start..dst_end].copy_from_slice(&right[src_start..src_end]);
    }
    (out, w, h)
}
