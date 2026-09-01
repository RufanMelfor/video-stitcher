//! Per-export, human-readable sidecar log file.
//!
//! [`StitchJob::run`](crate::StitchJob::run) opens one of these next to
//! the export's output file (same folder, same name, `.log` extension)
//! before it starts, writes a "requested settings" header, then leaves
//! it active for the duration of the run. While active, every `log::*`
//! / `tracing::*` event the process emits - on any thread, since decode,
//! encode and detection each run on their own - is mirrored into the
//! file via [`record_event`], in addition to wherever the rest of the
//! app's tracing subscriber already sends it (console, the in-app debug
//! panel, ...). `StitchJob::run` writes a result footer and closes the
//! file when the export finishes, success or failure.
//!
//! Only one export log can be active per process. Reco never runs two
//! exports concurrently today (reco-gui's export worker is a single
//! thread; reco-cli's `stitch` command is one-shot), so a plain global
//! is enough - there is no per-job handle threaded through the call
//! stack. If that ever changes, this module would need to key its state
//! by job instead.
//!
//! `record_event` is deliberately dependency-light (`tracing` only, no
//! `tracing-subscriber`) so this crate doesn't have to pull in the
//! subscriber machinery. Wiring it into the global subscriber - via a
//! trivial `Layer` impl that just forwards to `record_event` - is each
//! binary's job (`reco-cli`/`reco-gui`'s `init_tracing`), matching how
//! those binaries already own their tracing setup independently.

use std::fs::File;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

struct ActiveLog {
    file: File,
    started: Instant,
}

fn state() -> &'static Mutex<Option<ActiveLog>> {
    static STATE: OnceLock<Mutex<Option<ActiveLog>>> = OnceLock::new();
    STATE.get_or_init(|| Mutex::new(None))
}

/// Sidecar log path for `output_path`: same directory and file stem,
/// `.log` extension - e.g. `render.mp4` -> `render.log`.
pub fn sidecar_path(output_path: &Path) -> PathBuf {
    output_path.with_extension("log")
}

/// Create (truncating) the export log next to `output_path` and write
/// `header`. Returns the log's path on success. On failure (e.g. the
/// output directory doesn't exist yet, or is read-only) logs a warning
/// through the normal `log` facade and returns `None` - a log file that
/// can't be created should not fail the export itself.
pub fn begin(output_path: &Path, header: &str) -> Option<PathBuf> {
    let path = sidecar_path(output_path);
    let mut file = match File::create(&path) {
        Ok(f) => f,
        Err(e) => {
            log::warn!("Export log: could not create {}: {e}", path.display());
            return None;
        }
    };
    if let Err(e) = file.write_all(header.as_bytes()) {
        log::warn!(
            "Export log: could not write header to {}: {e}",
            path.display()
        );
        return None;
    }
    let _ = file.flush();
    let mut guard = state().lock().unwrap_or_else(|e| e.into_inner());
    *guard = Some(ActiveLog {
        file,
        started: Instant::now(),
    });
    Some(path)
}

/// Write `footer` and deactivate the current export log. No-op if none
/// is active (e.g. `begin` failed, or the export targets a network
/// stream and never opened one - see `StitchJob::run`).
pub fn finish(footer: &str) {
    let mut guard = state().lock().unwrap_or_else(|e| e.into_inner());
    if let Some(mut active) = guard.take() {
        let _ = active.file.write_all(footer.as_bytes());
        let _ = active.file.flush();
    }
}

/// Mirror one tracing event into the active export log, if any. Called
/// from a trivial `tracing_subscriber::Layer::on_event` in each binary
/// (see the module doc). Silently does nothing when no export is
/// running, and swallows write errors - a full disk shouldn't turn a
/// logging side-channel into an export failure.
pub fn record_event(event: &tracing::Event<'_>) {
    let mut guard = match state().lock() {
        Ok(g) => g,
        Err(_) => return,
    };
    let Some(active) = guard.as_mut() else {
        return;
    };
    let mut visitor = MessageVisitor::default();
    event.record(&mut visitor);
    if visitor.message.is_empty() && visitor.extra.is_empty() {
        return;
    }
    let meta = event.metadata();
    // `log::*` macro calls arrive here bridged through `tracing-log`,
    // which reports the generic target "log" and instead carries the
    // real module as a `log.target` field (plus `log.module_path`/
    // `log.file`/`log.line`, stripped out in `MessageVisitor` - just
    // source-location noise for a settings/timeline log). Prefer that
    // real target when present; a plain `tracing::info!` call (no
    // bridge involved) has no such field and keeps its own target.
    let target = visitor
        .log_target
        .as_deref()
        .unwrap_or_else(|| meta.target());
    let mut line = format!(
        "[{}] {:<5} {}: {}",
        format_elapsed(active.started.elapsed()),
        meta.level(),
        target,
        visitor.message
    );
    for (key, value) in &visitor.extra {
        line.push_str(&format!(" {key}={value}"));
    }
    line.push('\n');
    let _ = active.file.write_all(line.as_bytes());
    let _ = active.file.flush();
}

/// Extracts the `message` field (and any other structured fields) from
/// a tracing event. `log::*` macro calls bridged via `tracing-log`
/// always carry their formatted text as a `message` field; plain
/// `tracing::info!` etc. calls do too. The bridge also tags every
/// event with `log.target`/`log.module_path`/`log.file`/`log.line` -
/// the first is useful (see `record_event`), the rest are just
/// source-location noise for this log, so they're dropped here rather
/// than in `extra`.
#[derive(Default)]
struct MessageVisitor {
    message: String,
    log_target: Option<String>,
    extra: Vec<(String, String)>,
}

impl MessageVisitor {
    const IGNORED_FIELDS: [&'static str; 3] = ["log.module_path", "log.file", "log.line"];
}

impl tracing::field::Visit for MessageVisitor {
    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        match field.name() {
            "message" => self.message = format!("{value:?}"),
            "log.target" => {
                self.log_target = Some(format!("{value:?}").trim_matches('"').to_string())
            }
            name if Self::IGNORED_FIELDS.contains(&name) => {}
            name => self.extra.push((name.to_string(), format!("{value:?}"))),
        }
    }

    fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
        match field.name() {
            "message" => self.message = value.to_string(),
            "log.target" => self.log_target = Some(value.to_string()),
            name if Self::IGNORED_FIELDS.contains(&name) => {}
            name => self.extra.push((name.to_string(), value.to_string())),
        }
    }
}

/// `MM:SS.mmm` since the log was opened - simpler and more useful for
/// reading an export's timeline than a repeated absolute timestamp.
fn format_elapsed(d: Duration) -> String {
    let total_ms = d.as_millis();
    let ms = total_ms % 1000;
    let total_s = total_ms / 1000;
    format!("{:02}:{:02}.{ms:03}", total_s / 60, total_s % 60)
}

/// `H:MM:SS` (or `M:SS` under an hour) - for the result footer's total
/// elapsed time, where a running clock since log-open isn't the point.
pub fn format_duration(d: Duration) -> String {
    let total = d.as_secs();
    let (h, m, s) = (total / 3600, (total % 3600) / 60, total % 60);
    if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m}:{s:02}")
    }
}

/// `YYYY-MM-DD HH:MM:SS UTC`, computed from `SystemTime` alone (no
/// `chrono`/`time` dependency) via Howard Hinnant's `civil_from_days`
/// algorithm - good enough for a log header/footer stamp.
pub fn format_utc_now() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let (days, tod) = (secs / 86_400, secs % 86_400);
    let (h, m, s) = (tod / 3600, (tod % 3600) / 60, tod % 60);

    let z = days as i64 + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365; // [0, 399]
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let day = doy - (153 * mp + 2) / 5 + 1; // [1, 31]
    let month = if mp < 10 { mp + 3 } else { mp - 9 }; // [1, 12]
    let year = if month <= 2 { y + 1 } else { y };

    format!("{year:04}-{month:02}-{day:02} {h:02}:{m:02}:{s:02} UTC")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sidecar_path_swaps_extension_only() {
        assert_eq!(
            sidecar_path(Path::new(r"D:\Matches\Ajax\export.mp4")),
            Path::new(r"D:\Matches\Ajax\export.log")
        );
    }

    #[test]
    fn duration_formats_under_and_over_an_hour() {
        assert_eq!(format_duration(Duration::from_secs(75)), "1:15");
        assert_eq!(format_duration(Duration::from_secs(3661)), "1:01:01");
    }

    #[test]
    fn elapsed_formats_minutes_seconds_millis() {
        assert_eq!(format_elapsed(Duration::from_millis(65_432)), "01:05.432");
    }

    #[test]
    fn utc_now_is_plausibly_shaped() {
        let s = format_utc_now();
        assert_eq!(s.len(), "2026-08-29 20:14:03 UTC".len());
        assert!(s.ends_with(" UTC"));
    }
}
