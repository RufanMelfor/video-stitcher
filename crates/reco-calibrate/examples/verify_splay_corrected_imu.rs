//! FRICTION.md point 26, round 6: cross-validate round 4's per-camera
//! correction against real (non-level) match footage, this time using
//! round 5's splay-corrected expected values instead of a flat target.
//!
//! Applies round 4's winning per-camera fix - `conjugate` for the
//! physically-rotated camera (left, on this rig), a 180-degree
//! quaternion composition for the normally-mounted one (right) - then
//! reads off BOTH pitch (`atan2(gy,gx)`, the same formula `rig_tilt`
//! uses) and lateral roll (`atan2(gz,gx)`, what `differential_orientation`
//! calls `roll`/`tilt` - same formula, note the naming overlap) for each
//! camera, and compares against what the rig's ~20-degree forward pitch
//! should produce once decomposed through each camera's own 42-degree
//! yaw offset (round 5): pitch ~= 20*cos(42) ~= 14.9 deg (same sign,
//! both cameras), roll ~= 20*sin(42) ~= 13.4 deg (OPPOSITE sign, left
//! vs. right).
//!
//! Usage:
//!   cargo run -p reco-calibrate --example verify_splay_corrected_imu -- \
//!     <left.mp4> <right.mp4> [skip_secs]

use reco_calibrate::telemetry::{self, TelemetryData};

type Quat = (f64, f64, f64, f64); // (w, x, y, z)

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
    let (mut aw, mut ax, mut ay, mut az) = (0.0, 0.0, 0.0, 0.0);
    for &(_, [w, x, y, z]) in window {
        let dot = w * w0 + x * x0 + y * y0 + z * z0;
        let sign = if dot < 0.0 { -1.0 } else { 1.0 };
        aw += w * sign;
        ax += x * sign;
        ay += y * sign;
        az += z * sign;
    }
    let inv_n = 1.0 / window.len() as f64;
    let (aw, ax, ay, az) = (aw * inv_n, ax * inv_n, ay * inv_n, az * inv_n);
    let len = (aw * aw + ax * ax + ay * ay + az * az).sqrt();
    if len < 1e-10 {
        return None;
    }
    Some((aw / len, ax / len, ay / len, az / len))
}

fn gravity_from_quat((w, x, y, z): Quat) -> [f64; 3] {
    let gx = -2.0 * (x * y - w * z);
    let gy = -(1.0 - 2.0 * (x * x + z * z));
    let gz = -2.0 * (y * z + w * x);
    [gx, gy, gz]
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 3 {
        eprintln!(
            "Usage: {} <left.mp4> <right.mp4> [skip_secs]",
            args.first()
                .map(String::as_str)
                .unwrap_or("verify_splay_corrected_imu")
        );
        std::process::exit(1);
    }
    let left_path = std::path::Path::new(&args[1]);
    let right_path = std::path::Path::new(&args[2]);
    let skip_secs: f64 = args.get(3).and_then(|s| s.parse().ok()).unwrap_or(0.0);

    eprintln!("Extracting left telemetry...");
    let left = telemetry::extract(left_path).expect("left telemetry extract failed");
    eprintln!("Extracting right telemetry...");
    let right = telemetry::extract(right_path).expect("right telemetry extract failed");

    let lq = average_quaternion(&left, skip_secs).expect("no left quaternion average");
    let rq = average_quaternion(&right, skip_secs).expect("no right quaternion average");

    // Round 4's winning per-camera fix.
    let q180y: Quat = (0.0, 0.0, 1.0, 0.0);
    let lq_fixed = quat_conjugate(lq); // left: physically rotated 180 -> conjugate alone
    let rq_fixed = quat_mul(q180y, rq); // right: normal mount -> needs the extra 180

    let lg = gravity_from_quat(lq_fixed);
    let rg = gravity_from_quat(rq_fixed);

    let left_pitch = lg[1].atan2(lg[0]).to_degrees();
    let right_pitch = rg[1].atan2(rg[0]).to_degrees();
    let left_roll = lg[2].atan2(lg[0]).to_degrees();
    let right_roll = rg[2].atan2(rg[0]).to_degrees();

    println!("Corrected (round 4 fix) readings on real match footage:");
    println!("  left  pitch = {left_pitch:7.2} deg   left  roll = {left_roll:7.2} deg");
    println!("  right pitch = {right_pitch:7.2} deg   right roll = {right_roll:7.2} deg");

    let rig_tilt_deg = 20.0_f64;
    let half_splay_deg = 42.0_f64;
    let expected_pitch = rig_tilt_deg * half_splay_deg.to_radians().cos();
    let expected_roll = rig_tilt_deg * half_splay_deg.to_radians().sin();
    println!(
        "\nExpected from a {rig_tilt_deg:.0} deg rig-forward pitch through a {half_splay_deg:.0} \
         deg per-camera yaw offset (round 5):"
    );
    println!(
        "  pitch (both cameras, same sign)  ~= {:.1} deg",
        expected_pitch
    );
    println!(
        "  roll (opposite sign, left vs right) ~= {:.1} deg",
        expected_roll
    );
}
