//! The published one-swipe-per-press choice, read the way a real press reads
//! it: through [`set_one_swipe_per_press`] and [`SwipeAccumulator::begin`].
//! The choice is process-wide, so this binary holds one test and nothing else
//! reads the choice while it runs.

#![expect(
    clippy::tests_outside_test_module,
    reason = "an integration test binary has no test module"
)]

use openlogi_core::binding::{
    GESTURE_REPEAT_DISTANCE, GESTURE_SWIPE_THRESHOLD, GestureDirection, SwipeAccumulator,
    set_one_swipe_per_press,
};

const SWIPE: i32 = GESTURE_SWIPE_THRESHOLD + 10;

/// Every swipe a hold commits while the hand sweeps right for three repeat
/// distances, reverses, rests, and swipes right again.
fn sweep(acc: &mut SwipeAccumulator) -> Vec<GestureDirection> {
    let mut fired = Vec::new();
    for _ in 0..(3 * GESTURE_REPEAT_DISTANCE / 10) {
        fired.extend(acc.accumulate(10, 0));
    }
    fired.extend(acc.accumulate(-SWIPE, 0));
    acc.rest_for_test();
    fired.extend(acc.accumulate(SWIPE, 0));
    fired
}

/// Press, swipe right, sweep, and release: every swipe the press commits.
fn press(acc: &mut SwipeAccumulator) -> Vec<GestureDirection> {
    acc.begin();
    acc.backdate_hold_for_test();
    let mut fired: Vec<_> = acc.accumulate(SWIPE, 0).into_iter().collect();
    fired.extend(sweep(acc));
    assert!(!acc.end(), "a press that swiped is not a click");
    fired
}

#[test]
fn published_swipe_policy_is_kept_for_each_press() {
    let mut acc = SwipeAccumulator::default();

    set_one_swipe_per_press(true);
    assert_eq!(
        press(&mut acc),
        [GestureDirection::Right],
        "on: one press is one swipe however far the hand goes"
    );

    set_one_swipe_per_press(false);
    let chained = press(&mut acc);
    assert!(
        chained.len() > 1,
        "off: the sweep chains repeats and reversals (#1652), got {chained:?}"
    );

    // A hold keeps the choice it began with; the next press takes the new one.
    acc.begin();
    acc.backdate_hold_for_test();
    set_one_swipe_per_press(true);
    assert_eq!(acc.accumulate(SWIPE, 0), Some(GestureDirection::Right));
    assert!(
        !sweep(&mut acc).is_empty(),
        "a hold begun with the choice off still chains"
    );
    acc.end();
    assert_eq!(press(&mut acc), [GestureDirection::Right]);

    acc.begin();
    acc.backdate_hold_for_test();
    set_one_swipe_per_press(false);
    assert_eq!(acc.accumulate(SWIPE, 0), Some(GestureDirection::Right));
    assert!(
        sweep(&mut acc).is_empty(),
        "a hold begun with the choice on swipes once"
    );
    acc.end();
    assert!(press(&mut acc).len() > 1);
}
