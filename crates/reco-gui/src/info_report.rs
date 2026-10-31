//! Plain-text "Info" report: the footage of both cameras, their lenses and,
//! when a setup is loaded, the rig's angular coverage (overlap in degrees,
//! angle between the cameras, panorama width, seam position).
//!
//! Pure formatting over already-probed data so it is unit-testable; the
//! caller probes the files (`reco_io::ffmpeg::media_info`) and supplies
//! the setup.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use reco_core::calibration::Calibration;
use reco_core::coverage::{horizon_coverage, lens_fov};
use reco_io::ffmpeg::media_info::MediaInfo;

/// One camera's footage and lens label.
pub struct CameraInfo {
    /// Every segment of the recording in order, with its probe result
    /// (`Err` = why it could not be read).
    pub segments: Vec<(PathBuf, Result<MediaInfo, String>)>,
    /// Lens profile as shown in the Lens section (camera, lens, source);
    /// empty when not known yet.
    pub lens_profile: String,
}

/// Everything the report shows.
pub struct ReportInput<'a> {
    /// Left camera (`lenses[0]`).
    pub left: CameraInfo,
    /// Right camera (`lenses[1]`).
    pub right: CameraInfo,
    /// The loaded setup, if any.
    pub calibration: Option<&'a Calibration>,
    /// Where the setup is saved, if it is.
    pub setup_path: Option<&'a Path>,
    /// Motion-sensor status line from the last calibration (may be empty).
    pub imu_status: &'a str,
}

/// `h:mm:ss` (or `m:ss` under an hour).
pub fn format_duration(secs: f64) -> String {
    let total = secs.max(0.0).round() as u64;
    let (h, m, s) = (total / 3600, (total / 60) % 60, total % 60);
    if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m}:{s:02}")
    }
}

/// Bytes as `x.y GB` / `x.y MB` (decimal units, like file managers).
pub fn format_size(bytes: u64) -> String {
    let b = bytes as f64;
    if b >= 1e9 {
        format!("{:.1} GB", b / 1e9)
    } else {
        format!("{:.1} MB", b / 1e6)
    }
}

/// Bits per second as `x.y Mbit/s` or `x kbit/s`.
pub fn format_bitrate(bps: u64) -> String {
    if bps >= 1_000_000 {
        format!("{:.1} Mbit/s", bps as f64 / 1e6)
    } else {
        format!("{} kbit/s", bps / 1000)
    }
}

fn channels_label(n: u32) -> String {
    match n {
        1 => "mono".into(),
        2 => "stereo".into(),
        n => format!("{n} channels"),
    }
}

fn file_name(p: &Path) -> String {
    p.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| p.display().to_string())
}

fn camera_section(
    out: &mut String,
    title: &str,
    cam: &CameraInfo,
    cal: Option<&Calibration>,
    lens_index: usize,
) {
    let _ = writeln!(out, "== {title} CAMERA ==");
    let probes: Vec<&MediaInfo> = cam
        .segments
        .iter()
        .filter_map(|(_, r)| r.as_ref().ok())
        .collect();
    let total_secs: f64 = probes.iter().filter_map(|m| m.duration_secs).sum();
    let total_size: u64 = probes.iter().filter_map(|m| m.file_size).sum();
    let _ = writeln!(
        out,
        "Files: {} segment(s), {} total, {}",
        cam.segments.len(),
        format_duration(total_secs),
        format_size(total_size)
    );
    for (i, (path, probe)) in cam.segments.iter().enumerate() {
        match probe {
            Ok(m) => {
                let _ = writeln!(
                    out,
                    "  {}. {}  {}  {}",
                    i + 1,
                    file_name(path),
                    m.duration_secs
                        .map(format_duration)
                        .unwrap_or_else(|| "?".into()),
                    m.file_size.map(format_size).unwrap_or_else(|| "?".into())
                );
            }
            Err(e) => {
                let _ = writeln!(
                    out,
                    "  {}. {}  (could not read: {e})",
                    i + 1,
                    file_name(path)
                );
            }
        }
    }

    // Stream details from the first readable segment (all segments of one
    // recording share them).
    if let Some(m) = probes.first() {
        match &m.video {
            Some(v) => {
                let mut line = format!("Video: {}", v.codec);
                if let Some(p) = &v.profile {
                    let _ = write!(line, " ({p})");
                }
                let _ = write!(line, ", {}x{}, {:.2} fps", v.width, v.height, v.fps);
                if let Some(d) = v.bit_depth {
                    let _ = write!(line, ", {d}-bit");
                }
                if let Some(pf) = &v.pixel_format {
                    let _ = write!(line, " ({pf})");
                }
                if let Some(b) = v.bit_rate {
                    let _ = write!(line, ", {}", format_bitrate(b));
                }
                let _ = writeln!(out, "{line}");
                if let Some(frames) = v.frames {
                    let _ = writeln!(out, "Frames (first segment): {frames}");
                }
            }
            None => {
                let _ = writeln!(out, "Video: none found");
            }
        }
        match &m.audio {
            Some(a) => {
                let mut line = format!(
                    "Audio: {}, {} Hz, {}",
                    a.codec,
                    a.sample_rate,
                    channels_label(a.channels)
                );
                if let Some(b) = a.bit_rate {
                    let _ = write!(line, ", {}", format_bitrate(b));
                }
                let _ = writeln!(out, "{line}");
            }
            None => {
                let _ = writeln!(out, "Audio: none (sync cannot use sound)");
            }
        }
        for (key, label) in [
            ("creation_time", "Recorded"),
            ("encoder", "Encoder"),
            ("timecode", "Timecode"),
        ] {
            if let Some((_, v)) = m.tags.iter().find(|(k, _)| k == key) {
                let _ = writeln!(out, "{label}: {v}");
            }
        }
        if m.other_streams > 0 {
            let _ = writeln!(
                out,
                "Other streams: {} (telemetry / metadata)",
                m.other_streams
            );
        }
        let _ = writeln!(out, "Container: {}", m.container);
    }

    if !cam.lens_profile.is_empty() {
        let _ = writeln!(out, "Lens profile: {}", cam.lens_profile);
    }
    match cal.and_then(|c| c.lenses.get(lens_index)) {
        Some(lens) => match lens_fov(lens) {
            Some(f) => {
                let _ = writeln!(
                    out,
                    "Lens field of view: {:.1} deg horizontal, {:.1} deg vertical, {:.1} deg diagonal",
                    f.horizontal_deg, f.vertical_deg, f.diagonal_deg
                );
            }
            None => {
                let _ = writeln!(
                    out,
                    "Lens field of view: cannot be computed from this profile"
                );
            }
        },
        None => {
            let _ = writeln!(
                out,
                "Lens field of view: needs a setup (calibrate or load one)"
            );
        }
    }
    let _ = writeln!(out);
}

/// Build the full report.
pub fn build_report(input: &ReportInput<'_>) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "RECO VIDEO INFO");
    let _ = writeln!(out);
    camera_section(&mut out, "LEFT", &input.left, input.calibration, 0);
    camera_section(&mut out, "RIGHT", &input.right, input.calibration, 1);

    let _ = writeln!(out, "== RIG ==");
    let Some(cal) = input.calibration else {
        let _ = writeln!(
            out,
            "No setup loaded: calibrate or load a setup to see the overlap."
        );
        return out;
    };
    if let Some(p) = input.setup_path {
        let _ = writeln!(out, "Setup: {}", p.display());
    }
    let fps = input
        .left
        .segments
        .iter()
        .find_map(|(_, r)| r.as_ref().ok()?.video.as_ref().map(|v| v.fps))
        .filter(|f| *f > 0.0);
    let n = cal.sync_offset;
    let ms = fps
        .map(|f| format!(" ({:.0} ms)", n as f64 * 1000.0 / f))
        .unwrap_or_default();
    let _ = writeln!(
        out,
        "Sync offset: {n} frame(s){ms} - right frame = left frame + offset"
    );

    let c = horizon_coverage(cal);
    match c.camera_angle_deg {
        Some(a) => {
            let _ = writeln!(out, "Angle between the cameras: {a:.1} deg");
        }
        None => {
            let _ = writeln!(
                out,
                "Angle between the cameras: unknown (the setup has no overlap)"
            );
        }
    }
    let _ = writeln!(
        out,
        "Overlap (seen by both cameras): {:.1} deg at the horizon",
        c.overlap_deg
    );
    let _ = writeln!(
        out,
        "Panorama width: {:.1} deg (left camera {:.1} + right camera {:.1} - overlap)",
        c.panorama_deg, c.left_span_deg, c.right_span_deg
    );
    let seam_offset = cal.topology.seam_offset;
    match c.seam_in_overlap_deg {
        Some([from_left, from_right]) => {
            let _ = writeln!(
                out,
                "Seam (position {seam_offset:.3}): {from_left:.1} deg from the overlap's left edge, {from_right:.1} deg from its right edge"
            );
        }
        None => {
            let _ = writeln!(
                out,
                "Seam (position {seam_offset:.3}): not inside the overlap"
            );
        }
    }
    if !input.imu_status.is_empty() {
        let _ = writeln!(out, "Motion sensors: {}", input.imu_status);
    }
    let _ = writeln!(
        out,
        "Angles are measured at the horizon from the setup and the lens profiles, treating both cameras as one viewpoint; accurate to about half a degree on a typical rig."
    );
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use reco_io::ffmpeg::media_info::{AudioStreamInfo, VideoStreamInfo};

    fn probe(secs: f64, bytes: u64) -> MediaInfo {
        MediaInfo {
            container: "mov,mp4".into(),
            duration_secs: Some(secs),
            file_size: Some(bytes),
            tags: vec![("encoder".into(), "DJI OsmoAction4".into())],
            video: Some(VideoStreamInfo {
                codec: "hevc".into(),
                profile: Some("Main 10".into()),
                width: 3840,
                height: 2880,
                fps: 29.97,
                bit_depth: Some(10),
                ..Default::default()
            }),
            audio: Some(AudioStreamInfo {
                codec: "aac".into(),
                sample_rate: 48000,
                channels: 1,
                bit_rate: None,
            }),
            ..Default::default()
        }
    }

    #[test]
    fn formats() {
        assert_eq!(format_duration(59.6), "1:00");
        assert_eq!(format_duration(3725.0), "1:02:05");
        assert_eq!(format_size(4_000_000_000), "4.0 GB");
        assert_eq!(format_size(250_000_000), "250.0 MB");
        assert_eq!(format_bitrate(109_891_041), "109.9 Mbit/s");
        assert_eq!(format_bitrate(285_375), "285 kbit/s");
    }

    #[test]
    fn report_without_setup_lists_footage_and_asks_for_setup() {
        let cam = |name: &str| CameraInfo {
            segments: vec![
                (
                    PathBuf::from(format!("{name}1.MP4")),
                    Ok(probe(1200.0, 4_000_000_000)),
                ),
                (PathBuf::from(format!("{name}2.MP4")), Err("missing".into())),
            ],
            lens_profile: String::new(),
        };
        let report = build_report(&ReportInput {
            left: cam("L"),
            right: cam("R"),
            calibration: None,
            setup_path: None,
            imu_status: "",
        });
        assert!(report.contains("Files: 2 segment(s), 20:00 total, 4.0 GB"));
        assert!(report.contains("Video: hevc (Main 10), 3840x2880, 29.97 fps, 10-bit"));
        assert!(report.contains("Audio: aac, 48000 Hz, mono"));
        assert!(report.contains("Encoder: DJI OsmoAction4"));
        assert!(report.contains("R2.MP4  (could not read: missing)"));
        assert!(report.contains("No setup loaded"));
        assert!(!report.contains("Overlap"));
    }
}
