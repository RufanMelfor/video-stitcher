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
//! In the default single-worker mode ([`new`](AsyncDetectThread::new)),
//! one thread processes jobs strictly in submission order, which is
//! exactly what the tracker contract requires (detections fed exactly
//! once per produce index, in order) - no sequence numbers or
//! reordering buffer needed on top of the channel itself.
//!
//! ## Dual mode: one worker per camera
//!
//! Profiling after the single-worker fix landed found `yolo_inference`
//! at ~90% of wall-clock, running Left then Right sequentially on the
//! one worker thread (~2x per-camera cost per produce index).
//! [`new_dual`](AsyncDetectThread::new_dual) spawns two workers, each
//! with its own detector instance bound to one camera, so Left and
//! Right inference calls can overlap instead of serializing - at the
//! cost of a third loaded model instance (sync fallback path already
//! has one, `new`'s single worker adds a second; `new_dual` needs two
//! for the workers, so the *total* session footprint only grows by one
//! more instance beyond the already-shipped single-worker mode, not by
//! doubling it again). Per-camera submission order is still preserved
//! by each worker's own FIFO queue; results are merged back into a
//! single Left-then-Right `DetectResult` per produce index regardless
//! of which camera's worker finishes first, so callers observe the
//! exact same ordering contract as single-worker mode.

use std::collections::{HashMap, VecDeque};
use std::sync::Mutex;
use std::sync::mpsc::{self, Receiver, SyncSender, TrySendError};
use std::thread::{self, JoinHandle};

use crate::detect::detector::{
    Detection, DetectorError, DetectorFrame, PendingDetection, UnifiedDetector,
};
use crate::geometry::CameraId;

/// One produce index's worth of deferred detection work - usually one
/// [`PendingDetection`] per camera (2), submitted together so the
/// worker's single result message covers the whole produce index.
/// Used by single-worker mode only; dual mode splits this per camera
/// (see [`CameraJob`]).
struct DetectJob {
    produce_index: u64,
    pending: Vec<PendingDetection>,
}

/// One camera's share of a produce index's deferred work, dispatched
/// to that camera's own dedicated worker thread in dual mode.
struct CameraJob {
    produce_index: u64,
    pending: PendingDetection,
}

/// A single camera worker's finished piece of a produce index's
/// result. Both dual-mode workers feed the same channel (`SyncSender`
/// is `Clone`); [`recv`](AsyncDetectThread::recv)/
/// [`try_recv`](AsyncDetectThread::try_recv) merge matching produce
/// indices back into one [`DetectResult`], always Left-then-Right
/// regardless of arrival order.
struct CameraResult {
    produce_index: u64,
    camera: CameraId,
    detections: Vec<Detection>,
}

/// A produce index's resolved detections (both cameras merged, raw
/// camera-space coordinates - the caller still owns panorama mapping
/// and any wrapper post-processing via the `finish` closure captured
/// at submit time).
pub struct DetectResult {
    pub produce_index: u64,
    pub detections: Vec<Detection>,
}

/// How work is dispatched: one shared worker (`new`), or one worker
/// per camera (`new_dual`).
enum Submitter {
    Single(SyncSender<DetectJob>),
    Dual {
        left: SyncSender<CameraJob>,
        right: SyncSender<CameraJob>,
    },
}

/// Dual-mode only: accumulates per-camera pieces until a produce index
/// has both, then releases it in submission order. `Mutex`-guarded
/// because [`recv`](AsyncDetectThread::recv)/
/// [`try_recv`](AsyncDetectThread::try_recv) take `&self` (matching
/// single-worker mode's `Receiver::recv(&self)` signature) but need to
/// mutate this across calls.
#[derive(Default)]
struct MergeState {
    /// Produce indices in submission order - the front is the next
    /// result `recv`/`try_recv` may release, once complete.
    order: VecDeque<u64>,
    left: HashMap<u64, Vec<Detection>>,
    right: HashMap<u64, Vec<Detection>>,
    /// Which cameras a produce index actually expects (a job might
    /// only cover one camera in principle, though today's callers
    /// always submit both) - `recv` only waits on cameras present
    /// here, not unconditionally on both.
    expects: HashMap<u64, (bool, bool)>,
}

impl MergeState {
    /// If the front of `order` has all its expected cameras present,
    /// pop and return it as a merged, Left-then-Right result.
    fn try_take_front(&mut self) -> Option<DetectResult> {
        let &front = self.order.front()?;
        let (expect_left, expect_right) = *self.expects.get(&front)?;
        let have_left = !expect_left || self.left.contains_key(&front);
        let have_right = !expect_right || self.right.contains_key(&front);
        if !have_left || !have_right {
            return None;
        }
        self.order.pop_front();
        self.expects.remove(&front);
        let mut detections = self.left.remove(&front).unwrap_or_default();
        detections.extend(self.right.remove(&front).unwrap_or_default());
        Some(DetectResult {
            produce_index: front,
            detections,
        })
    }

    fn record(&mut self, result: CameraResult) {
        let slot = match result.camera {
            CameraId::Left => &mut self.left,
            CameraId::Right => &mut self.right,
        };
        slot.insert(result.produce_index, result.detections);
    }
}

/// Async detector that runs deferred [`PendingDetection`] jobs on a
/// dedicated thread (or two, in dual mode).
///
/// Created via [`new`](Self::new) (one shared worker - matches every
/// prior release) or [`new_dual`](Self::new_dual) (one worker per
/// camera - lets Left/Right inference overlap). Both moved detector(s)
/// are **separate instances** from whatever detector (if any) the
/// caller keeps for synchronous fallback frames, so they never contend
/// over the same `&mut self`. Call [`submit`](Self::submit) to queue a
/// produce index's jobs, then [`recv`](Self::recv) to block for the
/// next resolved result in FIFO order (matches the tracker's
/// exactly-once-per-produce-index, in-order contract) regardless of
/// which mode is active.
pub struct AsyncDetectThread {
    submitter: Option<Submitter>,
    /// Single-worker mode's finished-result channel. Unused (a
    /// disconnected placeholder) in dual mode - see `dual_result_rx`.
    rx: Receiver<DetectResult>,
    /// Only populated in dual mode; single mode's worker already
    /// merges cameras itself before sending, so `rx` above receives
    /// finished [`DetectResult`]s directly and this stays `None`.
    merge: Option<Mutex<MergeState>>,
    /// Dual mode's shared per-camera result channel. `None` in
    /// single-worker mode.
    dual_result_rx: Option<Receiver<CameraResult>>,
    handles: Vec<JoinHandle<()>>,
}

impl AsyncDetectThread {
    /// Move `inner` to a single background thread. `queue_depth`
    /// bounds how many produce indices' worth of jobs can be in flight
    /// before [`submit`](Self::submit) blocks (backpressure) - should
    /// track the session's lookahead depth so the worker never needs
    /// to get more than one buffer's worth ahead.
    pub fn new(inner: Box<dyn UnifiedDetector>, queue_depth: usize) -> Self {
        let queue_depth = queue_depth.max(1);
        let (tx, job_rx) = mpsc::sync_channel::<DetectJob>(queue_depth);
        let (result_tx, rx) = mpsc::sync_channel::<DetectResult>(queue_depth);

        let handle = thread::Builder::new()
            .name("detect".into())
            .spawn(move || Self::single_worker_loop(job_rx, result_tx, inner))
            .expect("spawn detect thread");

        Self {
            submitter: Some(Submitter::Single(tx)),
            rx,
            merge: None,
            dual_result_rx: None,
            handles: vec![handle],
        }
    }

    /// Move `left`/`right` to two dedicated background threads, one
    /// per camera, so their inference calls can overlap instead of
    /// running back-to-back on a single worker. Otherwise behaves
    /// identically to [`new`](Self::new): same backpressure semantics
    /// per camera queue, same FIFO-by-produce-index contract on
    /// [`recv`](Self::recv)/[`try_recv`](Self::try_recv), merging both
    /// cameras' pieces Left-then-Right once both have arrived.
    pub fn new_dual(
        left: Box<dyn UnifiedDetector>,
        right: Box<dyn UnifiedDetector>,
        queue_depth: usize,
    ) -> Self {
        let queue_depth = queue_depth.max(1);
        let (left_tx, left_rx) = mpsc::sync_channel::<CameraJob>(queue_depth);
        let (right_tx, right_rx) = mpsc::sync_channel::<CameraJob>(queue_depth);
        // Both workers share one result channel (`SyncSender` clones
        // fine for multi-producer use) - `recv`/`try_recv` merge
        // whichever piece arrives first with its sibling.
        let (result_tx, result_rx) = mpsc::sync_channel::<CameraResult>(queue_depth * 2);

        let left_result_tx = result_tx.clone();
        let left_handle = thread::Builder::new()
            .name("detect-left".into())
            .spawn(move || Self::camera_worker_loop(left_rx, left_result_tx, left))
            .expect("spawn left detect thread");

        let right_handle = thread::Builder::new()
            .name("detect-right".into())
            .spawn(move || Self::camera_worker_loop(right_rx, result_tx, right))
            .expect("spawn right detect thread");

        Self {
            submitter: Some(Submitter::Dual {
                left: left_tx,
                right: right_tx,
            }),
            rx: {
                // `rx` (single-worker's DetectResult channel) is
                // unused in dual mode - `recv`/`try_recv` read from
                // `dual_result_rx` instead. Keep a disconnected
                // placeholder so the struct doesn't need an `Option`
                // on the hot field.
                let (_placeholder_tx, placeholder_rx) = mpsc::sync_channel(1);
                placeholder_rx
            },
            merge: Some(Mutex::new(MergeState::default())),
            dual_result_rx: Some(result_rx),
            handles: vec![left_handle, right_handle],
        }
    }

    /// Queue `pending` (typically one job per camera) for `produce_index`.
    /// Blocks only if a worker has fallen more than `queue_depth`
    /// produce indices behind - in steady state, with the lookahead
    /// buffer providing slack between produce and resolution, this
    /// should rarely if ever block.
    pub fn submit(&self, produce_index: u64, pending: Vec<PendingDetection>) {
        match self.submitter.as_ref() {
            Some(Submitter::Single(tx)) => {
                let job = DetectJob {
                    produce_index,
                    pending,
                };
                match tx.try_send(job) {
                    Ok(()) => {}
                    Err(TrySendError::Full(job)) => {
                        // Real backpressure: the worker hasn't kept up.
                        // Block - nothing useful to overlap with while
                        // the queue is genuinely full.
                        let _ = tx.send(job);
                    }
                    Err(TrySendError::Disconnected(_)) => {
                        log::warn!("AsyncDetectThread: worker thread died, dropping submit");
                    }
                }
            }
            Some(Submitter::Dual { left, right }) => {
                let (expect_left, expect_right) = (
                    pending.iter().any(|p| p.camera == CameraId::Left),
                    pending.iter().any(|p| p.camera == CameraId::Right),
                );
                if let Some(merge) = &self.merge {
                    let mut st = merge.lock().expect("merge state poisoned");
                    st.order.push_back(produce_index);
                    st.expects
                        .insert(produce_index, (expect_left, expect_right));
                }
                for p in pending {
                    let (tx, label) = match p.camera {
                        CameraId::Left => (left, "left"),
                        CameraId::Right => (right, "right"),
                    };
                    let job = CameraJob {
                        produce_index,
                        pending: p,
                    };
                    match tx.try_send(job) {
                        Ok(()) => {}
                        Err(TrySendError::Full(job)) => {
                            let _ = tx.send(job);
                        }
                        Err(TrySendError::Disconnected(_)) => {
                            log::warn!(
                                "AsyncDetectThread: {label} worker thread died, dropping submit"
                            );
                        }
                    }
                }
            }
            None => {} // worker(s) already shut down
        }
    }

    /// Block for the next resolved result, in strict FIFO submission
    /// order. Returns `None` once the worker(s) have shut down and no
    /// more results are coming.
    pub fn recv(&self) -> Option<DetectResult> {
        let Some(merge) = &self.merge else {
            return self.rx.recv().ok();
        };
        let dual_rx = self.dual_result_rx();
        loop {
            if let Some(result) = merge.lock().expect("merge state poisoned").try_take_front() {
                return Some(result);
            }
            let camera_result = dual_rx.recv().ok()?;
            merge
                .lock()
                .expect("merge state poisoned")
                .record(camera_result);
        }
    }

    /// Non-blocking poll for a result, if one is already ready.
    pub fn try_recv(&self) -> Option<DetectResult> {
        let Some(merge) = &self.merge else {
            return self.rx.try_recv().ok();
        };
        let dual_rx = self.dual_result_rx();
        // Drain whatever's immediately available without blocking,
        // then see if the front of the order is complete.
        while let Ok(camera_result) = dual_rx.try_recv() {
            merge
                .lock()
                .expect("merge state poisoned")
                .record(camera_result);
        }
        merge.lock().expect("merge state poisoned").try_take_front()
    }

    fn single_worker_loop(
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

    fn camera_worker_loop(
        rx: Receiver<CameraJob>,
        result_tx: SyncSender<CameraResult>,
        mut inner: Box<dyn UnifiedDetector>,
    ) {
        while let Ok(job) = rx.recv() {
            crate::profile_scope!("async_detect_worker");
            let p = job.pending;
            let frame = DetectorFrame::PreprocessedChw {
                data: &p.tensor,
                input_size: p.input_size,
                src_width: p.src_width,
                src_height: p.src_height,
            };
            let detections = match inner.detect(p.camera, &frame) {
                Ok(dets) => dets,
                Err(DetectorError::UnsupportedFrameKind) => {
                    log::debug!(
                        "AsyncDetectThread: detector '{}' does not support PreprocessedChw",
                        inner.name()
                    );
                    Vec::new()
                }
                Err(e) => {
                    log::warn!(
                        "AsyncDetectThread: detector '{}' {:?}: {e}",
                        inner.name(),
                        p.camera
                    );
                    Vec::new()
                }
            };
            if result_tx
                .send(CameraResult {
                    produce_index: job.produce_index,
                    camera: p.camera,
                    detections,
                })
                .is_err()
            {
                break; // receiver gone, session is shutting down
            }
        }
    }

    /// Stop accepting new work and join the worker thread(s). Any jobs
    /// already queued still get processed; their results remain
    /// available via [`recv`](Self::recv)/[`try_recv`](Self::try_recv)
    /// until the channel(s) drain.
    pub fn finish(&mut self) {
        self.submitter.take();
        for handle in self.handles.drain(..) {
            let _ = handle.join();
        }
    }

    fn dual_result_rx(&self) -> &Receiver<CameraResult> {
        self.dual_result_rx
            .as_ref()
            .expect("dual_result_rx set whenever merge is Some")
    }
}

impl Drop for AsyncDetectThread {
    fn drop(&mut self) {
        // Drop the sender(s) BEFORE joining, same reasoning as
        // `AsyncEncodeThread::drop` - otherwise a worker's `rx.recv()`
        // never sees a disconnect and the join blocks forever.
        self.submitter.take();
        for handle in self.handles.drain(..) {
            let _ = handle.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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

    #[test]
    fn dual_resolves_results_in_submission_order() {
        let thread = AsyncDetectThread::new_dual(
            Box::new(CountingDetector { next_id: 0 }),
            Box::new(CountingDetector { next_id: 100 }),
            4,
        );
        for i in 0..5u64 {
            thread.submit(
                i,
                vec![make_pending(CameraId::Left), make_pending(CameraId::Right)],
            );
        }
        for expected_index in 0..5u64 {
            let result = thread.recv().expect("result available");
            assert_eq!(result.produce_index, expected_index);
            assert_eq!(result.detections.len(), 2);
            assert_eq!(result.detections[0].camera, CameraId::Left);
            assert_eq!(result.detections[1].camera, CameraId::Right);
        }
    }

    #[test]
    fn dual_single_camera_job_does_not_wait_on_the_other() {
        let thread = AsyncDetectThread::new_dual(
            Box::new(CountingDetector { next_id: 0 }),
            Box::new(CountingDetector { next_id: 100 }),
            4,
        );
        thread.submit(0, vec![make_pending(CameraId::Left)]);
        let result = thread.recv().expect("result available");
        assert_eq!(result.produce_index, 0);
        assert_eq!(result.detections.len(), 1);
        assert_eq!(result.detections[0].camera, CameraId::Left);
    }

    #[test]
    fn dual_drop_without_finish_does_not_hang() {
        let thread = AsyncDetectThread::new_dual(
            Box::new(CountingDetector { next_id: 0 }),
            Box::new(CountingDetector { next_id: 100 }),
            4,
        );
        thread.submit(
            0,
            vec![make_pending(CameraId::Left), make_pending(CameraId::Right)],
        );
        // Deliberately dropped without draining - must not deadlock.
    }
}
