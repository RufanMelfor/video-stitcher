//! Container and stream metadata of a video file, for "what is this
//! footage" reports (codec, bit depth, bitrates, audio, container tags).
//!
//! Reads headers only - nothing is decoded. Call [`crate::init`] first.

extern crate ffmpeg_next as ffmpeg;

use std::ffi::CStr;
use std::path::Path;

use ffmpeg::ffi;
use ffmpeg::media::Type;

use super::decoder::DecodeError;

/// Everything [`probe_media_info`] reads from one file.
#[derive(Debug, Clone, Default)]
pub struct MediaInfo {
    /// Container format name (e.g. `mov,mp4,m4a,3gp,3g2,mj2`).
    pub container: String,
    /// Duration in seconds, when the container states it.
    pub duration_secs: Option<f64>,
    /// Overall bitrate in bits per second, when known.
    pub bit_rate: Option<u64>,
    /// File size in bytes.
    pub file_size: Option<u64>,
    /// Container-level tags in file order (e.g. `creation_time`, `encoder`).
    pub tags: Vec<(String, String)>,
    /// The best video stream.
    pub video: Option<VideoStreamInfo>,
    /// The best audio stream.
    pub audio: Option<AudioStreamInfo>,
    /// Streams that are neither video nor audio (e.g. telemetry tracks).
    pub other_streams: usize,
}

/// Video stream details.
#[derive(Debug, Clone, Default)]
pub struct VideoStreamInfo {
    /// Codec name (e.g. `hevc`).
    pub codec: String,
    /// Codec profile (e.g. `Main 10`), when known.
    pub profile: Option<String>,
    /// Coded width in pixels.
    pub width: u32,
    /// Coded height in pixels.
    pub height: u32,
    /// Average frame rate.
    pub fps: f64,
    /// Frame count stated by the container, when present.
    pub frames: Option<u64>,
    /// Pixel format name (e.g. `yuv420p10le`).
    pub pixel_format: Option<String>,
    /// Bits per luma sample (8, 10, ...).
    pub bit_depth: Option<u8>,
    /// Stream bitrate in bits per second, when known.
    pub bit_rate: Option<u64>,
}

/// Audio stream details.
#[derive(Debug, Clone, Default)]
pub struct AudioStreamInfo {
    /// Codec name (e.g. `aac`).
    pub codec: String,
    /// Sample rate in Hz.
    pub sample_rate: u32,
    /// Channel count.
    pub channels: u32,
    /// Stream bitrate in bits per second, when known.
    pub bit_rate: Option<u64>,
}

/// Positive value as `Some`, else `None` (FFmpeg uses 0 / negative for
/// "unknown").
fn known(v: i64) -> Option<u64> {
    (v > 0).then_some(v as u64)
}

fn c_str(p: *const std::os::raw::c_char) -> Option<String> {
    if p.is_null() {
        return None;
    }
    // SAFETY: FFmpeg returns pointers to static, NUL-terminated strings
    // (codec / pixel-format / profile name tables), checked non-null above.
    Some(unsafe { CStr::from_ptr(p) }.to_string_lossy().into_owned())
}

/// Read container and stream metadata of `path` (headers only).
pub fn probe_media_info(path: &Path) -> Result<MediaInfo, DecodeError> {
    let ictx = ffmpeg::format::input(path)?;
    let mut info = MediaInfo {
        container: ictx.format().name().to_string(),
        duration_secs: known(ictx.duration()).map(|d| d as f64 / f64::from(ffi::AV_TIME_BASE)),
        bit_rate: known(ictx.bit_rate()),
        file_size: std::fs::metadata(path).ok().map(|m| m.len()),
        tags: ictx
            .metadata()
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect(),
        ..Default::default()
    };

    let best_video = ictx.streams().best(Type::Video).map(|s| s.index());
    let best_audio = ictx.streams().best(Type::Audio).map(|s| s.index());
    for stream in ictx.streams() {
        let params = stream.parameters();
        // SAFETY: `params` owns a valid AVCodecParameters for its lifetime;
        // only plain fields are read.
        let raw = unsafe { &*params.as_ptr() };
        let codec = params.id().name().to_string();
        match params.medium() {
            Type::Video if Some(stream.index()) == best_video => {
                let rate = stream.avg_frame_rate();
                let fps = if rate.denominator() != 0 {
                    f64::from(rate.numerator()) / f64::from(rate.denominator())
                } else {
                    0.0
                };
                // Pixel format via ffmpeg-next's typed conversion (unknown
                // values map to `Pixel::None`).
                let pix: Option<ffi::AVPixelFormat> =
                    ffmpeg::codec::context::Context::from_parameters(params.clone())
                        .and_then(|c| c.decoder().video())
                        .ok()
                        .map(|v| v.format())
                        .filter(|p| *p != ffmpeg::format::Pixel::None)
                        .map(Into::into);
                // SAFETY: plain table lookups keyed by valid enum values;
                // all return null for unknown keys (checked).
                let (pixel_format, bit_depth, profile) = unsafe {
                    let (name, depth) = match pix {
                        Some(pix) => {
                            let desc = ffi::av_pix_fmt_desc_get(pix);
                            (
                                c_str(ffi::av_get_pix_fmt_name(pix)),
                                (!desc.is_null()).then(|| (*desc).comp[0].depth as u8),
                            )
                        }
                        None => (None, None),
                    };
                    (
                        name,
                        depth,
                        c_str(ffi::avcodec_profile_name(raw.codec_id, raw.profile)),
                    )
                };
                info.video = Some(VideoStreamInfo {
                    codec,
                    profile,
                    width: raw.width.max(0) as u32,
                    height: raw.height.max(0) as u32,
                    fps,
                    frames: known(stream.frames()),
                    pixel_format,
                    bit_depth,
                    bit_rate: known(raw.bit_rate),
                });
            }
            Type::Audio if Some(stream.index()) == best_audio => {
                info.audio = Some(AudioStreamInfo {
                    codec,
                    sample_rate: raw.sample_rate.max(0) as u32,
                    channels: raw.ch_layout.nb_channels.max(0) as u32,
                    bit_rate: known(raw.bit_rate),
                });
            }
            Type::Video | Type::Audio => {}
            _ => info.other_streams += 1,
        }
    }
    Ok(info)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_filters_unknown_values() {
        assert_eq!(known(0), None);
        assert_eq!(known(-1), None);
        assert_eq!(known(42), Some(42));
    }

    #[test]
    fn missing_file_is_an_error() {
        crate::init();
        assert!(probe_media_info(Path::new("definitely/not/here.mp4")).is_err());
    }
}
