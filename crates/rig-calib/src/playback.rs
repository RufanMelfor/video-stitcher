//! Video playback controller.
//!
//! Wraps an `FfmpegFileSource` with play/pause/step/seek state and
//! frame timing. Delivers YUV frame data on demand, paced by FPS.

use std::time::{Duration, Instant};

use reco_core::source::{FrameSource, SourceError, SourceInfo, YuvData};
use reco_io::adapters::FfmpegFileSource;
use reco_io::stitch_job::InputPath;

/// Playback state machine.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlayState {
    /// No source loaded.
    Empty,
    /// Paused on a frame.
    Paused,
    /// Playing at source FPS.
    Playing,
    /// Reached end of file.
    Finished,
}

/// Owned stereo YUV frame pair.
pub struct StereoYuv {
    pub left: YuvData,
    pub right: YuvData,
}

/// Controls video file playback for the GUI.
///
/// Uses drift-free scheduling: frame advance timing is computed from a
/// wall-clock anchor set when playback starts, not from the last
/// successful advance. This prevents the cumulative lag that comes
/// from snapping the anchor to actual advance times (which are always
/// slightly late relative to the ideal frame boundary).
pub struct Playback {
    source: Option<FfmpegFileSource>,
    info: Option<SourceInfo>,
    state: PlayState,
    current_frame: Option<StereoYuv>,
    frame_index: u64,
    total_frames: Option<u64>,
    frame_duration: Duration,
    /// Wall-clock time when playback started (or resumed) and the
    /// frame index at that moment. Used to compute the ideal target
    /// frame index without drift.
    playback_anchor: Option<(Instant, u64)>,
    /// Playback speed multiplier (1.0 = normal). Scales how fast wall-
    /// clock time advances the target frame index in `tick()`; the
    /// target is always a whole frame count derived from the source's
    /// real `frame_duration`, so any speed value stays frame-accurate.
    speed: f64,
}

/// Clamp range for [`Playback::set_speed`]. Below `0.05x` a "tick" would
/// almost never advance a frame; above `8x` decode throughput can't keep
/// up on typical hardware, so the pacing target would run away from what
/// can actually be decoded.
const SPEED_RANGE: (f64, f64) = (0.05, 8.0);

impl Playback {
    /// Create an empty playback controller (no source loaded).
    pub fn new() -> Self {
        Self {
            source: None,
            info: None,
            state: PlayState::Empty,
            current_frame: None,
            frame_index: 0,
            total_frames: None,
            frame_duration: Duration::from_millis(33), // ~30fps default
            playback_anchor: None,
            speed: 1.0,
        }
    }

    /// Open a stereo video source.
    ///
    /// Takes [`InputPath`] so multi-segment (concat) inputs span every file
    /// in the timeline - total frames, seeking, and the duration range cover
    /// the whole chain, not just the first segment.
    pub fn open(
        &mut self,
        left: &InputPath,
        right: &InputPath,
        sync_offset: i64,
    ) -> Result<(), SourceError> {
        let source = FfmpegFileSource::open_from_inputs(left, right, sync_offset)?;
        let info = source.info();
        let fps = info.fps;
        self.total_frames = source.total_frames();
        self.frame_duration = if fps > 0.0 {
            Duration::from_secs_f64(1.0 / fps)
        } else {
            Duration::from_millis(33)
        };
        self.info = Some(info);
        self.source = Some(source);
        self.state = PlayState::Paused;
        self.frame_index = 0;
        self.current_frame = None;
        self.playback_anchor = None;

        // Decode the first frame so we have something to display.
        self.step_forward()?;
        Ok(())
    }

    /// Advance one frame. Returns `true` if a new frame is available.
    pub fn step_forward(&mut self) -> Result<bool, SourceError> {
        let source = match self.source.as_mut() {
            Some(s) => s,
            None => return Ok(false),
        };

        match source.next_frame()? {
            Some(stereo) => {
                let (left, right) = match stereo {
                    reco_core::source::StereoFrame::Yuv420p(pair) => (pair.left, pair.right),
                    _ => {
                        return Err(SourceError::Read {
                            reason: "GUI preview expects Yuv420p frames".into(),
                        });
                    }
                };
                self.current_frame = Some(StereoYuv { left, right });
                self.frame_index += 1;
                Ok(true)
            }
            None => {
                self.state = PlayState::Finished;
                Ok(false)
            }
        }
    }

    /// Non-blocking frame advance for the GUI timer.
    ///
    /// Uses `try_next_frame()` to avoid blocking the UI thread on decode.
    /// Returns `true` if a new frame was consumed.
    ///
    /// Timing is drift-free: the ideal frame index is computed from the
    /// wall-clock elapsed time since playback started, so any single
    /// late tick is caught up on the next tick rather than compounding.
    pub fn tick(&mut self) -> Result<bool, SourceError> {
        if self.state != PlayState::Playing {
            return Ok(false);
        }

        let now = Instant::now();

        // Anchor on the first tick after play/resume/seek.
        let (start, start_frame) = match self.playback_anchor {
            Some(a) => a,
            None => {
                self.playback_anchor = Some((now, self.frame_index));
                (now, self.frame_index)
            }
        };

        // Drift-free target: where the playhead SHOULD be based on wall
        // clock, not based on when the last advance happened. Scaling
        // elapsed time by `speed` before dividing by the real frame
        // duration keeps the target frame-accurate at any speed - it's
        // still always a whole multiple of the source's actual FPS.
        let elapsed = now.duration_since(start);
        let frames_since_start =
            (elapsed.as_secs_f64() * self.speed / self.frame_duration.as_secs_f64()) as u64;
        let target_frame = start_frame + frames_since_start;

        if self.frame_index >= target_frame {
            // On schedule or ahead — no advance needed this tick.
            return Ok(false);
        }

        let source = match self.source.as_mut() {
            Some(s) => s,
            None => return Ok(false),
        };

        // Non-blocking: returns None if no frame decoded yet.
        match source.try_next_frame()? {
            Some(stereo) => {
                let (left, right) = match stereo {
                    reco_core::source::StereoFrame::Yuv420p(pair) => (pair.left, pair.right),
                    _ => {
                        return Err(SourceError::Read {
                            reason: "GUI preview expects Yuv420p frames".into(),
                        });
                    }
                };
                self.current_frame = Some(StereoYuv { left, right });
                self.frame_index += 1;
                Ok(true)
            }
            None => {
                // Could be "not ready yet" or "end of stream".
                // `FrameSource::is_exhausted()` answers unambiguously once
                // the decoder channel has disconnected, so no timeout
                // heuristic is needed.
                if source.is_exhausted() {
                    self.state = PlayState::Finished;
                }
                Ok(false)
            }
        }
    }

    /// Toggle play/pause. Returns the new state.
    pub fn toggle(&mut self) -> PlayState {
        match self.state {
            PlayState::Paused | PlayState::Finished => {
                self.state = PlayState::Playing;
                self.playback_anchor = None;
            }
            PlayState::Playing => {
                self.state = PlayState::Paused;
            }
            PlayState::Empty => {}
        }
        self.state
    }

    /// Seek to a normalized position (0.0 to 1.0).
    pub fn seek(&mut self, fraction: f32) -> Result<(), SourceError> {
        let total = match self.total_frames {
            Some(t) if t > 0 => t,
            _ => return Ok(()),
        };
        let target = ((fraction as f64) * total as f64) as u64;
        let target = target.min(total.saturating_sub(1));

        if let Some(source) = self.source.as_mut() {
            source.seek(target)?;
            self.frame_index = target;
            // Reset pacing anchor so playback resumes cleanly from the
            // new position without trying to "catch up" N seconds of
            // skipped frames.
            self.playback_anchor = None;
            self.step_forward()?;
        }
        Ok(())
    }

    pub fn state(&self) -> PlayState {
        self.state
    }

    pub fn current_frame(&self) -> Option<&StereoYuv> {
        self.current_frame.as_ref()
    }

    pub fn frame_index(&self) -> u64 {
        self.frame_index
    }

    pub fn total_frames(&self) -> Option<u64> {
        self.total_frames
    }

    pub fn fps(&self) -> f64 {
        self.info.as_ref().map_or(0.0, |i| i.fps)
    }

    /// Set the playback speed multiplier (1.0 = normal), clamped to
    /// [`SPEED_RANGE`]. Resets the pacing anchor so the new speed takes
    /// effect immediately instead of "catching up" time accumulated at
    /// the old speed (same reset `seek()` does after jumping).
    pub fn set_speed(&mut self, speed: f64) {
        self.speed = speed.clamp(SPEED_RANGE.0, SPEED_RANGE.1);
        self.playback_anchor = None;
    }

    pub fn speed(&self) -> f64 {
        self.speed
    }

    pub fn input_dimensions(&self) -> Option<(u32, u32)> {
        self.info.as_ref().map(|i| (i.width, i.height))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn set_speed_clamps_to_range() {
        let mut p = Playback::new();
        p.set_speed(100.0);
        assert_eq!(p.speed(), SPEED_RANGE.1);
        p.set_speed(0.0);
        assert_eq!(p.speed(), SPEED_RANGE.0);
        p.set_speed(1.5);
        assert_eq!(p.speed(), 1.5);
    }

    #[test]
    fn default_speed_is_normal() {
        assert_eq!(Playback::new().speed(), 1.0);
    }
}
