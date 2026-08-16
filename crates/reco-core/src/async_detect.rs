//! Async detect thread for pipelined AI inference.
//!
//! Wraps any [`UnifiedDetector`] and runs its heavy inference calls on a
//! dedicated thread, decoupling the lookahead-buffer *produce* loop from
//! detection latency the same way [`crate::async_encode::AsyncEncodeThread`]
//! decouples rendering from encoder latency.
//!
//! ## Why this exists
//!
//! The lookahead buffer already decouples *when* detection runs from
//! *when* a frame renders (it pre-detects N frames ahead so the panner
//! can see future [`WorldState`](crate::detect::tracker::WorldState)s) -
//! but not *which thread* pays for it. Profiling found AI detection
//! (GPU readback + preprocess + inference) consuming ~87% of active
//! per-frame time in the buffered export loop, running synchronously
//! and blocking the same thread that decodes/stitches/encodes.
//!
//! ## Scope: inference only, not preprocessing
//!
//! [`UnifiedDetector::detect_split`] deliberately keeps GPU texture
//! readback + preprocessing synchronous (textures are borrowed from a
//! small reusable staging ring - not safe to hand to another thread)
//! and only defers the pure-tensor inference call, which this thread
//! runs. This is the safer partial fix identified during the initial
//! design pass: real reduction in blocking time without new,
//! uncompile-tested GPU cross-thread texture code.
//!
//! ## Ordering
//!
//! A single worker thread processes jobs strictly in submission order,
//! which is exactly what the tracker contract requires (detections fed
//! exactly once per produce index, in order) - no sequence numbers or
//! reordering buffer needed on top of the channel itself.

use std::sync::mpsc::{self, Receiver, SyncSender, TrySendError};
use std::thread::{self, JoinHandle};

use crate::detect::detector::{
    Detection, DetectorError, DetectorFrame, PendingDetection, UnifiedDetector,
};

/// One produce index's worth of deferred detection work - usually one
/// [`PendingDetection`] per camera (2), submitted together so the
/// worker's single result message covers the whole produce index.
struct DetectJob {
    produce_index: u64,
    pending: Vec<PendingDetection>,
}

/// A produce index's resolved detections (both cameras merged, raw
/// camera-space coordinates - the caller still owns panorama mapping
/// and any wrapper post-processing via the `finish` closure captured
/// at submit time).
pub struct DetectResult {
    pub produce_index: u64,
    pub detections: Vec<Detection>,
}

/// Async detector that runs deferred [`PendingDetection`] jobs on a
/// dedicated thread.
///
/// Created via [`new`](Self::new), which moves the inner detector to a
/// background thread - a **separate instance** from whatever detector
/// (if any) the caller keeps for synchronous fallback frames, so the
/// two never contend over the same `&mut self`. Call
/// [`submit`](Self::submit) to queue a produce index's jobs, then
/// [`recv`](Self::recv) to block for the next resolved result in FIFO
/// order (matches the tracker's exactly-once-per-produce-index, in-
/// order contract with zero extra bookkeeping).
pub struct AsyncDetectThread {
    tx: Option<SyncSender<DetectJob>>,
    rx: Receiver<DetectResult>,
    handle: Option<JoinHandle<()>>,
}

impl AsyncDetectThread {
    /// Move `inner` to a background thread. `queue_depth` bounds how
    /// many produce indices' worth of jobs can be in flight before
    /// [`submit`](Self::submit) blocks (backpressure) - should track
    /// the session's lookahead depth so the worker never needs to get
    /// more than one buffer's worth ahead.
    pub fn new(inner: Box<dyn UnifiedDetector>, queue_depth: usize) -> Self {
        let queue_depth = queue_depth.max(1);
        let (tx, job_rx) = mpsc::sync_channel::<DetectJob>(queue_depth);
        let (result_tx, rx) = mpsc::sync_channel::<DetectResult>(queue_depth);

        let handle = thread::Builder::new()
            .name("detect".into())
            .spawn(move || Self::detect_loop(job_rx, result_tx, inner))
            .expect("spawn detect thread");

        Self {
            tx: Some(tx),
            rx,
            handle: Some(handle),
        }
    }

    /// Queue `pending` (typically one job per camera) for `produce_index`.
    /// Blocks only if the worker has fallen more than `queue_depth`
    /// produce indices behind - in steady state, with the lookahead
    /// buffer providing slack between produce and resolution, this
    /// should rarely if ever block.
    pub fn submit(&self, produce_index: u64, pending: Vec<PendingDetection>) {
        let Some(tx) = self.tx.as_ref() else {
            return; // worker already shut down
        };
        let job = DetectJob {
            produce_index,
            pending,
        };
        match tx.try_send(job) {
            Ok(()) => {}
            Err(TrySendError::Full(job)) => {
                // Real backpressure: the worker hasn't kept up. Block -
                // there is nothing useful to overlap with while the
                // queue is genuinely full.
                let _ = tx.send(job);
            }
            Err(TrySendError::Disconnected(_)) => {
                log::warn!("AsyncDetectThread: worker thread died, dropping submit");
            }
        }
    }

    /// Block for the next resolved result, in strict FIFO submission
    /// order. Returns `None` once the worker has shut down and no more
    /// results are coming.
    pub fn recv(&self) -> Option<DetectResult> {
        self.rx.recv().ok()
    }

    /// Non-blocking poll for a result, if one is already ready.
    pub fn try_recv(&self) -> Option<DetectResult> {
        self.rx.try_recv().ok()
    }

    fn detect_loop(
        rx: Receiver<DetectJob>,
        result_tx: SyncSender<DetectResult>,
        mut inner: Box<dyn UnifiedDetector>,
    ) {
        while let Ok(job) = rx.recv() {
            crate::profile_scope!("async_detect_worker");
            let mut merged = Vec::new();
            for p in job.pending {
                let frame = DetectorFrame::PreprocessedChw {
                    data: &p.tensor,
                    input_size: p.input_size,
                    src_width: p.src_width,
                    src_height: p.src_height,
                };
                match inner.detect(p.camera, &frame) {
                    Ok(dets) => merged.extend(dets),
                    Err(DetectorError::UnsupportedFrameKind) => log::debug!(
                        "AsyncDetectThread: detector '{}' does not support PreprocessedChw",
                        inner.name()
                    ),
                    Err(e) => {
                        log::warn!(
                            "AsyncDetectThread: detector '{}' {:?}: {e}",
                            inner.name(),
                            p.camera
                        )
                    }
                }
            }
            if result_tx
                .send(DetectResult {
                    produce_index: job.produce_index,
                    detections: merged,
                })
                .is_err()
            {
                break; // receiver gone, session is shutting down
            }
        }
    }

    /// Stop accepting new work and join the worker thread. Any jobs
    /// already queued still get processed; their results remain
    /// available via [`recv`](Self::recv)/[`try_recv`](Self::try_recv)
    /// until the channel drains.
    pub fn finish(&mut self) {
        self.tx.take();
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

impl Drop for AsyncDetectThread {
    fn drop(&mut self) {
        // Drop the sender BEFORE joining, same reasoning as
        // `AsyncEncodeThread::drop` - otherwise the worker's `rx.recv()`
        // never sees a disconnect and the join blocks forever.
        self.tx.take();
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geometry::CameraId;

    /// Fake inner detector: returns one synthetic detection per call,
    /// tagged with an incrementing counter so tests can verify ordering.
    struct CountingDetector {
        next_id: u16,
    }

    impl UnifiedDetector for CountingDetector {
        fn name(&self) -> &'static str {
            "counting-fake"
        }

        fn detect(
            &mut self,
            camera: CameraId,
            frame: &DetectorFrame<'_>,
        ) -> Result<Vec<Detection>, DetectorError> {
            match frame {
                DetectorFrame::PreprocessedChw { .. } => {
                    let id = self.next_id;
                    self.next_id += 1;
                    Ok(vec![Detection {
                        camera,
                        class_id: id,
                        confidence: 0.9,
                        center_x: 0.5,
                        center_y: 0.5,
                        width: 0.1,
                        height: 0.1,
                    }])
                }
                _ => Err(DetectorError::UnsupportedFrameKind),
            }
        }
    }

    fn make_pending(camera: CameraId) -> PendingDetection {
        PendingDetection {
            camera,
            tensor: vec![0.0; 3 * 4 * 4],
            input_size: 4,
            src_width: 8,
            src_height: 8,
        }
    }

    #[test]
    fn resolves_results_in_submission_order() {
        let thread = AsyncDetectThread::new(Box::new(CountingDetector { next_id: 0 }), 4);

        for i in 0..5u64 {
            thread.submit(i, vec![make_pending(CameraId::Left)]);
        }

        for expected_index in 0..5u64 {
            let result = thread.recv().expect("result available");
            assert_eq!(result.produce_index, expected_index);
            assert_eq!(result.detections.len(), 1);
        }
    }

    #[test]
    fn merges_both_cameras_into_one_result() {
        let thread = AsyncDetectThread::new(Box::new(CountingDetector { next_id: 0 }), 4);
        thread.submit(
            0,
            vec![make_pending(CameraId::Left), make_pending(CameraId::Right)],
        );
        let result = thread.recv().expect("result available");
        assert_eq!(result.detections.len(), 2);
        assert_eq!(result.detections[0].camera, CameraId::Left);
        assert_eq!(result.detections[1].camera, CameraId::Right);
    }

    #[test]
    fn drop_without_finish_does_not_hang() {
        let thread = AsyncDetectThread::new(Box::new(CountingDetector { next_id: 0 }), 4);
        thread.submit(0, vec![make_pending(CameraId::Left)]);
        // Deliberately dropped without calling finish() or draining
        // recv() - must not deadlock (regression guard for the
        // AsyncEncodeThread::drop ordering lesson).
    }
}
