use reco_minimap::Config;
use reco_minimap::sport::{
    BUILTIN_IDS, SportProfile,
    rules::{AnalysisRules, PossessionRules, SceneRules},
};

#[test]
fn builtin_policies_reach_the_processing_configuration() {
    for id in BUILTIN_IDS {
        let profile = SportProfile::builtin(id).unwrap();
        let policy = AnalysisRules::for_sport(id);
        let cfg = Config::for_sport(&profile, [255, 0, 0], [0, 0, 255]);
        assert_eq!(cfg.ball.possession, policy.possession, "{id}");
        assert_eq!(cfg.tracker.max_per_team, profile.team_size);
        if matches!(*id, "volleyball" | "custom") {
            assert_eq!(policy, AnalysisRules::NONE);
        }
        if matches!(*id, "football" | "futsal" | "hockey" | "lacrosse") {
            assert_eq!(policy.possession, PossessionRules::None);
        }
    }
}

#[test]
fn custom_geometry_can_explicitly_choose_interpretation() {
    let profile = SportProfile::custom_rect("training-hall", 26.0, 14.0, 3, 1);
    assert_eq!(AnalysisRules::for_sport(&profile.id), AnalysisRules::NONE);
    let rules = AnalysisRules {
        scenes: SceneRules::Basketball,
        possession: PossessionRules::HandControl {
            hidden_timeout_secs: 0.8,
        },
    };
    let cfg = Config::for_sport_with_rules(&profile, rules, [255, 0, 0], [0, 0, 255]);
    assert_eq!(cfg.ball.possession, rules.possession);
    assert_eq!(cfg.field.length, 26.0);
    assert_eq!(cfg.tracker.max_per_team, 3);
}
