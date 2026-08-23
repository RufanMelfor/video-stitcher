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

/// Per-boundary silent-gap seconds for `EncoderConfig::pause_overlay_hold_secs`,
/// the audio counterpart of [`extend_for_pause_overlay`]'s video-side
/// frame schedule, computed independently and earlier (audio setup runs
/// before `window_limits`/fps-based frame counts exist - see
/// `StitchJob::run`). Both apply the identical "clamp hold to the
/// available cut duration" rule; a few milliseconds of rounding
/// difference between this seconds-based version and the later
/// frame-based one is inaudible in what is, by construction, a silent
/// gap. Returns one entry per internal boundary, same order
/// `keep_windows`' boundaries are crossed in.
pub fn pause_overlay_hold_seconds(keep_windows: &[(f64, Option<f64>)], hold_secs: f32) -> Vec<f32> {
    keep_windows
        .windows(2)
        .map(|pair| {
            let Some(win_end) = pair[0].1 else {
                return 0.0; // only the last window can be open-ended
            };
            let available = (pair[1].0 - win_end).max(0.0);
            (hold_secs as f64).min(available) as f32
        })
        .collect()
}

/// Extends `window_limits` (the cumulative convention documented on
/// [`keep_windows`]'s caller in `reco_io::stitch_job`) so every
/// INTERNAL boundary - between one kept window and the next, i.e. an
/// actual cut - gets `overlay.hold_secs` of additional real source
/// content decoded before the seek to the next window. That extra
/// content is real (already-recorded) footage from inside the cut
/// itself; it is never shown unmodified because the returned schedule
/// keeps the "PAUZE" overlay fully opaque across exactly that span -
/// see `reco_core::render::pause_overlay`'s module doc for why this
/// beats generating synthetic frames from scratch. `fade_secs` on
/// either side is not added here: it plays over content the window was
/// already going to show, so it needs no extra decoded frames at all.
///
/// A cut range shorter than `overlay.hold_secs` can't supply that much
/// hidden footage - its hold is silently clamped to what's actually
/// available (logged, not an error; the fades alone already hide a cut
/// that short reasonably well). Extension also never pushes a boundary
/// past `frame_cap` (the export's own `--max-frames` budget, if any).
///
/// Returns one [`reco_core::render::pause_overlay::PauseBoundary`] per
/// internal boundary still reachable within `window_limits` - fewer
/// than `keep_windows.len() - 1` when `max_frames` truncates the
/// export before a later cut.
pub fn extend_for_pause_overlay(
    keep_windows: &[(f64, Option<f64>)],
    window_limits: &mut [u64],
    fps: f64,
    frame_cap: u64,
    overlay: &reco_core::render::pause_overlay::PauseOverlayConfig,
) -> Vec<reco_core::render::pause_overlay::PauseBoundary> {
    use reco_core::render::pause_overlay::PauseBoundary;

    let fade_frames = (overlay.fade_secs as f64 * fps).round() as u64;
    let boundary_count = keep_windows
        .len()
        .saturating_sub(1)
        .min(window_limits.len().saturating_sub(1));

    let mut schedule = Vec::with_capacity(boundary_count);
    let mut hold_at = vec![0u64; window_limits.len()];
    let mut extra = 0u64;

    for (i, hold_slot) in hold_at.iter_mut().enumerate().take(boundary_count) {
        let Some(win_i_end) = keep_windows[i].1 else {
            break; // only the last kept window can be open-ended
        };
        let win_next_start = keep_windows[i + 1].0;
        let available_secs = (win_next_start - win_i_end).max(0.0);
        let hold_secs = (overlay.hold_secs as f64).min(available_secs);
        if hold_secs < overlay.hold_secs as f64 {
            log::warn!(
                "Pause overlay: cut at {win_i_end:.2}-{win_next_start:.2}s is shorter than the \
                 configured hold ({:.2}s) - clamping this transition's hold to {hold_secs:.2}s",
                overlay.hold_secs
            );
        }
        let hold_frames = (hold_secs * fps).round() as u64;

        let shifted_boundary = window_limits[i] + extra;
        let cap_room = frame_cap.saturating_sub(shifted_boundary);
        let hold_frames = hold_frames.min(cap_room);

        // Always push exactly one entry per boundary (even a
        // degenerate zero-length one, when fade_secs is 0 and this
        // specific cut clamped hold to 0 frames) so the schedule stays
        // index-aligned with anything else keyed by boundary number,
        // e.g. the audio side's per-boundary hold-seconds list.
        schedule.push(PauseBoundary {
            fade_out_start: shifted_boundary.saturating_sub(fade_frames),
            hold_start: shifted_boundary,
            hold_end: shifted_boundary + hold_frames,
            fade_in_end: shifted_boundary + hold_frames + fade_frames,
        });

        *hold_slot = hold_frames;
        extra += hold_frames;
    }

    let mut running = 0u64;
    for (limit, &hold) in window_limits.iter_mut().zip(hold_at.iter()) {
        running += hold;
        *limit += running;
    }

    schedule
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

    fn overlay(
        fade_secs: f32,
        hold_secs: f32,
    ) -> reco_core::render::pause_overlay::PauseOverlayConfig {
        reco_core::render::pause_overlay::PauseOverlayConfig::new(fade_secs, hold_secs, "PAUZE")
            .unwrap()
    }

    #[test]
    fn extend_for_pause_overlay_single_boundary_uses_full_hold() {
        // 30fps, window 0 is 0..100s (3000 frames), cut 100..105s (5s
        // available - more than enough for a 2s hold), window 1 resumes.
        let keep = vec![(0.0, Some(100.0)), (105.0, Some(200.0))];
        let mut limits = vec![3000u64, 6000u64]; // original cumulative, no overlay yet
        let cfg = overlay(3.0, 2.0);
        let schedule = extend_for_pause_overlay(&keep, &mut limits, 30.0, u64::MAX, &cfg);

        assert_eq!(schedule.len(), 1);
        let b = schedule[0];
        assert_eq!(b.hold_start, 3000);
        assert_eq!(b.hold_end, 3000 + 60); // 2s * 30fps
        assert_eq!(b.fade_out_start, 3000 - 90); // 3s * 30fps
        assert_eq!(b.fade_in_end, 3000 + 60 + 90);

        // window_limits[0] grew by the hold; window_limits[1] carries
        // the same extension forward (cumulative).
        assert_eq!(limits[0], 3000 + 60);
        assert_eq!(limits[1], 6000 + 60);
    }

    #[test]
    fn extend_for_pause_overlay_clamps_hold_to_short_cut() {
        // Cut is only 1s, configured hold wants 2s - clamp to what's there.
        let keep = vec![(0.0, Some(100.0)), (101.0, Some(200.0))];
        let mut limits = vec![3000u64, 6000u64];
        let cfg = overlay(3.0, 2.0);
        let schedule = extend_for_pause_overlay(&keep, &mut limits, 30.0, u64::MAX, &cfg);

        assert_eq!(schedule[0].hold_end - schedule[0].hold_start, 30); // 1s, not 2s
        assert_eq!(limits[0], 3000 + 30);
    }

    #[test]
    fn extend_for_pause_overlay_two_boundaries_accumulate() {
        let keep = vec![(0.0, Some(50.0)), (55.0, Some(100.0)), (110.0, Some(150.0))];
        let mut limits = vec![1500u64, 3000u64, 4500u64];
        let cfg = overlay(1.0, 2.0);
        let schedule = extend_for_pause_overlay(&keep, &mut limits, 30.0, u64::MAX, &cfg);

        assert_eq!(schedule.len(), 2);
        // Second boundary's hold_start must include the first boundary's
        // extension (60 frames = 2s * 30fps), not just its own original position.
        assert_eq!(schedule[1].hold_start, 3000 + 60);
        assert_eq!(limits[2], 4500 + 60 + 60); // both boundaries' holds carried forward
    }

    #[test]
    fn extend_for_pause_overlay_respects_frame_cap() {
        let keep = vec![(0.0, Some(100.0)), (105.0, Some(200.0))];
        let mut limits = vec![3000u64, 6000u64];
        let cfg = overlay(3.0, 2.0);
        // Cap lands exactly at the boundary - no room for a hold at all.
        let schedule = extend_for_pause_overlay(&keep, &mut limits, 30.0, 3000, &cfg);
        assert_eq!(schedule[0].hold_start, schedule[0].hold_end);
        assert_eq!(limits[0], 3000);
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
