//! rig-calib's persisted settings.
//!
//! Remembers the last-used left/right video and calibration file so the
//! app reopens with them pre-filled, mirroring reco-gui's
//! `recent_*` fields (see `reco-gui/src/settings.rs`). Stored at
//! `<config>/reco/rig-calib.json` via `reco_io::settings`.

use std::path::PathBuf;

use reco_io::settings::RecentFiles;
use serde::{Deserialize, Serialize};

/// rig-calib's on-disk settings.
///
/// All fields carry `#[serde(default)]` so adding new fields later
/// doesn't invalidate existing settings files.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct RigCalibSettings {
    /// Most recently opened left camera videos, most-recent-first.
    #[serde(default)]
    pub recent_left: RecentFiles,
    /// Most recently opened right camera videos, most-recent-first.
    #[serde(default)]
    pub recent_right: RecentFiles,
    /// Most recently loaded calibration JSON files, most-recent-first.
    #[serde(default)]
    pub recent_calibration: RecentFiles,
}

/// Namespace under the reco config directory: `<config>/reco/rig-calib.json`.
pub const NAMESPACE: &str = "rig-calib";

impl RigCalibSettings {
    /// Load settings from disk, falling back to defaults on first run
    /// or a malformed file (never fatal - see `reco_io::settings::load_or_default`).
    pub fn load() -> Self {
        reco_io::settings::load_or_default::<RigCalibSettings>(NAMESPACE)
    }

    /// Persist settings. Errors are logged and swallowed - a failed
    /// save should never block the user from continuing to work.
    pub fn save(&self) {
        if let Err(e) = reco_io::settings::save(NAMESPACE, self) {
            log::warn!("failed to save rig-calib settings: {e}");
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

    /// Record a newly-picked left video and persist immediately.
    pub fn push_left(&mut self, path: PathBuf) {
        self.recent_left.push(path);
        self.save();
    }

    /// Record a newly-picked right video and persist immediately.
    pub fn push_right(&mut self, path: PathBuf) {
        self.recent_right.push(path);
        self.save();
    }

    /// Record a newly-loaded/saved calibration file and persist immediately.
    pub fn push_calibration(&mut self, path: PathBuf) {
        self.recent_calibration.push(path);
        self.save();
    }
}
