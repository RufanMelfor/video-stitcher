//! Interpretation policy, deliberately separate from field geometry and model classes.
//!
//! A rectangular field does not imply basketball possession or throw-in rules.
//! Unknown sports therefore opt out rather than silently borrowing another sport's rules.

/// Which implemented scene rules may interpret the tracked observations.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SceneRules {
    None,
    Basketball,
    Football,
    Futsal,
    Handball,
    Hockey,
    Rugby,
    Lacrosse,
}

/// Bounding-box overlap is only a candidate for hand possession, not a universal
/// definition of control. Foot/stick control and volleyball contacts need their
/// own evidence models; until those exist, leave individual possession unknown.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum PossessionRules {
    None,
    HandControl { hidden_timeout_secs: f32 },
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct AnalysisRules {
    pub scenes: SceneRules,
    pub possession: PossessionRules,
}

impl AnalysisRules {
    /// Conservative policy for custom sports and analyses not implemented yet.
    pub const NONE: Self = Self {
        scenes: SceneRules::None,
        possession: PossessionRules::None,
    };

    pub fn for_sport(id: &str) -> Self {
        use SceneRules::*;
        let scenes = match id {
            "basketball" => Basketball,
            "football" => Football,
            "futsal" => Futsal,
            "handball" => Handball,
            "hockey" => Hockey,
            "rugby" => Rugby,
            "lacrosse" => Lacrosse,
            _ => None,
        };
        let possession = match id {
            "basketball" | "handball" | "rugby" | "american_football" => {
                PossessionRules::HandControl {
                    hidden_timeout_secs: 1.5,
                }
            }
            _ => PossessionRules::None,
        };
        Self { scenes, possession }
    }
}
