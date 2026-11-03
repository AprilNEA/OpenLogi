use openlogi_core::binding::{ActionRingIcon, GridKey};
use openlogi_device_registry::keypad::{MX_CREATIVE_KEYPAD_GRID, PanelPoint};

use super::*;

const DISPLAY: KeyDisplay = KeyDisplay {
    key_size: 118,
    origin: PanelPoint { x: 23, y: 6 },
    pitch: 158,
    grid: MX_CREATIVE_KEYPAD_GRID,
};

fn keypad(product_id: u16) -> DeviceRoute {
    DeviceRoute::Direct {
        vendor_id: 0x046d,
        product_id,
    }
}

fn spec(route: DeviceRoute, generation: u64, copy_on: &[GridKey]) -> KeyDisplaySpec {
    KeyDisplaySpec {
        route,
        display: DISPLAY,
        generation,
        faces: GridKey::ALL
            .into_iter()
            .map(|key| {
                let face = if copy_on.contains(&key) {
                    KeyFace::Icon(ActionRingIcon::Copy)
                } else {
                    KeyFace::Blank
                };
                (key, face)
            })
            .collect(),
    }
}

fn keys(paints: &[Paint]) -> Vec<GridKey> {
    paints.iter().map(|paint| paint.key).collect()
}

/// A write of `spec`'s first key — the one a failing keypad fails on.
fn first_key(spec: &KeyDisplaySpec) -> Paint<'_> {
    let (key, face) = spec.faces.first_key_value().unwrap();
    Paint {
        spec,
        key: *key,
        face: *face,
    }
}

fn record_all(painted: &mut Painted, paints: &[Paint]) {
    for paint in paints {
        painted.record(paint);
    }
}

#[test]
fn a_new_panel_is_painted_in_full() {
    let mut painted = Painted::default();
    let specs = [spec(keypad(0xc354), 0, &[GridKey::Middle])];
    let plan = painted.plan(&specs, Instant::now());

    assert_eq!(keys(&plan), GridKey::ALL);
    let middle = plan
        .iter()
        .find(|paint| paint.key == GridKey::Middle)
        .unwrap();
    assert_eq!(middle.face, KeyFace::Icon(ActionRingIcon::Copy));
}

#[test]
fn only_keys_whose_face_changed_are_repainted() {
    let mut painted = Painted::default();
    let specs = [spec(keypad(0xc354), 0, &[GridKey::Middle])];
    let first = painted.plan(&specs, Instant::now());
    record_all(&mut painted, &first);

    assert!(painted.plan(&specs, Instant::now()).is_empty());
    let rebound = [spec(keypad(0xc354), 0, &[GridKey::TopLeft])];
    assert_eq!(
        keys(&painted.plan(&rebound, Instant::now())),
        [GridKey::TopLeft, GridKey::Middle]
    );
}

#[test]
fn an_unacknowledged_key_stays_pending() {
    let mut painted = Painted::default();
    let specs = [spec(keypad(0xc354), 0, &[])];
    let first = painted.plan(&specs, Instant::now());
    // Only the first key's write was acknowledged.
    painted.record(&first[0]);

    assert_eq!(
        keys(&painted.plan(&specs, Instant::now())),
        GridKey::ALL[1..]
    );
}

#[test]
fn a_moved_generation_repaints_the_whole_panel() {
    let mut painted = Painted::default();
    let specs = [spec(keypad(0xc354), 4, &[GridKey::Middle])];
    let first = painted.plan(&specs, Instant::now());
    record_all(&mut painted, &first);

    let after_reconnect = [spec(keypad(0xc354), 5, &[GridKey::Middle])];
    assert_eq!(
        keys(&painted.plan(&after_reconnect, Instant::now())),
        GridKey::ALL
    );
}

#[test]
fn a_panel_that_left_and_returned_is_repainted_in_full() {
    let mut painted = Painted::default();
    let specs = [spec(keypad(0xc354), 0, &[])];
    let first = painted.plan(&specs, Instant::now());
    record_all(&mut painted, &first);

    assert!(painted.plan(&[], Instant::now()).is_empty());
    assert_eq!(keys(&painted.plan(&specs, Instant::now())), GridKey::ALL);
}

#[test]
fn an_acknowledgement_from_before_a_reset_is_not_trusted() {
    let mut painted = Painted::default();
    let before = [spec(keypad(0xc354), 1, &[])];
    let stale = painted.plan(&before, Instant::now());
    // The panel reset (generation 2) while generation 1's writes were out.
    let after = [spec(keypad(0xc354), 2, &[])];
    let current = painted.plan(&after, Instant::now());
    painted.record(&current[0]);
    record_all(&mut painted, &stale);

    assert_eq!(
        keys(&painted.plan(&after, Instant::now())),
        GridKey::ALL[1..]
    );
}

#[test]
fn panels_are_tracked_independently() {
    let mut painted = Painted::default();
    let both = [
        spec(keypad(0xc354), 0, &[]),
        spec(keypad(0xc355), 0, &[GridKey::BottomRight]),
    ];
    let first = painted.plan(&both, Instant::now());
    assert_eq!(first.len(), 18);
    record_all(&mut painted, &first);

    let rebound = [
        spec(keypad(0xc354), 0, &[GridKey::TopRight]),
        spec(keypad(0xc355), 0, &[GridKey::BottomRight]),
    ];
    let changed = painted.plan(&rebound, Instant::now());
    assert_eq!(changed.len(), 1);
    assert_eq!(changed[0].spec.route, keypad(0xc354));
    assert_eq!(changed[0].key, GridKey::TopRight);
}

#[test]
fn identical_faces_render_once() {
    let mut tiles = Tiles::default();
    let face = KeyFace::Icon(ActionRingIcon::Copy);
    let first = tiles.get(face, 118).unwrap();
    let second = tiles.get(face, 118).unwrap();
    assert!(Arc::ptr_eq(&first, &second));
    assert!(!Arc::ptr_eq(
        &first,
        &tiles.get(KeyFace::Blank, 118).unwrap()
    ));
}

#[test]
fn a_keypad_that_keeps_failing_is_reported_once_until_it_recovers() {
    let mut painted = Painted::default();
    let now = Instant::now();
    let pad = spec(keypad(0xc354), 0, &[]);

    assert!(painted.failed(&first_key(&pad), now));
    assert!(!painted.failed(&first_key(&pad), now));
    // Another keypad's first failure is its own.
    assert!(painted.failed(&first_key(&spec(keypad(0xc355), 0, &[])), now));

    let specs = [pad.clone()];
    let plan = painted.plan(&specs, now + MAX_RETRY_DELAY);
    painted.record(&plan[0]);
    assert!(painted.failed(&first_key(&pad), now));
}

#[test]
fn a_keypad_that_leaves_and_returns_is_reported_afresh() {
    let mut painted = Painted::default();
    let now = Instant::now();
    let pad = spec(keypad(0xc354), 0, &[]);
    assert!(painted.failed(&first_key(&pad), now));

    painted.plan(std::slice::from_ref(&pad), now);
    assert!(!painted.failed(&first_key(&pad), now));

    painted.plan(&[], now);
    assert!(painted.failed(&first_key(&pad), now));
}

#[test]
fn nothing_failing_needs_no_retry() {
    assert_eq!(Painted::default().next_retry(), None);
}

#[test]
fn a_keypad_that_keeps_failing_is_retried_less_often_up_to_the_cap() {
    let mut painted = Painted::default();
    let now = Instant::now();
    let pad = spec(keypad(0xc354), 0, &[]);

    painted.failed(&first_key(&pad), now);
    assert_eq!(painted.next_retry(), Some(now + RETRY_DELAY));
    painted.failed(&first_key(&pad), now);
    assert_eq!(painted.next_retry(), Some(now + RETRY_DELAY * 2));
    for _ in 0..100 {
        painted.failed(&first_key(&pad), now);
    }
    assert_eq!(painted.next_retry(), Some(now + MAX_RETRY_DELAY));
}

#[test]
fn a_failing_keypad_is_not_painted_before_its_deadline() {
    let mut painted = Painted::default();
    let now = Instant::now();
    let specs = [spec(keypad(0xc354), 0, &[])];
    painted.failed(&first_key(&specs[0]), now);

    assert!(painted.plan(&specs, now).is_empty());
    // A respec (an app switch, a rebind) does not bring the retry forward.
    let rebound = [spec(keypad(0xc354), 0, &[GridKey::Middle])];
    assert!(painted.plan(&rebound, now).is_empty());

    assert_eq!(
        keys(&painted.plan(&rebound, now + RETRY_DELAY)),
        GridKey::ALL
    );
}

#[test]
fn each_failing_keypad_waits_out_its_own_deadline() {
    let mut painted = Painted::default();
    let now = Instant::now();
    let specs = [spec(keypad(0xc354), 0, &[]), spec(keypad(0xc355), 0, &[])];
    for _ in 0..4 {
        painted.failed(&first_key(&specs[0]), now);
    }
    painted.failed(&first_key(&specs[1]), now);

    // The fresh failure is due first, and is retried alone: the long-failing
    // keypad keeps its own, later deadline.
    let due = now + RETRY_DELAY;
    assert_eq!(painted.next_retry(), Some(due));
    let plan = painted.plan(&specs, due);
    assert!(plan.iter().all(|paint| paint.spec.route == keypad(0xc355)));
    assert_eq!(plan.len(), GridKey::ALL.len());
}

#[test]
fn a_reconnect_retries_a_failing_keypad_at_once() {
    let mut painted = Painted::default();
    let now = Instant::now();
    for _ in 0..4 {
        painted.failed(&first_key(&spec(keypad(0xc354), 0, &[])), now);
    }

    let after_reconnect = [spec(keypad(0xc354), 1, &[])];
    assert_eq!(keys(&painted.plan(&after_reconnect, now)), GridKey::ALL);
    assert_eq!(painted.next_retry(), None);
}

#[test]
fn an_acknowledged_write_ends_the_retries() {
    let mut painted = Painted::default();
    let now = Instant::now();
    let specs = [spec(keypad(0xc354), 0, &[])];
    painted.failed(&first_key(&specs[0]), now);

    let plan = painted.plan(&specs, now + RETRY_DELAY);
    painted.record(&plan[0]);

    assert_eq!(painted.next_retry(), None);
}

#[test]
fn a_key_whose_write_failed_is_repainted_even_when_its_old_face_returns() {
    let mut painted = Painted::default();
    let now = Instant::now();
    let copy = [spec(keypad(0xc354), 0, &[GridKey::Middle])];
    let plan = painted.plan(&copy, now);
    record_all(&mut painted, &plan);

    // A rebind whose write fails — perhaps only its acknowledgement was
    // lost, so the key may show the new face — then a return to the old one.
    let blank = [spec(keypad(0xc354), 0, &[])];
    let plan = painted.plan(&blank, now);
    assert_eq!(keys(&plan), [GridKey::Middle]);
    painted.failed(&plan[0], now);

    assert!(painted.plan(&copy, now).is_empty());
    assert_eq!(painted.next_retry(), Some(now + RETRY_DELAY));
    let retry = painted.plan(&copy, now + RETRY_DELAY);
    assert_eq!(keys(&retry), [GridKey::Middle]);
    assert_eq!(retry[0].face, KeyFace::Icon(ActionRingIcon::Copy));
    painted.record(&retry[0]);

    assert!(painted.plan(&copy, now + RETRY_DELAY).is_empty());
    assert_eq!(painted.next_retry(), None);
}
