//! Dump the exact raw camera frame a detection came from, with its
//! bounding box(es) drawn on top, for visual debugging of missed or
//! surprisingly small detections.
//!
//! Reads a `--events` JSONL (as produced by `reco stitch --events`) and,
//! for each requested output `frame_index`, decodes the *exact* source
//! video frame that fed the detector at that point - sequential
//! frame-by-frame decode-and-discard, the same technique
//! `reco-calibrate/examples/dump_undistorted.rs` uses, not a lossy
//! `ffmpeg -ss` timestamp seek (which lands on the nearest keyframe, not
//! the exact frame - easily off by a GOP's worth of frames on a fast-
//! moving ball). `camera_center`/`camera_size` in `detections_raw` are
//! already in raw-camera normalized coordinates (see
//! `reco_core::detect::director::MappedDetection`'s doc comments), so no
//! undistort/letterbox math is needed - just scale by the frame's own
//! pixel dimensions.
//!
//! Also draws the calibration's field ROI polygon (yellow outline) if
//! present - `RoiFilteredDetector` drops any detection whose anchor
//! point falls outside this polygon *before* it ever reaches
//! `detections_raw` (see `reco-autocam/src/roi_filter.rs`'s doc comment:
//! "the filter runs on the post-inference `Vec<Detection>`"). A ball the
//! model found but that never shows up in `detections_raw` - visibly
//! present in the dumped frame, no box drawn around it - is exactly what
//! an ROI that's too tight looks like from this tool; the yellow outline
//! makes that diagnosis visual instead of requiring a second guess.
//!
//! Usage:
//! ```text
//! cargo run -p reco-io --example dump_detection_frames -- \
//!   <left.mp4> <right.mp4> <events.jsonl> <calibration.json> <output_dir> \
//!   <start_time_secs> <sync_offset_frames> [--camera Left|Right] [--clean] \
//!   <frame_index_or_range...>
//! ```
//!
//! `start_time_secs` and `sync_offset_frames` must match the original
//! `reco stitch` invocation exactly (printed in its own log as
//! "skipping N frames (start_time=...)" and "sync offset: skipped N
//! right frames") - `events.jsonl`'s `frame_index` is 0-based from
//! *after* that skip, so reproducing it exactly is what makes this
//! frame-accurate rather than another guess.
//!
//! `<frame_index_or_range...>` accepts either a single index (`719`) or
//! an inclusive range (`690-719`), any mix of both.
//!
//! `--camera Left|Right` decodes only that camera - skip the other
//! entirely (faster, and the point when you already know which side has
//! the interesting footage).
//!
//! `--clean` skips drawing detection boxes/crosshairs/ROI outline
//! entirely - just the bare decoded frame, full native resolution,
//! matching this project's own Label Studio upload convention (see
//! `docs/YOLO26_Training.md`: never crop or stretch, `reco-detect` always
//! letterboxes the whole uncropped frame at real inference time, so
//! training/review images must match that). Use this to produce frames
//! meant for annotation review, not diagnosis - a debug overlay baked
//! into a training image would bias whoever reviews it.

use std::collections::HashMap;
use std::path::Path;

use reco_io::ffmpeg::decoder::VideoDecoder;

/// One `detections_raw` entry, loosely parsed - only the fields this
/// tool draws. Avoids depending on reco-core's `PipelineEvent` schema
/// directly so this example doesn't churn every time that enum grows a
/// field.
struct RawDet {
    camera: String,
    class_id: i64,
    confidence: f32,
    center: (f32, f32),
    size: (f32, f32),
}

fn main() {
    reco_io::init();

    let args: Vec<String> = std::env::args().collect();
    if args.len() < 8 {
        eprintln!(
            "Usage: {} <left.mp4> <right.mp4> <events.jsonl> <calibration.json> \
             <output_dir> <start_time_secs> <sync_offset_frames> \
             [--camera Left|Right] [--clean] <frame_index_or_range...>",
            args[0]
        );
        eprintln!(
            "  start_time_secs / sync_offset_frames must match the original \
             `reco stitch` invocation exactly (see its own log output)."
        );
        eprintln!("  frame_index_or_range: e.g. `719` or `690-719`, any mix.");
        std::process::exit(1);
    }
    let left_path = &args[1];
    let right_path = &args[2];
    let events_path = &args[3];
    let calibration_path = &args[4];
    let out_dir = Path::new(&args[5]);
    let start_time_secs: f64 = args[6].parse().expect("start_time_secs must be a number");
    let sync_offset: u64 = args[7]
        .parse()
        .expect("sync_offset_frames must be a number");

    // Trailing args: optional --camera/--clean flags (any order, before
    // the frame specs), then one or more `N` or `N-M` frame specs.
    let mut only_camera: Option<&str> = None;
    let mut clean = false;
    let mut frame_specs: Vec<&str> = Vec::new();
    let mut rest = args[8..].iter();
    while let Some(a) = rest.next() {
        match a.as_str() {
            "--camera" => {
                let v = rest.next().expect("--camera needs a value (Left|Right)");
                only_camera = Some(match v.as_str() {
                    "Left" | "Right" => v.as_str(),
                    other => panic!("--camera must be Left or Right, got {other}"),
                });
            }
            "--clean" => clean = true,
            spec => frame_specs.push(spec),
        }
    }
    let target_frames: Vec<u64> = frame_specs
        .iter()
        .flat_map(|spec| -> Box<dyn Iterator<Item = u64>> {
            if let Some((lo, hi)) = spec.split_once('-') {
                let lo: u64 = lo.parse().expect("range start must be a number");
                let hi: u64 = hi.parse().expect("range end must be a number");
                Box::new(lo..=hi)
            } else {
                Box::new(std::iter::once(
                    spec.parse().expect("frame_index must be a number"),
                ))
            }
        })
        .collect();
    if target_frames.is_empty() {
        eprintln!("No frame indices given - nothing to dump.");
        std::process::exit(1);
    }
    std::fs::create_dir_all(out_dir).expect("failed to create output_dir");

    // Field ROI polygons, if the calibration has one - same normalized
    // raw-camera-frame `[0,1]` space as `camera_center`/`camera_size`
    // above, so the *points* need no conversion. The *edges* do, though:
    // see `densify_roi_polygon`'s doc comment - straight lines between
    // these raw-space vertices do not trace the same boundary the user
    // saw (and intended) in the GUI's rectified ROI editor.
    let (roi_left, roi_right) = load_field_roi(calibration_path);
    if roi_left.is_some() || roi_right.is_some() {
        println!(
            "Field ROI: left={} point(s), right={} point(s)",
            roi_left.as_ref().map_or(0, Vec::len),
            roi_right.as_ref().map_or(0, Vec::len)
        );
    } else {
        println!("No field ROI in calibration (or file unreadable) - skipping ROI overlay.");
    }
    let (lens_left, lens_right) = load_lenses(calibration_path);

    // Load detections_raw per frame_index from the events JSONL. Loose
    // JSON parsing (serde_json::Value) - see the RawDet doc comment.
    let events_text = std::fs::read_to_string(events_path).expect("failed to read events file");
    let mut by_frame: HashMap<u64, Vec<RawDet>> = HashMap::new();
    for line in events_text.lines() {
        let v: serde_json::Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(_) => continue,
        };
        if v.get("kind").and_then(|k| k.as_str()) != Some("detections_raw") {
            continue;
        }
        let Some(frame_index) = v.get("frame_index").and_then(|f| f.as_u64()) else {
            continue;
        };
        let dets = v
            .get("detections")
            .and_then(|d| d.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|d| {
                        Some(RawDet {
                            camera: d.get("camera")?.as_str()?.to_string(),
                            class_id: d.get("class_id")?.as_i64()?,
                            confidence: d.get("confidence")?.as_f64()? as f32,
                            center: (
                                d.get("camera_center")?.get(0)?.as_f64()? as f32,
                                d.get("camera_center")?.get(1)?.as_f64()? as f32,
                            ),
                            size: (
                                d.get("camera_size")?.get(0)?.as_f64()? as f32,
                                d.get("camera_size")?.get(1)?.as_f64()? as f32,
                            ),
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();
        by_frame.insert(frame_index, dets);
    }
    println!(
        "Loaded detections_raw for {} frames from {events_path}",
        by_frame.len()
    );

    let max_target = *target_frames.iter().max().unwrap();
    let left_skip = (start_time_secs.max(0.0) * left_fps(left_path)).round() as u64;
    let right_skip = left_skip + sync_offset;

    let cameras: Vec<&str> = match only_camera {
        Some(c) => vec![c],
        None => vec!["Left", "Right"],
    };
    for camera in cameras {
        let (path, skip, roi, lens) = if camera == "Left" {
            (
                left_path,
                left_skip,
                roi_left.as_deref(),
                lens_left.as_ref(),
            )
        } else {
            (
                right_path,
                right_skip,
                roi_right.as_deref(),
                lens_right.as_ref(),
            )
        };
        let mut dec = VideoDecoder::open(Path::new(path))
            .unwrap_or_else(|e| panic!("failed to open {path}: {e}"));
        println!(
            "{camera}: decoding from frame {skip} up to {} ({path})",
            skip + max_target
        );

        // Skip to the frame right before the first target, sequentially -
        // exact by construction, no seek-imprecision. Matches
        // reco-calibrate/examples/dump_undistorted.rs's own convention.
        for _ in 0..skip {
            if dec.next_frame().unwrap().is_none() {
                panic!("{camera}: source ended during skip at frame < {skip}");
            }
        }

        let mut sorted_targets = target_frames.clone();
        sorted_targets.sort_unstable();
        let mut next_output_index = 0u64;
        for &target in &sorted_targets {
            // Advance frame-by-frame to the target (targets are sorted,
            // so this never needs to seek backward).
            while next_output_index < target {
                if dec.next_frame().unwrap().is_none() {
                    panic!("{camera}: source ended before frame {target}");
                }
                next_output_index += 1;
            }
            let Some(frame) = dec.next_frame().unwrap() else {
                eprintln!("{camera}: source ended exactly at frame {target}, skipping");
                break;
            };
            next_output_index += 1;

            let dets = by_frame.get(&target).map(Vec::as_slice).unwrap_or(&[]);
            let cam_dets: Vec<&RawDet> = dets.iter().filter(|d| d.camera == camera).collect();

            let img = yuv420_to_rgba(&frame.y, &frame.u, &frame.v, frame.width, frame.height);
            let mut img = image::RgbaImage::from_raw(frame.width, frame.height, img)
                .expect("frame buffer size mismatch");
            if !clean && let Some(roi) = roi {
                // Straight raw-space edges between the calibration's few
                // stored vertices do not trace the boundary the GUI
                // showed while it was drawn (see
                // `reco_core::lens::densify_polygon`'s doc comment) -
                // densify through rectified space first when the lens is
                // available, falling back to the raw vertices as-is
                // otherwise (still better than nothing). Same function
                // `RoiFilteredDetector`'s callers now use in production,
                // not a separate copy - see `FieldRoi::densified`.
                let drawn = match lens {
                    Some(l) => reco_core::lens::densify_polygon(
                        roi,
                        l,
                        reco_core::lens::ROI_DENSIFY_SAMPLES_PER_EDGE,
                    ),
                    None => roi.to_vec(),
                };
                draw_polygon(&mut img, &drawn, image::Rgba([255, 230, 0, 255]));
            }
            for d in &cam_dets {
                // Always logged to console for reference, even in clean
                // mode - only the pixel overlay is skipped there.
                println!(
                    "    class={} conf={:.2} center=({:.0},{:.0}) size={:.0}x{:.0}px",
                    d.class_id,
                    d.confidence,
                    d.center.0 * img.width() as f32,
                    d.center.1 * img.height() as f32,
                    d.size.0 * img.width() as f32,
                    d.size.1 * img.height() as f32
                );
                if !clean {
                    draw_detection(&mut img, d);
                }
            }

            let out_path = if clean {
                out_dir.join(format!("frame{target}_{camera}.png"))
            } else {
                out_dir.join(format!("frame{target}_{camera}_{}dets.png", cam_dets.len()))
            };
            img.save(&out_path).expect("failed to save PNG");
            println!(
                "  frame {target}: {} {camera} detection(s) -> {}",
                cam_dets.len(),
                out_path.display()
            );
        }
    }
}

/// A polygon in normalized `[0,1]` camera-space, or its absence.
type OptPolygon = Option<Vec<[f64; 2]>>;

/// Read `field_roi.left`/`field_roi.right` straight out of a calibration
/// JSON file with `serde_json::Value` - avoids taking a hard dependency
/// on `reco_core::calibration::Calibration`'s full schema for two
/// polygon fields. Returns `(None, None)` (with a message on stderr) if
/// the file can't be read/parsed or has no `field_roi` - never fatal,
/// since the ROI overlay is a nice-to-have, not the point of the dump.
fn load_field_roi(calibration_path: &str) -> (OptPolygon, OptPolygon) {
    let parse_side = |v: &serde_json::Value| -> Option<Vec<[f64; 2]>> {
        let arr = v.as_array()?;
        if arr.len() < 3 {
            return None; // matches RoiFilteredDetector's own "< 3 points = no filter"
        }
        arr.iter()
            .map(|p| {
                let p = p.as_array()?;
                Some([p.first()?.as_f64()?, p.get(1)?.as_f64()?])
            })
            .collect()
    };
    let text = match std::fs::read_to_string(calibration_path) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("warning: couldn't read calibration {calibration_path}: {e}");
            return (None, None);
        }
    };
    let v: serde_json::Value = match serde_json::from_str(&text) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("warning: couldn't parse calibration {calibration_path}: {e}");
            return (None, None);
        }
    };
    let Some(roi) = v.get("field_roi") else {
        return (None, None);
    };
    (
        roi.get("left").and_then(parse_side),
        roi.get("right").and_then(parse_side),
    )
}

/// Read `lenses[0]`/`lenses[1]` (left/right) out of a calibration JSON
/// file, typed via `reco_core::calibration::Lens`'s own `Deserialize` -
/// only the two polygon fields get the hand-rolled `serde_json::Value`
/// treatment in [`load_field_roi`] (to avoid a hard dependency on the
/// full `Calibration` schema); the lens intrinsics/distortion are
/// exactly `Lens`'s own shape already, so there's no reason to
/// re-parse them by hand. `None` (with a stderr message) on any read/
/// parse/shape failure - like the ROI, this is a nice-to-have (lets the
/// ROI overlay follow the true lens curve) not the point of the dump,
/// so a bad calibration here degrades to the straight-edge fallback
/// rather than panicking.
fn load_lenses(
    calibration_path: &str,
) -> (
    Option<reco_core::calibration::Lens>,
    Option<reco_core::calibration::Lens>,
) {
    let text = match std::fs::read_to_string(calibration_path) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("warning: couldn't read calibration {calibration_path}: {e}");
            return (None, None);
        }
    };
    let v: serde_json::Value = match serde_json::from_str(&text) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("warning: couldn't parse calibration {calibration_path}: {e}");
            return (None, None);
        }
    };
    let Some(lenses) = v.get("lenses").and_then(|l| l.as_array()) else {
        return (None, None);
    };
    let parse = |i: usize| -> Option<reco_core::calibration::Lens> {
        serde_json::from_value(lenses.get(i)?.clone()).ok()
    };
    (parse(0), parse(1))
}

/// Probe a video's frame rate without a full decoder setup (just enough
/// to convert `start_time_secs` to a frame count the same way
/// `StitchJob` does: `(start_secs * fps).round()`).
fn left_fps(path: &str) -> f64 {
    let dec = VideoDecoder::open(Path::new(path)).unwrap_or_else(|e| panic!("{path}: {e}"));
    let fps = dec.fps();
    if fps > 0.0 {
        fps
    } else {
        eprintln!("warning: could not read fps from {path}, assuming 30.0");
        30.0
    }
}

/// Standard limited-range BT.601 YUV420P -> RGBA8 conversion. Not
/// colorimetrically exact (real playback would honor the stream's actual
/// color primaries/range) - fine for a debug viewer where the point is
/// spotting a ball-sized blob, not color-critical grading.
fn yuv420_to_rgba(y: &[u8], u: &[u8], v: &[u8], w: u32, h: u32) -> Vec<u8> {
    let (w, h) = (w as usize, h as usize);
    let mut out = vec![0u8; w * h * 4];
    for row in 0..h {
        for col in 0..w {
            let yv = y[row * w + col] as f32;
            let uv_row = row / 2;
            let uv_col = col / 2;
            let uv_w = w.div_ceil(2);
            let uv = u[uv_row * uv_w + uv_col] as f32 - 128.0;
            let vv = v[uv_row * uv_w + uv_col] as f32 - 128.0;
            let r = (yv + 1.402 * vv).clamp(0.0, 255.0) as u8;
            let g = (yv - 0.344136 * uv - 0.714136 * vv).clamp(0.0, 255.0) as u8;
            let b = (yv + 1.772 * uv).clamp(0.0, 255.0) as u8;
            let idx = (row * w + col) * 4;
            out[idx] = r;
            out[idx + 1] = g;
            out[idx + 2] = b;
            out[idx + 3] = 255;
        }
    }
    out
}

/// Draw the field ROI polygon as a closed outline (thick, so it reads
/// clearly against grass at full-frame zoom). Points are normalized
/// camera-space `[0,1]`, same convention as detections.
fn draw_polygon(img: &mut image::RgbaImage, points: &[[f64; 2]], color: image::Rgba<u8>) {
    if points.len() < 2 {
        return;
    }
    let (w, h) = (img.width() as f32, img.height() as f32);
    let px = |p: &[f64; 2]| ((p[0] as f32 * w) as i32, (p[1] as f32 * h) as i32);
    let thickness = 2i32;
    for i in 0..points.len() {
        let (x0, y0) = px(&points[i]);
        let (x1, y1) = px(&points[(i + 1) % points.len()]);
        for t in -thickness..=thickness {
            draw_line(img, x0, y0 + t, x1, y1 + t, color);
            draw_line(img, x0 + t, y0, x1 + t, y1, color);
        }
    }
}

/// Draw a detection's bounding box + a filled confidence-proportional
/// corner marker, color-coded by class (0=person blue, 1=ball red,
/// 2=referee green, other=yellow). Box coordinates come in as
/// normalized camera-space `[0,1]` (see `RawDet`) - multiply by the
/// image's own pixel dimensions to place them.
fn draw_detection(img: &mut image::RgbaImage, d: &RawDet) {
    let (w, h) = (img.width() as f32, img.height() as f32);
    let (cx, cy) = (d.center.0 * w, d.center.1 * h);
    let (bw, bh) = (d.size.0 * w, d.size.1 * h);
    let x0 = (cx - bw / 2.0).max(0.0) as i32;
    let y0 = (cy - bh / 2.0).max(0.0) as i32;
    let x1 = (cx + bw / 2.0).min(w - 1.0) as i32;
    let y1 = (cy + bh / 2.0).min(h - 1.0) as i32;
    let color = match d.class_id {
        0 => image::Rgba([64, 128, 255, 255]), // person: blue
        1 => image::Rgba([255, 32, 32, 255]),  // ball: red
        2 => image::Rgba([32, 220, 32, 255]),  // referee: green
        _ => image::Rgba([255, 220, 32, 255]), // other: yellow
    };

    // A tiny box (the common case here - see the session's own findings)
    // can be just a few pixels, invisible as a 1px outline. Draw a
    // generously thick outline (3px) plus a crosshair through the
    // center so it's findable at a glance even when the box itself
    // rounds to nothing.
    let thickness = 3i32;
    for t in 0..thickness {
        draw_rect_outline(img, x0 - t, y0 - t, x1 + t, y1 + t, color);
    }
    let cross_len = 20i32;
    draw_line(
        img,
        cx as i32 - cross_len,
        cy as i32,
        cx as i32 + cross_len,
        cy as i32,
        color,
    );
    draw_line(
        img,
        cx as i32,
        cy as i32 - cross_len,
        cx as i32,
        cy as i32 + cross_len,
        color,
    );
}

fn draw_rect_outline(
    img: &mut image::RgbaImage,
    x0: i32,
    y0: i32,
    x1: i32,
    y1: i32,
    color: image::Rgba<u8>,
) {
    draw_line(img, x0, y0, x1, y0, color);
    draw_line(img, x0, y1, x1, y1, color);
    draw_line(img, x0, y0, x0, y1, color);
    draw_line(img, x1, y0, x1, y1, color);
}

/// Simple Bresenham-ish line for axis-aligned or near-axis-aligned
/// segments - all callers here draw horizontal/vertical lines, so no
/// need for a general line algorithm.
fn draw_line(
    img: &mut image::RgbaImage,
    x0: i32,
    y0: i32,
    x1: i32,
    y1: i32,
    color: image::Rgba<u8>,
) {
    let (w, h) = (img.width() as i32, img.height() as i32);
    let steps = (x1 - x0).abs().max((y1 - y0).abs()).max(1);
    for i in 0..=steps {
        let x = x0 + (x1 - x0) * i / steps;
        let y = y0 + (y1 - y0) * i / steps;
        if x >= 0 && x < w && y >= 0 && y < h {
            img.put_pixel(x as u32, y as u32, color);
        }
    }
}
