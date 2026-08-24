//! Standalone benchmark: does disabling Chrome's own GPU compositor
//! (`enable_gpu(false)`, the runtime.rs fix for GPU contention with the
//! main render pipeline) meaningfully slow down scoreboard screenshot
//! capture? Not part of the crate's public API or test suite -
//! `cargo run -p reco-scoreboard --example gpu_bench --release`.
//!
//! Loads the real `scoreboards/football` package, captures N screenshots
//! back to back (the same call `runtime.rs::capture` makes), once with
//! Chrome's GPU compositor on and once off, and reports elapsed time.

use std::time::Instant;

use headless_chrome::browser::{LaunchOptionsBuilder, default_executable};
use headless_chrome::protocol::cdp::{Emulation, Page};
use headless_chrome::{Browser, Tab};

const CAPTURES: u32 = 150;

fn main() -> anyhow::Result<()> {
    let directory =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../scoreboards/football");
    let package = reco_scoreboard::ScoreboardPackage::load(&directory)?;
    let viewport = (
        package.manifest.viewport.width,
        package.manifest.viewport.height,
    );
    let browser_path = default_executable().map_err(|e| anyhow::anyhow!("{e}"))?;

    println!(
        "Package: {} ({}x{})",
        package.manifest.id, viewport.0, viewport.1
    );
    println!("{CAPTURES} captures per run\n");

    let gpu_on = run(&browser_path, &package.entry_path, viewport, true)?;
    let gpu_off = run(&browser_path, &package.entry_path, viewport, false)?;

    println!(
        "enable_gpu(true):  {:>7.1} ms total, {:>6.2} ms/capture",
        gpu_on.as_secs_f64() * 1000.0,
        gpu_on.as_secs_f64() * 1000.0 / f64::from(CAPTURES)
    );
    println!(
        "enable_gpu(false): {:>7.1} ms total, {:>6.2} ms/capture",
        gpu_off.as_secs_f64() * 1000.0,
        gpu_off.as_secs_f64() * 1000.0 / f64::from(CAPTURES)
    );
    let ratio = gpu_off.as_secs_f64() / gpu_on.as_secs_f64();
    println!("\ngpu-off is {ratio:.2}x the wall time of gpu-on");

    Ok(())
}

fn run(
    browser_path: &std::path::Path,
    entry_path: &std::path::Path,
    viewport: (u32, u32),
    enable_gpu: bool,
) -> anyhow::Result<std::time::Duration> {
    let options = LaunchOptionsBuilder::default()
        .path(Some(browser_path.to_path_buf()))
        .headless(true)
        .sandbox(true)
        .enable_gpu(enable_gpu)
        .window_size(Some(viewport))
        .build()
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    let browser = Browser::new(options).map_err(|e| anyhow::anyhow!("{e}"))?;
    let tab = browser.new_tab().map_err(|e| anyhow::anyhow!("{e}"))?;
    set_viewport(&tab, viewport)?;
    tab.set_transparent_background_color()
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    // `canonicalize()` (used by `ScoreboardPackage::load`) returns Windows'
    // `\\?\`-prefixed extended-length form, which breaks a `file://` URL.
    let display_path = entry_path.display().to_string();
    let display_path = display_path.strip_prefix(r"\\?\").unwrap_or(&display_path);
    let url = format!("file:///{}", display_path.replace('\\', "/"));
    tab.navigate_to(&url)
        .and_then(|tab| tab.wait_until_navigated())
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    // Let the page settle (fonts, layout) before timing starts.
    std::thread::sleep(std::time::Duration::from_millis(300));

    let t0 = Instant::now();
    for i in 0..CAPTURES {
        // Nudge the DOM each capture (a plain textContent write, same
        // order of magnitude as a scoreboard clock tick) so this isn't
        // just measuring a cached/unchanged paint.
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
        std::hint::black_box(&png);
    }
    let elapsed = t0.elapsed();
    Ok(elapsed)
}

fn set_viewport(tab: &Tab, viewport: (u32, u32)) -> anyhow::Result<()> {
    tab.call_method(Emulation::SetDeviceMetricsOverride {
        width: viewport.0,
        height: viewport.1,
        device_scale_factor: 1.0,
        mobile: false,
        scale: None,
        screen_width: Some(viewport.0),
        screen_height: Some(viewport.1),
        position_x: None,
        position_y: None,
        dont_set_visible_size: None,
        screen_orientation: None,
        viewport: None,
        display_feature: None,
        device_posture: None,
    })
    .map_err(|e| anyhow::anyhow!("{e}"))?;
    Ok(())
}
