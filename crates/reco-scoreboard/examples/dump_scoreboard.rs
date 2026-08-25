//! Throwaway visual-verification tool (not part of the crate's public API
//! or test suite): renders the real `scoreboards/football` package at a
//! given render scale using the exact same zoom mechanism as
//! `runtime.rs::apply_zoom`, pushes a representative state, and saves the
//! capture to disk for manual inspection.
//!
//! `cargo run -p reco-scoreboard --example dump_scoreboard --release -- <scale> <out.png>`

use headless_chrome::Browser;
use headless_chrome::browser::{LaunchOptionsBuilder, default_executable};
use headless_chrome::protocol::cdp::{Emulation, Page};

fn main() -> anyhow::Result<()> {
    let mut args = std::env::args().skip(1);
    let scale: f32 = args
        .next()
        .ok_or_else(|| anyhow::anyhow!("usage: dump_scoreboard <scale> <out.png>"))?
        .parse()?;
    let out_path = args
        .next()
        .ok_or_else(|| anyhow::anyhow!("usage: dump_scoreboard <scale> <out.png>"))?;

    let directory =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../scoreboards/football");
    let package = reco_scoreboard::ScoreboardPackage::load(&directory)?;
    let design_size = (
        package.manifest.viewport.width,
        package.manifest.viewport.height,
    );
    let physical = (
        ((design_size.0 as f32) * scale).round() as u32,
        ((design_size.1 as f32) * scale).round() as u32,
    );
    println!("design {design_size:?}, scale {scale}, physical {physical:?}");

    let browser_path = default_executable().map_err(|e| anyhow::anyhow!("{e}"))?;
    let options = LaunchOptionsBuilder::default()
        .path(Some(browser_path))
        .headless(true)
        .sandbox(true)
        .enable_gpu(false)
        .window_size(Some(physical))
        .build()
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    let browser = Browser::new(options).map_err(|e| anyhow::anyhow!("{e}"))?;
    let tab = browser.new_tab().map_err(|e| anyhow::anyhow!("{e}"))?;

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
    tab.set_transparent_background_color()
        .map_err(|e| anyhow::anyhow!("{e}"))?;

    let display_path = package.entry_path.display().to_string();
    let display_path = display_path.strip_prefix(r"\\?\").unwrap_or(&display_path);
    let url = format!("file:///{}", display_path.replace('\\', "/"));
    tab.navigate_to(&url)
        .and_then(|tab| tab.wait_until_navigated())
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    tab.evaluate(
        &format!(r#"document.documentElement.style.zoom = "{scale}";"#),
        false,
    )
    .map_err(|e| anyhow::anyhow!("{e}"))?;

    // Same host-bridge contract runtime.rs uses, minimal inline copy - push
    // a representative real-match state (matches what the user's own OJC
    // vs BG Sport export showed: 1-0, 03:53, 1st half).
    tab.evaluate(
        r#"(() => {
            if (typeof window.RecoScoreboard.init === "function") {
                window.RecoScoreboard.init({apiVersion:1, package:{id:"football"}, viewport:{width:1920,height:1080}});
            }
            window.RecoScoreboard.update({
                version: 1,
                game: {clock: "03:53", period: 1, running: true, status: "live"},
                home: {shortName: "OJC", score: 1},
                away: {shortName: "BG Sport", score: 0},
                sport: {homeYellowCards: 1, awayYellowCards: 0, homeRedCards: 0, awayRedCards: 0}
            });
        })()"#,
        false,
    )
    .map_err(|e| anyhow::anyhow!("{e}"))?;
    std::thread::sleep(std::time::Duration::from_millis(300));

    let png = tab
        .capture_screenshot(Page::CaptureScreenshotFormatOption::Png, None, None, true)
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    std::fs::write(&out_path, &png)?;
    println!("saved {out_path}");
    Ok(())
}
