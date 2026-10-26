use reco_minimap::{detect::Tile, scheduler::TileScheduler, tracker::BBox};

#[test]
fn independent_recovery_covers_unobserved_tiles_and_wraps() {
    let mut s = reco_minimap::scheduler::RecoverySweep::default();
    assert_eq!(s.select(5, 2), vec![0, 1]);
    assert_eq!(s.select(5, 2), vec![2, 3]);
    assert_eq!(s.select(5, 2), vec![4, 0]);
    assert_eq!(s.select(0, 2), Vec::<usize>::new());
    assert_eq!(s.select(5, 0), Vec::<usize>::new());
    assert_eq!(s.select(3, 9).len(), 3);
}

fn tiles() -> Vec<Tile> {
    (0..4)
        .map(|i| Tile {
            x: i * 100,
            y: 0,
            w: 100,
            h: 100,
        })
        .collect()
}
fn target() -> BBox {
    BBox {
        cx: 0.125,
        cy: 0.5,
        w: 0.05,
        h: 0.2,
    }
}

#[test]
fn full_search_then_subset_and_periodic_recovery() {
    let mut s = TileScheduler::new(2.0);
    let ts = tiles();
    assert_eq!(
        s.select(0.0, &ts, 400, 100, &[target()]),
        (vec![0, 1, 2, 3], true)
    );
    assert_eq!(s.select(0.5, &ts, 400, 100, &[target()]), (vec![0], false));
    assert_eq!(
        s.select(2.0, &ts, 400, 100, &[target()]),
        (vec![0, 1, 2, 3], true)
    );
    assert!(s.select(1.0, &ts, 400, 100, &[target()]).1);
}

#[test]
fn missing_or_invalid_evidence_falls_back() {
    let mut s = TileScheduler::new(2.0);
    let ts = tiles();
    s.select(0.0, &ts, 400, 100, &[target()]);
    assert!(s.select(0.1, &ts, 400, 100, &[]).1);
    assert!(
        s.select(
            0.2,
            &ts,
            400,
            100,
            &[BBox {
                cx: f32::NAN,
                ..target()
            }]
        )
        .1
    );
    assert!(
        s.select(
            0.3,
            &ts[..1],
            400,
            100,
            &[BBox {
                cx: 0.9,
                ..target()
            }]
        )
        .1
    );
    assert!(s.select(0.4, &[], 400, 100, &[target()]).1);
}

#[test]
fn covers_box_margin_across_tiles_in_original_order() {
    let mut s = TileScheduler::new(2.0);
    let ts = tiles();
    let b = BBox {
        cx: 0.25,
        ..target()
    };
    s.select(0.0, &ts, 400, 100, &[b]);
    assert_eq!(s.select(0.1, &ts, 400, 100, &[b]), (vec![0, 1], false));
}
