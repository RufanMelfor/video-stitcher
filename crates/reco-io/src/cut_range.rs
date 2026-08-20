//! Time ranges excluded from a stitch export (e.g. a halftime pause).
//!
//! A [`CutRange`] marks `[start_secs, end_secs)` of the *source*
//! timeline as skipped: the decoder seeks past it, the encoder's
//! output PTS counter never sees a gap (see
//! `crate::ffmpeg::encoder::VideoEncoder`'s `frame_count`), and audio
//! passthrough excludes the same source range so picture and sound
//! stay aligned. See [`keep_windows`] for how a list of cuts turns
//! into the sequence of `[start, end)` windows actually decoded.

/// A `[start_secs, end_secs)` range of the source timeline to exclude
/// from the export.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CutRange {
    pub start_secs: f64,
    pub end_secs: f64,
}

impl CutRange {
    /// Build a cut range, rejecting non-finite, negative, or
    /// zero/negative-duration input.
    pub fn new(start_secs: f64, end_secs: f64) -> Result<Self, String> {
        if !start_secs.is_finite() || !end_secs.is_finite() {
            return Err(format!(
                "cut range must use finite times, got {start_secs}..{end_secs}"
            ));
        }
        if start_secs < 0.0 {
            return Err(format!(
                "cut range start ({start_secs:.2}s) must not be negative"
            ));
        }
        if end_secs <= start_secs {
            return Err(format!(
                "cut range end ({end_secs:.2}s) must be after start ({start_secs:.2}s)"
            ));
        }
        Ok(Self {
            start_secs,
            end_secs,
        })
    }

    pub fn duration_secs(&self) -> f64 {
        self.end_secs - self.start_secs
    }
}

/// Sort cut ranges by start time and reject any that overlap.
///
/// Overlaps are rejected rather than silently merged - an ambiguous
/// input (e.g. two ranges submitted in the wrong order, or a typo'd
/// timestamp) should fail loudly, not produce a export that quietly
/// cuts more or less than the user asked for.
pub fn validate_cut_ranges(mut ranges: Vec<CutRange>) -> Result<Vec<CutRange>, String> {
    ranges.sort_by(|a, b| a.start_secs.total_cmp(&b.start_secs));
    for pair in ranges.windows(2) {
        if pair[1].start_secs < pair[0].end_secs {
            return Err(format!(
                "cut ranges overlap: {:.2}-{:.2}s and {:.2}-{:.2}s",
                pair[0].start_secs, pair[0].end_secs, pair[1].start_secs, pair[1].end_secs
            ));
        }
    }
    Ok(ranges)
}

/// Turn `[export_start, export_end)` plus a sorted, non-overlapping
/// list of excluded `cut_ranges` into the sequence of `[start, end)`
/// windows that should actually be decoded/rendered, back to back.
///
/// `export_end` of `None` means "to the end of the source" - the
/// returned last window's end is `None` in that case too (or when a
/// cut range's own end reaches all the way to `export_end`, nothing is
/// left to keep after it and no trailing window is produced at all).
/// Cut ranges outside `[export_start, export_end)` are clipped to it;
/// a cut range entirely outside is silently dropped (nothing to
/// exclude from a part of the timeline that isn't being exported
/// anyway).
pub fn keep_windows(
    export_start: f64,
    export_end: Option<f64>,
    cut_ranges: &[CutRange],
) -> Vec<(f64, Option<f64>)> {
    let mut windows = Vec::new();
    let mut cursor = export_start;
    for cut in cut_ranges {
        let cut_end = match export_end {
            Some(e) => cut.end_secs.min(e),
            None => cut.end_secs,
        };
        if cut.start_secs >= cut_end || cut_end <= cursor {
            // Entirely outside the export window, or already covered
            // by a preceding (clipped) cut - nothing new to exclude.
            continue;
        }
        let cut_start = cut.start_secs.max(cursor);
        if cut_start > cursor {
            windows.push((cursor, Some(cut_start)));
        }
        cursor = cut_end;
    }
    let trailing_needed = match export_end {
        Some(e) => cursor < e,
        None => true,
    };
    if trailing_needed {
        windows.push((cursor, export_end));
    }
    windows
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_rejects_invalid_ranges() {
        assert!(CutRange::new(10.0, 20.0).is_ok());
        assert!(CutRange::new(-1.0, 20.0).is_err());
        assert!(CutRange::new(20.0, 10.0).is_err());
        assert!(CutRange::new(10.0, 10.0).is_err());
        assert!(CutRange::new(f64::NAN, 20.0).is_err());
        assert!(CutRange::new(10.0, f64::INFINITY).is_err());
    }

    #[test]
    fn duration_secs_is_end_minus_start() {
        let r = CutRange::new(100.0, 130.5).unwrap();
        assert_eq!(r.duration_secs(), 30.5);
    }

    #[test]
    fn validate_sorts_and_accepts_non_overlapping() {
        let ranges = vec![
            CutRange::new(100.0, 130.0).unwrap(),
            CutRange::new(10.0, 20.0).unwrap(),
        ];
        let sorted = validate_cut_ranges(ranges).unwrap();
        assert_eq!(sorted[0].start_secs, 10.0);
        assert_eq!(sorted[1].start_secs, 100.0);
    }

    #[test]
    fn validate_rejects_overlap() {
        let ranges = vec![
            CutRange::new(10.0, 30.0).unwrap(),
            CutRange::new(20.0, 40.0).unwrap(),
        ];
        assert!(validate_cut_ranges(ranges).is_err());
    }

    #[test]
    fn validate_accepts_back_to_back_ranges() {
        // end == next start is NOT an overlap (half-open ranges).
        let ranges = vec![
            CutRange::new(10.0, 20.0).unwrap(),
            CutRange::new(20.0, 30.0).unwrap(),
        ];
        assert!(validate_cut_ranges(ranges).is_ok());
    }

    #[test]
    fn keep_windows_no_cuts_is_one_window() {
        let windows = keep_windows(0.0, Some(100.0), &[]);
        assert_eq!(windows, vec![(0.0, Some(100.0))]);

        let windows = keep_windows(5.0, None, &[]);
        assert_eq!(windows, vec![(5.0, None)]);
    }

    #[test]
    fn keep_windows_single_middle_cut() {
        // A halftime-style cut in the middle of a bounded export.
        let cuts = vec![CutRange::new(100.0, 130.0).unwrap()];
        let windows = keep_windows(0.0, Some(200.0), &cuts);
        assert_eq!(windows, vec![(0.0, Some(100.0)), (130.0, Some(200.0))]);
    }

    #[test]
    fn keep_windows_cut_extends_to_open_end() {
        let cuts = vec![CutRange::new(100.0, 130.0).unwrap()];
        let windows = keep_windows(0.0, None, &cuts);
        assert_eq!(windows, vec![(0.0, Some(100.0)), (130.0, None)]);
    }

    #[test]
    fn keep_windows_cut_touches_export_end_leaves_no_trailing_window() {
        let cuts = vec![CutRange::new(100.0, 200.0).unwrap()];
        let windows = keep_windows(0.0, Some(200.0), &cuts);
        assert_eq!(windows, vec![(0.0, Some(100.0))]);
    }

    #[test]
    fn keep_windows_cut_at_export_start_leaves_no_leading_window() {
        let cuts = vec![CutRange::new(0.0, 50.0).unwrap()];
        let windows = keep_windows(0.0, Some(200.0), &cuts);
        assert_eq!(windows, vec![(50.0, Some(200.0))]);
    }

    #[test]
    fn keep_windows_multiple_cuts() {
        let cuts = vec![
            CutRange::new(50.0, 60.0).unwrap(),
            CutRange::new(100.0, 130.0).unwrap(),
            CutRange::new(180.0, 190.0).unwrap(),
        ];
        let windows = keep_windows(0.0, Some(200.0), &cuts);
        assert_eq!(
            windows,
            vec![
                (0.0, Some(50.0)),
                (60.0, Some(100.0)),
                (130.0, Some(180.0)),
                (190.0, Some(200.0)),
            ]
        );
    }

    #[test]
    fn keep_windows_cut_outside_export_window_is_clipped_or_dropped() {
        // Cut entirely before export_start: dropped.
        let cuts = vec![CutRange::new(0.0, 5.0).unwrap()];
        let windows = keep_windows(10.0, Some(100.0), &cuts);
        assert_eq!(windows, vec![(10.0, Some(100.0))]);

        // Cut straddling export_start: clipped to the export window.
        let cuts = vec![CutRange::new(5.0, 15.0).unwrap()];
        let windows = keep_windows(10.0, Some(100.0), &cuts);
        assert_eq!(windows, vec![(15.0, Some(100.0))]);

        // Cut straddling export_end: clipped.
        let cuts = vec![CutRange::new(90.0, 150.0).unwrap()];
        let windows = keep_windows(10.0, Some(100.0), &cuts);
        assert_eq!(windows, vec![(10.0, Some(90.0))]);
    }
}
