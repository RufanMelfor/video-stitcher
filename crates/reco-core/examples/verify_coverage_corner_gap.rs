//! Diagnostic: does `CoverageBoundary::safe_clamp` ever approve a
//! viewport whose *corner* pokes past real per-lens coverage?
//!
//! Context: `safe_clamp` (see `projection::coverage`) picks a pitch
//! against the *global* min/max pitch bounds, then looks up the yaw
//! range at that one (center) pitch and uses it for the viewport's
//! full height. The coverage region is not a rectangle - its own
//! doc comment says it tapers to its narrowest "typically at the
//! seam between planes" - so a wide-FOV viewport whose center passes
//! the check can still have a top or bottom corner land outside the
//! true (narrower-there) coverage.
//!
//! This script does NOT trust that reasoning - it independently
//! re-derives where each of the four rendered corners actually
//! points in world (yaw, pitch), the same way the renderer would
//! (via the public `view_matrix` + perspective frustum math), and
//! checks each corner against `CoverageBoundary::yaw_range_at` at
//! *its own* pitch - the same precomputed, real-lens-sampled ground
//! truth `safe_clamp` itself is built from. No rendering, no GPU.
//!
//! Usage: `cargo run -p reco-core --example verify_coverage_corner_gap -- <calibration.json> [output_aspect]`
//! `output_aspect` defaults to 2560/1440 (16:9).

use std::env;
use std::path::PathBuf;

use nalgebra::Vector3;
use reco_core::calibration::Calibration;
use reco_core::geometry::{VirtualCamera, view_matrix, world_to_render_pose};
use reco_core::projection::CoverageBoundary;
use reco_core::render::scene::SceneGeometry;

/// FOV values (vertical, degrees) actually used by a real export's
/// autocam config (see the `.log` sidecar's `fov_tight`/`fov_default`/
/// `fov_wide`) - the range worth sweeping, not an arbitrary guess.
const FOV_SWEEP_DEG: [f32; 3] = [35.0, 45.0, 55.0];

/// How finely to sweep yaw/pitch candidates across the coverage's own
/// extent. 200x60 x 3 FOVs = 36000 candidates, each four corners -
/// still well under a second.
const YAW_STEPS: usize = 200;
const PITCH_STEPS: usize = 60;

/// Degrees of slack before a corner miss counts as a real leak, not
/// float/sampling noise from the 400-slice coverage table's own
/// linear interpolation.
const LEAK_EPS_DEG: f32 = 0.05;

struct Leak {
    yaw: f32,
    pitch: f32,
    fov: f32,
    render_yaw_deg: f32,
    render_pitch_deg: f32,
    corner: &'static str,
    corner_yaw_deg: f32,
    corner_pitch_deg: f32,
    range_lo_deg: f32,
    range_hi_deg: f32,
    overshoot_deg: f32,
}

fn main() {
    let args: Vec<String> = env::args().collect();
    let cal_path = args
        .get(1)
        .map(PathBuf::from)
        .expect("usage: verify_coverage_corner_gap <calibration.json> [output_aspect]");
    let output_aspect: f32 = args
        .get(2)
        .map(|s| s.parse().expect("output_aspect must be a float"))
        .unwrap_or(2560.0 / 1440.0);

    // Self-test: trivial zero-tilt/roll, yaw=0/pitch=0, 90deg square FOV
    // case where the four corners have an obvious hand-checkable answer
    // (+-45deg yaw, +-45deg pitch) - isolates whether the corner-ray
    // construction itself is right before trusting it on real data.
    {
        let position = [1.0_f32, 0.0, 1.0];
        let cam = VirtualCamera::new(&position);
        let view = view_matrix(&position, 0.0, 0.0, 0.0, 0.0);
        let r_t = view.fixed_view::<3, 3>(0, 0).into_owned().transpose();
        println!(
            "SELFTEST basis: r_t*(1,0,0)={:?} base_right={:?}",
            (r_t * Vector3::new(1.0_f32, 0.0, 0.0)).data.0,
            cam.base_right.data.0
        );
        println!(
            "SELFTEST basis: r_t*(0,1,0)={:?} world_up={:?}",
            (r_t * Vector3::new(0.0_f32, 1.0, 0.0)).data.0,
            VirtualCamera::world_up().data.0
        );
        println!(
            "SELFTEST basis: r_t*(0,0,-1)={:?} base_forward={:?}",
            (r_t * Vector3::new(0.0_f32, 0.0, -1.0)).data.0,
            cam.base_forward.data.0
        );
        let half = (45.0_f32).to_radians().tan();
        for &(sx, sy, label) in &[
            (-1.0_f32, -1.0_f32, "bottom-left"),
            (1.0, -1.0, "bottom-right"),
            (-1.0, 1.0, "top-left"),
            (1.0, 1.0, "top-right"),
        ] {
            let view_dir = Vector3::new(sx * half, sy * half, -1.0).normalize();
            let world_dir = (r_t * view_dir).normalize();
            let pos = cam.direction_to_yaw_pitch(&world_dir);
            println!(
                "SELFTEST {label}: yaw={:.2} pitch={:.2} (expect ~{:.0},~{:.0})",
                pos.yaw.to_degrees(),
                pos.pitch.to_degrees(),
                sx * 45.0,
                sy * 45.0
            );
        }
    }

    let cal = Calibration::from_file(&cal_path).expect("failed to load calibration");
    let lens_aspect = cal.lenses[0].width as f32 / cal.lenses[0].height as f32;
    let scene = SceneGeometry::new(&cal.topology, &cal.framing, lens_aspect);
    let coverage = CoverageBoundary::from_calibration(&cal, &scene);
    let cam = VirtualCamera::new(&scene.camera_position);
    let rig_tilt = cal.framing.tilt as f32;
    let rig_roll = cal.framing.roll as f32;

    println!(
        "Coverage: pitch [{:.2}, {:.2}] deg, global yaw {:?} deg, max_fov {:.1} deg",
        coverage.pitch_min.to_degrees(),
        coverage.pitch_max.to_degrees(),
        {
            let (lo, hi) = coverage.yaw_range();
            (lo.to_degrees(), hi.to_degrees())
        },
        coverage.max_fov_degrees(),
    );
    println!(
        "Rig: tilt={:.2} deg, roll={:.2} deg, output_aspect={:.4}",
        rig_tilt.to_degrees(),
        rig_roll.to_degrees(),
        output_aspect,
    );
    println!(
        "Sweeping {} FOVs x {YAW_STEPS}x{PITCH_STEPS} candidates...\n",
        FOV_SWEEP_DEG.len()
    );

    let (yaw_lo, yaw_hi) = coverage.yaw_range();
    let mut leaks: Vec<Leak> = Vec::new();
    let mut checked = 0u64;

    for &fov in &FOV_SWEEP_DEG {
        let half_vfov = (fov * 0.5_f32).to_radians();
        let half_hfov = (output_aspect * half_vfov.tan()).atan();

        for iy in 0..YAW_STEPS {
            let yaw = yaw_lo + (yaw_hi - yaw_lo) * (iy as f32 / (YAW_STEPS - 1) as f32);
            for ip in 0..PITCH_STEPS {
                let pitch = coverage.pitch_min
                    + (coverage.pitch_max - coverage.pitch_min)
                        * (ip as f32 / (PITCH_STEPS - 1) as f32);

                let clamped = coverage.safe_clamp(yaw, pitch, fov, output_aspect);

                // `view_matrix` consumes RENDER-space yaw/pitch (composed
                // within the tilted+rolled rig frame), not the WORLD-space
                // yaw/pitch `safe_clamp` returns - skipping this step was
                // this script's first-draft bug (produced >100deg garbage
                // "overshoots": the reconstructed view matrix pointed
                // somewhere else entirely under this rig's real 26.5deg
                // tilt). This is the same conversion `resolve_render_pose`
                // composes internally.
                let (render_yaw, render_pitch) =
                    world_to_render_pose(&cam, clamped.yaw, clamped.pitch, rig_tilt, rig_roll);

                // Skip poses whose RENDER pitch is near the pole - under a
                // large rig tilt like this one (26.5deg), a plausible-
                // looking WORLD (yaw, pitch) can map to a render pose close
                // to gimbal lock, where any FOV's corners fan out across a
                // huge yaw range by simple geometry (a camera pointed near
                // straight up sees "left" and "right" meet). That is a
                // real property of near-polar aim, not a coverage-clamp
                // defect, and not a pose an autocam tracking play on the
                // field would ever actually select - filter it out so the
                // remaining leaks (if any) reflect realistic operating
                // conditions.
                if render_pitch.abs().to_degrees() > 75.0 {
                    continue;
                }
                checked += 1;

                // Reconstruct the actual rendered frustum's four corners
                // in world (yaw, pitch), the same math the GPU's view
                // matrix implies: view_matrix maps world -> view (camera
                // looks down -Z in view space for look_at_rh), so a
                // view-space corner ray transforms to world via the
                // rotation's transpose (orthonormal => inverse).
                let view = view_matrix(
                    &scene.camera_position,
                    render_yaw,
                    render_pitch,
                    rig_tilt,
                    rig_roll,
                );
                let r = view.fixed_view::<3, 3>(0, 0).into_owned();
                let r_t = r.transpose();

                // Sanity check: the center ray (sx=sy=0, straight down
                // -Z in view space) must round-trip back to exactly
                // (clamped.yaw, clamped.pitch) - that's what
                // world_to_render_pose + view_matrix jointly promise.
                // If this ever fires, the bug is in the view-matrix
                // reconstruction, not the frustum corner math.
                {
                    let center_view_dir = Vector3::new(0.0_f32, 0.0, -1.0);
                    let center_world_dir = (r_t * center_view_dir).normalize();
                    let center_pos = cam.direction_to_yaw_pitch(&center_world_dir);
                    let dyaw = (center_pos.yaw - clamped.yaw).to_degrees();
                    let dpitch = (center_pos.pitch - clamped.pitch).to_degrees();
                    if dyaw.abs() > 0.5 || dpitch.abs() > 0.5 {
                        eprintln!(
                            "CENTER ROUND-TRIP MISS: clamped=({:.2},{:.2}) render=({:.2},{:.2}) \
                             recovered=({:.2},{:.2}) d=({:.2},{:.2})",
                            clamped.yaw.to_degrees(),
                            clamped.pitch.to_degrees(),
                            render_yaw.to_degrees(),
                            render_pitch.to_degrees(),
                            center_pos.yaw.to_degrees(),
                            center_pos.pitch.to_degrees(),
                            dyaw,
                            dpitch
                        );
                    }
                }

                for &(sx, sy, label) in &[
                    (-1.0_f32, -1.0_f32, "bottom-left"),
                    (1.0, -1.0, "bottom-right"),
                    (-1.0, 1.0, "top-left"),
                    (1.0, 1.0, "top-right"),
                ] {
                    let view_dir =
                        Vector3::new(sx * half_hfov.tan(), sy * half_vfov.tan(), -1.0).normalize();
                    let world_dir = (r_t * view_dir).normalize();
                    let corner_pos = cam.direction_to_yaw_pitch(&world_dir);

                    // Ground truth at this corner's own pitch - the same
                    // precomputed, real-lens-sampled table safe_clamp
                    // itself consults, but evaluated locally instead of
                    // at the center pitch.
                    let (range_lo, range_hi) = coverage.yaw_range_at(corner_pos.pitch);

                    // Degenerate slice (no coverage at all at this
                    // pitch) - not a corner-approximation leak, a
                    // different failure mode; skip.
                    if range_lo > range_hi {
                        continue;
                    }

                    let over_lo = range_lo - corner_pos.yaw;
                    let over_hi = corner_pos.yaw - range_hi;
                    let overshoot = over_lo.max(over_hi).max(0.0).to_degrees();

                    if overshoot > LEAK_EPS_DEG {
                        leaks.push(Leak {
                            yaw,
                            pitch,
                            fov,
                            render_yaw_deg: render_yaw.to_degrees(),
                            render_pitch_deg: render_pitch.to_degrees(),
                            corner: label,
                            corner_yaw_deg: corner_pos.yaw.to_degrees(),
                            corner_pitch_deg: corner_pos.pitch.to_degrees(),
                            range_lo_deg: range_lo.to_degrees(),
                            range_hi_deg: range_hi.to_degrees(),
                            overshoot_deg: overshoot,
                        });
                    }
                }
            }
        }
    }

    println!(
        "Checked {checked} candidate poses ({} corner tests). Leaks found: {}\n",
        checked * 4,
        leaks.len()
    );

    if leaks.is_empty() {
        println!(
            "No corner leaks found - the single-center-pitch-slice approximation \
             held for every candidate in this sweep. Hypothesis NOT confirmed by \
             this test (may still leak outside the swept FOV/yaw/pitch range, or \
             the black wedge has a different cause)."
        );
        return;
    }

    leaks.sort_by(|a, b| b.overshoot_deg.partial_cmp(&a.overshoot_deg).unwrap());

    let buckets = [
        (0.05_f32, 1.0_f32, "0.05-1"),
        (1.0, 5.0, "1-5"),
        (5.0, 20.0, "5-20"),
        (20.0, f32::INFINITY, "20+"),
    ];
    println!("Overshoot distribution (degrees):");
    for (lo, hi, label) in buckets {
        let n = leaks
            .iter()
            .filter(|l| l.overshoot_deg >= lo && l.overshoot_deg < hi)
            .count();
        println!("  {label:>8} deg: {n}");
    }
    println!();

    println!("Leak concentration:");
    println!(
        "  fov: {:?}",
        FOV_SWEEP_DEG
            .iter()
            .map(|&f| (f, leaks.iter().filter(|l| l.fov == f).count()))
            .collect::<Vec<_>>()
    );
    println!(
        "  corner: bl={} br={} tl={} tr={}",
        leaks.iter().filter(|l| l.corner == "bottom-left").count(),
        leaks.iter().filter(|l| l.corner == "bottom-right").count(),
        leaks.iter().filter(|l| l.corner == "top-left").count(),
        leaks.iter().filter(|l| l.corner == "top-right").count(),
    );
    let yaw_edge = yaw_hi.to_degrees() - 10.0; // within 10deg of the global yaw extreme
    let yaw_edge_lo = yaw_lo.to_degrees() + 10.0;
    let near_edge = leaks
        .iter()
        .filter(|l| l.yaw.to_degrees() >= yaw_edge || l.yaw.to_degrees() <= yaw_edge_lo)
        .count();
    println!(
        "  within 10deg of global yaw extreme [{:.1},{:.1}]: {near_edge} / {}",
        yaw_lo.to_degrees(),
        yaw_hi.to_degrees(),
        leaks.len()
    );
    let unique_poses: std::collections::HashSet<(i32, i32, i32)> = leaks
        .iter()
        .map(|l| {
            (
                (l.render_yaw_deg * 10.0).round() as i32,
                (l.render_pitch_deg * 10.0).round() as i32,
                l.fov as i32,
            )
        })
        .collect();
    println!(
        "  unique underlying (render_yaw,render_pitch,fov) clamp outputs leaking: {}",
        unique_poses.len()
    );
    println!();

    println!("One representative leak per unique underlying clamp output:");
    let mut seen: std::collections::HashSet<(i32, i32, i32)> = std::collections::HashSet::new();
    for l in &leaks {
        let key = (
            (l.render_yaw_deg * 10.0).round() as i32,
            (l.render_pitch_deg * 10.0).round() as i32,
            l.fov as i32,
        );
        if seen.insert(key) {
            println!(
                "  world=({:.2},{:.2}) fov={:.0} -> render=({:.2},{:.2}) corner={} c=({:.2},{:.2}) valid=[{:.2},{:.2}] overshoot={:.2}deg",
                l.yaw.to_degrees(),
                l.pitch.to_degrees(),
                l.fov,
                l.render_yaw_deg,
                l.render_pitch_deg,
                l.corner,
                l.corner_yaw_deg,
                l.corner_pitch_deg,
                l.range_lo_deg,
                l.range_hi_deg,
                l.overshoot_deg
            );
        }
    }
    println!();

    println!("Worst 20 leaks (by overshoot):");
    println!(
        "{:>8} {:>8} {:>6} {:>10} {:>10} {:>12} {:>10} {:>10} {:>18} {:>10}",
        "yaw",
        "pitch",
        "fov",
        "r_yaw",
        "r_pitch",
        "corner",
        "c_yaw",
        "c_pitch",
        "valid_range",
        "overshoot"
    );
    for l in leaks.iter().take(20) {
        println!(
            "{:>8.2} {:>8.2} {:>6.1} {:>10.2} {:>10.2} {:>12} {:>10.2} {:>10.2} [{:>6.2},{:>6.2}] {:>9.3}deg",
            l.yaw.to_degrees(),
            l.pitch.to_degrees(),
            l.fov,
            l.render_yaw_deg,
            l.render_pitch_deg,
            l.corner,
            l.corner_yaw_deg,
            l.corner_pitch_deg,
            l.range_lo_deg,
            l.range_hi_deg,
            l.overshoot_deg,
        );
    }

    let worst = &leaks[0];
    println!(
        "\nHypothesis CONFIRMED: safe_clamp approves poses whose corner exits real \
         coverage by up to {:.2} deg (worst case: yaw={:.1} pitch={:.1} fov={:.0} \
         corner={}).",
        worst.overshoot_deg,
        worst.yaw.to_degrees(),
        worst.pitch.to_degrees(),
        worst.fov,
        worst.corner
    );
}
