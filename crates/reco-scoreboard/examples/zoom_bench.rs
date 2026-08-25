//! Standalone benchmark (throwaway, not part of the crate's public API or
//! test suite): is CSS `zoom` (this session's fix for the DSF<1 text-
//! hinting bug in `runtime.rs::apply_zoom`) meaningfully slower per-capture
//! than the `device_scale_factor` approach it replaced, at a realistic
//! render scale? `cargo run -p reco-scoreboard --example zoom_bench --release`.
//!
//! Loads the real `scoreboards/football` package, captures N screenshots
//! back to back at design_size * 0.533 (matches a real user's calibration:
//! placement.scale 0.4 into a 2560x1440 16:9 export), once via `zoom` and
//! once via `device_scale_factor`, and reports elapsed time.

use std::time::Instant;

use headless_chrome::Browser;
use headless_chrome::browser::{LaunchOptionsBuilder, default_executable};
use headless_chrome::protocol::cdp::{Emulation, Page};

const CAPTURES: u32 = 150;
const SCALE: f32 = 0.533;

fn main() -> anyhow::Result<()> {
    let directory =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../scoreboards/football");
    let package = reco_scoreboard::ScoreboardPackage::load(&directory)?;
    let design_size = (
        package.manifest.viewport.width,
        package.manifest.viewport.height,
    );
    let browser_path = default_executable().map_err(|e| anyhow::anyhow!("{e}"))?;

    println!(
        "Package: {} (design {}x{}, scale {SCALE})",
        package.manifest.id, design_size.0, design_size.1
    );
    println!("{CAPTURES} captures per run\n");

    let zoom = run(&browser_path, &package.entry_path, design_size, true)?;
    let dsf = run(&browser_path, &package.entry_path, design_size, false)?;

    println!(
        "zoom:   {:>7.1} ms total, {:>6.2} ms/capture",
        zoom.as_secs_f64() * 1000.0,
        zoom.as_secs_f64() * 1000.0 / f64::from(CAPTURES)
    );
    println!(
        "dsf:    {:>7.1} ms total, {:>6.2} ms/capture",
        dsf.as_secs_f64() * 1000.0,
        dsf.as_secs_f64() * 1000.0 / f64::from(CAPTURES)
    );
    let ratio = zoom.as_secs_f64() / dsf.as_secs_f64();
    println!("\nzoom is {ratio:.2}x the wall time of dsf");

    Ok(())
}

fn run(
    browser_path: &std::path::Path,
    entry_path: &std::path::Path,
    design_size: (u32, u32),
    use_zoom: bool,
) -> anyhow::Result<std::time::Duration> {
    let physical = (
        ((design_size.0 as f32) * SCALE).round() as u32,
        ((design_size.1 as f32) * SCALE).round() as u32,
    );
    let options = LaunchOptionsBuilder::default()
        .path(Some(browser_path.to_path_buf()))
        .headless(true)
        .sandbox(true)
        .enable_gpu(false)
        .window_size(Some(if use_zoom { physical } else { design_size }))
        .build()
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    let browser = Browser::new(options).map_err(|e| anyhow::anyhow!("{e}"))?;
    let tab = browser.new_tab().map_err(|e| anyhow::anyhow!("{e}"))?;

    if use_zoom {
        tab.call_method(Emulation::SetDeviceMetricsOverride {
            width: physical.0,
            height: physical.1,
            device_scale_factor: 1.0,
            mobile: false,
            scale: None,
            screen_width: Some(physical.0),
            screen_height: Some(physical.1),
            position_x: None,
            position_y: None,
            dont_set_visible_size: None,
            screen_orientation: None,
            viewport: None,
            display_feature: None,
            device_posture: None,
        })
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    } else {
        tab.call_method(Emulation::SetDeviceMetricsOverride {
            width: design_size.0,
            height: design_size.1,
            device_scale_factor: f64::from(SCALE),
            mobile: false,
            scale: None,
            screen_width: Some(design_size.0),
            screen_height: Some(design_size.1),
            position_x: None,
            position_y: None,
            dont_set_visible_size: None,
            screen_orientation: None,
            viewport: None,
            display_feature: None,
            device_posture: None,
        })
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    }
    tab.set_transparent_background_color()
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    let display_path = entry_path.display().to_string();
    let display_path = display_path.strip_prefix(r"\\?\").unwrap_or(&display_path);
    let url = format!("file:///{}", display_path.replace('\\', "/"));
    tab.navigate_to(&url)
        .and_then(|tab| tab.wait_until_navigated())
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    if use_zoom {
        tab.evaluate(
            &format!(r#"document.documentElement.style.zoom = "{SCALE}";"#),
            false,
        )
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    }
    std::thread::sleep(std::time::Duration::from_millis(300));

    // Both paths produce `physical`-sized PNGs - that's the whole point of
    // the zoom fix (matching the DSF path's output size exactly).
    let expected = physical;
    let t0 = Instant::now();
    for i in 0..CAPTURES {
        let _ = tab.evaluate(
            &format!(
                r##"(() => {{ const el = document.querySelector("#clock"); if (el) el.textContent = "00:{:02}"; }})()"##,
                i % 60
            ),
            false,
        );
        let png = tab
            .capture_screenshot(Page::CaptureScreenshotFormatOption::Png, None, None, true)
            .map_err(|e| anyhow::anyhow!("{e}"))?;
        let dims = image::load_from_memory_with_format(&png, image::ImageFormat::Png)?
            .into_rgba8()
            .dimensions();
        if i == 0 {
            println!(
                "  [{}] first capture: {}x{} (expected ~{}x{})",
                if use_zoom { "zoom" } else { "dsf " },
                dims.0,
                dims.1,
                expected.0,
                expected.1
            );
        }
        std::hint::black_box(&png);
    }
    let elapsed = t0.elapsed();
    Ok(elapsed)
}
