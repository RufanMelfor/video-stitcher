//! Throwaway benchmark: how much slower is tiled dual-crop inference
//! (2 tiles/camera x 2 cameras = 4 inference calls/produce-index) than
//! today's whole-frame letterbox inference (1 call/camera = 2 calls/
//! produce-index)?
//!
//! Not wired into any product code path - answers the question with a
//! real measurement before touching `trt/mod.rs`/`ort_gpu.rs`'s NPP
//! resize/inference paths for something whose cost might rule it out
//! (same "measure first, build second" discipline as `bench_lr_batch.rs`,
//! which this file's structure mirrors).
//!
//! Pure inference-latency comparison - the input tensor content is
//! irrelevant (grey letterbox fill for the whole-frame case, grey fill
//! for the tile case too since NPP resize/crop cost, not tensor content,
//! is what's being measured), only wall-clock per call matters. This
//! does NOT model the NPP crop+resize cost itself (that's GPU-side NPP
//! work this ORT-only benchmark never touches) - it isolates just the
//! `session.run()` inference cost of 2 vs 4 calls, which was reported as
//! the dominant component (~69ms of ~127ms) in the existing measured
//! per-detection-tick budget (SESSION_HANDOFF.md 2026-08-15 finding).
//!
//! Usage:
//! ```text
//! cargo run --release --example bench_tiled_vs_whole -p reco-detect --features tensorrt -- \
//!     <model.onnx>
//! ```

use std::time::Instant;

use ort::value::TensorRef;

const INPUT_SIZE: usize = 1920;
const ITERS: usize = 60;
const WARMUP: usize = 10;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let model_path = match args.as_slice() {
        [_, m] => m.clone(),
        _ => {
            eprintln!("usage: bench_tiled_vs_whole <model.onnx>");
            std::process::exit(1);
        }
    };

    let sz = INPUT_SIZE;
    let plane = sz * sz;
    let grey = 114.0 / 255.0_f32;
    let one_frame: Vec<f32> = vec![grey; 3 * plane];

    println!("Loading model: {model_path}");
    let (mut session, input_size, _) =
        reco_detect::ort_session::create_ort_session(model_path.as_ref(), Vec::new())
            .expect("load model");
    assert_eq!(input_size as usize, sz, "model input size mismatch");

    // Warmup: TensorRT engine JIT-compile / cache load happens on the
    // first call(s) - exclude that from the timed measurement. Run 4
    // calls per warmup iter so both paths below are equally warmed up.
    for _ in 0..WARMUP {
        for _ in 0..4 {
            let t = TensorRef::from_array_view(([1, 3, sz, sz], one_frame.as_slice())).unwrap();
            let _ = session.run(ort::inputs![t]).unwrap();
        }
    }

    // Today's path: 2 calls/produce-index (one whole-frame inference
    // per camera, no tiling).
    let start = Instant::now();
    for _ in 0..ITERS {
        for _ in 0..2 {
            let t = TensorRef::from_array_view(([1, 3, sz, sz], one_frame.as_slice())).unwrap();
            let _ = session.run(ort::inputs![t]).unwrap();
        }
    }
    let whole_frame = start.elapsed();

    // Proposed tiled path: 4 calls/produce-index (2 tiles x 2 cameras).
    let start = Instant::now();
    for _ in 0..ITERS {
        for _ in 0..4 {
            let t = TensorRef::from_array_view(([1, 3, sz, sz], one_frame.as_slice())).unwrap();
            let _ = session.run(ort::inputs![t]).unwrap();
        }
    }
    let tiled = start.elapsed();

    let whole_avg_ms = whole_frame.as_secs_f64() * 1000.0 / ITERS as f64;
    let tiled_avg_ms = tiled.as_secs_f64() * 1000.0 / ITERS as f64;
    println!();
    println!("iters={ITERS}");
    println!(
        "whole-frame (2x calls/produce-index): {whole_frame:?} total, {whole_avg_ms:.2}ms/produce-index avg"
    );
    println!(
        "tiled       (4x calls/produce-index): {tiled:?} total, {tiled_avg_ms:.2}ms/produce-index avg"
    );
    println!(
        "slowdown: {:.2}x (inference-only; excludes NPP crop/resize cost on either path)",
        tiled_avg_ms / whole_avg_ms.max(f64::EPSILON)
    );
}
