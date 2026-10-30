//! Measure how well each setup lines the two cameras up, in rendered
//! pixels - the "does it look right" metric, independent of any matched
//! feature points - and optionally refine a setup against it. Thin
//! front end over [`reco_calibrate::seam_refine`]; see its module doc
//! for the two score modes.
//!
//! Per setup it prints the ZNCC per far / mid / near third of the overlap
//! (1.0 = identical; doubled content lowers it), the vertical disparity
//! |dy| per 16-row band (0 = lines continue across the seam), and in seam
//! mode the zoomed far-field view. The view always looks straight at the
//! seam (`SingleCameraRenderer` ignores the setup's framing tilt/roll).
//!
//! `--seam-offset S` scores in seam mode at S (the value used for
//! export); without it, overlap mode. `--refine` runs
//! `seam_refine::refine_alignment` from the LAST setup (both stages, or
//! only the seam stage with `--seam-only`), scores the result on
//! `--holdout` frames (never used in the search) and writes
//! `<output_dir>/refined_setup.json`.
//!
//! Usage:
//!   cargo run --release -p reco-calibrate --features io --example seam_alignment -- \
//!     <left.mp4> <right.mp4> <output_dir> --frames N1,N2,... \
//!     [--holdout M1,M2,...] [--seam-offset S] [--refine] [--seam-only] \
//!     [--fov-degrees F] [--far-pitch P] [--far-fov F] [--far-weight W] \
//!     [--seam-strip PX] [--max-iters N] [--eval-width W] [--eval-height H] \
//!     <setup.json>...
//!
//! Frame numbers are left-camera frames; the right camera uses the first
//! setup's `sync_offset`. Composites of the first frame (scored region
//! outlined red) are written to `<output_dir>/seam_<setup>[_far].png`.

use std::path::Path;
use std::sync::atomic::AtomicBool;

use reco_calibrate::seam_refine::{
    self, REFINE_PARAMS, ScoreMode, SeamEvaluator, SeamRefineOptions, SeamScore,
};
use reco_core::calibration::Calibration;
use reco_core::gpu::GpuContext;
use reco_core::source::YuvFrame;

fn print_score(label: &str, s: &SeamScore) {
    println!("\n== {label}");
    let stats = |v: &[f32]| {
        let mut v = v.to_vec();
        v.sort_by(f32::total_cmp);
        let median = v.get(v.len() / 2).copied().unwrap_or(f32::NAN);
        let max = v.last().copied().unwrap_or(f32::NAN);
        format!(
            "|dy| median {median:5.2} px  max {max:5.2} px  [{} bands]",
            v.len()
        )
    };
    for (ti, name) in ["far ", "mid ", "near"].iter().enumerate() {
        println!("   {name}: {}   zncc {:.4}", stats(&s.dy[ti]), s.zncc[ti]);
    }
    if let Some(z) = s.far_zoom {
        println!("   far zoom: {}   zncc {z:.4}", stats(&s.far_dy));
    }
    println!("   objective {:.4}", s.objective);
}

fn save_composites(
    eval: &SeamEvaluator,
    cal: &Calibration,
    pair: &(YuvFrame, YuvFrame),
    mode: ScoreMode,
    base: &str,
) {
    let o = eval.options();
    for (i, rgba) in eval.composites(cal, pair, mode).into_iter().enumerate() {
        let path = if i == 0 {
            format!("{base}.png")
        } else {
            format!("{base}_far.png")
        };
        image::RgbaImage::from_raw(o.eval_width, o.eval_height, rgba)
            .expect("rgba buffer size mismatch")
            .save(&path)
            .expect("cannot write composite");
    }
}

/// Decode left frames and the matching right frames (`sync_offset`).
fn load_pairs(
    left: &str,
    right: &str,
    frames: &[u64],
    sync_offset: i64,
) -> Vec<(YuvFrame, YuvFrame)> {
    let right_indices: Vec<u64> = frames
        .iter()
        .map(|f| (*f as i64 + sync_offset).max(0) as u64)
        .collect();
    let l = reco_io::ffmpeg::calibration_io::extract_frames(Path::new(left), frames)
        .expect("failed to extract left frames");
    let r = reco_io::ffmpeg::calibration_io::extract_frames(Path::new(right), &right_indices)
        .expect("failed to extract right frames");
    l.into_iter().zip(r).collect()
}

fn parse_frames(value: &str, flag: &str) -> Vec<u64> {
    value
        .split(',')
        .map(|s| {
            s.trim()
                .parse()
                .unwrap_or_else(|_| panic!("{flag}: comma-separated u64"))
        })
        .collect()
}

fn main() {
    reco_io::init();

    let args: Vec<String> = std::env::args().collect();
    if args.len() < 6 {
        eprintln!(
            "Usage: {} <left.mp4> <right.mp4> <output_dir> --frames N1,N2,... \
             [--holdout M1,M2,...] [--seam-offset S] [--refine] [--seam-only] \
             [--fov-degrees F] [--far-pitch P] [--far-fov F] [--far-weight W] \
             [--seam-strip PX] [--max-iters N] [--eval-width W] [--eval-height H] \
             <setup.json>...",
            args[0]
        );
        std::process::exit(1);
    }
    let (left_path, right_path, output_dir) = (&args[1], &args[2], &args[3]);
    let rest = &args[4..];

    let mut frames: Vec<u64> = Vec::new();
    let mut holdout: Vec<u64> = Vec::new();
    let mut refine = false;
    let mut seam_only = false;
    let mut seam_offset: Option<f32> = None;
    let mut opts = SeamRefineOptions::default();
    let mut setup_paths: Vec<&String> = Vec::new();
    let mut i = 0;
    while i < rest.len() {
        let value = || rest.get(i + 1).expect("flag needs a value");
        let mut step = 2;
        match rest[i].as_str() {
            "--frames" => frames = parse_frames(value(), "--frames"),
            "--holdout" => holdout = parse_frames(value(), "--holdout"),
            "--seam-offset" => seam_offset = Some(value().parse().expect("--seam-offset: number")),
            "--fov-degrees" => opts.fov_degrees = value().parse().expect("--fov-degrees: number"),
            "--far-pitch" => opts.far_pitch = value().parse().expect("--far-pitch: rad"),
            "--far-fov" => opts.far_fov = value().parse().expect("--far-fov: deg (0 = off)"),
            "--far-weight" => opts.far_weight = value().parse().expect("--far-weight: number"),
            "--seam-strip" => opts.seam_strip_px = value().parse().expect("--seam-strip: px"),
            "--max-iters" => opts.max_iters = value().parse().expect("--max-iters: integer"),
            "--eval-width" => opts.eval_width = value().parse().expect("--eval-width: integer"),
            "--eval-height" => opts.eval_height = value().parse().expect("--eval-height: integer"),
            "--refine" => {
                refine = true;
                step = 1;
            }
            "--seam-only" => {
                seam_only = true;
                step = 1;
            }
            _ => {
                setup_paths.push(&rest[i]);
                step = 1;
            }
        }
        i += step;
    }
    assert!(!frames.is_empty(), "--frames is required");
    assert!(
        !setup_paths.is_empty(),
        "at least one setup.json is required"
    );
    assert!(
        !refine || seam_offset.is_some(),
        "--refine needs --seam-offset (the seam position to refine for)"
    );
    std::fs::create_dir_all(output_dir).expect("cannot create output_dir");

    let setups: Vec<(String, Calibration)> = setup_paths
        .iter()
        .map(|p| {
            let cal = Calibration::from_file(Path::new(p))
                .unwrap_or_else(|e| panic!("cannot load setup {p}: {e}"));
            let name = Path::new(p)
                .file_stem()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| (*p).clone());
            (name, cal)
        })
        .collect();
    let sync_offset = setups[0].1.sync_offset;
    if setups.iter().any(|(_, c)| c.sync_offset != sync_offset) {
        eprintln!("note: setups differ in sync_offset; using the first setup's ({sync_offset})");
    }

    let pairs = load_pairs(left_path, right_path, &frames, sync_offset);
    let holdout_pairs = if holdout.is_empty() {
        Vec::new()
    } else {
        load_pairs(left_path, right_path, &holdout, sync_offset)
    };
    let gpu = GpuContext::new_blocking().expect("no GPU");
    let (in_w, in_h) = (pairs[0].0.width, pairs[0].0.height);
    let mode = match seam_offset {
        Some(s) => ScoreMode::Seam { seam_offset: s },
        None => ScoreMode::Overlap,
    };
    println!("Frames {frames:?} (holdout {holdout:?}), mode {mode:?}, {opts:?}");
    let eval = SeamEvaluator::new(&gpu, in_w, in_h, opts.clone());

    let report = |name: &str, cal: &Calibration| {
        save_composites(
            &eval,
            cal,
            &pairs[0],
            mode,
            &format!("{output_dir}/seam_{name}"),
        );
        print_score(name, &eval.score(cal, &pairs, mode, true));
        if !holdout_pairs.is_empty() {
            print_score(
                &format!("{name} [holdout]"),
                &eval.score(cal, &holdout_pairs, mode, true),
            );
        }
    };
    for (name, cal) in &setups {
        report(name, cal);
    }

    let Some(seam_offset) = seam_offset.filter(|_| refine) else {
        return;
    };
    let (base_name, base) = setups.last().expect("at least one setup");
    println!(
        "\nRefining '{base_name}' ({} stages, {} iterations max each) ...",
        if seam_only { 1 } else { 2 },
        opts.max_iters
    );
    let started = std::time::Instant::now();
    let outcome = seam_refine::refine_alignment(
        &gpu,
        base,
        &pairs,
        seam_offset,
        seam_only,
        opts,
        &mut |_, _, _| {},
        &AtomicBool::new(false),
    )
    .expect("refinement failed");
    println!(
        "Refined in {:.0} s, objective {:.4} -> {:.4}",
        started.elapsed().as_secs_f32(),
        outcome.before.objective,
        outcome.after.objective
    );
    let from = seam_refine::get_params(base);
    let to = seam_refine::get_params(&outcome.calibration);
    for (name, (a, b)) in REFINE_PARAMS.iter().zip(from.iter().zip(&to)) {
        println!("   {name:14} {a:+.5} -> {b:+.5}");
    }
    report("refined", &outcome.calibration);
    let out = format!("{output_dir}/refined_setup.json");
    std::fs::write(
        &out,
        serde_json::to_string_pretty(&outcome.calibration).expect("serialize setup"),
    )
    .expect("cannot write refined setup");
    println!("\nWrote {out}");
}
