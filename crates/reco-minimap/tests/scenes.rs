use reco_minimap::ball::{BallOut, BallState};
use reco_minimap::geom::Vec2;
use reco_minimap::scenes::{SceneDetector, SceneEvent, SceneKind};
use reco_minimap::sport::SportProfile;
use reco_minimap::team::Team;
use reco_minimap::tracker::TrackState;
use reco_minimap::{AgentOut, FrameState};

struct P {
    id: u32,
    team: Team,
    x: f32,
    y: f32,
    speed: f32,
}

fn p(id: u32, team: Team, x: f32, y: f32) -> P {
    P {
        id,
        team,
        x,
        y,
        speed: 0.1,
    }
}

fn frame(t: f64, ps: &[P], ball: BallOut) -> FrameState {
    FrameState {
        t,
        players: ps
            .iter()
            .map(|q| AgentOut {
                segment: 0,
                keeper: false,
                identity: Default::default(),
                id: q.id,
                referee: false,
                team: q.team,
                pos: Vec2::new(q.x, q.y),
                vel: Vec2::new(q.speed, 0.0),
                state: TrackState::Visible,
                alpha: 1.0,
                jersey: None,
            })
            .collect(),
        referees: vec![],
        ball,
        events: vec![],
    }
}

fn no_ball() -> BallOut {
    BallOut {
        state: BallState::Unknown,
        pos: None,
        vel: None,
    }
}

#[test]
fn unsupported_sports_do_not_inherit_throw_ins() {
    // This boundary formation triggers basketball's inbound rule, but must not
    // invent an inbound for volleyball or an unknown custom sport.
    let ps = vec![
        p(1, H, 10.0, 0.3),
        p(2, H, 8.0, 3.0),
        p(3, A, 12.0, 3.0),
        p(4, A, 10.0, 5.0),
    ];
    for id in ["volleyball", "american_football", "custom"] {
        let mut det = SceneDetector::new(SportProfile::builtin(id).unwrap());
        assert!(hold(&mut det, 0.0, 4.0, &ps, no_ball()).is_empty(), "{id}");
    }
    let mut profile = SportProfile::basketball();
    profile.id = "local-hall".into();
    let mut det = SceneDetector::new(profile.clone());
    assert!(hold(&mut det, 0.0, 4.0, &ps, no_ball()).is_empty());
    let mut det =
        SceneDetector::with_rules(profile, reco_minimap::sport::rules::SceneRules::Basketball);
    assert!(
        hold(&mut det, 0.0, 4.0, &ps, no_ball())
            .iter()
            .any(|e| e.kind == SceneKind::ThrowIn)
    );
}

/// Feed the same formation for `secs` seconds (5 frames per second) and collect what fires.
fn hold(det: &mut SceneDetector, t0: f64, secs: f64, ps: &[P], ball: BallOut) -> Vec<SceneEvent> {
    let mut ev = Vec::new();
    let n = (secs * 5.0) as usize;
    for k in 0..=n {
        ev.extend(det.update(&frame(t0 + k as f64 * 0.2, ps, ball)));
    }
    ev
}

use Team::{Away as A, Home as H};

fn filler(start_id: u32, n: usize, team: Team, x0: f32, y0: f32, dx: f32, dy: f32) -> Vec<P> {
    (0..n)
        .map(|i| {
            p(
                start_id + i as u32,
                team,
                x0 + dx * i as f32,
                y0 + dy * i as f32,
            )
        })
        .collect()
}

// ---------------------------------------------------------------- basketball

#[test]
fn free_throw_requires_both_edges_and_rejects_ball_contradictions() {
    let good = vec![
        p(1, H, 5.8, 7.5),
        p(2, A, 2.0, 5.0),
        p(3, H, 3.0, 5.0),
        p(4, A, 2.0, 10.0),
        p(5, H, 3.0, 10.0),
    ];
    let mut det = SceneDetector::new(SportProfile::basketball());
    let ev = hold(&mut det, 0.0, 3.0, &good, no_ball());
    let ft = ev.iter().find(|e| e.kind == SceneKind::FreeThrow).unwrap();
    assert!(ft.evidence.contains(&"rebounders_both_lane_edges"));
    assert_eq!(ft.started_at, 0.0);
    let mut one_side = good
        .iter()
        .map(|q| p(q.id, q.team, q.x, 5.0))
        .collect::<Vec<_>>();
    one_side[0] = p(1, H, 5.8, 7.5);
    let mut middle = good
        .iter()
        .map(|q| p(q.id, q.team, q.x, q.y))
        .collect::<Vec<_>>();
    middle.push(p(6, A, 3.5, 7.5));
    for ps in [&one_side, &middle] {
        let mut det = SceneDetector::new(SportProfile::basketball());
        assert!(
            !hold(&mut det, 0.0, 3.0, ps, no_ball())
                .iter()
                .any(|e| e.kind == SceneKind::FreeThrow)
        );
    }
    let mut det = SceneDetector::new(SportProfile::basketball());
    assert!(
        !hold(
            &mut det,
            0.0,
            3.0,
            &good,
            BallOut {
                state: BallState::Held { track: 2 },
                pos: None,
                vel: None
            }
        )
        .iter()
        .any(|e| e.kind == SceneKind::FreeThrow)
    );
    // Preparation followed by a release towards this basket is a separate attempt.
    let mut det = SceneDetector::new(SportProfile::basketball());
    hold(
        &mut det,
        0.0,
        3.0,
        &good,
        BallOut {
            state: BallState::Held { track: 1 },
            pos: Some(Vec2::new(5.8, 7.5)),
            vel: None,
        },
    );
    let ev = det.update(&frame(
        3.2,
        &good,
        BallOut {
            state: BallState::InFlight,
            pos: Some(Vec2::new(3.0, 7.5)),
            vel: None,
        },
    ));
    assert!(
        ev.iter().any(|e| e.kind == SceneKind::FreeThrow
            && e.evidence.contains(&"ball_release_towards_basket"))
    );
    assert!(!ev.iter().any(|e| e.kind == SceneKind::ShotTwo));
}

#[test]
fn inbound_links_recent_substitution_without_upgrading_it_and_seek_resets() {
    let ps = vec![
        p(1, H, 14.0, 0.0),
        p(2, A, 12.0, 3.0),
        p(3, H, 16.0, 4.0),
        p(4, A, 13.0, 5.0),
    ];
    let mut det = SceneDetector::new(SportProfile::basketball());
    let mut s = frame(0.0, &ps, no_ball());
    s.events.push(reco_minimap::SubEvent {
        t: 0.0,
        team: A,
        slot: 7,
        out_number: None,
        in_number: None,
        kind: reco_minimap::SubKind::Possible,
    });
    det.update(&s);
    let ev = hold(&mut det, 0.2, 2.0, &ps, no_ball());
    let ti = ev.iter().find(|e| e.kind == SceneKind::ThrowIn).unwrap();
    assert_eq!(ti.related_substitutions.len(), 1);
    assert!(matches!(
        ti.related_substitutions[0].kind,
        reco_minimap::SubKind::Possible
    ));
    let ev = hold(&mut det, 40.0, 2.0, &ps, no_ball());
    assert!(ev.iter().all(|e| e.related_substitutions.is_empty()));
}

#[test]
fn basketball_tip_off_free_throw_and_inbound() {
    let mut det = SceneDetector::new(SportProfile::basketball());
    // tip-off: two opponents in the centre circle, the other eight around it
    let mut ps = vec![p(1, H, 13.6, 7.5), p(2, A, 14.4, 7.5)];
    ps.extend(filler(3, 4, H, 10.5, 4.0, 0.0, 2.4));
    ps.extend(filler(7, 4, A, 17.5, 4.0, 0.0, 2.4));
    let ev = hold(&mut det, 0.0, 2.0, &ps, no_ball());
    assert_eq!(
        ev.iter().filter(|e| e.kind == SceneKind::TipOff).count(),
        1,
        "{ev:?}"
    );
    // the same formation does not fire again while it lasts
    assert!(hold(&mut det, 2.2, 3.0, &ps, no_ball()).is_empty());
    // a third player in the circle is a jump-ball scrum, not a tip-off
    let mut det2 = SceneDetector::new(SportProfile::basketball());
    let mut ps2 = ps
        .iter()
        .map(|q| p(q.id, q.team, q.x, q.y))
        .collect::<Vec<_>>();
    ps2.push(p(11, H, 14.0, 8.2));
    assert!(
        hold(&mut det2, 0.0, 3.0, &ps2, no_ball())
            .iter()
            .all(|e| e.kind != SceneKind::TipOff)
    );

    // free throw at the left basket: shooter on the line, lane players along the key, others waiting
    let mut det = SceneDetector::new(SportProfile::basketball());
    let mut ps = vec![
        p(1, H, 5.8, 7.5),
        p(2, H, 2.0, 5.4),
        p(3, A, 2.6, 5.4),
        p(4, A, 2.0, 9.6),
        p(5, H, 2.6, 9.6),
    ];
    ps.extend(filler(6, 5, A, 12.0, 3.0, 0.0, 2.2));
    let ev = hold(&mut det, 0.0, 3.0, &ps, no_ball());
    let ft: Vec<_> = ev
        .iter()
        .filter(|e| e.kind == SceneKind::FreeThrow)
        .collect();
    assert_eq!(ft.len(), 1, "{ev:?}");
    assert_eq!((ft[0].side, ft[0].team), (Some('l'), Some(H)));
    // same people, but in a running game (fast): no free throw
    let mut det = SceneDetector::new(SportProfile::basketball());
    let running: Vec<P> = ps
        .iter()
        .map(|q| P {
            id: q.id,
            team: q.team,
            x: q.x,
            y: q.y,
            speed: 4.0,
        })
        .collect();
    assert!(
        hold(&mut det, 0.0, 3.0, &running, no_ball())
            .iter()
            .all(|e| e.kind != SceneKind::FreeThrow)
    );
    // a shooter on the line without anybody in the lane is not a free throw
    let mut det = SceneDetector::new(SportProfile::basketball());
    let lone = vec![
        p(1, H, 5.8, 7.5),
        p(2, A, 14.0, 4.0),
        p(3, A, 18.0, 9.0),
        p(4, H, 20.0, 7.5),
        p(5, H, 22.0, 3.0),
    ];
    assert!(
        hold(&mut det, 0.0, 3.0, &lone, no_ball())
            .iter()
            .all(|e| e.kind != SceneKind::FreeThrow)
    );

    // inbound at the sideline: one player alone at the line, the game gathered around him
    let mut det = SceneDetector::new(SportProfile::basketball());
    let mut ps = vec![p(1, A, 14.0, -0.2)];
    ps.extend(filler(2, 4, H, 11.0, 3.0, 1.5, 1.0));
    ps.extend(filler(6, 4, A, 13.0, 4.0, 1.5, 0.5));
    let ball = BallOut {
        state: BallState::Held { track: 1 },
        pos: Some(Vec2::new(14.0, 0.0)),
        vel: None,
    };
    let ev = hold(&mut det, 0.0, 2.0, &ps, ball);
    let ti: Vec<_> = ev.iter().filter(|e| e.kind == SceneKind::ThrowIn).collect();
    assert_eq!(ti.len(), 1, "{ev:?}");
    assert!(ti[0].confidence >= 0.75, "ball in hand raises confidence");
    // somebody standing right next to him (a defender guarding the passer) is not a quiet restart
    let mut det = SceneDetector::new(SportProfile::basketball());
    ps.push(p(20, H, 14.6, 0.6));
    assert!(
        hold(&mut det, 0.0, 2.0, &ps, no_ball())
            .iter()
            .all(|e| e.kind != SceneKind::ThrowIn)
    );
    let mut det = SceneDetector::new(SportProfile::basketball());
    assert!(
        hold(&mut det, 0.0, 2.0, &ps, ball)
            .iter()
            .any(|e| e.kind == SceneKind::ThrowIn)
    );
}

#[test]
fn basketball_shot_attempts_two_and_three() {
    let release = |x: f32, y: f32| {
        let mut det = SceneDetector::new(SportProfile::basketball());
        let ps = vec![
            p(1, H, x, y),
            p(2, A, x + 2.0, y),
            p(3, H, 8.0, 4.0),
            p(4, A, 9.0, 11.0),
            p(5, H, 12.0, 7.0),
        ];
        let mut ev = Vec::new();
        // ball held, then in flight towards the left basket (1.575, 7.5)
        for k in 0..4 {
            ev.extend(det.update(&frame(
                k as f64 * 0.2,
                &ps,
                BallOut {
                    state: BallState::Held { track: 1 },
                    pos: Some(Vec2::new(x, y)),
                    vel: None,
                },
            )));
        }
        let goal = Vec2::new(1.575, 7.5);
        for k in 0..6 {
            let f = (k + 1) as f32 / 6.0;
            let bp = Vec2::new(x, y).lerp(goal, f * 0.95);
            ev.extend(det.update(&frame(
                0.8 + k as f64 * 0.2,
                &ps,
                BallOut {
                    state: BallState::InFlight,
                    pos: Some(bp),
                    vel: Some(Vec2::new(-8.0, 0.0)),
                },
            )));
        }
        ev
    };
    let three = release(10.0, 7.5); // 8.4 m from the basket, well behind the arc
    assert_eq!(three.len(), 1, "{three:?}");
    assert_eq!(three[0].kind, SceneKind::ShotThree);
    assert_eq!(
        (three[0].team, three[0].side, three[0].borderline),
        (Some(H), Some('l'), false)
    );
    let two = release(4.0, 6.0); // about 2.8 m
    assert_eq!(two.len(), 1, "{two:?}");
    assert_eq!(two[0].kind, SceneKind::ShotTwo);
    let edge = release(8.2, 7.5); // 6.6 m: right at the line
    assert_eq!(edge.len(), 1);
    assert!(
        edge[0].borderline,
        "a release this close to the arc is flagged: {edge:?}"
    );
    // a pass sideways does not travel towards the basket -> no shot
    let mut det = SceneDetector::new(SportProfile::basketball());
    let ps = vec![
        p(1, H, 9.0, 7.5),
        p(2, H, 9.0, 12.0),
        p(3, A, 12.0, 4.0),
        p(4, A, 14.0, 9.0),
        p(5, H, 16.0, 7.0),
    ];
    let mut ev = Vec::new();
    for k in 0..4 {
        ev.extend(det.update(&frame(
            k as f64 * 0.2,
            &ps,
            BallOut {
                state: BallState::Held { track: 1 },
                pos: Some(Vec2::new(9.0, 7.5)),
                vel: None,
            },
        )));
    }
    for k in 0..6 {
        let bp = Vec2::new(9.0, 7.5).lerp(Vec2::new(9.0, 12.0), (k + 1) as f32 / 6.0);
        ev.extend(det.update(&frame(
            0.8 + k as f64 * 0.2,
            &ps,
            BallOut {
                state: BallState::InFlight,
                pos: Some(bp),
                vel: Some(Vec2::new(0.0, 8.0)),
            },
        )));
    }
    assert!(ev.is_empty(), "{ev:?}");
    // Shooting from this spot alone is not evidence of a free throw.
    let ft = release(5.8, 7.5);
    assert_eq!(ft.len(), 1);
    assert_eq!(ft[0].kind, SceneKind::ShotTwo);
}

// ---------------------------------------------------------------- football

fn football_halves() -> Vec<P> {
    let mut ps = vec![p(1, H, 52.5, 34.0), p(2, H, 50.0, 36.0)];
    ps.extend(filler(3, 9, H, 15.0, 8.0, 4.0, 5.0));
    ps.extend(filler(12, 11, A, 60.0, 6.0, 4.0, 5.0));
    ps
}

#[test]
fn football_kickoff_corner_penalty_and_throw_in() {
    let mut det = SceneDetector::new(SportProfile::football());
    let ev = hold(&mut det, 0.0, 2.5, &football_halves(), no_ball());
    let ko: Vec<_> = ev.iter().filter(|e| e.kind == SceneKind::Kickoff).collect();
    assert_eq!(ko.len(), 1, "{ev:?}");
    assert_eq!(ko[0].team, Some(H));
    // teams standing mixed across the pitch: not a kick-off
    let mut det = SceneDetector::new(SportProfile::football());
    let mixed: Vec<P> = football_halves()
        .into_iter()
        .enumerate()
        .map(|(i, q)| P {
            x: if i % 2 == 0 {
                20.0 + i as f32 * 3.0
            } else {
                80.0 - i as f32 * 3.0
            },
            ..q
        })
        .collect();
    assert!(
        hold(&mut det, 0.0, 3.0, &mixed, no_ball())
            .iter()
            .all(|e| e.kind != SceneKind::Kickoff)
    );

    // corner at the top-right: taker at the flag, six players in the right penalty area
    let mut det = SceneDetector::new(SportProfile::football());
    let mut ps = vec![p(1, H, 104.3, 0.4)];
    ps.extend(filler(2, 4, H, 96.0, 24.0, 2.0, 4.0));
    ps.extend(filler(6, 4, A, 97.0, 26.0, 2.0, 4.0));
    ps.extend(filler(10, 4, A, 60.0, 10.0, 6.0, 10.0));
    let ev = hold(&mut det, 0.0, 2.5, &ps, no_ball());
    let c: Vec<_> = ev.iter().filter(|e| e.kind == SceneKind::Corner).collect();
    assert_eq!(c.len(), 1, "{ev:?}");
    assert_eq!((c[0].side, c[0].team), (Some('r'), Some(H)));
    // only two players in the box: no corner
    let mut det = SceneDetector::new(SportProfile::football());
    let thin: Vec<P> = ps
        .iter()
        .filter(|q| q.id < 4 || q.id >= 10)
        .map(|q| P {
            id: q.id,
            team: q.team,
            x: q.x,
            y: q.y,
            speed: 0.1,
        })
        .collect();
    assert!(
        hold(&mut det, 0.0, 3.0, &thin, no_ball())
            .iter()
            .all(|e| e.kind != SceneKind::Corner)
    );

    // penalty at the right goal: kicker on the spot (94, 34), keeper on the line, everybody else outside the area
    let mut det = SceneDetector::new(SportProfile::football());
    let mut ps = vec![p(1, H, 93.2, 34.0), p(2, A, 104.5, 34.0)];
    ps.extend(filler(3, 8, H, 70.0, 8.0, 2.0, 6.0));
    ps.extend(filler(11, 8, A, 71.0, 10.0, 2.0, 5.0));
    let ev = hold(&mut det, 0.0, 3.0, &ps, no_ball());
    let pk: Vec<_> = ev.iter().filter(|e| e.kind == SceneKind::Penalty).collect();
    assert_eq!(pk.len(), 1, "{ev:?}");
    assert_eq!((pk[0].side, pk[0].team), (Some('r'), Some(H)));
    // one more player inside the area -> not a penalty
    let mut det = SceneDetector::new(SportProfile::football());
    ps.push(p(30, A, 95.0, 25.0));
    assert!(
        hold(&mut det, 0.0, 3.0, &ps, no_ball())
            .iter()
            .all(|e| e.kind != SceneKind::Penalty)
    );

    // throw-in on the touchline in the middle of the pitch; the same spot in a corner is left to the corner detector
    let mut det = SceneDetector::new(SportProfile::football());
    let mut ps = vec![p(1, A, 40.0, 0.2)];
    ps.extend(filler(2, 4, H, 36.0, 6.0, 3.0, 3.0));
    ps.extend(filler(6, 4, A, 38.0, 8.0, 3.0, 3.0));
    ps.extend(filler(10, 6, H, 80.0, 10.0, 2.0, 8.0));
    assert_eq!(
        hold(&mut det, 0.0, 2.0, &ps, no_ball())
            .iter()
            .filter(|e| e.kind == SceneKind::ThrowIn)
            .count(),
        1
    );
    let mut det = SceneDetector::new(SportProfile::football());
    let mut ps = vec![p(1, A, 1.0, 0.3)];
    ps.extend(filler(2, 6, H, 4.0, 6.0, 2.0, 5.0));
    ps.extend(filler(8, 6, A, 6.0, 8.0, 2.0, 4.0));
    assert!(
        hold(&mut det, 0.0, 2.0, &ps, no_ball())
            .iter()
            .all(|e| e.kind != SceneKind::ThrowIn)
    );
}

// ---------------------------------------------------------------- handball

#[test]
fn handball_seven_metre_and_throw_in() {
    let mut det = SceneDetector::new(SportProfile::handball());
    // left goal, 7 m mark at (7, 10): thrower there, keeper in goal, the rest behind the 9 m line
    let mut ps = vec![p(1, H, 6.8, 10.0), p(2, A, 0.6, 10.0)];
    ps.extend(filler(3, 5, H, 11.0, 3.0, 2.0, 3.0));
    ps.extend(filler(8, 5, A, 12.0, 4.0, 2.0, 3.0));
    let ev = hold(&mut det, 0.0, 3.0, &ps, no_ball());
    let sm: Vec<_> = ev
        .iter()
        .filter(|e| e.kind == SceneKind::SevenMeter)
        .collect();
    assert_eq!(sm.len(), 1, "{ev:?}");
    assert_eq!((sm[0].side, sm[0].team), (Some('l'), Some(H)));
    // a defender stepping up inside the 6 m area: not a 7 m throw
    let mut det = SceneDetector::new(SportProfile::handball());
    ps.push(p(30, A, 4.0, 6.0));
    assert!(
        hold(&mut det, 0.0, 3.0, &ps, no_ball())
            .iter()
            .all(|e| e.kind != SceneKind::SevenMeter)
    );
    // throw-in at the sideline
    let mut det = SceneDetector::new(SportProfile::handball());
    let mut ps = vec![p(1, H, 20.0, 20.1)];
    ps.extend(filler(2, 4, H, 16.0, 14.0, 2.0, 1.0));
    ps.extend(filler(6, 4, A, 17.0, 13.0, 2.0, 1.0));
    ps.extend(filler(10, 4, A, 30.0, 6.0, 1.0, 2.0));
    assert_eq!(
        hold(&mut det, 0.0, 2.0, &ps, no_ball())
            .iter()
            .filter(|e| e.kind == SceneKind::ThrowIn)
            .count(),
        1
    );
}

#[test]
fn a_formation_can_fire_again_after_it_broke_up_and_the_cooldown_passed() {
    let mut det = SceneDetector::new(SportProfile::football());
    let a = hold(&mut det, 0.0, 2.5, &football_halves(), no_ball());
    assert_eq!(a.iter().filter(|e| e.kind == SceneKind::Kickoff).count(), 1);
    // play: everybody moves
    let moving: Vec<P> = football_halves()
        .into_iter()
        .map(|q| P { speed: 5.0, ..q })
        .collect();
    hold(&mut det, 2.6, 3.0, &moving, no_ball());
    // second kick-off 10 s later (after a goal)
    let b = hold(&mut det, 12.0, 2.5, &football_halves(), no_ball());
    assert_eq!(
        b.iter().filter(|e| e.kind == SceneKind::Kickoff).count(),
        1,
        "{b:?}"
    );
}
