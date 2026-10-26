use reco_minimap::ball::BallState;
use reco_minimap::detect::*;
use reco_minimap::geom::*;
use reco_minimap::team::Team;
use reco_minimap::tracker::hungarian;
use reco_minimap::*;

const HOME: [u8; 3] = [220, 40, 40];
const AWAY: [u8; 3] = [40, 110, 230];

/// Synthetic planar camera: court metres -> normalized panorama (and back).
fn projector() -> (HomographyProjector, Homography) {
    let court = [
        Vec2::new(0.0, 0.0),
        Vec2::new(28.0, 0.0),
        Vec2::new(28.0, 15.0),
        Vec2::new(0.0, 15.0),
        Vec2::new(14.0, 7.5),
    ];
    let pano = [
        Vec2::new(0.05, 0.30),
        Vec2::new(0.95, 0.30),
        Vec2::new(0.99, 0.80),
        Vec2::new(0.01, 0.80),
        Vec2::new(0.50, 0.53936),
    ];
    let to_court = HomographyProjector::calibrate(&pano, &court).unwrap();
    let to_pano = Homography::from_points(&court, &pano).unwrap();
    (to_court, to_pano)
}

fn player_det(to_pano: &Homography, p: Vec2, class: Class, color: Option<[u8; 3]>) -> Det {
    let f = to_pano.apply(p).unwrap();
    let (w, h) = (0.012, 0.09);
    let mut d = Det::new(class, 0.9, f.x, f.y - h / 2.0, w, h);
    d.color = color;
    d
}

#[test]
fn homography_roundtrip() {
    let (p, _) = projector();
    let c = p.to_court(0.5, 0.53936).unwrap();
    assert!(
        (c.x - 14.0).abs() < 0.01 && (c.y - 7.5).abs() < 0.01,
        "{:?}",
        c
    );
    let inv = p.pano_to_court.inverse().unwrap();
    let back = inv.apply(c).unwrap();
    assert!((back.x - 0.5).abs() < 1e-3);
}

/// Missing ball evidence expires by video time, not by processing frame count.
#[test]
fn possession_policy_and_timeout_are_independent_of_sampling_rate() {
    use reco_minimap::sport::rules::PossessionRules;
    for dt in [0.04, 0.2] {
        for possession in [
            PossessionRules::None,
            PossessionRules::HandControl {
                hidden_timeout_secs: 1.5,
            },
        ] {
            let (proj, to_pano) = projector();
            let mut cfg = Config::new(HOME, AWAY);
            cfg.ball.possession = possession;
            let mut mm = Minimap::new(cfg, Box::new(proj));
            let (home, away, refs) = lineup();
            for k in 0..10 {
                let mut ds = frame_dets(&to_pano, &home, &away, &refs, &[]);
                let f = to_pano.apply(home[2]).unwrap();
                ds.push(Det::new(Class::Ball, 0.9, f.x, f.y - 0.05, 0.004, 0.007));
                let s = mm.process(k as f64 * dt, &ds);
                if k == 9 {
                    assert_eq!(
                        matches!(s.ball.state, BallState::Held { .. }),
                        possession != PossessionRules::None
                    );
                }
            }
            let last_seen = 9.0 * dt;
            // The player stays visible: only the ball evidence disappears.
            let ds = frame_dets(&to_pano, &home, &away, &refs, &[]);
            let s = mm.process(last_seen + 1.0, &ds);
            if possession != PossessionRules::None {
                assert!(matches!(s.ball.state, BallState::Held { .. }));
            }
            let s = mm.process(last_seen + 1.6, &ds);
            assert_eq!(s.ball.state, BallState::Unknown);
        }
    }
}

#[test]
fn decoder_maps_classes_and_filters() {
    let cfg = DecodeCfg::basketball_v1();
    // 3 queries: player (logit high), ignore (high), ball below threshold
    let boxes = [0.5, 0.5, 0.1, 0.2, 0.3, 0.3, 0.1, 0.1, 0.7, 0.7, 0.02, 0.02];
    let logits = [
        -5.0, 4.0, -3.0, -4.0, /* q1 */ -5.0, -5.0, -5.0, 3.0, /* q2 */ -2.0, -4.0, -4.0,
        -4.0,
    ];
    let d = decode_rfdetr(&boxes, &logits, &cfg).unwrap();
    assert_eq!(d.len(), 1);
    assert_eq!(d[0].class, Class::Player);
    assert!(d[0].conf > 0.97);
    assert!(decode_rfdetr(&boxes, &logits[..5], &cfg).is_err());
}

#[test]
fn tiling_covers_roi_and_maps_back() {
    let roi = Tile {
        x: 0,
        y: 600,
        w: 4096,
        h: 800,
    };
    let tiles = plan_tiles(roi, 512, 0.25);
    assert!(
        tiles
            .iter()
            .all(|t| t.x + t.w <= 4096 && t.y + t.h <= 1400 && t.y >= 600)
    );
    assert_eq!(tiles.iter().map(|t| t.x + t.w).max().unwrap(), 4096); // right edge covered
    let d = Det::new(Class::Ball, 0.9, 0.5, 0.5, 0.02, 0.02);
    let m = tile_to_pano(
        &d,
        &Tile {
            x: 1024,
            y: 700,
            w: 512,
            h: 512,
        },
        4096,
        1500,
    );
    assert!((m.cx - (1024.0 + 256.0) / 4096.0).abs() < 1e-6);
}

#[test]
fn nms_removes_tile_duplicates() {
    let a = Det::new(Class::Player, 0.9, 0.5, 0.5, 0.02, 0.1);
    let b = Det::new(Class::Player, 0.7, 0.502, 0.5, 0.02, 0.1);
    let c = Det::new(Class::Referee, 0.8, 0.5, 0.5, 0.02, 0.1);
    assert_eq!(nms(vec![a, b, c], 0.5).len(), 2);
}

#[test]
fn hungarian_is_optimal() {
    let c = vec![
        vec![4.0, 1.0, 3.0],
        vec![2.0, 0.0, 5.0],
        vec![3.0, 2.0, 2.0],
    ];
    let r = hungarian(&c);
    let total: f64 = r.iter().enumerate().map(|(i, j)| c[i][j.unwrap()]).sum();
    assert_eq!(total, 5.0);
    // more rows than cols
    let r = hungarian(&[vec![1.0], vec![0.5], vec![2.0]]);
    assert_eq!(r.iter().filter(|x| x.is_some()).count(), 1);
    assert_eq!(r[1], Some(0));
}

fn lineup() -> (Vec<Vec2>, Vec<Vec2>, Vec<Vec2>) {
    let home = (0..5)
        .map(|i| Vec2::new(8.0 + 2.0 * i as f32, 3.0 + 2.2 * i as f32))
        .collect();
    let away = (0..5)
        .map(|i| Vec2::new(9.0 + 2.0 * i as f32, 3.5 + 2.2 * i as f32))
        .collect();
    let refs = vec![Vec2::new(14.0, 0.5), Vec2::new(20.0, 14.5)];
    (home, away, refs)
}

fn frame_dets(
    to_pano: &Homography,
    home: &[Vec2],
    away: &[Vec2],
    refs: &[Vec2],
    hide: &[usize],
) -> Vec<Det> {
    let mut v = Vec::new();
    for (i, p) in home.iter().enumerate() {
        if !hide.contains(&i) {
            v.push(player_det(to_pano, *p, Class::Player, Some(HOME)));
        }
    }
    for p in away {
        v.push(player_det(to_pano, *p, Class::Player, Some(AWAY)));
    }
    for p in refs {
        v.push(player_det(to_pano, *p, Class::Referee, None));
    }
    v
}

#[test]
fn five_five_two_slots_are_stable() {
    let (proj, to_pano) = projector();
    let mut mm = Minimap::new(Config::new(HOME, AWAY), Box::new(proj));
    let (home, away, refs) = lineup();
    let mut last = None;
    for k in 0..50 {
        last = Some(mm.process(
            k as f64 * 0.04,
            &frame_dets(&to_pano, &home, &away, &refs, &[]),
        ));
    }
    let s = last.unwrap();
    assert_eq!(s.players.iter().filter(|p| p.team == Team::Home).count(), 5);
    assert_eq!(s.players.iter().filter(|p| p.team == Team::Away).count(), 5);
    assert_eq!(s.referees.len(), 2);
    // a 6th detection with home colour must NOT create a 6th dot
    let mut d = frame_dets(&to_pano, &home, &away, &refs, &[]);
    d.push(player_det(
        &to_pano,
        Vec2::new(3.0, 12.0),
        Class::Player,
        Some(HOME),
    ));
    let s = mm.process(2.1, &d);
    assert_eq!(s.players.iter().filter(|p| p.team == Team::Home).count(), 5);
}

#[test]
fn lost_player_coasts_plausibly_and_reacquires_without_teleport() {
    let (proj, to_pano) = projector();
    let mut mm = Minimap::new(Config::new(HOME, AWAY), Box::new(proj));
    let (mut home, away, refs) = lineup();
    let dt = 0.04f64;
    let mut t = 0.0;
    // player 0 runs +x at 1.5 m/s (stays on court for 12 s); others stand
    let mut speed_max = 0.0f32;
    let mut prev: Option<Vec2> = None;
    let mut track_id = None;
    for k in 0..300 {
        home[0].x = 8.0 + 1.5 * (k as f32 * dt as f32);
        let hide: &[usize] = if (75..175).contains(&k) { &[0] } else { &[] }; // 4 s occlusion
        let s = mm.process(t, &frame_dets(&to_pano, &home, &away, &refs, hide));
        t += dt;
        // find "our" player = home track nearest the true position at start; remember its id
        if k == 10 {
            track_id = s
                .players
                .iter()
                .filter(|p| p.team == Team::Home)
                .min_by(|a, b| {
                    a.pos
                        .dist(home[0])
                        .partial_cmp(&b.pos.dist(home[0]))
                        .unwrap()
                })
                .map(|p| p.id);
        }
        if let Some(id) = track_id {
            let p = s
                .players
                .iter()
                .find(|p| p.id == id)
                .expect("slot must persist");
            if let Some(q) = prev {
                speed_max = speed_max.max(p.pos.dist(q) / dt as f32);
            }
            prev = Some(p.pos);
            assert!(in_court(p.pos, 1.0), "left court: {:?}", p.pos);
            if k == 150 {
                assert!(p.state != reco_minimap::tracker::TrackState::Visible); // occluded now
                assert!(p.alpha < 1.0);
            }
        }
        // slot count never exceeds 5
        assert!(s.players.iter().filter(|p| p.team == Team::Home).count() <= 5);
    }
    assert!(
        speed_max <= 9.0 * 1.35,
        "display speed too high: {speed_max}"
    );
    // after reacquire the same id follows the true position again
    let s = mm.process(t, &frame_dets(&to_pano, &home, &away, &refs, &[]));
    let p = s.players.iter().find(|p| Some(p.id) == track_id).unwrap();
    assert!(
        p.pos.dist(home[0]) < 3.0,
        "did not reconverge: {:?} vs {:?}",
        p.pos,
        home[0]
    );
}

#[test]
fn ball_held_then_thrown_then_loose() {
    let (proj, to_pano) = projector();
    let mut mm = Minimap::new(Config::new(HOME, AWAY), Box::new(proj));
    let (home, away, refs) = lineup();
    let mut t = 0.0;
    let mut state = None;
    let ball_at_hands = |p: Vec2| {
        let f = to_pano.apply(p).unwrap();
        Det::new(Class::Ball, 0.8, f.x, f.y - 0.05, 0.004, 0.007)
    };
    // 1 s: ball in home[2]'s hands
    for _ in 0..25 {
        let mut d = frame_dets(&to_pano, &home, &away, &refs, &[]);
        d.push(ball_at_hands(home[2]));
        state = Some(mm.process(t, &d));
        t += 0.04;
    }
    let s = state.take().unwrap();
    let BallState::Held { track } = s.ball.state else {
        panic!("expected Held, got {:?}", s.ball.state)
    };
    let holder = s.players.iter().find(|p| p.id == track).unwrap();
    assert!(holder.pos.dist(home[2]) < 1.0);
    // ball hidden in hands for 1 s: still held
    for _ in 0..25 {
        state = Some(mm.process(t, &frame_dets(&to_pano, &home, &away, &refs, &[])));
        t += 0.04;
    }
    assert!(matches!(
        state.take().unwrap().ball.state,
        BallState::Held { .. }
    ));
    // thrown: flies away fast in the air above everyone
    let mut flying = false;
    for k in 0..12 {
        let x = 12.0 + 1.0 * k as f32;
        let f = to_pano.apply(Vec2::new(x, 5.0)).unwrap();
        let mut d = frame_dets(&to_pano, &home, &away, &refs, &[]);
        d.push(Det::new(Class::Ball, 0.8, f.x, f.y - 0.22, 0.004, 0.007));
        let s = mm.process(t, &d);
        t += 0.04;
        if s.ball.state == BallState::InFlight {
            flying = true;
        }
    }
    assert!(flying, "throw not detected");
}

#[test]
fn renders_and_exports_ppm() {
    let (proj, to_pano) = projector();
    let mut mm = Minimap::new(Config::new(HOME, AWAY), Box::new(proj));
    let (home, away, refs) = lineup();
    let mut s = None;
    for k in 0..10 {
        let mut d = frame_dets(&to_pano, &home, &away, &refs, &[]);
        let f = to_pano.apply(home[1]).unwrap();
        d.push(Det::new(Class::Ball, 0.8, f.x, f.y - 0.05, 0.004, 0.007));
        s = Some(mm.process(k as f64 * 0.04, &d));
    }
    let r = render::Renderer::new(render::Theme::default());
    let c = r.render(&s.unwrap());
    assert_eq!((c.w, c.h), r.size());
    let ppm = c.write_ppm();
    assert!(ppm.len() > c.w * c.h * 3);
    std::fs::write(std::env::temp_dir().join("minimap_test.ppm"), ppm).unwrap();
}

#[test]
fn letterboxed_tile_maps_back() {
    // 400x200 tile letterboxed into a square: content spans y in [0.25, 0.75] of the input
    let t = Tile {
        x: 100,
        y: 50,
        w: 400,
        h: 200,
    };
    let d = Det::new(Class::Player, 0.9, 0.5, 0.5, 0.25, 0.25); // centre of the input
    let m = tile_to_pano(&d, &t, 1000, 500);
    assert!(
        (m.cx - 0.3).abs() < 1e-5 && (m.cy - 0.3).abs() < 1e-5,
        "{m:?}"
    ); // tile centre (300,150)
    assert!((m.w - 0.1).abs() < 1e-5, "{m:?}"); // 0.25*400px/1000
    assert!((m.h - 0.2).abs() < 1e-5, "{m:?}"); // 0.25 of input = 100 source px / 500
}

#[test]
fn black_unmatched_players_become_referees() {
    let (proj, to_pano) = projector();
    let mut mm = Minimap::new(Config::new(HOME, AWAY), Box::new(proj));
    let (home, away, refs) = lineup();
    let mut s = None;
    for k in 0..10 {
        let mut d = frame_dets(&to_pano, &home, &away, &[], &[]);
        // the model labels referees as "player"; torso colour is black
        for r in &refs {
            d.push(player_det(&to_pano, *r, Class::Player, Some([12, 12, 14])));
        }
        s = Some(mm.process(k as f64 * 0.04, &d));
    }
    let s = s.unwrap();
    assert_eq!(s.referees.len(), 2, "referees from colour");
    assert_eq!(s.players.len(), 10);
}

#[test]
fn far_false_positive_ball_does_not_steal_tracking() {
    let (proj, to_pano) = projector();
    let mut mm = Minimap::new(Config::new(HOME, AWAY), Box::new(proj));
    let (home, away, refs) = lineup();
    let mut t = 0.0;
    let mut last = None;
    for k in 0..20 {
        let mut d = frame_dets(&to_pano, &home, &away, &refs, &[]);
        let f = to_pano.apply(Vec2::new(3.0, 12.0)).unwrap(); // true loose ball, low confidence
        d.push(Det::new(Class::Ball, 0.45, f.x, f.y - 0.004, 0.004, 0.007));
        if k >= 10 {
            let g = to_pano.apply(Vec2::new(24.0, 2.0)).unwrap(); // stronger false positive far away
            d.push(Det::new(Class::Ball, 0.8, g.x, g.y - 0.004, 0.004, 0.007));
        }
        last = Some(mm.process(t, &d));
        t += 0.04;
    }
    let p = last.unwrap().ball.pos.expect("ball position");
    assert!(p.dist(Vec2::new(3.0, 12.0)) < 2.0, "ball jumped to {p:?}");
}

#[test]
fn black_referee_next_to_dark_navy_team_needs_ref_color() {
    let navy = [47u8, 53, 78];
    let (proj, to_pano) = projector();
    let mut cfg = Config::new(HOME, navy);
    cfg.referee_rgb = Some([10, 10, 12]);
    let mut mm = Minimap::new(cfg, Box::new(proj));
    let (home, away, refs) = lineup();
    let mut s = None;
    for k in 0..10 {
        let mut d = Vec::new();
        for p in &home {
            d.push(player_det(&to_pano, *p, Class::Player, Some(HOME)));
        }
        for p in &away {
            d.push(player_det(&to_pano, *p, Class::Player, Some(navy)));
        }
        for r in &refs {
            d.push(player_det(&to_pano, *r, Class::Player, Some([14, 14, 16]))); // labelled "player" by the model
        }
        s = Some(mm.process(k as f64 * 0.04, &d));
    }
    let s = s.unwrap();
    assert_eq!(s.referees.len(), 2);
    assert_eq!(s.players.iter().filter(|p| p.team == Team::Away).count(), 5);
}

#[test]
fn ball_held_is_detected_at_low_processing_rate() {
    // only every 8th video frame is processed (0.32 s apart)
    let (proj, to_pano) = projector();
    let mut mm = Minimap::new(Config::new(HOME, AWAY), Box::new(proj));
    let (home, away, refs) = lineup();
    let mut s = None;
    for k in 0..6 {
        let mut d = frame_dets(&to_pano, &home, &away, &refs, &[]);
        let f = to_pano.apply(home[2]).unwrap();
        d.push(Det::new(Class::Ball, 0.7, f.x, f.y - 0.05, 0.004, 0.007));
        s = Some(mm.process(k as f64 * 0.32, &d));
    }
    assert!(matches!(s.unwrap().ball.state, BallState::Held { .. }));
}

#[test]
fn frame_state_interpolation() {
    use reco_minimap::ball::BallOut;
    use reco_minimap::tracker::TrackState;
    let ag = |id: u32, x: f32, a: f32| AgentOut {
        segment: 0,
        keeper: false,
        identity: Default::default(),
        id,
        referee: false,
        team: Team::Home,
        pos: Vec2::new(x, 5.0),
        vel: Vec2::new(1.0, 0.0),
        state: TrackState::Visible,
        alpha: a,
        jersey: None,
    };
    let s0 = FrameState {
        t: 1.0,
        players: vec![ag(1, 2.0, 1.0), ag(2, 10.0, 1.0)],
        referees: vec![],
        ball: BallOut {
            state: BallState::Loose,
            pos: Some(Vec2::new(0.0, 0.0)),
            vel: None,
        },
        events: vec![],
    };
    let s1 = FrameState {
        t: 2.0,
        players: vec![ag(1, 4.0, 0.4), ag(3, 7.0, 1.0)],
        referees: vec![],
        ball: BallOut {
            state: BallState::InFlight,
            pos: Some(Vec2::new(4.0, 2.0)),
            vel: Some(Vec2::new(5.0, 0.0)),
        },
        events: vec![],
    };
    let m = s0.lerp(&s1, 0.5);
    assert!((m.t - 1.5).abs() < 1e-9);
    let p1 = m.players.iter().find(|p| p.id == 1).unwrap();
    assert!((p1.pos.x - 3.0).abs() < 1e-5 && (p1.alpha - 0.7).abs() < 1e-5);
    assert!(m.players.iter().any(|p| p.id == 3), "new agent appears");
    assert!(
        !m.players.iter().any(|p| p.id == 2),
        "vanished agent dropped"
    );
    assert_eq!(m.ball.pos.unwrap(), Vec2::new(2.0, 1.0));
    assert_eq!(s0.lerp(&s1, 0.0).ball.state, BallState::Loose);
    assert_eq!(s0.lerp(&s1, 1.0).ball.state, BallState::InFlight);
}

#[test]
fn extrapolation_moves_with_velocity_and_stays_in_court() {
    use reco_minimap::ball::BallOut;
    use reco_minimap::tracker::TrackState;
    let ag = |id: u32, x: f32, vx: f32| AgentOut {
        segment: 0,
        keeper: false,
        identity: Default::default(),
        id,
        referee: false,
        team: Team::Home,
        pos: Vec2::new(x, 7.0),
        vel: Vec2::new(vx, 0.0),
        state: TrackState::Visible,
        alpha: 1.0,
        jersey: None,
    };
    let s = FrameState {
        t: 10.0,
        players: vec![ag(1, 5.0, 4.0), ag(2, 27.5, 8.0)],
        referees: vec![],
        ball: BallOut {
            state: BallState::Loose,
            pos: Some(Vec2::new(5.0, 5.0)),
            vel: Some(Vec2::new(2.0, 0.0)),
        },
        events: vec![],
    };
    let e = s.extrapolate(0.4);
    let p1 = e.players[0].pos;
    // damped displacement: tau*(1-exp(-dt/tau))*v = 0.8*(1-exp(-0.5))*4
    assert!(
        (p1.x - (5.0 + 0.8 * (1.0 - (-0.5f32).exp()) * 4.0)).abs() < 1e-4,
        "{p1:?}"
    );
    assert!(
        e.players[1].pos.x <= 29.0 + 1e-4,
        "must stay in the court margin"
    );
    assert!(e.ball.pos.unwrap().x > 5.0);
    assert!((e.t - 10.4).abs() < 1e-6);
    let still = s.extrapolate(0.0);
    assert_eq!(still.players[0].pos, s.players[0].pos);
    assert!(
        s.extrapolate(99.0).players[0].pos.x < 5.0 + 0.8 * 4.0 + 1e-3,
        "horizon is capped and velocity decays"
    );
}

#[test]
fn sport_profiles_are_consistent() {
    use reco_minimap::sport::{BUILTIN_IDS, SportProfile};
    for id in BUILTIN_IDS {
        let p = SportProfile::builtin(id).unwrap_or_else(|| panic!("{id}"));
        let (l, w) = (p.field.length, p.field.width);
        assert!(l > w && w > 1.0, "{id}");
        for pl in &p.lines {
            assert!(pl.len() >= 2, "{id} polyline");
            for q in pl {
                assert!(
                    q.x >= -0.01 && q.x <= l + 0.01 && q.y >= -0.01 && q.y <= w + 0.01,
                    "{id}: line point {q:?} outside {l}x{w}"
                );
            }
        }
        let mut keys = std::collections::HashSet::new();
        for m in &p.landmarks {
            assert!(
                m.x >= 0.0 && m.x <= l && m.y >= 0.0 && m.y <= w,
                "{id}: landmark {} outside",
                m.key
            );
            assert!(
                keys.insert(m.key.clone()),
                "{id}: duplicate landmark key {}",
                m.key
            );
        }
        assert!(p.landmarks.len() >= 7 && p.team_size >= 1, "{id}");
        assert!(p.densified_lines(1.0).iter().all(|pl| pl.len() >= 2));
    }
    // spot-check known standard dimensions
    let f = SportProfile::football();
    assert_eq!(
        (f.field.length, f.field.width, f.team_size),
        (105.0, 68.0, 11)
    );
    assert!(f.landmarks.iter().any(|m| m.key == "z1_l_far_inner"
        && (m.x - 16.5).abs() < 1e-4
        && (m.y - 13.84).abs() < 1e-4));
    assert_eq!(
        SportProfile::basketball().field,
        reco_minimap::geom::Field::basketball()
    );
    assert!(SportProfile::builtin("curling").is_none());
}

// ---- jersey numbers: CTC decoding

/// Build a CTC output (blank + digits per step) that spells `digits` with some blank steps and noise.
fn ctc_probs(digits: &[u8], strength: f32) -> (Vec<f32>, usize) {
    use reco_minimap::jersey::DIGIT_CLASSES;
    let mut steps: Vec<usize> = vec![0];
    for (i, d) in digits.iter().enumerate() {
        if i > 0 && digits[i - 1] == *d {
            steps.push(0); // repeated digits need a blank in between
        }
        steps.push(*d as usize + 1);
        steps.push(*d as usize + 1);
        steps.push(0);
    }
    let t = steps.len();
    let mut p = vec![0f32; t * DIGIT_CLASSES];
    for (s, c) in steps.iter().enumerate() {
        for k in 0..DIGIT_CLASSES {
            p[s * DIGIT_CLASSES + k] = (1.0 - strength) / (DIGIT_CLASSES as f32 - 1.0);
        }
        p[s * DIGIT_CLASSES + c] = strength;
    }
    (p, t)
}

#[test]
fn ctc_greedy_and_roster_decoding() {
    use reco_minimap::jersey::*;
    for n in [7u8, 10, 23, 11, 99, 0, 5] {
        let digits: Vec<u8> = n.to_string().bytes().map(|b| b - b'0').collect();
        let (p, t) = ctc_probs(&digits, 0.9);
        let (s, c) = ctc_greedy(&p, t);
        assert_eq!(parse_number(&s), Some(n), "greedy {n}: {s}");
        assert!(c > 0.85);
        let roster = [4u8, 5, 7, 10, 11, 23, 0, 99, 17];
        let (r, post) = ctc_roster(&p, t, &roster).unwrap();
        assert_eq!(r, n);
        assert!(post > 0.9, "posterior {post}");
    }
    // a confusable pair: with a roster the uncertain reading is pulled to the number that exists
    let (mut p, t) = ctc_probs(&[1, 7], 0.55);
    for s in 0..t {
        // make the second digit a coin flip between 7 and 1
        let base = s * reco_minimap::jersey::DIGIT_CLASSES;
        if p[base + 8] > 0.5 {
            p[base + 8] = 0.40;
            p[base + 2] = 0.40;
        }
    }
    let (r, _) = ctc_roster(&p, t, &[17u8, 4, 5]).unwrap();
    assert_eq!(r, 17);
    assert_eq!(parse_number("007"), None);
    assert_eq!(parse_number("07"), Some(7));
    assert_eq!(parse_number(""), None);
    assert_eq!(parse_roster("4, 5;7  10,10 x 23"), vec![4, 5, 7, 10, 23]);
    assert!(ctc_roster(&p, t, &[]).is_none());
}

// ---- substitutions via numbers

fn digits_probs(n: u8) -> (Vec<f32>, usize) {
    let d: Vec<u8> = n.to_string().bytes().map(|b| b - b'0').collect();
    ctc_probs(&d, 0.92)
}

fn home_tracks_at(mm: &Minimap, x: f32) -> Option<u32> {
    mm.tracker()
        .tracks()
        .iter()
        .filter(|t| t.team == Team::Home)
        .min_by(|a, b| {
            a.pos
                .dist(Vec2::new(x, 0.0))
                .partial_cmp(&b.pos.dist(Vec2::new(x, 0.0)))
                .unwrap()
        })
        .map(|t| t.id)
}

#[test]
fn number_changes_and_reentries_are_reported_as_substitutions() {
    use reco_minimap::SubKind;
    let (proj, to_pano) = projector();
    let mut cfg = Config::new(HOME, AWAY);
    cfg.roster_home = vec![4, 5, 7, 9, 10, 12, 13, 15];
    let mut mm = Minimap::new(cfg, Box::new(proj));
    let (home, away, refs) = lineup();
    let mut t = 0.0;
    let mut events = Vec::new();
    // 8 s of play, player 0 (home) is read as #7 several times
    for k in 0..80 {
        let st = mm.process(t, &frame_dets(&to_pano, &home, &away, &refs, &[]));
        events.extend(st.events);
        if k % 4 == 0 {
            let id = mm
                .tracker()
                .tracks()
                .iter()
                .filter(|x| x.team == Team::Home)
                .min_by(|a, b| {
                    a.pos
                        .dist(home[0])
                        .partial_cmp(&b.pos.dist(home[0]))
                        .unwrap()
                })
                .unwrap()
                .id;
            let (p, n) = digits_probs(7);
            mm.add_number_probs(id, &p, n);
        }
        t += 0.1;
    }
    let slot = mm
        .tracker()
        .tracks()
        .iter()
        .filter(|x| x.team == Team::Home)
        .min_by(|a, b| {
            a.pos
                .dist(home[0])
                .partial_cmp(&b.pos.dist(home[0]))
                .unwrap()
        })
        .unwrap()
        .id;
    let st = mm.process(t, &frame_dets(&to_pano, &home, &away, &refs, &[]));
    let shown = st.players.iter().find(|p| p.id == slot).unwrap().jersey;
    assert_eq!(shown, Some(7), "number confirmed after repeated reads");
    assert!(
        events.is_empty(),
        "a first confirmation is not a substitution: {events:?}"
    );

    // the player walks to the sideline, then off the court (out of the field) for 4 s ...
    for k in 1..=15 {
        let mut walk = home.clone();
        walk[0] = Vec2::new(home[0].x, home[0].y - 0.18 * k as f32);
        events.extend(
            mm.process(t, &frame_dets(&to_pano, &walk, &away, &refs, &[]))
                .events,
        );
        t += 0.1;
    }
    let mut off = home.clone();
    off[0] = Vec2::new(home[0].x, -5.0);
    for _ in 0..40 {
        let st = mm.process(t, &frame_dets(&to_pano, &off, &away, &refs, &[0]));
        events.extend(st.events);
        t += 0.1;
    }
    // ... and somebody enters at the same sideline
    let mut incoming = home.clone();
    incoming[0] = Vec2::new(home[0].x + 1.0, 0.4);
    for _ in 0..10 {
        let st = mm.process(t, &frame_dets(&to_pano, &incoming, &away, &refs, &[]));
        events.extend(st.events);
        t += 0.1;
    }
    let possible: Vec<_> = events
        .iter()
        .filter(|e| e.kind == SubKind::Possible)
        .collect();
    assert_eq!(possible.len(), 1, "one possible substitution: {events:?}");
    assert_eq!(possible[0].out_number, Some(7));

    // Re-acquisition must not silently identify the incoming person as #7.
    let st = mm.process(t, &frame_dets(&to_pano, &incoming, &away, &refs, &[]));
    let occupant = st.players.iter().find(|p| p.id == slot).unwrap();
    assert_eq!(occupant.segment, 1);
    assert_eq!(occupant.jersey, None);
    assert_eq!(
        occupant.identity.status,
        reco_minimap::identity::IdentityStatus::Unknown
    );

    // evidence for #12 arrives -> confirmed substitution 7 -> 12 on the same slot
    for k in 0..30 {
        let st = mm.process(t, &frame_dets(&to_pano, &incoming, &away, &refs, &[]));
        events.extend(st.events);
        if k % 3 == 0 {
            let id = home_tracks_at(&mm, 0.0).unwrap();
            let id = mm
                .tracker()
                .tracks()
                .iter()
                .filter(|x| x.team == Team::Home)
                .min_by(|a, b| {
                    a.pos
                        .dist(incoming[0])
                        .partial_cmp(&b.pos.dist(incoming[0]))
                        .unwrap()
                })
                .map(|x| x.id)
                .unwrap_or(id);
            let (p, n) = digits_probs(12);
            mm.add_number_probs(id, &p, n);
        }
        t += 0.1;
    }
    let confirmed: Vec<_> = events
        .iter()
        .filter(|e| e.kind == SubKind::Confirmed)
        .collect();
    assert_eq!(confirmed.len(), 1, "{events:?}");
    assert_eq!(
        (confirmed[0].out_number, confirmed[0].in_number),
        (Some(7), Some(12))
    );
    assert_eq!(mm.events_log().len(), events.len());
}

#[test]
fn same_number_after_leaving_is_not_a_confirmed_substitution() {
    use reco_minimap::SubKind;
    let (proj, to_pano) = projector();
    let mut cfg = Config::new(HOME, AWAY);
    cfg.roster_home = vec![7, 12];
    let mut mm = Minimap::new(cfg, Box::new(proj));
    let (home, away, refs) = lineup();
    let mut t = 0.0;
    let id_of = |mm: &Minimap, p: Vec2| {
        mm.tracker()
            .tracks()
            .iter()
            .filter(|x| x.team == Team::Home)
            .min_by(|a, b| a.pos.dist(p).partial_cmp(&b.pos.dist(p)).unwrap())
            .unwrap()
            .id
    };
    for k in 0..60 {
        mm.process(t, &frame_dets(&to_pano, &home, &away, &refs, &[]));
        if k % 3 == 0 {
            let (p, n) = digits_probs(7);
            let id = id_of(&mm, home[0]);
            mm.add_number_probs(id, &p, n);
        }
        t += 0.1;
    }
    let mut events = Vec::new();
    for k in 1..=15 {
        let mut walk = home.clone();
        walk[0] = Vec2::new(home[0].x, home[0].y - 0.18 * k as f32);
        events.extend(
            mm.process(t, &frame_dets(&to_pano, &walk, &away, &refs, &[]))
                .events,
        );
        t += 0.1;
    }
    let mut off = home.clone();
    off[0] = Vec2::new(home[0].x, -5.0);
    for _ in 0..40 {
        events.extend(
            mm.process(t, &frame_dets(&to_pano, &off, &away, &refs, &[0]))
                .events,
        );
        t += 0.1;
    }
    let mut back = home.clone();
    back[0] = Vec2::new(home[0].x + 1.0, 0.4);
    for k in 0..40 {
        events.extend(
            mm.process(t, &frame_dets(&to_pano, &back, &away, &refs, &[]))
                .events,
        );
        if k % 3 == 0 {
            let (p, n) = digits_probs(7);
            let id = id_of(&mm, back[0]);
            mm.add_number_probs(id, &p, n);
        }
        t += 0.1;
    }
    assert!(
        events.iter().any(|e| e.kind == SubKind::Possible),
        "a leave/re-enter at the sideline is flagged as possible: {events:?}"
    );
    assert!(
        events.iter().all(|e| e.kind == SubKind::Possible),
        "same number must never be confirmed as a swap: {events:?}"
    );
    let slot = id_of(&mm, back[0]);
    assert_eq!(
        mm.tracker()
            .tracks()
            .iter()
            .find(|x| x.id == slot)
            .unwrap()
            .confirmed,
        Some(7)
    );
}

#[test]
fn merge_tiles_drops_boxes_cut_by_a_tile_edge() {
    use reco_minimap::detect::merge_tiles;
    let (pw, ph) = (2048u32, 512u32);
    let (left, right) = (
        Tile {
            x: 0,
            y: 0,
            w: 512,
            h: 512,
        },
        Tile {
            x: 384,
            y: 0,
            w: 512,
            h: 512,
        },
    );
    // a player 40 px wide and 100 px tall at pano x 490..530: the left tile ends at 512 and sees only 22 px of him
    let cut = Det::new(
        Class::Player,
        0.9,
        501.0 / 512.0,
        0.5,
        22.0 / 512.0,
        100.0 / 512.0,
    );
    let whole = Det::new(
        Class::Player,
        0.7,
        (510.0 - 384.0) / 512.0,
        0.5,
        40.0 / 512.0,
        100.0 / 512.0,
    );
    let out = merge_tiles(&[(left, vec![cut]), (right, vec![whole])], pw, ph, 0.5);
    assert_eq!(out.len(), 1, "{out:?}");
    assert!(
        (out[0].w * pw as f32 - 40.0).abs() < 0.5,
        "the complete box wins although it is less confident"
    );
    assert!(
        (out[0].conf - 0.9).abs() < 1e-6,
        "and carries the confidence of the clearer view"
    );

    // nobody saw him whole: the cut box is kept
    assert_eq!(
        merge_tiles(&[(left, vec![cut]), (right, vec![])], pw, ph, 0.5).len(),
        1
    );

    // a box at the panorama border is not "cut": the left tile's left edge is the image edge
    let at_border = Det::new(
        Class::Player,
        0.9,
        10.0 / 512.0,
        0.5,
        20.0 / 512.0,
        100.0 / 512.0,
    );
    let inner = Det::new(
        Class::Player,
        0.8,
        30.0 / 512.0,
        0.5,
        60.0 / 512.0,
        110.0 / 512.0,
    );
    assert_eq!(
        merge_tiles(&[(left, vec![at_border, inner])], pw, ph, 0.5).len(),
        2
    );

    // the same person as player and as referee: one detection, the more confident class
    let p = Det::new(Class::Player, 0.8, 0.5, 0.5, 0.08, 0.2);
    let r = Det::new(Class::Referee, 0.6, 0.501, 0.5, 0.08, 0.2);
    let out = merge_tiles(&[(left, vec![p, r])], pw, ph, 0.5);
    assert_eq!(out.len(), 1);
    assert_eq!(out[0].class, Class::Player);
    // a ball in front of a player is untouched
    let b = Det::new(Class::Ball, 0.5, 0.5, 0.5, 0.02, 0.02);
    assert_eq!(merge_tiles(&[(left, vec![p, b])], pw, ph, 0.5).len(), 2);
}

#[test]
fn resting_ball_beside_the_court_is_parked_and_the_game_ball_takes_over() {
    let (proj, to_pano) = projector();
    let mut mm = Minimap::new(Config::new(HOME, AWAY), Box::new(proj));
    let (home, away, refs) = lineup();
    let ball = |p: Vec2, conf: f32| {
        let f = to_pano.apply(p).unwrap();
        Det::new(Class::Ball, conf, f.x, f.y - 0.004, 0.004, 0.007)
    };
    let spare = Vec2::new(14.0, 17.4); // 2.4 m outside the near sideline: inside the ball margin, beyond the clamp
    let (mut t, dt) = (0.0, 1.0 / 6.0);
    let mut flight = 0;
    let mut s = None;
    for _ in 0..36 {
        let mut d = frame_dets(&to_pano, &home, &away, &refs, &[]);
        d.push(ball(spare, 0.6));
        let st = mm.process(t, &d);
        flight += (st.ball.state == BallState::InFlight) as u32;
        s = Some(st);
        t += dt;
    }
    assert_eq!(flight, 0, "a resting ball must never read as flying");
    assert_eq!(
        s.unwrap().ball.state,
        BallState::Unknown,
        "after 3 s at rest it is parked"
    );

    // the game ball appears elsewhere with modest confidence while the spare one is still detected
    let mut s = None;
    for k in 0..6 {
        let mut d = frame_dets(&to_pano, &home, &away, &refs, &[]);
        d.push(ball(spare, 0.6));
        d.push(ball(Vec2::new(20.0 - k as f32 * 0.3, 3.0), 0.45));
        s = Some(mm.process(t, &d));
        t += dt;
    }
    let p = s.unwrap().ball.pos.expect("game ball tracked");
    assert!(p.dist(Vec2::new(18.5, 3.0)) < 1.5, "ball at {p:?}");
}

/// True if a visible player of `team` is shown within 0.5 m of `p`.
fn shown(s: &FrameState, team: Team, p: Vec2) -> bool {
    s.players
        .iter()
        .any(|a| a.team == team && a.state == tracker::TrackState::Visible && a.pos.dist(p) < 0.5)
}

#[test]
fn one_wrong_colour_sample_does_not_move_the_other_teams_player() {
    let (proj, to_pano) = projector();
    let mut mm = Minimap::new(Config::new(HOME, AWAY), Box::new(proj));
    let (home, mut away, refs) = lineup();
    away[0] = home[0] + Vec2::new(1.6, 0.0); // a defender 1.6 m away: inside the normal matching gate
    let mut t = 0.0;
    for _ in 0..25 {
        mm.process(t, &frame_dets(&to_pano, &home, &away, &refs, &[]));
        t += 0.04;
    }
    // one frame: the defender is hidden and the home player's torso sample comes out in the away colour
    let mut d: Vec<Det> = frame_dets(&to_pano, &home, &away[1..], &refs, &[0]);
    d.push(player_det(&to_pano, home[0], Class::Player, Some(AWAY)));
    let s = mm.process(t, &d);
    assert!(
        shown(&s, Team::Home, home[0]),
        "the home player keeps his own detection"
    );
    let a = mm
        .tracker()
        .tracks()
        .iter()
        .find(|x| x.team == Team::Away && x.filter_pos().dist(away[0]) < 0.3);
    assert!(
        a.is_some(),
        "the hidden defender must not be pulled onto the home player"
    );
    assert_eq!(mm.tracker().team_stats.cross_matches, 1);
    assert_eq!(mm.tracker().team_stats.flips, 0);
}

#[test]
fn track_started_in_the_wrong_team_follows_the_colour_evidence() {
    let (proj, to_pano) = projector();
    let mut mm = Minimap::new(Config::new(HOME, AWAY), Box::new(proj));
    let (home, away, refs) = lineup();
    let mut t = 0.0;
    // first two frames: home[0] is sampled in the away colour (shadow, another shirt in the crop); away[4] not seen yet
    for _ in 0..2 {
        let mut d = frame_dets(&to_pano, &home, &away[..4], &refs, &[0]);
        d.push(player_det(&to_pano, home[0], Class::Player, Some(AWAY)));
        mm.process(t, &d);
        t += 0.04;
    }
    let id = mm
        .tracker()
        .tracks()
        .iter()
        .find(|x| x.filter_pos().dist(home[0]) < 0.3)
        .map(|x| (x.id, x.team))
        .unwrap();
    assert_eq!(id.1, Team::Away);
    let mut s = None;
    for _ in 0..10 {
        s = Some(mm.process(t, &frame_dets(&to_pano, &home, &away, &refs, &[])));
        t += 0.04;
    }
    let s = s.unwrap();
    for p in &home {
        assert!(
            shown(&s, Team::Home, *p),
            "home player at {p:?} missing: {:?}",
            s.players
        );
    }
    for p in &away {
        assert!(
            shown(&s, Team::Away, *p),
            "away player at {p:?} missing: {:?}",
            s.players
        );
    }
    assert_eq!(s.players.len(), 10, "no ghost slot left behind");
    let same = mm.tracker().tracks().iter().find(|x| x.id == id.0).unwrap();
    assert_eq!(
        same.team,
        Team::Home,
        "the same track changed team instead of a new one being made"
    );
}

#[test]
fn hidden_players_are_not_possible_substitutions() {
    let (proj, to_pano) = projector();
    let mut mm = Minimap::new(Config::new(HOME, AWAY), Box::new(proj));
    let (mut home, mut away, refs) = lineup();
    home[0] = Vec2::new(27.4, 7.2); // under the basket: 0.6 m from the baseline, far from both sidelines
    home[1] = Vec2::new(25.8, 1.0); // at the sideline
    away[0] = Vec2::new(20.0, 14.2); // at the other sideline
    let mut t = 0.0;
    let mut events = Vec::new();
    let mut run = |mm: &mut Minimap, hide_home: &[usize], away: &[Vec2], n: usize, t: &mut f64| {
        for _ in 0..n {
            events.extend(
                mm.process(*t, &frame_dets(&to_pano, &home, away, &refs, hide_home))
                    .events,
            );
            *t += 0.1;
        }
    };
    run(&mut mm, &[], &away, 20, &mut t);
    // hidden under the basket for 4 s and found again in the same place
    run(&mut mm, &[0], &away, 40, &mut t);
    run(&mut mm, &[], &away, 5, &mut t);
    // standing at the sideline, missed by the detector for 2.5 s: too short for someone to leave and another to enter
    run(&mut mm, &[1], &away, 25, &mut t);
    run(&mut mm, &[], &away, 5, &mut t);
    // the detector loses three players at once for 4 s, one of them at a sideline
    run(&mut mm, &[2, 3], &away[1..], 40, &mut t);
    run(&mut mm, &[], &away, 5, &mut t);
    assert!(events.is_empty(), "{events:?}");
}

#[test]
fn weak_detections_keep_a_track_but_never_start_one() {
    use reco_minimap::tracker::{BBox, Obs, TrackState, Tracker, TrackerCfg};
    let cfg = TrackerCfg {
        spawn_conf: 0.6,
        ..TrackerCfg::default()
    };
    let mut tr = Tracker::new(cfg);
    let obs = |p: Vec2, conf: f32| Obs {
        referee: false,
        team: Team::Home,
        keeper: false,
        pos: p,
        bbox: BBox::default(),
        conf,
    };
    let (a, ghost) = (Vec2::new(10.0, 7.0), Vec2::new(20.0, 3.0));
    let mut t = 0.0;
    // a confident start, then only weak detections of the same player; a weak false positive elsewhere all along
    for k in 0..60 {
        let p = a + Vec2::new(0.05 * k as f32, 0.0);
        tr.update(
            t,
            &[obs(p, if k < 3 { 0.8 } else { 0.35 }), obs(ghost, 0.45)],
        );
        t += 0.1;
    }
    assert_eq!(
        tr.tracks().len(),
        1,
        "the weak false positive never became a player"
    );
    let x = &tr.tracks()[0];
    assert_eq!(x.state, TrackState::Visible);
    assert!(
        x.filter_pos().dist(a + Vec2::new(0.05 * 59.0, 0.0)) < 0.5,
        "followed on weak detections: {:?}",
        x.filter_pos()
    );

    // once lost, a weak detection does not bring the slot back; a confident one does
    for _ in 0..30 {
        tr.update(t, &[]);
        t += 0.1;
    }
    assert_eq!(tr.tracks()[0].state, TrackState::Lost);
    tr.update(t, &[obs(Vec2::new(13.0, 7.0), 0.4)]);
    assert_eq!(tr.tracks()[0].state, TrackState::Lost);
    tr.update(t + 0.1, &[obs(Vec2::new(13.0, 7.0), 0.7)]);
    assert_eq!(tr.tracks()[0].state, TrackState::Visible);
}

#[test]
fn correcting_ocr_on_a_continuous_track_is_not_a_substitution() {
    let (proj, to_pano) = projector();
    let mut mm = Minimap::new(Config::new(HOME, AWAY), Box::new(proj));
    let (home, away, refs) = lineup();
    let ds = frame_dets(&to_pano, &home, &away, &refs, &[]);
    for k in 0..120 {
        let s = mm.process(k as f64 * 0.1, &ds);
        assert!(
            s.events.is_empty(),
            "OCR corrections alone must not emit substitutions"
        );
        let id = mm
            .tracker()
            .tracks()
            .iter()
            .find(|p| p.team == Team::Home)
            .unwrap()
            .id;
        mm.add_jersey_reading(id, if k < 20 { 7 } else { 12 }, 0.95);
    }
    let track = mm
        .tracker()
        .tracks()
        .iter()
        .find(|p| p.team == Team::Home)
        .unwrap();
    assert_eq!(track.segment, 0);
    assert_eq!(track.confirmed, Some(12));
}
