use reco_minimap::{
    AgentOut, FrameState,
    ball::{BallOut, BallState},
    geom::Vec2,
    identity::{IdentityAssignment, IdentityStatus, PlayerKey, assign},
    team::Team,
    tracker::TrackState,
};

fn agent(id: u32, team: Team, number: Option<u8>) -> AgentOut {
    AgentOut {
        id,
        segment: 0,
        identity: IdentityAssignment::default(),
        referee: false,
        team,
        keeper: false,
        pos: Vec2::new(2.0, 3.0),
        vel: Vec2::default(),
        state: TrackState::Visible,
        alpha: 1.0,
        jersey: number,
    }
}

#[test]
fn same_number_on_opposing_teams_is_not_a_conflict() {
    let mut players = vec![agent(1, Team::Home, Some(7)), agent(2, Team::Away, Some(7))];
    assign(&mut players);
    assert!(
        players
            .iter()
            .all(|p| p.identity.status == IdentityStatus::Candidate)
    );
    assert_ne!(players[0].identity.player, players[1].identity.player);
}

#[test]
fn duplicate_claims_are_symmetric_and_not_displayed_as_identified() {
    let mut players = vec![agent(1, Team::Home, Some(7)), agent(2, Team::Home, Some(7))];
    assign(&mut players);
    for p in &players {
        assert_eq!(p.identity.status, IdentityStatus::Conflict);
        assert_eq!(
            p.identity.player,
            Some(PlayerKey {
                team: Team::Home,
                number: 7
            })
        );
        assert_eq!(p.jersey, None);
    }
}

#[test]
fn lost_tracks_and_unknown_teams_cannot_claim_an_identity() {
    let mut players = vec![
        agent(1, Team::Home, Some(7)),
        agent(2, Team::Home, Some(7)),
        agent(3, Team::Unknown, Some(7)),
    ];
    players[1].state = TrackState::Lost;
    assign(&mut players);
    assert_eq!(players[0].identity.status, IdentityStatus::Candidate);
    assert!(
        players[1..]
            .iter()
            .all(|p| p.identity == IdentityAssignment::default())
    );
}

#[test]
fn interpolating_a_reused_slot_does_not_join_two_people() {
    let frame = |t, a| FrameState {
        t,
        players: vec![a],
        referees: vec![],
        ball: BallOut {
            state: BallState::Unknown,
            pos: None,
            vel: None,
        },
        events: vec![],
    };
    let old = agent(1, Team::Home, Some(7));
    let mut new = agent(1, Team::Home, None);
    new.segment = 1;
    new.pos = Vec2::new(20.0, 10.0);
    let a = frame(0.0, old);
    let b = frame(1.0, new);
    let early = a.lerp(&b, 0.25).players[0];
    let late = a.lerp(&b, 0.75).players[0];
    assert_eq!(early.pos, old.pos);
    assert_eq!(late.pos, new.pos);
    assert_eq!(late.jersey, None);
    assert_eq!(late.segment, 1);
}
