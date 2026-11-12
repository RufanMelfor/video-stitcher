//! Setup presets: shareable starting values that carry no camera data.
//!
//! A [`SetupPreset`] holds the parts of a setup that describe *how* a match
//! should be filmed and exported (AI tracking tuning, color grade,
//! sharpening, scoreboard timing, export defaults) and none of the parts
//! that describe *which rig* filmed it (lenses, topology, framing, sync
//! offset, field ROI) or *which machine* is used (model, logo and match
//! logger paths). Applying a preset to a [`Calibration`] therefore never
//! changes how the two cameras are stitched.
//!
//! [`football`] is the preset shipped with the program.

use serde::{Deserialize, Serialize};

use crate::calibration::{AutocamDefaults, Calibration, ExportDefaults, ScoreboardSettings};

/// Current preset document schema version.
const SCHEMA_VERSION: u32 = 1;

/// Image look values a preset may set. Every field is optional; unset
/// fields leave the calibration's value alone.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct PresetLook {
    /// Enable per-camera color matching on the seam band.
    #[serde(default)]
    pub color_match_enabled: Option<bool>,
    /// Final grade brightness (1.0 = unchanged).
    #[serde(default)]
    pub color_grade_brightness: Option<f32>,
    /// Final grade saturation (1.0 = unchanged).
    #[serde(default)]
    pub color_grade_saturation: Option<f32>,
    /// Final grade gamma (1.0 = unchanged).
    #[serde(default)]
    pub color_grade_gamma: Option<f32>,
    /// Sharpen amount (0.0 = off).
    #[serde(default)]
    pub sharpen_amount: Option<f32>,
    /// Sharpen radius.
    #[serde(default)]
    pub sharpen_radius: Option<f32>,
    /// Dehaze strength (0.0 = off).
    #[serde(default)]
    pub dehaze_strength: Option<f32>,
}

/// A named set of starting values. See the [module docs](self).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SetupPreset {
    /// Preset schema version.
    #[serde(default = "default_schema_version")]
    pub schema_version: u32,
    /// Short name, e.g. `Football`.
    pub name: String,
    /// One-sentence description shown to the user.
    #[serde(default)]
    pub description: String,
    /// Image look values.
    #[serde(default)]
    pub look: Option<PresetLook>,
    /// AI tracking / panner tuning.
    #[serde(default)]
    pub autocam_defaults: Option<AutocamDefaults>,
    /// Scoreboard style and timing (never paths or placement).
    #[serde(default)]
    pub scoreboard: Option<ScoreboardSettings>,
    /// Export resolution, codec and quality.
    #[serde(default)]
    pub export_defaults: Option<ExportDefaults>,
}

fn default_schema_version() -> u32 {
    SCHEMA_VERSION
}

/// Errors from loading a preset.
#[derive(Debug, thiserror::Error)]
pub enum PresetError {
    /// The JSON could not be parsed as a preset.
    #[error("invalid preset JSON: {0}")]
    Parse(#[from] serde_json::Error),
    /// The preset declares a schema version this build does not understand.
    #[error("preset schema version {found} is not supported (expected {SCHEMA_VERSION})")]
    UnsupportedSchemaVersion {
        /// The version the file declared.
        found: u32,
    },
}

impl SetupPreset {
    /// Parse a preset from JSON text.
    pub fn from_json(json: &str) -> Result<Self, PresetError> {
        let preset: Self = serde_json::from_str(json)?;
        if preset.schema_version != SCHEMA_VERSION {
            return Err(PresetError::UnsupportedSchemaVersion {
                found: preset.schema_version,
            });
        }
        Ok(preset)
    }

    /// Apply this preset to a calibration.
    ///
    /// Sections the calibration already has (`autocam_defaults`,
    /// `scoreboard`, `export_defaults`) are kept: a preset only fills what
    /// is missing, so it never overwrites choices the user made. The image
    /// look is written only when `apply_look` is true, because it lives in
    /// the topology next to values that came from calibration.
    ///
    /// Lenses, topology geometry, framing, sync offset and field ROI are
    /// never touched.
    pub fn apply_to(&self, cal: &mut Calibration, apply_look: bool) {
        if cal.autocam_defaults.is_none() {
            cal.autocam_defaults = self.autocam_defaults.clone();
        }
        if cal.scoreboard.is_none() {
            cal.scoreboard = self.scoreboard.clone();
        }
        if cal.export_defaults.is_none() {
            cal.export_defaults = self.export_defaults.clone();
        }
        if let (true, Some(look)) = (apply_look, self.look.as_ref()) {
            let t = &mut cal.topology;
            if let Some(v) = look.color_match_enabled {
                t.color_match_enabled = v;
            }
            if let Some(v) = look.color_grade_brightness {
                t.color_grade_brightness = v;
            }
            if let Some(v) = look.color_grade_saturation {
                t.color_grade_saturation = v;
            }
            if let Some(v) = look.color_grade_gamma {
                t.color_grade_gamma = v;
            }
            if let Some(v) = look.sharpen_amount {
                t.sharpen_amount = v;
            }
            if let Some(v) = look.sharpen_radius {
                t.sharpen_radius = v;
            }
            if let Some(v) = look.dehaze_strength {
                t.dehaze_strength = v;
            }
        }
    }
}

/// The football preset shipped with the program.
pub fn football() -> SetupPreset {
    SetupPreset::from_json(include_str!("../presets/football.json"))
        .expect("bundled football preset is valid (checked by a unit test)")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::calibration::tests::valid_cal;

    fn bare_calibration() -> Calibration {
        let mut cal = valid_cal();
        cal.sync_offset = 7;
        cal
    }

    #[test]
    fn bundled_football_preset_parses_and_has_no_personal_data() {
        let json = include_str!("../presets/football.json");
        let p = SetupPreset::from_json(json).unwrap();
        assert_eq!(p.name, "Football");
        let ac = p.autocam_defaults.as_ref().unwrap();
        assert!(ac.model_path.is_none());
        let sb = p.scoreboard.as_ref().unwrap();
        assert!(sb.match_logger_path.is_none());
        assert!(sb.home_logo_path.is_none() && sb.away_logo_path.is_none());
        assert!(sb.package_dir.is_none());
        assert!(!sb.enabled);
        for needle in ["D:\\", "C:\\", "\"lenses\"", "field_roi", "NEXTGEN", "Dean"] {
            assert!(!json.contains(needle), "preset contains {needle}");
        }
    }

    #[test]
    fn apply_never_touches_camera_data() {
        let mut cal = bare_calibration();
        let snapshot = |c: &Calibration| {
            serde_json::to_string(&(&c.lenses, &c.framing, c.sync_offset)).unwrap()
        };
        let before = snapshot(&cal);
        let seam = (
            cal.topology.intersect,
            cal.topology.seam_offset,
            cal.topology.ground_tilt_x,
        );
        football().apply_to(&mut cal, true);
        assert_eq!(snapshot(&cal), before);
        assert_eq!(
            (
                cal.topology.intersect,
                cal.topology.seam_offset,
                cal.topology.ground_tilt_x
            ),
            seam
        );
        assert!(cal.field_roi.is_none());
        assert!(cal.autocam_defaults.is_some());
        assert!(cal.scoreboard.is_some());
        assert!(cal.export_defaults.is_some());
        cal.validate().unwrap();
    }

    #[test]
    fn apply_keeps_existing_sections_and_skips_look_when_asked() {
        let mut cal = bare_calibration();
        cal.export_defaults = Some(ExportDefaults {
            width: 1920,
            height: 1080,
            codec: "h264".into(),
            quality: "high".into(),
        });
        let saturation = cal.topology.color_grade_saturation;
        football().apply_to(&mut cal, false);
        assert_eq!(cal.export_defaults.as_ref().unwrap().width, 1920);
        assert_eq!(cal.topology.color_grade_saturation, saturation);
    }

    #[test]
    fn apply_look_sets_grade_values() {
        let mut cal = bare_calibration();
        football().apply_to(&mut cal, true);
        assert!((cal.topology.color_grade_saturation - 1.15).abs() < 1e-6);
        assert_eq!(cal.topology.dehaze_strength, 0.0);
    }

    #[test]
    fn unsupported_schema_version_is_rejected() {
        let err = SetupPreset::from_json(r#"{"schema_version":9,"name":"x"}"#).unwrap_err();
        assert!(matches!(
            err,
            PresetError::UnsupportedSchemaVersion { found: 9 }
        ));
    }
}
