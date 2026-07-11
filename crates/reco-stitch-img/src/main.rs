//! Standalone GPU image stitcher for pipeline benchmarking and optimization.
//!
//! Feeds two PNG/JPEG images through the reco-core stitch pipeline and saves
//! the stitched result. Use `--frames N` to repeat the render N times and
//! measure throughput.
//!
//! ```text
//! reco-stitch-img --left left.jpg --right right.jpg \
//!                 --calibration match.json --output stitched.png
//! ```

use std::path::PathBuf;
use std::time::Instant;

use anyhow::{Context, Result};
use clap::Parser;

use reco_core::{
    calibration::MatchCalibration,
    gpu::{GpuContext, OutputFormat, rgba_readback::RgbaReadback},
    render::{
        pipeline::{BgraPlanes, StitchPipeline},
        renderer::InputFormat,
        viewport::ViewportConfig,
    },
};

#[derive(Parser)]
#[command(name = "reco-stitch-img", about = "Standalone GPU image stitcher")]
struct Args {
    /// Left camera image (PNG or JPEG, fisheye-distorted).
    #[arg(short, long)]
    left: PathBuf,

    /// Right camera image (PNG or JPEG, fisheye-distorted).
    #[arg(short, long)]
    right: PathBuf,

    /// Calibration JSON file produced by `reco calibrate`.
    #[arg(short, long)]
    calibration: PathBuf,

    /// Output image path (.png).
    #[arg(short, long)]
    output: PathBuf,

    /// Output width in pixels.
    #[arg(long, default_value_t = 3840)]
    width: u32,

    /// Output height in pixels.
    #[arg(long, default_value_t = 1080)]
    height: u32,

    /// Virtual camera yaw in degrees (left/right pan).
    #[arg(long, default_value_t = 0.0)]
    yaw: f32,

    /// Virtual camera pitch in degrees (up/down tilt).
    #[arg(long, default_value_t = 0.0)]
    pitch: f32,

    /// Vertical field of view in degrees.
    #[arg(long, default_value_t = 75.0)]
    fov: f32,

    /// Seam blend width in UV space [0.0–1.0].
    #[arg(long, default_value_t = 0.05)]
    blend: f32,

    /// Number of render passes. Use >1 to benchmark steady-state throughput.
    #[arg(long, default_value_t = 1)]
    frames: u32,
}

fn main() -> Result<()> {
    env_logger::init();
    let args = Args::parse();

    // --- Calibration ---
    let cal_json = std::fs::read_to_string(&args.calibration)
        .with_context(|| format!("reading calibration: {}", args.calibration.display()))?;
    let calibration: MatchCalibration =
        serde_json::from_str(&cal_json).context("parsing calibration JSON")?;

    let in_w = calibration.left.width;
    let in_h = calibration.left.height;
    eprintln!("Calibration input dimensions: {in_w}x{in_h}");

    // --- Input images ---
    let left_img = image::open(&args.left)
        .with_context(|| format!("opening left image: {}", args.left.display()))?;
    let right_img = image::open(&args.right)
        .with_context(|| format!("opening right image: {}", args.right.display()))?;

    let left_rgba = resize_to(&left_img, in_w, in_h);
    let right_rgba = resize_to(&right_img, in_w, in_h);

    // --- GPU init ---
    let gpu = GpuContext::new_blocking().context("GPU initialization")?;
    eprintln!("GPU: {} ({})", gpu.gpu_name(), gpu.backend_name());

    // --- Pipeline ---
    let viewport = ViewportConfig {
        width: args.width,
        height: args.height,
        fov_degrees: args.fov,
        blend_width: args.blend,
        rig_tilt: calibration.rig_tilt as f32,
        rig_roll: calibration.rig_roll as f32,
        lens_correction_amount: calibration.lens_correction_amount,
        ..ViewportConfig::default()
    };

    // InputFormat::Bgra: skips YUV→RGB conversion, samples RGBA directly.
    // Correct for pre-decoded PNG/JPEG inputs that are already in RGB space.
    let pipeline = StitchPipeline::with_gpu(
        gpu.clone(),
        calibration,
        viewport,
        in_w,
        in_h,
        OutputFormat::Rgba8Unorm,
        InputFormat::Bgra,
    )
    .context("pipeline setup")?;

    let left_planes = BgraPlanes::from_rgba(left_rgba.as_raw());
    let right_planes = BgraPlanes::from_rgba(right_rgba.as_raw());

    let mut readback =
        RgbaReadback::new(&gpu, args.width, args.height).context("readback setup")?;

    // --- Render loop ---
    let n = args.frames.max(1);
    let yaw_rad = args.yaw.to_radians();
    let pitch_rad = args.pitch.to_radians();

    let t0 = Instant::now();
    let mut last_rgba: Option<Vec<u8>> = None;

    for _ in 0..n {
        let cmd = pipeline
            .render_to_target_bgra(&left_planes, &right_planes, yaw_rad, pitch_rad)
            .context("render")?;

        let render_target = pipeline.render_target();
        if let Some(data) = readback
            .readback(&gpu, render_target, cmd)
            .context("readback")?
        {
            last_rgba = Some(data.to_vec());
        }
    }

    // Drain the triple-buffer pipeline to get the last pending frames.
    while let Some(data) = readback.flush_pending(&gpu).context("flush")? {
        last_rgba = Some(data.to_vec());
    }

    let elapsed = t0.elapsed();
    eprintln!(
        "Rendered {n} frame(s) in {:.1}ms  ({:.1} fps)",
        elapsed.as_secs_f64() * 1000.0,
        n as f64 / elapsed.as_secs_f64(),
    );

    // --- Save output ---
    let rgba = last_rgba.context("no output frame produced")?;
    let img = image::RgbaImage::from_raw(args.width, args.height, rgba)
        .context("assembling output image")?;
    img.save(&args.output)
        .with_context(|| format!("saving output: {}", args.output.display()))?;
    eprintln!("Saved → {}", args.output.display());

    Ok(())
}

/// Return an RGBA copy of `img`, resized to `(w, h)` if needed.
fn resize_to(img: &image::DynamicImage, w: u32, h: u32) -> image::RgbaImage {
    if img.width() == w && img.height() == h {
        return img.to_rgba8();
    }
    eprintln!("Resizing {}x{} → {w}x{h}", img.width(), img.height());
    img.resize_exact(w, h, image::imageops::FilterType::Lanczos3)
        .to_rgba8()
}
