//! Throwaway diagnostic for FRICTION.md point 26.
//!
//! Real DJI footage gives wildly implausible `differential_orientation`
//! values (roll -128 deg, pitch -140 deg, tilt_diff 64 deg) even though
//! this exact camera model's *real*, feature-matched/RANSAC'd mounting
//! misalignment (z_rx, `optimizer.rs`'s own bounds comment) is only
//! ~-8 deg.
//!
//! Round 1 (see FRICTION.md point 26) tested 180-degree negations of
//! the already-derived gravity *vector* - none matched, ruling out the
//! simplest version of the "left camera is mounted rotated 180 deg"
//! hypothesis.
//!
//! Round 2 (this version) goes one level deeper, to the *quaternion*
//! itself, for two independent reasons found by re-reading the code:
//!
//! 1. `gravity_vector_from_quaternions`'s own comment says it computes
//!    `g_cam = q^-1 * v_world * q`, but the formula actually coded
//!    (verified by hand against the standard quaternion-rotation
//!    matrix, `x'=(1-2(y2+z2))x+2(xy-wz)y+2(xz+wy)z` etc.) computes
//!    `q * v_world * q^-1` instead - the *other* sandwich order. Since
//!    `TelemetryData`'s own doc says the quaternion represents
//!    "rotation from camera frame to gravity-aligned world frame" (a
//!    camera-to-world quaternion), converting a *world*-frame vector
//!    into camera frame needs the `q^-1 * v * q` order the comment
//!    describes, not the one actually implemented. If real, this is a
//!    global formula bug affecting both cameras identically, not a
//!    per-camera mount-rotation gap.
//! 2. Separately, telemetry-parser's DJI backend already composes its
//!    own 180-degree "horizon lock" rotation into the quaternion before
//!    we ever see it - a mount-rotation correction should plausibly be
//!    composed with the quaternion too, not applied to its
//!    already-derived output vector (round 1's approach).
//!
//! This does NOT change any real code path. It duplicates the
//! quaternion averaging (`gravity_vector_from_quaternions`) and the
//! gravity-rotation formula (verified byte-for-byte equivalent to the
//! real one when fed the *same* quaternion) so alternate sandwich
//! orders and candidate mount-correction quaternions can be tried
//! freely, and prints `differential_orientation`'s roll/pitch/tilt_diff
//! for each against the known ~-8 deg baseline.
//!
//! Usage:
//!   cargo run -p reco-calibrate --example check_imu_mount_rotation -- \
//!     <left.mp4> <right.mp4> [skip_secs]

use reco_calibrate::telemetry::{self, TelemetryData};

type Quat = (f64, f64, f64, f64); // (w, x, y, z)

/// Hamilton product, standard convention.
fn quat_mul(a: Quat, b: Quat) -> Quat {
    let (aw, ax, ay, az) = a;
    let (bw, bx, by, bz) = b;
    (
        aw * bw - ax * bx - ay * by - az * bz,
        aw * bx + ax * bw + ay * bz - az * by,
        aw * by - ax * bz + ay * bw + az * bx,
        aw * bz + ax * by - ay * bx + az * bw,
    )
}

fn quat_conjugate((w, x, y, z): Quat) -> Quat {
    (w, -x, -y, -z)
}

/// Duplicates `gravity_vector_from_quaternions`'s window-selection and
/// hemisphere-aligned averaging (telemetry.rs) exactly, returning the
/// normalized average quaternion instead of an already-rotated gravity
/// vector - so different rotation formulas/corrections can be applied
/// to the *same* averaged quaternion afterward.
fn average_quaternion(data: &TelemetryData, skip_secs: f64) -> Option<Quat> {
    if data.quaternions.len() < 10 {
        return None;
    }
    let start_idx = if skip_secs > 0.0 {
        data.quaternions
            .iter()
            .position(|&(t, _)| t >= skip_secs)
            .unwrap_or(0)
    } else {
        0
    };
    let end_idx = (start_idx + 100).min(data.quaternions.len());
    let window = &data.quaternions[start_idx..end_idx];
    if window.len() < 10 {
        return None;
    }

    let [w0, x0, y0, z0] = window[0].1;
    let mut aw = 0.0;
    let mut ax = 0.0;
    let mut ay = 0.0;
    let mut az = 0.0;
    for &(_, [w, x, y, z]) in window {
        let dot = w * w0 + x * x0 + y * y0 + z * z0;
        let sign = if dot < 0.0 { -1.0 } else { 1.0 };
        aw += w * sign;
        ax += x * sign;
        ay += y * sign;
        az += z * sign;
    }
    let inv_n = 1.0 / window.len() as f64;
    aw *= inv_n;
    ax *= inv_n;
    ay *= inv_n;
    az *= inv_n;

    let len = (aw * aw + ax * ax + ay * ay + az * az).sqrt();
    if len < 1e-10 {
        return None;
    }
    Some((aw / len, ax / len, ay / len, az / len))
}

/// Rotates world gravity `[0, -1, 0]` into camera frame via the sandwich
/// `q_in * v * q_in^-1`. Verified by hand to reproduce the real
/// `gravity_vector_from_quaternions`'s exact `gx`/`gy`/`gz` formula when
/// `q_in` is the raw averaged quaternion - so passing `quat_conjugate(q)`
/// here instead computes the *other* sandwich order
/// (`q^-1 * v * q`, what `gravity_vector_from_quaternions`'s own comment
/// says it does) without duplicating a second, independently-derived
/// formula that could itself have a new mistake in it.
fn gravity_from_quat((w, x, y, z): Quat) -> [f64; 3] {
    let gx = -2.0 * (x * y - w * z);
    let gy = -(1.0 - 2.0 * (x * x + z * z));
    let gz = -2.0 * (y * z + w * x);
    [gx, gy, gz]
}

/// Same three formulas `differential_orientation` uses (telemetry.rs).
fn roll_pitch_tilt(lg: [f64; 3], rg: [f64; 3]) -> (f64, f64, f64) {
    let left_roll = lg[2].atan2(lg[0]);
    let right_roll = rg[2].atan2(rg[0]);
    let roll_diff = right_roll - left_roll;

    let left_pitch = lg[1].atan2(lg[0]);
    let right_pitch = rg[1].atan2(rg[0]);
    let pitch_diff = right_pitch - left_pitch;

    let left_tilt = lg[2].atan2(lg[0]);
    let right_tilt = rg[2].atan2(rg[0]);
    let rig_tilt_avg = (left_tilt + right_tilt) / 2.0;
    let tilt_diff = left_tilt - rig_tilt_avg;

    (roll_diff, pitch_diff, tilt_diff)
}

fn print_row(name: &str, lg: [f64; 3], rg: [f64; 3]) {
    let (roll, pitch, tilt) = roll_pitch_tilt(lg, rg);
    println!(
        "{:52} {:>9.2}\u{b0} {:>9.2}\u{b0} {:>11.2}\u{b0}",
        name,
        roll.to_degrees(),
        pitch.to_degrees(),
        tilt.to_degrees()
    );
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 3 {
        eprintln!(
            "Usage: {} <left.mp4> <right.mp4> [skip_secs]",
            args.first()
                .map(String::as_str)
                .unwrap_or("check_imu_mount_rotation")
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

    let lq = average_quaternion(&left, skip_secs).expect("no left quaternion average");
    let rq = average_quaternion(&right, skip_secs).expect("no right quaternion average");
    println!(
        "left avg quat:  w={:.4} x={:.4} y={:.4} z={:.4}",
        lq.0, lq.1, lq.2, lq.3
    );
    println!(
        "right avg quat: w={:.4} x={:.4} y={:.4} z={:.4}\n",
        rq.0, rq.1, rq.2, rq.3
    );

    // Sanity check: this must reproduce today's real logged numbers
    // exactly (roll=-128.26, pitch=-140.53, tilt_diff=64.13) before any
    // of the rest of this table means anything.
    println!(
        "{:52} {:>10} {:>10} {:>12}",
        "candidate", "roll", "pitch", "tilt_diff"
    );
    print_row(
        "BASELINE (real code today)",
        gravity_from_quat(lq),
        gravity_from_quat(rq),
    );
    println!(
        "Known-true z_rx (what tilt_diff seeds) for this rig: ~-8 deg \
         (optimizer.rs's own bounds comment)\n"
    );

    // --- Round 2a: sandwich-order fix (a global formula/comment
    // mismatch, not a per-camera thing - see module doc point 1) ---
    print_row(
        "conjugate sandwich, LEFT only",
        gravity_from_quat(quat_conjugate(lq)),
        gravity_from_quat(rq),
    );
    print_row(
        "conjugate sandwich, RIGHT only",
        gravity_from_quat(lq),
        gravity_from_quat(quat_conjugate(rq)),
    );
    print_row(
        "conjugate sandwich, BOTH",
        gravity_from_quat(quat_conjugate(lq)),
        gravity_from_quat(quat_conjugate(rq)),
    );
    println!();

    // --- Round 2b: compose a candidate 180-degree mount-rotation
    // quaternion with the left camera's quaternion, both multiplication
    // orders, both sandwich orders (6 x 2 = 12 combinations) ---
    let q180: [(&str, Quat); 3] = [
        ("180 about X", (0.0, 1.0, 0.0, 0.0)),
        ("180 about Y", (0.0, 0.0, 1.0, 0.0)),
        ("180 about Z", (0.0, 0.0, 0.0, 1.0)),
    ];
    for (axis_name, qc) in q180 {
        for (mul_name, lq_corrected) in [("lq*qc", quat_mul(lq, qc)), ("qc*lq", quat_mul(qc, lq))] {
            print_row(
                &format!("{axis_name}, {mul_name}, normal sandwich"),
                gravity_from_quat(lq_corrected),
                gravity_from_quat(rq),
            );
            print_row(
                &format!("{axis_name}, {mul_name}, conjugate sandwich"),
                gravity_from_quat(quat_conjugate(lq_corrected)),
                gravity_from_quat(quat_conjugate(rq)),
            );
        }
    }
}
