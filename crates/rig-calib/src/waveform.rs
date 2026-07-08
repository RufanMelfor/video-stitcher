//! Audio waveform envelope extraction for the audio-sync check panel.
//!
//! Downsamples a short PCM window around the playhead into a coarse
//! peak-amplitude envelope for overlaying left/right camera audio as a
//! visual sync check — a real `sync_offset` error shows up as a shifted
//! transient spike between the two traces. Extraction shells out to
//! `ffmpeg` per call, so callers must not invoke this on the UI thread.

use std::path::Path;

use reco_io::ffmpeg::calibration_io::{self, CalibrationIoError};

/// Sample rate used for envelope extraction. Low enough to keep the
/// `ffmpeg` call fast; far more than enough resolution for a peak
/// envelope over a multi-second window.
const ENVELOPE_SAMPLE_RATE: u32 = 8_000;

/// Downsample PCM samples into `buckets` peak-amplitude values,
/// normalized to `[0.0, 1.0]`. Always returns exactly `buckets` values
/// (zero-filled if `samples` is empty).
pub fn compute_envelope(samples: &[i16], buckets: usize) -> Vec<f32> {
    if buckets == 0 {
        return Vec::new();
    }
    if samples.is_empty() {
        return vec![0.0; buckets];
    }
    let len = samples.len();
    (0..buckets)
        .map(|i| {
            let start = i * len / buckets;
            let end = ((i + 1) * len / buckets).max(start + 1).min(len);
            let peak = samples[start..end]
                .iter()
                .map(|&s| (s as i32).unsigned_abs())
                .max()
                .unwrap_or(0);
            peak as f32 / i16::MAX as f32
        })
        .collect()
}

/// Extract a peak-amplitude envelope for a window of audio centered on
/// `center_secs` in the file at `path`.
pub fn extract_window_envelope(
    path: &Path,
    center_secs: f64,
    window_secs: f64,
    buckets: usize,
) -> Result<Vec<f32>, CalibrationIoError> {
    let start_secs = (center_secs - window_secs / 2.0).max(0.0);
    let samples = calibration_io::extract_audio_pcm_window(
        path,
        ENVELOPE_SAMPLE_RATE,
        start_secs,
        window_secs,
    )?;
    Ok(compute_envelope(&samples, buckets))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compute_envelope_empty_samples_returns_zero_filled() {
        assert_eq!(compute_envelope(&[], 5), vec![0.0; 5]);
    }

    #[test]
    fn compute_envelope_zero_buckets_returns_empty() {
        assert!(compute_envelope(&[1, 2, 3], 0).is_empty());
    }

    #[test]
    fn compute_envelope_returns_exact_bucket_count() {
        let samples: Vec<i16> = (0..997).map(|i| (i % 100) as i16).collect();
        assert_eq!(compute_envelope(&samples, 300).len(), 300);
    }

    #[test]
    fn compute_envelope_finds_peak_per_bucket() {
        // Two buckets: first half has a loud spike, second half is quiet.
        let mut samples = vec![0i16; 200];
        samples[50] = i16::MAX;
        samples[150] = 100;
        let env = compute_envelope(&samples, 2);
        assert!((env[0] - 1.0).abs() < 1e-6, "loud bucket should read ~1.0");
        assert!(env[1] < 0.01, "quiet bucket should read ~0.0");
    }

    #[test]
    fn compute_envelope_handles_min_i16_without_overflow() {
        let samples = vec![i16::MIN; 10];
        let env = compute_envelope(&samples, 1);
        assert!((env[0] - 1.0).abs() < 1e-3);
    }
}
