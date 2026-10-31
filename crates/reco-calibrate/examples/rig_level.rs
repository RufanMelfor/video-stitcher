//! Measure the rig level (tilt / roll) from vertical structures in the
//! footage and compare with each setup's framing (see
//! `reco_calibrate::rig_level`).
//!
//! Usage:
//!   cargo run --release -p reco-calibrate --example rig_level -- \
//!     <left.mp4> <right.mp4> --frames N1,N2,... <setup.json>...
//!
//! Frame numbers are left-camera frames; the right camera uses each
//! setup's `sync_offset`.

use std::path::Path;

use reco_calibrate::rig_level::estimate_framing;
use reco_core::calibration::Calibration;
use reco_core::coverage::physical_rig_level;

fn main() {
    reco_io::init();
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 5 || args[3] != "--frames" {
        eprintln!(
            "Usage: {} <left.mp4> <right.mp4> --frames N1,N2,... <setup.json>...",
            args[0]
        );
        std::process::exit(1);
    }
    let frames: Vec<u64> = args[4]
        .split(',')
        .map(|s| s.trim().parse().expect("--frames: comma-separated u64"))
        .collect();
    for p in &args[5..] {
        let cal = Calibration::from_file(Path::new(p)).unwrap_or_else(|e| panic!("{p}: {e}"));
        let right: Vec<u64> = frames
            .iter()
            .map(|f| (*f as i64 + cal.sync_offset).max(0) as u64)
            .collect();
        let l = reco_io::ffmpeg::calibration_io::extract_frames(Path::new(&args[1]), &frames)
            .expect("left frames");
        let r = reco_io::ffmpeg::calibration_io::extract_frames(Path::new(&args[2]), &right)
            .expect("right frames");
        let pairs: Vec<_> = l.into_iter().zip(r).collect();
        println!("== {p}");
        println!(
            "   setup framing: tilt {:.2} roll {:.2} deg -> physical {:?}",
            cal.framing.tilt.to_degrees(),
            cal.framing.roll.to_degrees(),
            physical_rig_level(&cal)
        );
        let started = std::time::Instant::now();
        match estimate_framing(&cal, &pairs) {
            Some(e) => {
                for (name, c) in [("left ", e.left), ("right", e.right)] {
                    let (pitch, roll) = c.pitch_roll_deg();
                    println!(
                        "   {name} camera: pitch {pitch:.2} roll {roll:.2} deg ({} of {} edges, {:.2} deg rms)",
                        c.inliers, c.candidates, c.residual_deg
                    );
                }
                println!(
                    "   measured framing: tilt {:.2} roll {:.2} deg -> physical {:?}  [{:.1} s]",
                    e.framing_tilt.to_degrees(),
                    e.framing_roll.to_degrees(),
                    e.physical,
                    started.elapsed().as_secs_f32()
                );
            }
            None => println!("   measurement failed (too few vertical edges)"),
        }
    }
}
