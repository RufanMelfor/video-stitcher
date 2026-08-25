//! Background Headless Chromium runtime with cached transparent RGBA output.

use std::sync::mpsc::{Receiver, SyncSender, TryRecvError, TrySendError};
use std::time::{Duration, Instant};

use headless_chrome::browser::{LaunchOptionsBuilder, default_executable};
use headless_chrome::protocol::cdp::{Emulation, Page};
use headless_chrome::{Browser, Tab};
use reco_core::render::overlay::{OverlayFrame, OverlayFrameSource};
use serde::Deserialize;
use serde_json::Value;
use thiserror::Error;

use crate::local_server::{EditorState, LocalPackageServer};
use crate::manifest::ScoreboardPackage;

enum RuntimeCommand {
    Update(String),
    Reset,
    /// Re-render at a new zoom level against the package's unchanged
    /// CSS viewport - see [`ScoreboardRuntime::set_render_scale`].
    SetRenderScale(f32),
    Shutdown,
}

/// Handle to an independently rendered scoreboard page.
pub struct ScoreboardRuntime {
    command_tx: SyncSender<RuntimeCommand>,
    frame_rx: Receiver<Result<OverlayFrame, String>>,
    editor_url: Option<String>,
    network_editor_url: Option<String>,
    _asset_server: LocalPackageServer,
}

impl ScoreboardRuntime {
    /// Start a sandboxed browser worker for one validated package.
    ///
    /// Browser startup and rendering happen off the caller thread. The local
    /// Chrome/Chromium executable is resolved synchronously so an unavailable
    /// engine can be shown to the user immediately.
    pub fn start(package: ScoreboardPackage, max_fps: u32) -> Result<Self, RuntimeError> {
        Self::start_with_state(package, max_fps, None)
    }

    /// Start a renderer and apply generic state before its first capture.
    pub fn start_with_state(
        package: ScoreboardPackage,
        max_fps: u32,
        initial_state: Option<Value>,
    ) -> Result<Self, RuntimeError> {
        let browser_path = default_executable().map_err(RuntimeError::BrowserNotFound)?;
        let max_fps = max_fps.clamp(1, 30);
        let asset_server = LocalPackageServer::start(package.directory.clone())
            .map_err(|error| RuntimeError::AssetServer(error.to_string()))?;
        let page_url = asset_server.url_for(&package.manifest.entry);
        let editor_url = package
            .manifest
            .editor
            .as_deref()
            .map(|editor| asset_server.editor_url_for(editor));
        let network_editor_url = package
            .manifest
            .editor
            .as_deref()
            .and_then(|editor| asset_server.network_editor_url_for(editor));
        let editor_state = asset_server.editor_state();
        let initial_json = initial_state
            .map(|state| serde_json::to_string(&state))
            .transpose()
            .map_err(RuntimeError::SerializeState)?;
        if let Some(json) = initial_json.as_ref() {
            editor_state.replace(json.clone());
        }
        let (command_tx, command_rx) = std::sync::mpsc::sync_channel(32);
        let (frame_tx, frame_rx) = std::sync::mpsc::sync_channel(2);
        std::thread::Builder::new()
            .name(format!("scoreboard-{}", package.manifest.id))
            .spawn(move || {
                if let Err(error) = run_worker(
                    package,
                    browser_path,
                    page_url,
                    editor_state,
                    initial_json,
                    max_fps,
                    &command_rx,
                    &frame_tx,
                ) {
                    let _ = frame_tx.try_send(Err(error.to_string()));
                }
            })
            .map_err(RuntimeError::Spawn)?;
        Ok(Self {
            command_tx,
            frame_rx,
            editor_url,
            network_editor_url,
            _asset_server: asset_server,
        })
    }

    /// Queue a generic JSON state update without waiting for JavaScript.
    pub fn update(&self, state: &Value) -> Result<(), RuntimeError> {
        let json = serde_json::to_string(state).map_err(RuntimeError::SerializeState)?;
        try_send_command(&self.command_tx, RuntimeCommand::Update(json))
    }

    /// Queue the package's optional `reset()` hook.
    pub fn reset(&self) -> Result<(), RuntimeError> {
        try_send_command(&self.command_tx, RuntimeCommand::Reset)
    }

    /// Re-render at a new zoom level, so future captures come out at
    /// roughly `scale` times the package's declared CSS viewport
    /// instead of always at its full native resolution.
    ///
    /// The CSS layout itself (`scoreboards/football/index.html`'s
    /// fixed-px grid/fonts) is untouched in *design* terms - it's
    /// re-laid-out at `scale`x via CSS `zoom` (Blink's own page-zoom
    /// mechanism, the same one behind a real browser's Ctrl+-/Ctrl++),
    /// not resampled after the fact, so Chrome's font
    /// hinting/anti-aliasing picks the right glyphs for the final
    /// physical size instead of sub-sampling glyphs rasterized for a
    /// different size. An earlier attempt drove this via
    /// `device_scale_factor` instead - looked equivalent, but Chrome's
    /// text rasterizer assumes DSF >= 1 and produces visibly *worse*
    /// text than a naive resize once `scale` drops below 1 (which it
    /// almost always does - the scoreboard banner is a small fraction
    /// of the frame). `device_scale_factor` is now always left at 1.0;
    /// see `set_viewport`/`apply_zoom`. The caller (`reco-gui`) knows
    /// how large this frame will actually end up on screen (its own
    /// placement scale times how the compositor fits the package's
    /// design resolution into the video frame) and should pass that
    /// combined ratio here - see
    /// [`reco_core::render::overlay::OverlayFrame::design_size`] for
    /// the other half of this (the frame's *placement* stays keyed to
    /// the stable design resolution regardless of this scale).
    ///
    /// Cheap to call on every placement drag/resize tick - a resize
    /// command still only actually reaches the browser a few times a
    /// second, same as [`Self::update`].
    pub fn set_render_scale(&self, scale: f32) -> Result<(), RuntimeError> {
        try_send_command(&self.command_tx, RuntimeCommand::SetRenderScale(scale))
    }

    /// Return the authenticated local editor URL declared by the package.
    pub fn editor_url(&self) -> Option<&str> {
        self.editor_url.as_deref()
    }

    /// Return the authenticated editor URL reachable from the local network.
    pub fn network_editor_url(&self) -> Option<&str> {
        self.network_editor_url.as_deref()
    }

    /// Return the latest state published by the package editor.
    pub fn current_editor_state(&self) -> Option<Value> {
        self._asset_server
            .editor_state()
            .current()
            .and_then(|json| serde_json::from_str(&json).ok())
    }
}

impl OverlayFrameSource for ScoreboardRuntime {
    fn try_frame(&mut self) -> Result<Option<OverlayFrame>, String> {
        let mut newest = None;
        loop {
            match self.frame_rx.try_recv() {
                Ok(Ok(frame)) => newest = Some(frame),
                Ok(Err(error)) => return Err(error),
                Err(TryRecvError::Empty) => return Ok(newest),
                Err(TryRecvError::Disconnected) => {
                    return if newest.is_some() {
                        Ok(newest)
                    } else {
                        Err("HTML renderer stopped unexpectedly".into())
                    };
                }
            }
        }
    }
}

impl Drop for ScoreboardRuntime {
    fn drop(&mut self) {
        let _ = self.command_tx.try_send(RuntimeCommand::Shutdown);
    }
}

fn try_send_command(
    sender: &SyncSender<RuntimeCommand>,
    command: RuntimeCommand,
) -> Result<(), RuntimeError> {
    match sender.try_send(command) {
        Ok(()) => Ok(()),
        Err(TrySendError::Full(_)) => Err(RuntimeError::CommandQueueFull),
        Err(TrySendError::Disconnected(_)) => Err(RuntimeError::RendererStopped),
    }
}

/// How many times a lost connection may be retried before giving up
/// and disabling the overlay for good - bounds a crash *loop* (a
/// genuinely broken environment) while still comfortably covering an
/// isolated crash or two over a long export. Reset back to full budget
/// whenever a session survives past [`RESTART_BUDGET_RESET_AFTER`], so
/// two unrelated crashes far apart in the same export both get the
/// full retry budget rather than sharing one.
const MAX_RESTART_ATTEMPTS: u32 = 3;
const RESTART_BUDGET_RESET_AFTER: Duration = Duration::from_secs(60);
/// Brief pause before relaunching, in case whatever killed the
/// browser (e.g. a GPU driver hiccup) needs a moment to settle.
const RESTART_BACKOFF: Duration = Duration::from_millis(750);

/// Runs the browser session, transparently relaunching it on a lost
/// connection instead of permanently disabling the overlay.
///
/// A lost DevTools connection ("Unable to make method calls because
/// underlying connection is closed") used to end the worker thread for
/// good: the error bubbles up through [`OverlayFrameSource::try_frame`]
/// and `reco_core::session::frame_processing::refresh_overlay` treats
/// any error as fatal, dropping the overlay source - so on a
/// multi-minute export, one Chrome crash meant the scoreboard was gone
/// for the rest of the video, with no recovery. Retried here instead,
/// relaunching the browser and reseeding it with the last state this
/// worker actually applied (both the regular `Update` stream and the
/// package's own editor state, which survives externally in
/// `editor_state` regardless of restarts) - the visible effect is the
/// overlay briefly freezing on its last frame during the relaunch,
/// never disappearing.
///
/// Only [`RuntimeError::Browser`] (a CDP/connection-level failure) is
/// retried. Every other variant means the package or setup itself is
/// broken (a bad manifest, a JS contract violation, no browser
/// installed, ...) - relaunching wouldn't fix that, and would just
/// spin up Chrome processes forever.
fn run_worker(
    package: ScoreboardPackage,
    browser_path: std::path::PathBuf,
    page_url: String,
    editor_state: EditorState,
    initial_json: Option<String>,
    max_fps: u32,
    command_rx: &Receiver<RuntimeCommand>,
    frame_tx: &SyncSender<Result<OverlayFrame, String>>,
) -> Result<(), RuntimeError> {
    let mut state_json = initial_json;
    // Survives restarts the same way `state_json` does - a relaunch
    // mid-export must keep rendering at whatever placement scale was
    // last set, not silently snap back to native resolution.
    let mut render_scale = 1.0_f32;
    let mut attempts_left = MAX_RESTART_ATTEMPTS;
    loop {
        let session_start = Instant::now();
        match run_session(
            &package,
            &browser_path,
            &page_url,
            &editor_state,
            state_json.clone(),
            render_scale,
            max_fps,
            command_rx,
            frame_tx,
            &mut state_json,
            &mut render_scale,
        ) {
            Ok(()) => return Ok(()),
            Err(RuntimeError::Browser(error)) if attempts_left > 0 => {
                attempts_left = if session_start.elapsed() >= RESTART_BUDGET_RESET_AFTER {
                    MAX_RESTART_ATTEMPTS - 1
                } else {
                    attempts_left - 1
                };
                log::warn!(
                    "scoreboard renderer lost its connection, restarting \
                     ({attempts_left} attempt(s) left): {error}"
                );
                std::thread::sleep(RESTART_BACKOFF);
            }
            Err(error) => return Err(error),
        }
    }
}

/// One browser launch, tab session, and its capture loop - see
/// [`run_worker`] for the retry wrapper around this. `state_json` and
/// `render_scale` are both the seed applied on entry and, on return,
/// updated to the most recent value actually applied - so a caller
/// that relaunches can reseed the new session with them.
#[allow(clippy::too_many_arguments)]
fn run_session(
    package: &ScoreboardPackage,
    browser_path: &std::path::Path,
    page_url: &str,
    editor_state: &EditorState,
    initial_json: Option<String>,
    initial_scale: f32,
    max_fps: u32,
    command_rx: &Receiver<RuntimeCommand>,
    frame_tx: &SyncSender<Result<OverlayFrame, String>>,
    state_json: &mut Option<String>,
    render_scale: &mut f32,
) -> Result<(), RuntimeError> {
    let viewport = (
        package.manifest.viewport.width,
        package.manifest.viewport.height,
    );
    let options = LaunchOptionsBuilder::default()
        .path(Some(browser_path.to_path_buf()))
        .headless(true)
        .sandbox(true)
        // The scoreboard packages this compositor supports are plain
        // DOM/CSS (no canvas/WebGL), so Chrome's own GPU process buys
        // nothing here - and disabling it removes a real source of
        // instability: it used to contend for the same GPU as the
        // app's own wgpu render pipeline + AI detection + video decode
        // during an export, which could crash Chrome's GPU process
        // (and with it this DevTools connection) under sustained load.
        .enable_gpu(false)
        .ignore_certificate_errors(false)
        .window_size(Some(viewport))
        // headless_chrome's default is 30s, but that timer isn't really
        // about *our* connection health - it's a fixed idle window on
        // browser-LEVEL events (new tab opened/closed), which our
        // single-tab package never generates regardless of how long the
        // session runs. Left at the default, it reliably logs a scary
        // "Got a timeout while listening for browser events" error()
        // roughly 30s into every session (live preview or export) even
        // though the tab connection actually used for updates/captures
        // is untouched - a red herring that looks exactly like a real
        // connection failure. A generous ceiling here means it only ever
        // fires for a genuinely abandoned browser process, not a normal
        // multi-minute-or-longer scoreboard session.
        .idle_browser_timeout(Duration::from_secs(6 * 60 * 60))
        .build()
        .map_err(|error| RuntimeError::BrowserLaunch(error.to_string()))?;
    let browser = Browser::new(options).map_err(RuntimeError::Browser)?;
    let tab = browser.new_tab().map_err(RuntimeError::Browser)?;
    tab.set_default_timeout(Duration::from_secs(10));
    // The CSS viewport (`viewport`, the package's declared design
    // resolution) never changes here - only `capture_size`, the
    // *physical* pixel resolution captures come out at, does, as
    // `SetRenderScale` commands arrive. Every `capture()` call below
    // reports `viewport` as the frame's `design_size` regardless of
    // `capture_size`, so placement stays keyed to the stable design
    // resolution - see `OverlayFrame::design_size`.
    let mut capture_size = set_viewport(&tab, viewport, initial_scale)?;
    install_error_hook(&tab)?;
    tab.set_transparent_background_color()
        .map_err(RuntimeError::Browser)?;
    tab.navigate_to(page_url)
        .and_then(|tab| tab.wait_until_navigated())
        .map_err(RuntimeError::Browser)?;
    // Only meaningful once a real document exists - `set_viewport`
    // above ran against the pre-navigation `about:blank` target, which
    // has no layout to zoom.
    apply_zoom(&tab, initial_scale)?;

    install_host_bridge(&tab, package)?;
    if let Some(json) = initial_json.as_deref() {
        evaluate_update(&tab, json)?;
    }
    let initial_status = render_status(&tab)?;
    if let Some(error) = initial_status.error {
        return Err(RuntimeError::JavaScript(error));
    }
    let _ = capture(&tab, capture_size, viewport, frame_tx)?;
    let mut last_version = initial_status.version;
    // Always replay from scratch (not gated on whether this launch had
    // its own `initial_json`) - a freshly launched tab, including one
    // spun up mid-export by `run_worker`'s restart loop, has no DOM
    // state of its own yet, so any editor content published before
    // this particular launch (`editor_state` persists across restarts
    // even though this local counter doesn't) needs a chance to reapply.
    let mut editor_version = 0;
    let interval = Duration::from_secs_f64(1.0 / f64::from(max_fps));
    let mut next_capture_check = Instant::now() + interval;

    loop {
        if let Some((version, json)) = editor_state.newer_than(editor_version) {
            evaluate_update(&tab, &json)?;
            editor_version = version;
        }
        loop {
            match command_rx.try_recv() {
                Ok(RuntimeCommand::Update(json)) => {
                    evaluate_update(&tab, &json)?;
                    *state_json = Some(json);
                }
                Ok(RuntimeCommand::Reset) => evaluate_optional(&tab, "reset")?,
                Ok(RuntimeCommand::SetRenderScale(scale)) => {
                    capture_size = set_viewport(&tab, viewport, scale)?;
                    apply_zoom(&tab, scale)?;
                    *render_scale = scale;
                    // Push a fresh capture at the new resolution right
                    // away, rather than waiting for the next natural
                    // version-change tick - the DOM content hasn't
                    // changed, only how many physical pixels it's
                    // rasterized into, so nothing else would trigger one.
                    if capture(&tab, capture_size, viewport, frame_tx)? {
                        last_version = render_status(&tab)?.version;
                    }
                }
                Ok(RuntimeCommand::Shutdown) | Err(TryRecvError::Disconnected) => {
                    let _ = evaluate_optional(&tab, "destroy");
                    return Ok(());
                }
                Err(TryRecvError::Empty) => break,
            }
        }

        let now = Instant::now();
        if now >= next_capture_check {
            let status = render_status(&tab)?;
            if let Some(error) = status.error {
                return Err(RuntimeError::JavaScript(error));
            }
            if (status.version != last_version || status.animated)
                && capture(&tab, capture_size, viewport, frame_tx)?
            {
                last_version = status.version;
            }
            next_capture_check = now + interval;
        }
        std::thread::sleep(Duration::from_millis(2));
    }
}

/// Resizes the CDP viewport to the *physical* pixel resolution a
/// `capture()` call will now produce - `design_size * scale`, rounded,
/// floored to at least 1px per dimension so a pathologically small
/// placement never asks for a 0x0 capture. `device_scale_factor` is
/// always left at 1.0: the zoom-vs-DSF distinction only matters for
/// how the page's own content is *laid out* within this viewport,
/// which is [`apply_zoom`]'s job, not this one - always call both
/// together (see the two call sites in `run_session`).
fn set_viewport(
    tab: &Tab,
    design_size: (u32, u32),
    scale: f32,
) -> Result<(u32, u32), RuntimeError> {
    let scale = if scale.is_finite() {
        scale.max(0.01)
    } else {
        1.0
    };
    let w = ((design_size.0 as f32) * scale).round().max(1.0) as u32;
    let h = ((design_size.1 as f32) * scale).round().max(1.0) as u32;
    tab.call_method(Emulation::SetDeviceMetricsOverride {
        width: w,
        height: h,
        device_scale_factor: 1.0,
        mobile: false,
        scale: None,
        screen_width: Some(w),
        screen_height: Some(h),
        position_x: None,
        position_y: None,
        dont_set_visible_size: None,
        screen_orientation: None,
        viewport: None,
        display_feature: None,
        device_posture: None,
    })
    .map_err(RuntimeError::Browser)?;
    Ok((w, h))
}

/// Applies CSS `zoom` to the live document's root element so its
/// fixed-px design layout still measures out at the package's full
/// declared CSS viewport, while Chrome paints that into the smaller
/// physical viewport [`set_viewport`] just configured - the same
/// mechanism behind a real browser's Ctrl+-/Ctrl++, which re-lays-out
/// text (and picks new font hinting) for the final physical size
/// instead of resampling glyphs rasterized for a different one. Must
/// be called after the target document has loaded (the root element
/// doesn't exist yet on an unloaded `about:blank` navigation target).
fn apply_zoom(tab: &Tab, scale: f32) -> Result<(), RuntimeError> {
    let scale = if scale.is_finite() {
        scale.max(0.01)
    } else {
        1.0
    };
    tab.evaluate(
        &format!(r#"document.documentElement.style.zoom = "{scale}";"#),
        false,
    )
    .map_err(|error| RuntimeError::JavaScript(error.to_string()))?;
    Ok(())
}

fn install_error_hook(tab: &Tab) -> Result<(), RuntimeError> {
    tab.call_method(Page::AddScriptToEvaluateOnNewDocument {
        source: r#"
            window.__recoLastError = null;
            window.addEventListener("error", event => {
                window.__recoLastError = event.message || "JavaScript error";
            });
            window.addEventListener("unhandledrejection", event => {
                window.__recoLastError = String(event.reason || "Unhandled promise rejection");
            });
        "#
        .into(),
        world_name: None,
        include_command_line_api: None,
        run_immediately: None,
    })
    .map_err(RuntimeError::Browser)?;
    Ok(())
}

fn install_host_bridge(tab: &Tab, package: &ScoreboardPackage) -> Result<(), RuntimeError> {
    let context = serde_json::json!({
        "apiVersion": 1,
        "package": {
            "id": package.manifest.id,
            "sport": package.manifest.sport,
            "version": package.manifest.version,
        },
        "viewport": {
            "width": package.manifest.viewport.width,
            "height": package.manifest.viewport.height,
        }
    });
    let script = format!(
        r#"(() => {{
            let version = 1;
            window.__recoMarkDirty = () => {{ version += 1; }};
            Object.defineProperty(window, "__recoRenderVersion", {{ get: () => version }});
            window.__recoLastError ??= null;
            new MutationObserver(window.__recoMarkDirty).observe(document.documentElement, {{
                attributes: true, childList: true, characterData: true, subtree: true
            }});
            if (!window.RecoScoreboard || window.RecoScoreboard.apiVersion !== 1 ||
                typeof window.RecoScoreboard.update !== "function") {{
                throw new Error("window.RecoScoreboard apiVersion 1 with update(state) is required");
            }}
            if (typeof window.RecoScoreboard.init === "function") {{
                window.RecoScoreboard.init({context});
            }}
            return true;
        }})()"#,
        context = context
    );
    tab.evaluate(&script, false)
        .map_err(|error| RuntimeError::JavaScript(error.to_string()))?;
    Ok(())
}

fn evaluate_update(tab: &Tab, json: &str) -> Result<(), RuntimeError> {
    let script = format!(
        r#"(() => {{
            if (!window.RecoScoreboard || typeof window.RecoScoreboard.update !== "function") {{
                throw new Error("RecoScoreboard.update is unavailable");
            }}
            window.RecoScoreboard.update({json});
            window.__recoMarkDirty();
        }})()"#
    );
    tab.evaluate(&script, false)
        .map_err(|error| RuntimeError::JavaScript(error.to_string()))?;
    Ok(())
}

fn evaluate_optional(tab: &Tab, method: &str) -> Result<(), RuntimeError> {
    let script = format!(
        r#"(() => {{
            if (window.RecoScoreboard && typeof window.RecoScoreboard.{method} === "function") {{
                window.RecoScoreboard.{method}();
                window.__recoMarkDirty?.();
            }}
        }})()"#
    );
    tab.evaluate(&script, false)
        .map_err(|error| RuntimeError::JavaScript(error.to_string()))?;
    Ok(())
}

#[derive(Deserialize)]
struct RenderStatus {
    version: u64,
    animated: bool,
    error: Option<String>,
}

fn render_status(tab: &Tab) -> Result<RenderStatus, RuntimeError> {
    let remote = tab
        .evaluate(
            r#"JSON.stringify({
                version: window.__recoRenderVersion || 0,
                animated: document.getAnimations().some(animation => animation.playState === "running"),
                error: window.__recoLastError || null
            })"#,
            false,
        )
        .map_err(RuntimeError::Browser)?;
    let json = remote
        .value
        .and_then(|value| value.as_str().map(str::to_owned))
        .ok_or_else(|| RuntimeError::JavaScript("renderer status returned no value".into()))?;
    serde_json::from_str(&json).map_err(RuntimeError::Status)
}

/// Captures the tab's current transparent screenshot. `expected_size`
/// is the *physical* pixel resolution this capture should come out at
/// (`design_size * render scale`, see `set_viewport`/`apply_zoom`) - a
/// mismatch is a real bug (a stale scale/viewport somewhere), not
/// tolerated.
/// `design_size` is always the package's unchanging CSS viewport,
/// carried into the emitted frame regardless of `expected_size` - see
/// [`OverlayFrame::design_size`].
fn capture(
    tab: &Tab,
    expected_size: (u32, u32),
    design_size: (u32, u32),
    frame_tx: &SyncSender<Result<OverlayFrame, String>>,
) -> Result<bool, RuntimeError> {
    let png = tab
        .capture_screenshot(Page::CaptureScreenshotFormatOption::Png, None, None, true)
        .map_err(RuntimeError::Browser)?;
    let rgba = image::load_from_memory_with_format(&png, image::ImageFormat::Png)
        .map_err(RuntimeError::DecodePng)?
        .into_rgba8();
    if rgba.dimensions() != expected_size {
        return Err(RuntimeError::UnexpectedSize {
            expected: expected_size,
            actual: rgba.dimensions(),
        });
    }
    let frame = OverlayFrame {
        width: expected_size.0,
        height: expected_size.1,
        design_size,
        rgba: rgba.into_raw(),
    };
    match frame_tx.try_send(Ok(frame)) {
        Ok(()) => Ok(true),
        Err(TrySendError::Full(_)) => Ok(false),
        Err(TrySendError::Disconnected(_)) => Err(RuntimeError::RendererStopped),
    }
}

/// Browser/runtime startup and communication failures.
#[derive(Debug, Error)]
pub enum RuntimeError {
    /// No supported browser executable is installed.
    #[error("Chrome or Chromium was not found: {0}")]
    BrowserNotFound(String),
    /// Worker thread could not be created.
    #[error("cannot start HTML renderer thread: {0}")]
    Spawn(std::io::Error),
    /// Browser options were invalid.
    #[error("cannot configure Chrome: {0}")]
    BrowserLaunch(String),
    /// Chrome DevTools operation failed.
    #[error("HTML renderer failed: {0}")]
    Browser(anyhow::Error),
    /// Loopback-only package asset server could not start.
    #[error("cannot serve scoreboard package: {0}")]
    AssetServer(String),
    /// JavaScript API or page code failed.
    #[error("scoreboard JavaScript failed: {0}")]
    JavaScript(String),
    /// State could not be encoded as JSON.
    #[error("cannot serialize scoreboard state: {0}")]
    SerializeState(serde_json::Error),
    /// Browser status response was malformed.
    #[error("invalid HTML renderer status: {0}")]
    Status(serde_json::Error),
    /// Transparent screenshot could not be decoded.
    #[error("cannot decode transparent HTML frame: {0}")]
    DecodePng(image::ImageError),
    /// Browser ignored the declared viewport.
    #[error("HTML renderer returned {actual:?}, expected {expected:?}")]
    UnexpectedSize {
        expected: (u32, u32),
        actual: (u32, u32),
    },
    /// Caller is updating faster than the browser can consume commands.
    #[error("HTML renderer command queue is full")]
    CommandQueueFull,
    /// Browser worker exited.
    #[error("HTML renderer stopped")]
    RendererStopped,
}

#[cfg(test)]
mod tests {
    use super::*;

    // No basketball_api_updates_dom_and_keeps_transparent_pixels test
    // here: scoreboards/basketball/ was deliberately not ported alongside
    // football (see the initial port commit) - out of scope for this
    // feature, not present in this tree at all.

    #[test]
    fn football_api_updates_dom_and_keeps_transparent_pixels() {
        let Ok(browser_path) = default_executable() else {
            eprintln!("Chrome/Chromium unavailable; skipping browser integration test");
            return;
        };
        let directory =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../scoreboards/football");
        let package = ScoreboardPackage::load(&directory).unwrap();
        let viewport = (
            package.manifest.viewport.width,
            package.manifest.viewport.height,
        );
        let options = LaunchOptionsBuilder::default()
            .path(Some(browser_path))
            .headless(true)
            .sandbox(true)
            .enable_gpu(false)
            .ignore_certificate_errors(false)
            .window_size(Some(viewport))
            .build()
            .unwrap();
        let browser = Browser::new(options).unwrap();
        let tab = browser.new_tab().unwrap();
        set_viewport(&tab, viewport, 1.0).unwrap();
        install_error_hook(&tab).unwrap();
        tab.set_transparent_background_color().unwrap();
        let asset_server = LocalPackageServer::start(package.directory.clone()).unwrap();
        let url = asset_server.url_for(&package.manifest.entry);
        tab.navigate_to(&url)
            .and_then(|tab| tab.wait_until_navigated())
            .unwrap();
        let content = tab.get_content().unwrap();
        assert!(
            content.contains("id=\"home-score\""),
            "scoreboard page did not load at {}: {content}",
            tab.get_url()
        );
        install_host_bridge(&tab, &package).unwrap();
        evaluate_update(
            &tab,
            r#"{
                "version":1,
                "game":{"clock":"63:18","period":2,"running":true,"status":"live"},
                "home":{"shortName":"HOME","score":2},
                "away":{"shortName":"AWAY","score":1},
                "sport":{"homeYellowCards":2,"awayYellowCards":1,"homeRedCards":0,"awayRedCards":1}
            }"#,
        )
        .unwrap();
        let remote = tab
            .evaluate(
                r##"JSON.stringify([
                    document.querySelector("#home-score").textContent,
                    document.querySelector("#away-score").textContent,
                    document.querySelector("#clock").textContent,
                    document.querySelector("#period").textContent
                ])"##,
                false,
            )
            .unwrap();
        let json = remote.value.unwrap();
        let values: Vec<String> = serde_json::from_str(json.as_str().unwrap()).unwrap();
        // "Nth Section", not "Nth Half" - `scoreboard.js`'s `periodLabel`
        // always says "Section" regardless of period count (see
        // SESSION_HANDOFF's 2026-08-23 entry).
        assert_eq!(values, ["2", "1", "63:18", "2nd Section"]);

        let png = tab
            .capture_screenshot(Page::CaptureScreenshotFormatOption::Png, None, None, true)
            .unwrap();
        let rgba = image::load_from_memory_with_format(&png, image::ImageFormat::Png)
            .unwrap()
            .into_rgba8();
        assert_eq!(rgba.dimensions(), viewport);
        assert_eq!(rgba.get_pixel(0, 0).0[3], 0);
        let visible_pixels = rgba.pixels().filter(|pixel| pixel.0[3] > 0).count();
        assert!(
            visible_pixels > 10_000,
            "scoreboard rendered only {visible_pixels} visible pixels"
        );

        let editor_target = package.manifest.editor.as_deref().unwrap();
        let editor_tab = browser.new_tab().unwrap();
        set_viewport(&editor_tab, viewport, 1.0).unwrap();
        editor_tab
            .navigate_to(&asset_server.editor_url_for(editor_target))
            .and_then(|tab| tab.wait_until_navigated())
            .unwrap();
        editor_tab.wait_for_element("#competition-input").unwrap();
        editor_tab
            .evaluate(
                r##"(() => {
                    const input = document.querySelector("#competition-input");
                    input.value = "Regional Final";
                    input.dispatchEvent(new Event("change", { bubbles: true }));
                    const addedTime = document.querySelector("#added-time-input");
                    addedTime.value = "3";
                    addedTime.dispatchEvent(new Event("change", { bubbles: true }));
                    document.querySelector("#home-yellow-btn").click();
                })()"##,
                false,
            )
            .unwrap();
        let state = asset_server.editor_state();
        let deadline = Instant::now() + Duration::from_secs(2);
        let editor_json = loop {
            if let Some((_, json)) = state.newer_than(0) {
                let value: Value = serde_json::from_str(&json).unwrap();
                if value["game"]["competition"] == "Regional Final"
                    && value["sport"]["addedTime"] == 3
                    && value["sport"]["homeYellowCards"] == 1
                {
                    break value;
                }
            }
            assert!(
                Instant::now() < deadline,
                "editor did not publish its state"
            );
            std::thread::sleep(Duration::from_millis(10));
        };
        assert_eq!(editor_json["sport"]["periodCount"], 2);
        assert_eq!(editor_json["sport"]["addedTime"], 3);
        assert_eq!(editor_json["sport"]["homeYellowCards"], 1);
    }
}
