//! FRICTION.md point 26, round 4: solve for a correction against the
//! real, confirmed-level measurement from round 3, instead of testing
//! hand-picked candidates against an indirect ~-8 deg estimate like
//! rounds 1-2 did.
//!
//! Ground truth this time is unambiguous and per-camera: on
//! `Waterpas Test met raster` (rig physically confirmed level - spirit
//! level, then the rig itself opened to confirm left is the
//! rotated-180 camera, right is normal), `rig_tilt` should read ~0 deg
//! for EACH camera independently. It reads -129.93 (left) and 172.21
//! (right) instead (round 3). Since round 3 already established both
//! cameras are wrong *independently* (no left/right comparison
//! involved in a single-camera `rig_tilt` call), this searches for a
//! correction against each camera's own raw quaternion separately, and
//! prints whether the *same* correction fixes both (a universal
//! formula bug) or each needs something different (bug entangled with
//! the physical mount rotation).
//!
//! Usage:
//!   cargo run -p reco-calibrate --example solve_imu_correction -- \
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

/// Duplicates `gravity_vector_from_quaternions`'s window-selection and
/// hemisphere-aligned averaging (telemetry.rs) exactly.
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

/// Reproduces the real `gravity_vector_from_quaternions`'s exact
/// `gx`/`gy`/`gz` formula, verified byte-for-byte against it in round 2.
fn gravity_from_quat((w, x, y, z): Quat) -> [f64; 3] {
    let gx = -2.0 * (x * y - w * z);
    let gy = -(1.0 - 2.0 * (x * x + z * z));
    let gz = -2.0 * (y * z + w * x);
    [gx, gy, gz]
}

/// Reproduces the real `rig_tilt`'s formula exactly.
fn tilt_deg(g: [f64; 3]) -> f64 {
    g[1].atan2(g[0]).to_degrees()
}

struct Candidate {
    name: &'static str,
    q: fn(Quat) -> Quat,
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 3 {
        eprintln!(
            "Usage: {} <left.mp4> <right.mp4> [skip_secs]",
            args.first()
                .map(String::as_str)
                .unwrap_or("solve_imu_correction")
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

    println!(
        "BASELINE (real code today): left tilt={:.2} deg, right tilt={:.2} deg (both should be ~0)\n",
        tilt_deg(gravity_from_quat(lq)),
        tilt_deg(gravity_from_quat(rq))
    );

    let q180x: Quat = (0.0, 1.0, 0.0, 0.0);
    let q180y: Quat = (0.0, 0.0, 1.0, 0.0);
    let q180z: Quat = (0.0, 0.0, 0.0, 1.0);
    let identity: Quat = (1.0, 0.0, 0.0, 0.0);

    let candidates: Vec<Candidate> = vec![
        Candidate {
            name: "identity (baseline)",
            q: |q| q,
        },
        Candidate {
            name: "conjugate (sandwich-order swap)",
            q: quat_conjugate,
        },
        Candidate {
            name: "q * 180X",
            q: |q| quat_mul(q, (0.0, 1.0, 0.0, 0.0)),
        },
        Candidate {
            name: "180X * q",
            q: |q| quat_mul((0.0, 1.0, 0.0, 0.0), q),
        },
        Candidate {
            name: "q * 180Y",
            q: |q| quat_mul(q, (0.0, 0.0, 1.0, 0.0)),
        },
        Candidate {
            name: "180Y * q",
            q: |q| quat_mul((0.0, 0.0, 1.0, 0.0), q),
        },
        Candidate {
            name: "q * 180Z",
            q: |q| quat_mul(q, (0.0, 0.0, 0.0, 1.0)),
        },
        Candidate {
            name: "180Z * q",
            q: |q| quat_mul((0.0, 0.0, 0.0, 1.0), q),
        },
        Candidate {
            name: "conj(q) * 180X",
            q: |q| quat_mul(quat_conjugate(q), (0.0, 1.0, 0.0, 0.0)),
        },
        Candidate {
            name: "180X * conj(q)",
            q: |q| quat_mul((0.0, 1.0, 0.0, 0.0), quat_conjugate(q)),
        },
        Candidate {
            name: "conj(q) * 180Y",
            q: |q| quat_mul(quat_conjugate(q), (0.0, 0.0, 1.0, 0.0)),
        },
        Candidate {
            name: "180Y * conj(q)",
            q: |q| quat_mul((0.0, 0.0, 1.0, 0.0), quat_conjugate(q)),
        },
        Candidate {
            name: "conj(q) * 180Z",
            q: |q| quat_mul(quat_conjugate(q), (0.0, 0.0, 0.0, 1.0)),
        },
        Candidate {
            name: "180Z * conj(q)",
            q: |q| quat_mul((0.0, 0.0, 0.0, 1.0), quat_conjugate(q)),
        },
    ];
    // Silence unused-variable warnings for the named quats above (kept
    // named for readability even though only used via the closures).
    let _ = (q180x, q180y, q180z, identity);

    println!(
        "{:34} {:>12} {:>12}   {:>10}",
        "candidate (applied independently)", "left tilt", "right tilt", "|max| err"
    );
    for c in &candidates {
        let lt = tilt_deg(gravity_from_quat((c.q)(lq)));
        let rt = tilt_deg(gravity_from_quat((c.q)(rq)));
        let err = lt.abs().max(rt.abs());
        println!(
            "{:34} {:>10.2}\u{b0} {:>10.2}\u{b0}   {:>9.2}\u{b0}",
            c.name, lt, rt, err
        );
    }
}
