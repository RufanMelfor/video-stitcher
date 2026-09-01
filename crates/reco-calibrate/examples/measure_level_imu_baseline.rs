//! Ground-truth measurement for FRICTION.md point 26.
//!
//! Calls the REAL, unmodified `telemetry::rig_tilt` and
//! `telemetry::differential_orientation` - not a duplicated/candidate
//! formula like `check_imu_mount_rotation.rs` - against footage where
//! both cameras are known, physically, to be level and stationary
//! (spirit level, not "looks about right"). Whatever these report under
//! that condition *is* each camera's raw IMU zero-point error, measured
//! directly instead of derived through hypothesized corrections.
//!
//! Usage:
//!   cargo run -p reco-calibrate --example measure_level_imu_baseline -- \
//!     <left.mp4> <right.mp4> [skip_secs]
//!
//! Ground truth for a genuinely level, stationary rig: every number
//! below should read close to 0 degrees. Whatever it actually reads is
//! the real, measured error - no correction hypothesis needed to
//! interpret it.

use reco_calibrate::telemetry;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 3 {
        eprintln!(
            "Usage: {} <left.mp4> <right.mp4> [skip_secs]",
            args.first()
                .map(String::as_str)
                .unwrap_or("measure_level_imu_baseline")
        );
        std::process::exit(1);
    }
    let left_path = std::path::Path::new(&args[1]);
    let right_path = std::path::Path::new(&args[2]);
    let skip_secs: f64 = args.get(3).and_then(|s| s.parse().ok()).unwrap_or(0.0);

    eprintln!("Extracting left telemetry from {}...", left_path.display());
    let left = telemetry::extract(left_path).expect("left telemetry extract failed");
    eprintln!(
        "Extracting right telemetry from {}...",
        right_path.display()
    );
    let right = telemetry::extract(right_path).expect("right telemetry extract failed");

    println!(
        "left camera:  {} {}",
        left.camera_type,
        left.camera_model.as_deref().unwrap_or("(unknown model)")
    );
    println!(
        "right camera: {} {}\n",
        right.camera_type,
        right.camera_model.as_deref().unwrap_or("(unknown model)")
    );

    let left_tilt = telemetry::rig_tilt(&left, skip_secs);
    let right_tilt = telemetry::rig_tilt(&right, skip_secs);
    println!(
        "rig_tilt(left)  = {}",
        left_tilt
            .map(|t| format!("{:.2} deg", t.to_degrees()))
            .unwrap_or_else(|| "None (no gravity source)".into())
    );
    println!(
        "rig_tilt(right) = {}\n",
        right_tilt
            .map(|t| format!("{:.2} deg", t.to_degrees()))
            .unwrap_or_else(|| "None (no gravity source)".into())
    );

    match telemetry::differential_orientation(&left, &right, skip_secs) {
        Some((roll, pitch, tilt)) => {
            println!(
                "differential_orientation: roll={:.2} deg, pitch={:.2} deg, tilt_diff={:.2} deg",
                roll.to_degrees(),
                pitch.to_degrees(),
                tilt.to_degrees()
            );
        }
        None => println!("differential_orientation: None (missing gravity source)"),
    }

    println!(
        "\nGround truth for a genuinely level, stationary rig: every number \
         above should read close to 0 deg. Whatever it actually reads IS \
         the real per-camera IMU zero-point error - measured, not derived."
    );
}
