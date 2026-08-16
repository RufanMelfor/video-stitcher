//! Throwaway benchmark: is batching Left+Right into one TensorRT call
//! faster than two sequential calls?
//!
//! Not wired into any product code path - answers the question with a
//! real measurement before investing in `AsyncDetectThread` batch
//! plumbing + CLI/GUI wiring for something that might not help (the
//! `--async-detect-dual` lesson: measure first, build second).
//!
//! Pure inference-latency comparison - the input tensor content is
//! irrelevant (grey letterbox fill), only wall-clock per call matters.
//!
//! Usage:
//! ```text
//! cargo run --release --example bench_lr_batch -p reco-detect --features tensorrt -- \
//!     <batch1.onnx> <batch2.onnx>
//! ```

use std::time::Instant;

use ort::value::TensorRef;

const INPUT_SIZE: usize = 1920;
const ITERS: usize = 60;
const WARMUP: usize = 10;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let (batch1_path, batch2_path) = match args.as_slice() {
        [_, b1, b2] => (b1.clone(), b2.clone()),
        _ => {
            eprintln!("usage: bench_lr_batch <batch1.onnx> <batch2.onnx>");
            std::process::exit(1);
        }
    };

    let sz = INPUT_SIZE;
    let plane = sz * sz;
    let grey = 114.0 / 255.0_f32;
    let one_frame: Vec<f32> = vec![grey; 3 * plane];
    let two_frames: Vec<f32> = {
        let mut v = one_frame.clone();
        v.extend_from_slice(&one_frame);
        v
    };

    println!("Loading batch=1 model: {batch1_path}");
    let (mut session1, input_size1, _) =
        reco_detect::ort_session::create_ort_session(batch1_path.as_ref(), Vec::new())
            .expect("load batch1 model");
    assert_eq!(input_size1 as usize, sz, "batch1 model input size mismatch");

    println!("Loading batch=2 model: {batch2_path}");
    let (mut session2, input_size2, _) =
        reco_detect::ort_session::create_ort_session(batch2_path.as_ref(), Vec::new())
            .expect("load batch2 model");
    assert_eq!(input_size2 as usize, sz, "batch2 model input size mismatch");

    // Warmup both (TensorRT engine JIT-compile / cache load happens on
    // the first call(s) - exclude that from the timed measurement).
    for _ in 0..WARMUP {
        let t = TensorRef::from_array_view(([1, 3, sz, sz], one_frame.as_slice())).unwrap();
        let _ = session1.run(ort::inputs![t]).unwrap();
        let t = TensorRef::from_array_view(([1, 3, sz, sz], one_frame.as_slice())).unwrap();
        let _ = session1.run(ort::inputs![t]).unwrap();
        let t2 = TensorRef::from_array_view(([2, 3, sz, sz], two_frames.as_slice())).unwrap();
        let _ = session2.run(ort::inputs![t2]).unwrap();
    }

    // Sequential: two batch=1 calls, timed together as one "produce
    // index" cost (matches today's single-worker AsyncDetectThread
    // behavior: L then R, back to back).
    let start = Instant::now();
    for _ in 0..ITERS {
        let t = TensorRef::from_array_view(([1, 3, sz, sz], one_frame.as_slice())).unwrap();
        let _ = session1.run(ort::inputs![t]).unwrap();
        let t = TensorRef::from_array_view(([1, 3, sz, sz], one_frame.as_slice())).unwrap();
        let _ = session1.run(ort::inputs![t]).unwrap();
    }
    let sequential = start.elapsed();

    // Batched: one batch=2 call per "produce index".
    let start = Instant::now();
    for _ in 0..ITERS {
        let t2 = TensorRef::from_array_view(([2, 3, sz, sz], two_frames.as_slice())).unwrap();
        let _ = session2.run(ort::inputs![t2]).unwrap();
    }
    let batched = start.elapsed();

    let seq_avg_ms = sequential.as_secs_f64() * 1000.0 / ITERS as f64;
    let batch_avg_ms = batched.as_secs_f64() * 1000.0 / ITERS as f64;
    println!();
    println!("iters={ITERS}");
    println!("sequential (2x batch=1): {sequential:?} total, {seq_avg_ms:.2}ms/produce-index avg");
    println!("batched   (1x batch=2): {batched:?} total, {batch_avg_ms:.2}ms/produce-index avg");
    println!(
        "speedup: {:.2}x",
        seq_avg_ms / batch_avg_ms.max(f64::EPSILON)
    );
}
