//! Print each lens's field of view and the horizon overlap of one or more
//! setup files (see `reco_core::coverage`).
//!
//! Usage:
//!   cargo run -p reco-core --example setup_coverage -- <setup.json>...

use std::path::Path;

use reco_core::calibration::Calibration;
use reco_core::coverage::{horizon_coverage, lens_fov};

fn main() {
    let paths: Vec<String> = std::env::args().skip(1).collect();
    if paths.is_empty() {
        eprintln!("Usage: setup_coverage <setup.json>...");
        std::process::exit(1);
    }
    for p in &paths {
        let cal = match Calibration::from_file(Path::new(p)) {
            Ok(c) => c,
            Err(e) => {
                eprintln!("{p}: {e}");
                continue;
            }
        };
        println!("== {p}");
        for (name, lens) in ["left ", "right"].iter().zip(&cal.lenses) {
            match lens_fov(lens) {
                Some(f) => println!(
                    "   {name} lens FOV: H {:.1}  V {:.1}  D {:.1} deg",
                    f.horizontal_deg, f.vertical_deg, f.diagonal_deg
                ),
                None => println!("   {name} lens FOV: not invertible"),
            }
        }
        let c = horizon_coverage(&cal);
        println!(
            "   horizon: left {:?}  right {:?}",
            c.left_deg.map(|r| r.map(|a| (a * 10.0).round() / 10.0)),
            c.right_deg.map(|r| r.map(|a| (a * 10.0).round() / 10.0))
        );
        println!(
            "   overlap {:.1} deg, panorama {:.1} deg, axis angle {:?}, seam at {:?} deg",
            c.overlap_deg,
            c.panorama_deg,
            c.axis_angle_deg.map(|a| (a * 10.0).round() / 10.0),
            c.seam_deg.map(|a| (a * 10.0).round() / 10.0)
        );
    }
}
