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
                away_logo: None,
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
