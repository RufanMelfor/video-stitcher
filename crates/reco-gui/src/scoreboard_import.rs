//! Match Logger JSON import and time-varying scoreboard replay.
//!
//! `scripts/match-logger/Match Logger.html` is a phone-side football event
//! logger: a human taps goal/card/period/pause buttons during the match and
//! exports a JSON file of timestamped events. This module turns that log
//! into the same `RecoScoreboard.update(state)` JSON contract the football
//! package (`scoreboards/football/scoreboard.js`) already consumes from its
//! live manual editor - just computed for an arbitrary point in match-time
//! instead of typed by a human in real time.
//!
//! No football rules live here beyond what's needed to reconstruct score/
//! cards/clock from the event log; the package itself still owns all
//! display logic.

use std::fmt;

use base64::Engine;
use serde::Deserialize;
use serde_json::{Value, json};

/// One event as exported by Match Logger's `buildExportFile()`
/// (`scripts/match-logger/Match Logger.html:1099-1114`).
#[derive(Debug, Clone, Deserialize)]
struct RawEvent {
    #[serde(rename = "type")]
    kind: String,
    ts: String,
    team: Option<String>,
    period: Option<u32>,
    minutes: Option<u32>,
}

#[derive(Debug, Clone, Deserialize)]
struct RawMeta {
    #[serde(default)]
    home: String,
    #[serde(default)]
    away: String,
    #[serde(default = "default_periods")]
    periods: u32,
}

fn default_periods() -> u32 {
    2
}

/// Raw export file shape - only the fields this module reads. Unknown
/// fields (`youtubeChapters`, `exportedAt`, ...) are ignored.
#[derive(Debug, Clone, Deserialize)]
struct RawExport {
    meta: RawMeta,
    events: Vec<RawEvent>,
}

/// One parsed, timestamped match event.
#[derive(Debug, Clone)]
struct MatchEvent {
    kind: EventKind,
    /// Milliseconds since the Unix epoch (UTC), parsed from the event's
    /// `ts` field.
    ts_ms: i64,
    team: Option<Team>,
    period: Option<u32>,
    minutes: Option<u32>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Team {
    Home,
    Away,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EventKind {
    VideoStart,
    PeriodStart,
    MatchEnd,
    Goal,
    YellowCard,
    RedCard,
    AddedTime,
    PauseStart,
    PauseEnd,
    /// `note` and any future/unknown event type - carries no replay state,
    /// kept only so `events.len()` in the GUI summary matches what Match
    /// Logger itself reports.
    Other,
}

/// A parsed Match Logger export, ready to be replayed at any point in
/// match-time via [`state_at`].
#[derive(Debug, Clone)]
pub struct MatchLoggerExport {
    pub home: String,
    pub away: String,
    pub period_count: u32,
    events: Vec<MatchEvent>,
}

/// Error parsing a Match Logger export file.
#[derive(Debug)]
pub enum ImportError {
    Read(std::io::Error),
    Json(serde_json::Error),
    /// One event's `ts` field wasn't the fixed
    /// `YYYY-MM-DDTHH:mm:ss.sssZ` shape `Date.prototype.toISOString()`
    /// always produces.
    BadTimestamp {
        index: usize,
        ts: String,
    },
}

impl fmt::Display for ImportError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ImportError::Read(e) => write!(f, "cannot read Match Logger export: {e}"),
            ImportError::Json(e) => write!(f, "invalid Match Logger export JSON: {e}"),
            ImportError::BadTimestamp { index, ts } => {
                write!(f, "event {index} has an unparseable timestamp {ts:?}")
            }
        }
    }
}

impl std::error::Error for ImportError {}

/// Parse a Match Logger export file from disk.
pub fn load(path: &std::path::Path) -> Result<MatchLoggerExport, ImportError> {
    let text = std::fs::read_to_string(path).map_err(ImportError::Read)?;
    parse(&text)
}

/// Upper bound on a file considered as a candidate Match Logger export by
/// [`find_export_in_folder`]. A real export is a few KB of timestamps
/// (the largest logged match to date is under 5KB); this only exists so
/// scanning a match folder never reads a large unrelated `.json` into
/// memory just to find out it isn't one.
const MAX_EXPORT_SCAN_BYTES: u64 = 4 * 1024 * 1024;

/// Find a Match Logger export sitting directly inside `dir`, for the
/// "Select Match Folder" picker to load without a second file dialog.
///
/// Identified by *content*, not filename: every `.json` small enough to
/// be one is parsed, and the first that yields a non-empty event log is
/// a match. A match folder legitimately holds several unrelated JSON
/// files (the per-match calibration, lens profiles, `clicks.json`), and
/// none of them parse as an export - there is no naming convention to
/// rely on, since the file is named by the phone that exported it.
///
/// With more than one export present (e.g. a re-export after fixing a
/// mistake), the most recently modified wins - that is the one the
/// operator produced last. Files whose modification time can't be read
/// sort oldest rather than being dropped, so a match folder on a
/// filesystem without mtimes still auto-loads something.
///
/// Returns the parsed export alongside its path: the scan has already
/// parsed every candidate to identify it, so handing that result back
/// keeps the caller from re-reading the file only to hit an error case
/// that cannot happen.
pub fn find_export_in_folder(
    dir: &std::path::Path,
) -> Option<(std::path::PathBuf, MatchLoggerExport)> {
    let entries = std::fs::read_dir(dir).ok()?;
    let mut found: Vec<(std::time::SystemTime, std::path::PathBuf, MatchLoggerExport)> = Vec::new();
    for entry in entries.filter_map(|e| e.ok()) {
        let path = entry.path();
        let is_json = path
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|ext| ext.eq_ignore_ascii_case("json"));
        if !is_json {
            continue;
        }
        let Ok(meta) = entry.metadata() else {
            continue;
        };
        if !meta.is_file() || meta.len() > MAX_EXPORT_SCAN_BYTES {
            continue;
        }
        match load(&path) {
            Ok(export) if export.event_count() > 0 => {
                found.push((
                    meta.modified().unwrap_or(std::time::UNIX_EPOCH),
                    path,
                    export,
                ));
            }
            // Not an export (or an empty one) - the common case for every
            // other JSON in a match folder, so not worth logging.
            _ => {}
        }
    }
    found.sort_by(|a, b| a.0.cmp(&b.0));
    found.pop().map(|(_, path, export)| (path, export))
}

/// Parse a Match Logger export from its JSON text.
pub fn parse(json_text: &str) -> Result<MatchLoggerExport, ImportError> {
    let raw: RawExport = serde_json::from_str(json_text).map_err(ImportError::Json)?;
    let mut events = Vec::with_capacity(raw.events.len());
    for (index, e) in raw.events.into_iter().enumerate() {
        let ts_ms = parse_iso8601_ms(&e.ts).ok_or_else(|| ImportError::BadTimestamp {
            index,
            ts: e.ts.clone(),
        })?;
        let kind = match e.kind.as_str() {
            "video_start" => EventKind::VideoStart,
            "period_start" => EventKind::PeriodStart,
            "match_end" => EventKind::MatchEnd,
            "goal" => EventKind::Goal,
            "yellow_card" => EventKind::YellowCard,
            "red_card" => EventKind::RedCard,
            "added_time" => EventKind::AddedTime,
            "pause_start" => EventKind::PauseStart,
            "pause_end" => EventKind::PauseEnd,
            _ => EventKind::Other,
        };
        let team = match e.team.as_deref() {
            Some("home") => Some(Team::Home),
            Some("away") => Some(Team::Away),
            _ => None,
        };
        events.push(MatchEvent {
            kind,
            ts_ms,
            team,
            period: e.period,
            minutes: e.minutes,
        });
    }
    // Match Logger always appends in order, but sort defensively - nothing
    // about the file format guarantees it, and an out-of-order log would
    // silently corrupt the replay below otherwise.
    events.sort_by_key(|e| e.ts_ms);
    Ok(MatchLoggerExport {
        home: raw.meta.home,
        away: raw.meta.away,
        period_count: raw.meta.periods,
        events,
    })
}

impl MatchLoggerExport {
    /// Number of logged events (for the GUI's "N events" summary).
    pub fn event_count(&self) -> usize {
        self.events.len()
    }

    /// The `video_start` event's timestamp, if the operator logged one -
    /// the natural default sync anchor (§5 of the plan): it's exactly what
    /// they tapped in sync with hitting record on the camera.
    pub fn video_start_ms(&self) -> Option<i64> {
        self.events
            .iter()
            .find(|e| e.kind == EventKind::VideoStart)
            .map(|e| e.ts_ms)
    }
}

/// Anchors one point in the Match Logger event log's wall-clock time to one
/// point in the video's own timeline, so [`state_at`] can convert between
/// the two. There is no automatic way to derive this - it always comes
/// from a human either accepting the `video_start` event as-is (assumed
/// `video_seconds: 0.0`) or scrubbing to the matching frame by hand.
#[derive(Debug, Clone, Copy)]
pub struct SyncAnchor {
    pub event_ts_ms: i64,
    pub video_seconds: f64,
}

/// Suggested `--start-time`/`export-start-secs` (video seconds) to trim
/// the dead time before kickoff, derived from the Match Logger's
/// `period_start`(1) event. `buffer_secs` of lead-in is kept before
/// kickoff so the trim doesn't cut right up against it. `None` when
/// there's no `period_start`(1) event, or kickoff is already within
/// `buffer_secs` of the anchor.
///
/// Deliberately **not** part of [`derived_cut_ranges`]: this is dead
/// air before the recording is even relevant to the match, the same
/// thing a manually-entered `--start-time` already trims elsewhere -
/// not a mid-stream pause, so it shouldn't get the "PAUZE" dip-to-
/// black treatment a cut range does (nothing needs announcing before
/// the video has even started), and a cut range starting exactly at
/// the export's own start historically hit a real bug where it wasn't
/// actually skipped (see `StitchJob::run`'s `first_window_start`,
/// fixed alongside this) - representing "skip to kickoff" the same
/// way any other `--start-time` is represented sidesteps that whole
/// class of edge case rather than relying on it being fixed correctly
/// forever.
pub fn derived_start_secs(
    export: &MatchLoggerExport,
    anchor: &SyncAnchor,
    buffer_secs: f64,
) -> Option<f64> {
    let kickoff_ms = export
        .events
        .iter()
        .find(|e| e.kind == EventKind::PeriodStart && e.period == Some(1))?
        .ts_ms;
    let to_video_secs =
        |ts_ms: i64| anchor.video_seconds + (ts_ms - anchor.event_ts_ms) as f64 / 1000.0;
    let start = to_video_secs(kickoff_ms) - buffer_secs;
    (start > 0.0).then_some(start)
}

/// Suggested `--end-time`/`export-end-secs` (video seconds) to trim
/// whatever was recorded after the match actually ended, derived from
/// the Match Logger's `match_end` event. `buffer_secs` of trailing
/// context is kept after the final whistle so the trim doesn't cut
/// right up against it. `None` when there's no `match_end` event -
/// same "a range nothing in the log justified" restraint as
/// [`derived_cut_ranges`], not a guess at where the recording happens
/// to stop.
///
/// Mirrors [`derived_start_secs`]'s pre-kickoff trim; see that
/// function's doc comment for why this is a `--end-time` seek rather
/// than a cut range too - nothing needs a "PAUZE" dip-to-black
/// treatment for footage past the final whistle, it just shouldn't be
/// encoded at all.
pub fn derived_end_secs(
    export: &MatchLoggerExport,
    anchor: &SyncAnchor,
    buffer_secs: f64,
) -> Option<f64> {
    let match_end_ms = export
        .events
        .iter()
        .find(|e| e.kind == EventKind::MatchEnd)?
        .ts_ms;
    let to_video_secs =
        |ts_ms: i64| anchor.video_seconds + (ts_ms - anchor.event_ts_ms) as f64 / 1000.0;
    Some(to_video_secs(match_end_ms) + buffer_secs)
}

/// Highlight windows (in video seconds, same space as reco-gui's manual
/// cut-range timeline) derived from a Match Logger export: one
/// `[start, end)` window per logged goal.
///
/// `lead_secs` before the logged moment and `trail_secs` after it. The
/// lead wants to be generous and is not symmetric with the trail: the
/// operator taps the goal button *after* seeing the ball go in, so the
/// logged timestamp already trails the event by a few seconds, and the
/// build-up worth watching started well before that. The trail only has
/// to cover the celebration.
///
/// Windows are returned in log order and may overlap - two goals in
/// quick succession are one continuous stretch of play, which
/// [`reco_io::cut_range::merge_windows`] is what resolves, not this
/// function. Negative starts are clamped to zero; nothing else is
/// clamped here, because this function has no idea how long the video
/// is - the export's own end time handles a window that runs past it.
///
/// Deliberately narrow, same as [`derived_cut_ranges`]: only `goal`
/// events. A highlight this misses is exactly a moment nothing in the
/// log justified keeping automatically.
pub fn derived_goal_windows(
    export: &MatchLoggerExport,
    anchor: &SyncAnchor,
    lead_secs: f64,
    trail_secs: f64,
) -> Vec<(f64, f64)> {
    let to_video_secs =
        |ts_ms: i64| anchor.video_seconds + (ts_ms - anchor.event_ts_ms) as f64 / 1000.0;
    export
        .events
        .iter()
        .filter(|e| e.kind == EventKind::Goal)
        .filter_map(|e| {
            let scored_at = to_video_secs(e.ts_ms);
            let start = (scored_at - lead_secs).max(0.0);
            let end = scored_at + trail_secs;
            (end > start).then_some((start, end))
        })
        .collect()
}

/// Cut ranges (in video seconds, same space as reco-gui's existing manual
/// cut-range timeline) derived from a Match Logger export: one range per
/// logged pause - the dead time during any in-match stoppage.
/// `lead_secs` of play is kept before the cut starts and `trail_secs`
/// after it ends, so a hard trim doesn't clip right up against the
/// moment play stopped or resumed. The two are separate because they
/// answer different questions: how much of the incident that caused the
/// stoppage to keep, versus how long to stay on the restart. Pre-roll
/// (before kickoff) is handled separately by [`derived_start_secs`], not
/// here.
///
/// Deliberately narrow: only derives from *explicit* `pause_start`/
/// `pause_end` events, never inferred gaps (e.g. a half-time break the
/// operator forgot to mark with Pause) - a range that doesn't show up
/// here is exactly a range nothing in the log justified cutting
/// automatically.
pub fn derived_cut_ranges(
    export: &MatchLoggerExport,
    anchor: &SyncAnchor,
    lead_secs: f64,
    trail_secs: f64,
) -> Vec<(f64, f64)> {
    let to_video_secs =
        |ts_ms: i64| anchor.video_seconds + (ts_ms - anchor.event_ts_ms) as f64 / 1000.0;
    let mut ranges = Vec::new();

    let mut pending_pause_start_ms: Option<i64> = None;
    for e in &export.events {
        match e.kind {
            EventKind::PauseStart => pending_pause_start_ms = Some(e.ts_ms),
            EventKind::PauseEnd => {
                // An unpaired pause_end (start missing/already consumed)
                // has nothing to cut from - skip it rather than guessing.
                if let Some(start_ms) = pending_pause_start_ms.take() {
                    let start_secs = (to_video_secs(start_ms) - lead_secs).max(0.0);
                    let end_secs = to_video_secs(e.ts_ms) + trail_secs;
                    if end_secs > start_secs {
                        ranges.push((start_secs, end_secs));
                    }
                }
            }
            _ => {}
        }
    }
    ranges
}

/// Reconstruct the `RecoScoreboard.update(state)` JSON contract
/// (`scoreboards/football/scoreboard.js`'s `Reco.onUpdate` handler) at a
/// given point in the video's timeline, by replaying every event up to
/// that point.
///
/// Mirrors Match Logger's own bookkeeping (`fmtClock`/`periodLabel` in
/// `Match Logger.html`) rather than reusing it directly, since that's
/// JavaScript running in a different process - this is a from-scratch
/// re-derivation of the same event-log-to-clock logic. `sport.status` is
/// computed for contract completeness but currently has no effect: the
/// football package's `scoreboard.js` does not read it.
pub fn state_at(export: &MatchLoggerExport, sync: &SyncAnchor, video_seconds: f64) -> Value {
    let wall_ms = sync.event_ts_ms + ((video_seconds - sync.video_seconds) * 1000.0).round() as i64;

    let mut home_score = 0u32;
    let mut away_score = 0u32;
    let mut home_yellow = 0u32;
    let mut away_yellow = 0u32;
    let mut home_red = 0u32;
    let mut away_red = 0u32;
    let mut current_period = 0u32;
    let mut period_start_ms: Option<i64> = None;
    let mut accumulated_pause_ms: i64 = 0;
    let mut pause_start_ms: Option<i64> = None;
    let mut added_time_this_period = 0u32;
    // Elapsed running time from periods that have already finished
    // (period_start or match_end already seen for them) - the football
    // clock counts up continuously across halves rather than resetting
    // each period (see scoreboard.js's `tickClock` comment), so this
    // carries forward instead of being dropped at each period boundary.
    let mut elapsed_before_current_period_ms: i64 = 0;
    let mut match_ended = false;

    for e in export.events.iter().filter(|e| e.ts_ms <= wall_ms) {
        match e.kind {
            EventKind::PeriodStart => {
                if let Some(start) = period_start_ms {
                    let running = (e.ts_ms - start - accumulated_pause_ms).max(0);
                    elapsed_before_current_period_ms += running;
                }
                period_start_ms = Some(e.ts_ms);
                accumulated_pause_ms = 0;
                pause_start_ms = None;
                added_time_this_period = 0;
                current_period = e.period.unwrap_or(current_period + 1);
            }
            EventKind::PauseStart => {
                if pause_start_ms.is_none() {
                    pause_start_ms = Some(e.ts_ms);
                }
            }
            EventKind::PauseEnd => {
                if let Some(start) = pause_start_ms.take() {
                    accumulated_pause_ms += e.ts_ms - start;
                }
            }
            EventKind::AddedTime => {
                added_time_this_period += e.minutes.unwrap_or(0);
            }
            EventKind::Goal => match e.team {
                Some(Team::Home) => home_score += 1,
                Some(Team::Away) => away_score += 1,
                None => {}
            },
            EventKind::YellowCard => match e.team {
                Some(Team::Home) => home_yellow += 1,
                Some(Team::Away) => away_yellow += 1,
                None => {}
            },
            EventKind::RedCard => match e.team {
                Some(Team::Home) => home_red += 1,
                Some(Team::Away) => away_red += 1,
                None => {}
            },
            EventKind::MatchEnd => {
                if let Some(start) = period_start_ms {
                    let running = (e.ts_ms - start - accumulated_pause_ms).max(0);
                    elapsed_before_current_period_ms += running;
                }
                period_start_ms = None;
                match_ended = true;
            }
            EventKind::VideoStart | EventKind::Other => {}
        }
    }

    let live_elapsed_ms = match period_start_ms {
        Some(start) => {
            let effective_now = pause_start_ms.unwrap_or(wall_ms);
            (effective_now - start - accumulated_pause_ms).max(0)
        }
        None => 0,
    };
    let total_elapsed_ms = elapsed_before_current_period_ms + live_elapsed_ms;
    let total_seconds = (total_elapsed_ms / 1000).max(0);
    let clock = format!("{:02}:{:02}", total_seconds / 60, total_seconds % 60);

    let running = !match_ended && period_start_ms.is_some() && pause_start_ms.is_none();
    let status = if match_ended {
        "post"
    } else if period_start_ms.is_some() {
        "live"
    } else if current_period > 0 {
        "break"
    } else {
        "pre"
    };

    json!({
        "version": 1,
        "game": {
            "clock": clock,
            "period": current_period.max(1),
            "running": running,
            "status": status,
        },
        "home": { "shortName": export.home, "score": home_score },
        "away": { "shortName": export.away, "score": away_score },
        "sport": {
            "periodCount": export.period_count,
            "addedTime": added_time_this_period,
            "homeYellowCards": home_yellow,
            "awayYellowCards": away_yellow,
            "homeRedCards": home_red,
            "awayRedCards": away_red,
        },
    })
}

/// GUI-only presentation fields for the "Edit Scoreboard" panel - team
/// crest images, their display size, a font family, and the banner's
/// background color. Kept out of [`state_at`] itself, which only knows
/// about match/sport data derived from the event log; these aren't
/// derived from anything, just carried through from whatever the panel
/// currently holds.
#[derive(Debug, Clone, Default)]
pub struct ScoreboardStyle {
    /// `data:` URI, read from a local image file - never a remote URL, so
    /// the sandboxed package renderer never makes a network fetch for it.
    pub home_logo: Option<String>,
    pub away_logo: Option<String>,
    /// Source file each logo's `data:` URI was encoded from - kept
    /// alongside it purely for persistence (see
    /// `reco_core::calibration::ScoreboardSettings`) and the GUI's "which
    /// file is this" indicator; not otherwise read by `apply_style`.
    pub home_logo_path: Option<std::path::PathBuf>,
    pub away_logo_path: Option<std::path::PathBuf>,
    /// CSS `font-family` value, e.g. `"Georgia, serif"`.
    pub font_family: Option<String>,
    /// Logo display size in CSS pixels (both team logos share one size).
    pub logo_size_px: Option<f32>,
    /// CSS color value for the banner background, e.g. `"#123456"`.
    pub banner_color: Option<String>,
}

/// Read an image file and encode it as a `data:` URI for
/// [`ScoreboardStyle::home_logo`]/`away_logo` - the sandboxed, offline
/// package renderer (`headless_chrome` with the `offline` feature) can't
/// fetch a plain file:// or http:// URL, so the bytes have to travel
/// inline in the JSON state itself.
pub fn image_data_uri(path: &std::path::Path) -> Result<String, String> {
    let mime = match path
        .extension()
        .and_then(|ext| ext.to_str())
        .map(str::to_ascii_lowercase)
        .as_deref()
    {
        Some("png") => "image/png",
        Some("jpg" | "jpeg") => "image/jpeg",
        Some("gif") => "image/gif",
        Some("webp") => "image/webp",
        Some("svg") => "image/svg+xml",
        _ => return Err("Unsupported image type - use PNG, JPG, GIF, WebP, or SVG".into()),
    };
    let bytes = std::fs::read(path).map_err(|error| format!("Cannot read image: {error}"))?;
    let encoded = base64::engine::general_purpose::STANDARD.encode(bytes);
    Ok(format!("data:{mime};base64,{encoded}"))
}

/// Merge `style` into an already-computed [`state_at`] payload. A no-op
/// per-field for anything left `None` in `style`.
pub fn apply_style(mut state: Value, style: &ScoreboardStyle) -> Value {
    if let Some(logo) = style.home_logo.as_deref() {
        state["home"]["logo"] = json!(logo);
    }
    if let Some(logo) = style.away_logo.as_deref() {
        state["away"]["logo"] = json!(logo);
    }
    // Built up as one object and assigned once (not nested indexing, since
    // `state_at` never populates `custom` - indexing into a not-yet-
    // existing nested object would panic on serde_json::Value) - each
    // field was previously a separate single-shot `state["custom"] = ...`
    // assignment, which meant setting the font silently wiped out an
    // already-set logo size or banner color (and vice versa).
    let mut custom = serde_json::Map::new();
    if let Some(font) = style.font_family.as_deref() {
        custom.insert("fontFamily".into(), json!(font));
    }
    if let Some(size) = style.logo_size_px {
        custom.insert("logoSize".into(), json!(size));
    }
    if let Some(color) = style.banner_color.as_deref() {
        custom.insert("bannerColor".into(), json!(color));
    }
    if !custom.is_empty() {
        state["custom"] = Value::Object(custom);
    }
    state
}

/// Parse the fixed `YYYY-MM-DDTHH:mm:ss.sssZ` shape
/// `Date.prototype.toISOString()` always produces (UTC, millisecond
/// precision, `Z` suffix) into milliseconds since the Unix epoch.
///
/// Deliberately not a general ISO-8601 parser (no timezone offsets, no
/// missing fields, no alternate separators) - Match Logger only ever
/// writes this one exact shape, so a small hand-rolled parser avoids
/// pulling in a full date/time crate for one narrow, fully-controlled
/// format.
fn parse_iso8601_ms(s: &str) -> Option<i64> {
    let b = s.as_bytes();
    if b.len() != 24
        || b[4] != b'-'
        || b[7] != b'-'
        || b[10] != b'T'
        || b[13] != b':'
        || b[16] != b':'
        || b[19] != b'.'
        || b[23] != b'Z'
    {
        return None;
    }
    let digits =
        |range: std::ops::Range<usize>| -> Option<i64> { s.get(range)?.parse::<i64>().ok() };
    let year = digits(0..4)?;
    let month = digits(5..7)?;
    let day = digits(8..10)?;
    let hour = digits(11..13)?;
    let minute = digits(14..16)?;
    let second = digits(17..19)?;
    let millis = digits(20..23)?;
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }

    // Days since the Unix epoch via Howard Hinnant's civil_from_days
    // inverse (well-known constant-time algorithm, proleptic Gregorian,
    // valid for any year - no leap-year special-casing needed).
    let y = if month <= 2 { year - 1 } else { year };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400; // [0, 399]
    let mp = (month + 9) % 12; // [0, 11], Mar=0 .. Feb=11
    let doy = (153 * mp + 2) / 5 + day - 1; // [0, 365]
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy; // [0, 146096]
    let days_since_epoch = era * 146097 + doe - 719468;

    let seconds = days_since_epoch * 86_400 + hour * 3600 + minute * 60 + second;
    Some(seconds * 1000 + millis)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> MatchLoggerExport {
        // A short, hand-written match: kickoff, an early goal, a yellow
        // card, a pause (injury break), added time called, half-time,
        // second half, a late goal, full time.
        let json_text = r#"{
            "meta": { "home": "Sharks", "away": "United", "periods": 2, "createdAt": "2026-08-21T14:00:00.000Z" },
            "exportedAt": "2026-08-21T16:00:00.000Z",
            "events": [
                { "type": "video_start", "ts": "2026-08-21T14:00:00.000Z" },
                { "type": "period_start", "ts": "2026-08-21T14:00:05.000Z", "period": 1 },
                { "type": "goal", "ts": "2026-08-21T14:05:05.000Z", "team": "home" },
                { "type": "yellow_card", "ts": "2026-08-21T14:10:05.000Z", "team": "away" },
                { "type": "pause_start", "ts": "2026-08-21T14:20:05.000Z" },
                { "type": "pause_end", "ts": "2026-08-21T14:22:05.000Z" },
                { "type": "added_time", "ts": "2026-08-21T14:44:05.000Z", "minutes": 2, "period": 1 },
                { "type": "period_start", "ts": "2026-08-21T15:01:05.000Z", "period": 2 },
                { "type": "goal", "ts": "2026-08-21T15:40:05.000Z", "team": "away" },
                { "type": "match_end", "ts": "2026-08-21T15:46:05.000Z" }
            ]
        }"#;
        parse(json_text).expect("fixture parses")
    }

    fn anchor() -> SyncAnchor {
        // video_start at video_seconds 0.0 - the recommended default.
        SyncAnchor {
            event_ts_ms: parse_iso8601_ms("2026-08-21T14:00:00.000Z").unwrap(),
            video_seconds: 0.0,
        }
    }

    #[test]
    fn apply_style_sets_only_the_fields_present() {
        let base = state_at(&fixture(), &anchor(), 0.0);
        let styled = apply_style(
            base.clone(),
            &ScoreboardStyle {
                home_logo: Some("data:image/png;base64,AAAA".into()),
                home_logo_path: None,
                away_logo: None,
                away_logo_path: None,
                font_family: Some("Georgia, serif".into()),
                logo_size_px: Some(48.0),
                banner_color: Some("#123456".into()),
            },
        );
        assert_eq!(styled["home"]["logo"], "data:image/png;base64,AAAA");
        assert!(styled["away"].get("logo").is_none());
        // All three `custom` fields must coexist - regression check for a
        // bug where each was a separate single-shot `state["custom"] = ..`
        // assignment, so setting one silently wiped the others.
        assert_eq!(styled["custom"]["fontFamily"], "Georgia, serif");
        assert_eq!(styled["custom"]["logoSize"], 48.0);
        assert_eq!(styled["custom"]["bannerColor"], "#123456");
        // Untouched fields survive the merge.
        assert_eq!(styled["home"]["shortName"], base["home"]["shortName"]);

        let unstyled = apply_style(base.clone(), &ScoreboardStyle::default());
        assert_eq!(unstyled, base);
    }

    #[test]
    fn parses_iso8601_millis() {
        // Cross-checked against the well-known Y2K reference instant
        // (widely cited exact Unix timestamp for 2000-01-01T00:00:00Z).
        assert_eq!(
            parse_iso8601_ms("2000-01-01T00:00:00.000Z").unwrap(),
            946_684_800_000
        );
        // One day and 500ms later - isolates the millisecond field and
        // exercises the day-rollover path, both relative to the checked
        // reference above rather than another hand-computed constant.
        assert_eq!(
            parse_iso8601_ms("2000-01-02T00:00:00.500Z").unwrap(),
            946_684_800_000 + 86_400_000 + 500
        );
        assert!(parse_iso8601_ms("not-a-date").is_none());
        assert!(parse_iso8601_ms("2026-08-21T14:00:00.000+02:00").is_none());
    }

    #[test]
    fn event_count_matches_export() {
        assert_eq!(fixture().event_count(), 10);
    }

    #[test]
    fn video_start_is_the_default_anchor() {
        let export = fixture();
        assert_eq!(
            export.video_start_ms(),
            Some(parse_iso8601_ms("2026-08-21T14:00:00.000Z").unwrap())
        );
    }

    #[test]
    fn derived_goal_windows_covers_each_goal_and_nothing_else() {
        let export = fixture();
        // The fixture's two goals sit at video_seconds 305.0 and 6005.0
        // (14:05:05 and 15:40:05, against a 14:00:00 anchor at video 0).
        // Every other event in it - kickoffs, a card, a pause, added
        // time, match end - must contribute no window at all.
        let windows = derived_goal_windows(&export, &anchor(), 15.0, 10.0);
        assert_eq!(windows, vec![(290.0, 315.0), (5990.0, 6015.0)]);
    }

    #[test]
    fn derived_goal_windows_lead_is_clamped_at_the_start_of_the_video() {
        let export = fixture();
        // A 400s lead on the 305s goal would start at -95s. A negative
        // start is not representable on the timeline, so it clamps to 0
        // rather than wrapping or dropping the highlight entirely.
        let windows = derived_goal_windows(&export, &anchor(), 400.0, 10.0);
        assert_eq!(windows[0], (0.0, 315.0));
    }

    /// The whole point of the asymmetric default: the operator taps the
    /// button after the ball is in, so the window has to reach back
    /// further than it reaches forward.
    #[test]
    fn derived_goal_windows_applies_lead_and_trail_independently() {
        let export = fixture();
        let windows = derived_goal_windows(&export, &anchor(), 20.0, 5.0);
        assert_eq!(windows[0], (285.0, 310.0));
    }

    /// End to end, in the shape the export actually uses it: two goals
    /// close enough to overlap must come out as one continuous clip,
    /// with no cut range in the middle of it.
    #[test]
    fn overlapping_goal_windows_become_one_highlight() {
        let mut export = fixture();
        let base = parse_iso8601_ms("2026-08-21T14:00:00.000Z").unwrap();
        // A second goal 20s after the first, with a 15s lead each.
        export.events.push(MatchEvent {
            kind: EventKind::Goal,
            ts_ms: base + 325_000,
            team: Some(Team::Home),
            period: None,
            minutes: None,
        });
        export.events.sort_by_key(|e| e.ts_ms);

        let windows =
            reco_io::cut_range::merge_windows(derived_goal_windows(&export, &anchor(), 15.0, 10.0));
        assert_eq!(windows, vec![(290.0, 335.0), (5990.0, 6015.0)]);
        assert_eq!(
            reco_io::cut_range::gaps_between(&windows),
            vec![(335.0, 5990.0)],
            "one cut between the two highlights, none inside the merged one"
        );
    }

    #[test]
    fn derived_cut_ranges_trims_only_the_pause() {
        let export = fixture();
        let ranges = derived_cut_ranges(&export, &anchor(), 2.0, 2.0);
        // pause_start/pause_end are at video_seconds 1205.0/1325.0 (see
        // the fixture's timestamps) - keep 2s of context on each side.
        // Pre-roll is no longer part of this list at all - see
        // `derived_start_secs`.
        assert_eq!(ranges, vec![(1203.0, 1327.0)]);
    }

    #[test]
    fn derived_cut_ranges_applies_lead_and_trail_independently() {
        let export = fixture();
        // Same pause as above, with the two margins deliberately
        // different: each boundary must move by its own value only.
        let ranges = derived_cut_ranges(&export, &anchor(), 5.0, 1.0);
        assert_eq!(ranges, vec![(1200.0, 1326.0)]);
    }

    #[test]
    fn derived_cut_ranges_lead_is_clamped_at_the_start_of_the_video() {
        let mut export = fixture();
        // A pause 3s into the video with a 10s lead would start at -7s;
        // a negative cut-range start is not representable on the
        // timeline, so it clamps to 0 rather than wrapping or being
        // dropped.
        let base = parse_iso8601_ms("2026-08-21T14:00:00.000Z").unwrap();
        export
            .events
            .retain(|e| e.kind != EventKind::PauseStart && e.kind != EventKind::PauseEnd);
        export.events.push(MatchEvent {
            kind: EventKind::PauseStart,
            ts_ms: base + 3_000,
            team: None,
            period: None,
            minutes: None,
        });
        export.events.push(MatchEvent {
            kind: EventKind::PauseEnd,
            ts_ms: base + 20_000,
            team: None,
            period: None,
            minutes: None,
        });
        export.events.sort_by_key(|e| e.ts_ms);
        let ranges = derived_cut_ranges(&export, &anchor(), 10.0, 1.0);
        assert_eq!(ranges, vec![(0.0, 21.0)]);
    }

    /// Minimal but genuinely valid export text, for the folder-scan
    /// tests below - `find_export_in_folder` decides purely on whether
    /// `parse` accepts a file, so these have to be real.
    fn export_text(home: &str) -> String {
        format!(
            r#"{{
                "meta": {{ "home": "{home}", "away": "United", "periods": 2 }},
                "events": [
                    {{ "type": "video_start", "ts": "2026-08-21T14:00:00.000Z" }},
                    {{ "type": "period_start", "ts": "2026-08-21T14:00:05.000Z", "period": 1 }}
                ]
            }}"#
        )
    }

    #[test]
    fn find_export_in_folder_ignores_the_other_json_files_a_match_folder_holds() {
        let dir = tempfile::tempdir().expect("tempdir");
        // The real shapes that sit next to an export in a match folder:
        // the per-match calibration, a lens profile, and clicks.json.
        std::fs::write(
            dir.path().join("Match_calibration.json"),
            r#"{"schema_version":2,"lenses":[],"topology":{},"framing":{}}"#,
        )
        .unwrap();
        std::fs::write(dir.path().join("clicks.json"), r#"{"points":[[1,2]]}"#).unwrap();
        std::fs::write(dir.path().join("log.json"), export_text("Sharks")).unwrap();

        let (path, export) = find_export_in_folder(dir.path()).expect("finds the export");
        assert_eq!(path.file_name().unwrap(), "log.json");
        assert_eq!(export.home, "Sharks");
    }

    #[test]
    fn find_export_in_folder_is_none_without_one() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join("clicks.json"), r#"{"points":[]}"#).unwrap();
        // An export with no events at all is not something to auto-load
        // either - it would replace a good import with an empty replay.
        std::fs::write(
            dir.path().join("empty.json"),
            r#"{"meta":{"home":"A","away":"B"},"events":[]}"#,
        )
        .unwrap();
        assert!(find_export_in_folder(dir.path()).is_none());
    }

    #[test]
    fn find_export_in_folder_prefers_the_most_recently_modified() {
        let dir = tempfile::tempdir().expect("tempdir");
        let old = dir.path().join("first_try.json");
        let new = dir.path().join("re_export.json");
        std::fs::write(&old, export_text("Sharks")).unwrap();
        std::fs::write(&new, export_text("Sharks")).unwrap();
        // Set the mtimes explicitly rather than relying on write order -
        // both files can land in the same filesystem timestamp tick.
        let base =
            std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_700_000_000);
        let set_mtime = |path: &std::path::Path, at: std::time::SystemTime| {
            std::fs::File::options()
                .write(true)
                .open(path)
                .unwrap()
                .set_modified(at)
                .unwrap();
        };
        set_mtime(&old, base);
        set_mtime(&new, base + std::time::Duration::from_secs(600));

        let (path, _) = find_export_in_folder(dir.path()).expect("finds an export");
        assert_eq!(path.file_name().unwrap(), "re_export.json");
    }

    #[test]
    fn derived_start_secs_trims_pre_roll() {
        let export = fixture();
        // Kickoff (period_start 1) is at video_seconds 5.0 - keep 2s of
        // lead-in, so --start-time should be 3.0.
        assert_eq!(derived_start_secs(&export, &anchor(), 2.0), Some(3.0));
    }

    #[test]
    fn derived_start_secs_none_when_kickoff_too_close_to_the_anchor() {
        let export = fixture();
        // A buffer bigger than the actual 5s gap to kickoff would trim a
        // negative (or zero) start - must be None, not clamped into
        // something nonsensical.
        assert_eq!(derived_start_secs(&export, &anchor(), 10.0), None);
    }

    #[test]
    fn derived_start_secs_none_without_a_period_start_event() {
        let mut export = fixture();
        export.events.retain(|e| e.kind != EventKind::PeriodStart);
        assert_eq!(derived_start_secs(&export, &anchor(), 2.0), None);
    }

    #[test]
    fn derived_end_secs_trims_post_match() {
        let export = fixture();
        // match_end (15:46:05) is 6365.0 video-seconds after the anchor
        // (video_start, 14:00:00) - keep 2s of trailing context past it.
        assert_eq!(derived_end_secs(&export, &anchor(), 2.0), Some(6367.0));
    }

    #[test]
    fn derived_end_secs_none_without_a_match_end_event() {
        let mut export = fixture();
        export.events.retain(|e| e.kind != EventKind::MatchEnd);
        assert_eq!(derived_end_secs(&export, &anchor(), 2.0), None);
    }

    #[test]
    fn pre_kickoff_shows_zero_zero_and_no_score() {
        let export = fixture();
        let state = state_at(&export, &anchor(), -1.0);
        assert_eq!(state["game"]["clock"], "00:00");
        assert_eq!(state["game"]["status"], "pre");
        assert_eq!(state["home"]["score"], 0);
        assert_eq!(state["away"]["score"], 0);
    }

    #[test]
    fn goal_counts_only_once_reached_in_video_time() {
        let export = fixture();
        // 4 minutes after kickoff (video_seconds 5 + 240 = 245): before the
        // 5-minute goal at match-time 14:05:05.
        let before = state_at(&export, &anchor(), 5.0 + 240.0);
        assert_eq!(before["home"]["score"], 0);
        // 6 minutes after kickoff: after the goal.
        let after = state_at(&export, &anchor(), 5.0 + 360.0);
        assert_eq!(after["home"]["score"], 1);
        assert_eq!(after["away"]["score"], 0);
    }

    #[test]
    fn pause_freezes_the_clock_but_not_video_time() {
        let export = fixture();
        // Pause starts at match-time 14:20:05, i.e. 20:00 of running clock
        // (period started 14:00:05). Sampling well into the 2-minute pause
        // should freeze the displayed clock at 20:00, not keep counting.
        let during_pause = state_at(&export, &anchor(), 20.0 * 60.0 + 60.0);
        assert_eq!(during_pause["game"]["clock"], "20:00");
        assert!(!during_pause["game"]["running"].as_bool().unwrap());
    }

    #[test]
    fn clock_carries_continuously_across_half_time() {
        let export = fixture();
        // Second half kicks off at 15:01:05, i.e. 61:00 after first-half
        // kickoff (14:00:05) by wall clock - minus the 2-minute pause,
        // that's 59:00 of *running* first-half clock behind it. Sample 5
        // minutes into the second half's running time.
        let sync = anchor();
        let second_half_start_video_s = (15 * 3600 + 60 + 5 - 14 * 3600) as f64;
        let state = state_at(&export, &sync, second_half_start_video_s + 5.0 * 60.0);
        assert_eq!(state["game"]["period"], 2);
        // 59:00 (first-half running time) + 5:00 (into the second half) = 64:00.
        assert_eq!(state["game"]["clock"], "64:00");
    }

    #[test]
    fn full_time_freezes_final_score_and_status() {
        let export = fixture();
        let long_after = state_at(&export, &anchor(), 4.0 * 3600.0);
        assert_eq!(long_after["home"]["score"], 1);
        assert_eq!(long_after["away"]["score"], 1);
        assert_eq!(long_after["game"]["status"], "post");
        assert!(!long_after["game"]["running"].as_bool().unwrap());
    }

    #[test]
    fn cards_are_attributed_to_the_right_team() {
        let export = fixture();
        let after_card = state_at(&export, &anchor(), 15.0 * 60.0);
        assert_eq!(after_card["sport"]["awayYellowCards"], 1);
        assert_eq!(after_card["sport"]["homeYellowCards"], 0);
    }

    #[test]
    fn added_time_reported_once_it_is_called() {
        let export = fixture();
        let before = state_at(&export, &anchor(), 44.0 * 60.0);
        assert_eq!(before["sport"]["addedTime"], 0);
        let after = state_at(&export, &anchor(), 44.5 * 60.0);
        assert_eq!(after["sport"]["addedTime"], 2);
        // A new period resets the added-time counter.
        let second_half_start_video_s = (15 * 3600 + 60 + 5 - 14 * 3600) as f64;
        let into_second_half = state_at(&export, &anchor(), second_half_start_video_s + 1.0);
        assert_eq!(into_second_half["sport"]["addedTime"], 0);
    }
}
