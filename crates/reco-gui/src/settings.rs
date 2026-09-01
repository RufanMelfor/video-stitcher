//! GUI-specific persisted settings.
//!
//! Wraps `reco_io::settings` with a concrete `GuiSettings` struct that
//! captures the subset of user-facing state worth remembering across
//! sessions: recent file pairs, default export configuration, AI model
//! path, last window size.
//!
//! The split is deliberate: reco-io owns the generic load/save/MRU
//! machinery and doesn't know what a "codec" or "blend width" is,
//! while this module owns the GUI's specific schema. If reco-cli ever
//! wants its own persisted defaults it would define a separate
//! `CliSettings` struct in its own crate and use the `"cli"` namespace.

use std::path::PathBuf;

use reco_core::calibration::{AutocamDefaults, ScoreboardSettings};
use reco_io::settings::RecentFiles;
use serde::{Deserialize, Serialize};

/// Reco-gui's on-disk settings. Stored at `<config>/reco/gui.json`.
///
/// All fields carry `#[serde(default)]` so adding new fields in future
/// releases does not invalidate existing settings files - missing
/// fields just fall back to the `Default` impl's value.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GuiSettings {
    /// Most recently opened left camera videos.
    #[serde(default)]
    pub recent_left: RecentFiles,
    /// Most recently opened right camera videos.
    #[serde(default)]
    pub recent_right: RecentFiles,
    /// Most recently loaded calibration JSON files.
    #[serde(default)]
    pub recent_calibration: RecentFiles,

    /// Default export codec (`"h264"`, `"hevc"`, `"av1"`).
    #[serde(default = "default_codec")]
    pub default_codec: String,
    /// Default export quality (`"fast"`, `"balanced"`, `"high"`).
    #[serde(default = "default_quality")]
    pub default_quality: String,
    /// Default seam blend width used when the export dialog opens.
    #[serde(default = "default_blend_width")]
    pub default_blend_width: f32,

    /// Last chosen AI model path (YOLOv26n, RF-DETR, etc.) so the
    /// export dialog doesn't force the user to pick it every time.
    #[serde(default)]
    pub ai_model_path: Option<PathBuf>,

    /// Calibration file to fall back to whenever no calibration is
    /// otherwise loaded/picked for a session (see `AppState`'s startup
    /// calibration resolution). `None` means no default is configured -
    /// the app behaves as it always has, requiring an explicit pick.
    #[serde(default)]
    pub default_calibration_path: Option<PathBuf>,

    /// Last window size, remembered across restarts. `None` means
    /// "use Slint's preferred-width / preferred-height defaults".
    #[serde(default)]
    pub window_size: Option<(u32, u32)>,

    /// Whether the window was maximized when last closed.
    #[serde(default)]
    pub window_maximized: bool,

    /// Recording codec preference (h264, hevc, av1).
    #[serde(default = "default_codec")]
    pub recording_codec: String,

    /// Recording quality preference (fast, balanced, high).
    #[serde(default = "default_quality")]
    pub recording_quality: String,

    /// Default folder for preview recordings. `None` means "same
    /// directory as the source video".
    #[serde(default)]
    pub recording_folder: Option<PathBuf>,

    /// Preview aspect ratio mode (fill, 16:9, 4:3, 21:9).
    #[serde(default = "default_preview_aspect")]
    pub preview_aspect: String,

    /// Opt-in anonymous telemetry. Default false - no data sent until
    /// the user explicitly enables it in preferences.
    #[serde(default)]
    pub telemetry_enabled: bool,

    /// Persistent anonymous client ID for telemetry. Generated once on
    /// first enable, never reset. UUID v4, no PII.
    #[serde(default)]
    pub telemetry_client_id: Option<String>,

    /// Dark mode preference. Default true.
    #[serde(default = "default_dark_mode")]
    pub dark_mode: bool,

    /// Full segment chain for the left camera video last used (in
    /// temporal order). Restores a multi-segment selection (e.g. DJI's
    /// auto-split 4GB recordings) across an app restart - `recent_left`
    /// only tracks single most-recent paths for the Recent-files
    /// dropdown, which isn't enough on its own to reconstruct a chain.
    /// Empty means "no chain to restore, fall back to `last_left()`".
    #[serde(default)]
    pub last_left_segments: Vec<PathBuf>,
    /// Same as `last_left_segments`, for the right camera video.
    #[serde(default)]
    pub last_right_segments: Vec<PathBuf>,

    /// Last-used AI Tracking / panner tuning, independent of any
    /// calibration. `Calibration::autocam_defaults` (see reco-core) is
    /// still the per-match/per-rig source of truth and takes priority
    /// once a calibration with its own defaults is loaded - this field
    /// only exists so the Export dialog's sliders don't reset to
    /// hardcoded literals on every app restart when the user hasn't
    /// explicitly clicked Save calibration. Updated on every slider
    /// edit (see `main.rs`'s `autocam-settings-changed` handler), not
    /// just on save.
    #[serde(default)]
    pub autocam_defaults: Option<AutocamDefaults>,

    /// Match folder last selected via "Select Match Folder" (see
    /// `AppState::match_folder`), so the export dialog keeps suggesting
    /// a path inside it across an app restart instead of silently
    /// falling back to "next to the left video" (i.e. inside `Left/`) -
    /// `AppState::match_folder` itself is session-only and would
    /// otherwise be lost on every restart even though the restored
    /// left/right paths still point into this same match folder.
    /// Cleared whenever the user manually re-picks a left/right video
    /// (see `on_pick_left_video`/`on_pick_right_video`) so a stale
    /// folder never gets restored after that.
    #[serde(default)]
    pub last_match_folder: Option<PathBuf>,

    /// Last-used state of the Export dialog's "AI Tracking" master
    /// checkbox. Deliberately separate from `autocam_defaults`
    /// (`reco_core::calibration::AutocamDefaults` excludes this on
    /// purpose - see that type's own doc comment: whether AI tracking
    /// runs at all is a per-run choice, not something a shared
    /// calibration file should force on everyone who opens it). This
    /// field is purely an app-level convenience so the checkbox itself
    /// doesn't reset to off on every restart.
    #[serde(default)]
    pub autocam_enabled: bool,
    /// Last-used state of the Export dialog's "Async Detect" checkbox.
    /// Same reasoning and same app-level-only scope as
    /// `autocam_enabled`.
    #[serde(default)]
    pub async_detect_enabled: bool,

    /// Last-used SCOREBOARD card settings - enabled/package, auto-cut
    /// kickoff+pauses, placement/size, font, banner color, logo size,
    /// and the loaded Match Logger export path + sync anchor. Reuses
    /// `reco_core::calibration::ScoreboardSettings` (the same struct a
    /// calibration file's own `scoreboard` field holds) purely as a
    /// convenient existing shape - this copy is app-level only, so the
    /// SCOREBOARD card doesn't reset to defaults every restart just
    /// because the user hasn't loaded a calibration with its own
    /// scoreboard settings yet. A calibration's own `scoreboard` field
    /// still overrides this once one is loaded (same precedence as
    /// `autocam_defaults`/`Calibration::autocam_defaults`).
    #[serde(default)]
    pub scoreboard_settings: Option<ScoreboardSettings>,
    /// Whether the export panel's "PAUZE overlay on cuts" checkbox was
    /// on, and the two durations next to it. App-level (not per-match):
    /// they express how the user wants a break to read on screen, which
    /// doesn't change from one match to the next - and having to retype
    /// them every session was the whole reason they were exposed.
    #[serde(default)]
    pub pause_overlay_enabled: bool,
    /// Seconds of dip-to-black in *and* back out. Composited over frames
    /// that were going to be encoded anyway, so it adds no export length.
    #[serde(default = "default_pause_overlay_fade_secs")]
    pub pause_overlay_fade_secs: f32,
    /// Seconds the picture stays fully black on "PAUZE" - the only part
    /// of the transition that lengthens the export.
    #[serde(default = "default_pause_overlay_hold_secs")]
    pub pause_overlay_hold_secs: f32,
}

fn default_dark_mode() -> bool {
    true
}

fn default_codec() -> String {
    "h264".into()
}
fn default_quality() -> String {
    "balanced".into()
}
fn default_blend_width() -> f32 {
    0.05
}
fn default_preview_aspect() -> String {
    "auto".into()
}
/// Mirrors `export-pause-overlay-fade-secs`'s default in main.slint -
/// keep the two in sync, a settings file predating these fields must
/// restore exactly what the UI would have shown without one.
fn default_pause_overlay_fade_secs() -> f32 {
    3.0
}
/// Mirrors `export-pause-overlay-hold-secs`'s default in main.slint.
fn default_pause_overlay_hold_secs() -> f32 {
    4.0
}

impl Default for GuiSettings {
    fn default() -> Self {
        Self {
            recent_left: RecentFiles::default(),
            recent_right: RecentFiles::default(),
            recent_calibration: RecentFiles::default(),
            default_codec: default_codec(),
            default_quality: default_quality(),
            default_blend_width: default_blend_width(),
            ai_model_path: None,
            default_calibration_path: None,
            window_size: None,
            window_maximized: false,
            recording_codec: default_codec(),
            recording_quality: default_quality(),
            recording_folder: None,
            preview_aspect: default_preview_aspect(),
            telemetry_enabled: false,
            telemetry_client_id: None,
            dark_mode: true,
            last_left_segments: Vec::new(),
            last_right_segments: Vec::new(),
            autocam_defaults: None,
            last_match_folder: None,
            autocam_enabled: false,
            async_detect_enabled: false,
            scoreboard_settings: None,
            pause_overlay_enabled: false,
            pause_overlay_fade_secs: default_pause_overlay_fade_secs(),
            pause_overlay_hold_secs: default_pause_overlay_hold_secs(),
        }
    }
}

/// Namespace used under the reco config directory. All reco-gui
/// settings live at `<config>/reco/gui.json`.
pub const NAMESPACE: &str = "gui";

impl GuiSettings {
    /// Load settings from disk. Missing or malformed files fall back
    /// to defaults per `reco_io::settings::load_or_default` (the
    /// fallback is logged but never fatal - we never refuse to start
    /// because a settings file went bad).
    pub fn load() -> Self {
        reco_io::settings::load_or_default::<GuiSettings>(NAMESPACE)
    }

    /// Persist settings atomically. Errors are logged and swallowed -
    /// a failure to save preferences should never block user work
    /// (worst case: the user has to re-pick defaults next session).
    pub fn save(&self) {
        if let Err(e) = reco_io::settings::save(NAMESPACE, self) {
            log::warn!("failed to save GUI settings: {e}");
        }
    }

    /// Most recently used left video, if any and if it still exists on disk.
    pub fn last_left(&self) -> Option<PathBuf> {
        self.recent_left
            .entries()
            .first()
            .filter(|p| p.exists())
            .cloned()
    }

    /// Most recently used right video, if any and if it still exists on disk.
    pub fn last_right(&self) -> Option<PathBuf> {
        self.recent_right
            .entries()
            .first()
            .filter(|p| p.exists())
            .cloned()
    }

    /// Most recently used calibration file, if any and if it still exists on disk.
    pub fn last_calibration(&self) -> Option<PathBuf> {
        self.recent_calibration
            .entries()
            .first()
            .filter(|p| p.exists())
            .cloned()
    }

    /// The configured default-calibration fallback, if set and if it
    /// still exists on disk (mirrors `last_calibration`'s existence
    /// check - a deleted/moved default should silently stop applying
    /// rather than pointing `calibration_path` at a dead file).
    pub fn default_calibration(&self) -> Option<PathBuf> {
        self.default_calibration_path
            .as_ref()
            .filter(|p| p.exists())
            .cloned()
    }

    /// Convenience: push a newly-picked left video into MRU and save.
    pub fn push_left(&mut self, path: PathBuf) {
        self.recent_left.push(path);
        self.save();
    }

    /// Convenience: push a newly-picked right video into MRU and save.
    pub fn push_right(&mut self, path: PathBuf) {
        self.recent_right.push(path);
        self.save();
    }

    /// Convenience: push a newly-loaded calibration file into MRU and save.
    pub fn push_calibration(&mut self, path: PathBuf) {
        self.recent_calibration.push(path);
        self.save();
    }

    /// Persist the full segment chain for the left video (see
    /// `last_left_segments`).
    pub fn set_last_left_segments(&mut self, paths: Vec<PathBuf>) {
        self.last_left_segments = paths;
        self.save();
    }

    /// Persist the full segment chain for the right video (see
    /// `last_right_segments`).
    pub fn set_last_right_segments(&mut self, paths: Vec<PathBuf>) {
        self.last_right_segments = paths;
        self.save();
    }

    /// Restore a previously-used (possibly multi-segment) input: prefers
    /// the persisted segment chain when non-empty and every segment
    /// still exists on disk, otherwise falls back to the single
    /// most-recent path from the Recent-files MRU.
    fn restore_input(
        segments: &[PathBuf],
        last_single: Option<PathBuf>,
    ) -> Option<reco_io::stitch_job::InputPath> {
        if !segments.is_empty() && segments.iter().all(|p| p.exists()) {
            return Some(if segments.len() == 1 {
                reco_io::stitch_job::InputPath::Single(segments[0].clone())
            } else {
                reco_io::stitch_job::InputPath::Chained(segments.to_vec())
            });
        }
        last_single.map(reco_io::stitch_job::InputPath::Single)
    }

    /// Restore the left video input last used (see [`Self::restore_input`]).
    pub fn restore_left_input(&self) -> Option<reco_io::stitch_job::InputPath> {
        Self::restore_input(&self.last_left_segments, self.last_left())
    }

    /// Restore the right video input last used (see [`Self::restore_input`]).
    pub fn restore_right_input(&self) -> Option<reco_io::stitch_job::InputPath> {
        Self::restore_input(&self.last_right_segments, self.last_right())
    }

    /// Persist the current AI Tracking / panner tuning as the app-level
    /// last-used default and save immediately. Called on every Export
    /// dialog slider edit - see `main.rs`'s `autocam-settings-changed`
    /// handler.
    pub fn set_autocam_defaults(&mut self, ac: AutocamDefaults) {
        self.autocam_defaults = Some(ac);
        self.save();
    }

    /// Persist the "AI Tracking" and "Async Detect" checkbox states
    /// (see their own doc comments for why they're separate from
    /// `autocam_defaults`).
    pub fn set_ai_toggle_defaults(&mut self, autocam_enabled: bool, async_detect_enabled: bool) {
        self.autocam_enabled = autocam_enabled;
        self.async_detect_enabled = async_detect_enabled;
        self.save();
    }

    /// Persist the SCOREBOARD card's settings (see
    /// `Self::scoreboard_settings`'s doc comment).
    pub fn set_scoreboard_settings(&mut self, settings: ScoreboardSettings) {
        self.scoreboard_settings = Some(settings);
        self.save();
    }

    /// Persist the PAUZE transition's checkbox and durations (see
    /// `Self::pause_overlay_enabled`'s doc comment).
    pub fn set_pause_overlay(&mut self, enabled: bool, fade_secs: f32, hold_secs: f32) {
        self.pause_overlay_enabled = enabled;
        self.pause_overlay_fade_secs = fade_secs;
        self.pause_overlay_hold_secs = hold_secs;
        self.save();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_values_are_sensible() {
        let s = GuiSettings::default();
        assert_eq!(s.default_codec, "h264");
        assert_eq!(s.default_quality, "balanced");
        assert!((s.default_blend_width - 0.05).abs() < 1e-6);
        assert!(s.recent_left.is_empty());
    }

    #[test]
    fn missing_fields_roundtrip_via_defaults() {
        // Simulate loading an older-version settings JSON where new
        // fields don't exist yet; the serde defaults should fill in.
        let json = r#"{ "default_codec": "hevc" }"#;
        let s: GuiSettings = serde_json::from_str(json).unwrap();
        assert_eq!(s.default_codec, "hevc");
        assert_eq!(s.default_quality, "balanced");
        assert!(s.recent_left.is_empty());
    }

    /// Creates `n` empty files under a unique temp subdirectory and
    /// returns their paths - real files on disk so `restore_input`'s
    /// `.exists()` filtering has something genuine to check.
    fn make_temp_files(label: &str, n: usize) -> Vec<PathBuf> {
        static COUNTER: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        let unique = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "reco-gui-settings-test-{label}-{}-{unique}",
            std::process::id(),
        ));
        std::fs::create_dir_all(&dir).unwrap();
        (0..n)
            .map(|i| {
                let p = dir.join(format!("segment_{i}.mp4"));
                std::fs::write(&p, b"").unwrap();
                p
            })
            .collect()
    }

    #[test]
    fn restore_input_prefers_segment_chain_over_single_mru_path() {
        let segments = make_temp_files("chain", 3);
        let mut s = GuiSettings::default();
        s.recent_left.push(segments[0].clone());
        s.last_left_segments = segments.clone();

        let restored = s.restore_left_input().expect("chain should restore");
        assert_eq!(restored.all_paths(), segments);
    }

    #[test]
    fn restore_input_falls_back_to_single_when_no_segments_persisted() {
        let single = make_temp_files("single", 1);
        let mut s = GuiSettings::default();
        s.recent_left.push(single[0].clone());
        // last_left_segments intentionally left empty.

        let restored = s.restore_left_input().expect("single path should restore");
        assert_eq!(restored.all_paths(), single);
    }

    #[test]
    fn restore_input_falls_back_when_a_persisted_segment_no_longer_exists() {
        let mut segments = make_temp_files("stale", 2);
        let single = make_temp_files("fallback", 1);
        // Simulate one segment having been deleted/moved since last save.
        std::fs::remove_file(&segments[1]).unwrap();
        segments.push(PathBuf::from("does-not-exist.mp4"));

        let mut s = GuiSettings::default();
        s.recent_left.push(single[0].clone());
        s.last_left_segments = segments;

        let restored = s
            .restore_left_input()
            .expect("should fall back to single path");
        assert_eq!(restored.all_paths(), single);
    }

    #[test]
    fn restore_input_none_when_nothing_persisted() {
        let s = GuiSettings::default();
        assert!(s.restore_left_input().is_none());
        assert!(s.restore_right_input().is_none());
    }

    #[test]
    fn default_calibration_none_when_unset() {
        let s = GuiSettings::default();
        assert!(s.default_calibration().is_none());
    }

    #[test]
    fn default_calibration_none_when_file_no_longer_exists() {
        let s = GuiSettings {
            default_calibration_path: Some(PathBuf::from("does-not-exist.json")),
            ..Default::default()
        };
        assert!(s.default_calibration().is_none());
    }

    #[test]
    fn default_calibration_returns_path_when_it_exists() {
        let paths = make_temp_files("default-cal", 1);
        let s = GuiSettings {
            default_calibration_path: Some(paths[0].clone()),
            ..Default::default()
        };
        assert_eq!(s.default_calibration(), Some(paths[0].clone()));
    }

    #[test]
    fn autocam_defaults_absent_until_set() {
        let s = GuiSettings::default();
        assert!(s.autocam_defaults.is_none());
    }

    #[test]
    fn ai_toggle_defaults_false_until_set() {
        let s = GuiSettings::default();
        assert!(!s.autocam_enabled);
        assert!(!s.async_detect_enabled);
    }

    #[test]
    fn set_ai_toggle_defaults_roundtrips_through_json() {
        let mut s = GuiSettings::default();
        s.set_ai_toggle_defaults(true, true);
        let json = serde_json::to_string(&s).unwrap();
        let restored: GuiSettings = serde_json::from_str(&json).unwrap();
        assert!(restored.autocam_enabled);
        assert!(restored.async_detect_enabled);
    }

    #[test]
    fn scoreboard_settings_absent_until_set() {
        let s = GuiSettings::default();
        assert!(s.scoreboard_settings.is_none());
    }

    #[test]
    fn set_scoreboard_settings_roundtrips_through_json() {
        let mut s = GuiSettings::default();
        let sb = ScoreboardSettings {
            enabled: true,
            package_id: "football".into(),
            match_logger_path: None,
            sync_event_ts_ms: None,
            sync_video_seconds: 0.0,
            placement: reco_core::render::overlay::OverlayPlacement {
                offset: (0.0, 0.35),
                scale: 0.4,
            },
            home_logo_path: None,
            away_logo_path: None,
            font_family: "Georgia, serif".into(),
            logo_size_px: 40.0,
            banner_color_name: "Navy".into(),
            derive_cut_ranges: true,
            cut_lead_secs: 4.0,
            cut_trail_secs: 1.0,
            kickoff_lead_secs: 6.0,
            match_end_trail_secs: 10.0,
            highlight_lead_secs: 18.0,
            highlight_trail_secs: 8.0,
        };
        s.set_scoreboard_settings(sb);
        let json = serde_json::to_string(&s).unwrap();
        let restored: GuiSettings = serde_json::from_str(&json).unwrap();
        let restored_sb = restored.scoreboard_settings.expect("should roundtrip");
        assert!(restored_sb.enabled);
        assert_eq!(restored_sb.package_id, "football");
        assert!((restored_sb.placement.scale - 0.4).abs() < 1e-6);
        assert_eq!(restored_sb.banner_color_name, "Navy");
        assert!(restored_sb.derive_cut_ranges);
        assert!((restored_sb.cut_lead_secs - 4.0).abs() < 1e-6);
        assert!((restored_sb.cut_trail_secs - 1.0).abs() < 1e-6);
        assert!((restored_sb.kickoff_lead_secs - 6.0).abs() < 1e-6);
        assert!((restored_sb.match_end_trail_secs - 10.0).abs() < 1e-6);
        assert!((restored_sb.highlight_lead_secs - 18.0).abs() < 1e-6);
        assert!((restored_sb.highlight_trail_secs - 8.0).abs() < 1e-6);
    }

    #[test]
    fn set_pause_overlay_roundtrips_through_json() {
        let mut s = GuiSettings::default();
        // Defaults first: an untouched install must match main.slint's
        // own property defaults, not 0s (an instant, invisible cut).
        assert!(!s.pause_overlay_enabled);
        assert!((s.pause_overlay_fade_secs - 3.0).abs() < 1e-6);
        assert!((s.pause_overlay_hold_secs - 4.0).abs() < 1e-6);

        s.pause_overlay_enabled = true;
        s.pause_overlay_fade_secs = 1.0;
        s.pause_overlay_hold_secs = 2.5;
        let json = serde_json::to_string(&s).unwrap();
        let restored: GuiSettings = serde_json::from_str(&json).unwrap();
        assert!(restored.pause_overlay_enabled);
        assert!((restored.pause_overlay_fade_secs - 1.0).abs() < 1e-6);
        assert!((restored.pause_overlay_hold_secs - 2.5).abs() < 1e-6);
    }

    #[test]
    fn settings_json_predating_the_pause_overlay_fields_keeps_the_ui_defaults() {
        // Same #[serde(default = "...")] contract as the autocut margins
        // in reco-core: absent must mean 3.0/4.0, not 0.0.
        let json = r#"{"default_codec":"h264"}"#;
        let s: GuiSettings = serde_json::from_str(json).unwrap();
        assert!(!s.pause_overlay_enabled);
        assert!((s.pause_overlay_fade_secs - 3.0).abs() < 1e-6);
        assert!((s.pause_overlay_hold_secs - 4.0).abs() < 1e-6);
    }

    #[test]
    fn missing_ai_toggle_fields_default_to_false() {
        // Simulate loading an older settings JSON predating these
        // fields - #[serde(default)] must let it parse as false, not
        // error.
        let json = r#"{ "autocam_defaults": null }"#;
        let s: GuiSettings = serde_json::from_str(json).unwrap();
        assert!(!s.autocam_enabled);
        assert!(!s.async_detect_enabled);
    }

    #[test]
    fn set_autocam_defaults_roundtrips_through_json() {
        let mut s = GuiSettings::default();
        let ac = AutocamDefaults {
            tracking_mode: "field".into(),
            detection_interval: 3,
            player_anchor_rad: 0.35,
            ball_coast_secs: 1.5,
            lookahead_secs: 0.5,
            lookahead_reduced_bit_depth: false,
            preset: "action".into(),
            framing: "action".into(),
            lock_pitch: false,
            cluster_mode: "trimmed_mean".into(),
            cluster_bandwidth_rad: 0.3,
            dead_zone_rad: 0.08,
            ball_weight: 0.35,
            ball_max_dist_from_cluster: 1.0,
            fov_tight: 20.0,
            fov_wide: 65.0,
            fov_default: 34.0,
            fov_alpha: 0.06,
            cluster_alpha: 0.05,
            confidence_threshold: 0.2,
        };
        s.autocam_defaults = Some(ac);
        let json = serde_json::to_string(&s).unwrap();
        let restored: GuiSettings = serde_json::from_str(&json).unwrap();
        let restored_ac = restored.autocam_defaults.expect("should roundtrip");
        assert!((restored_ac.fov_alpha - 0.06).abs() < 1e-6);
        assert!((restored_ac.cluster_alpha - 0.05).abs() < 1e-6);
    }

    #[test]
    fn missing_autocam_defaults_field_falls_back_to_none() {
        // Older settings files (pre-persistence-feature) won't have this
        // field at all - must not fail to deserialize.
        let json = r#"{ "default_codec": "hevc" }"#;
        let s: GuiSettings = serde_json::from_str(json).unwrap();
        assert!(s.autocam_defaults.is_none());
    }
}
