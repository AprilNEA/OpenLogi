use gpui::TestAppContext;

use super::*;

#[test]
fn the_warm_up_window_is_never_shown() {
    let options = options();
    assert!(
        !options.show,
        "a warm-up window on screen would flash at start"
    );
    assert!(
        !options.focus,
        "a warm-up window must not take the keyboard"
    );
}

#[gpui::test]
fn every_warm_up_window_closes(cx: &mut TestAppContext) {
    cx.update(warm_up);
    cx.run_until_parked();
    for round in 0..ROUNDS {
        assert_eq!(
            cx.update(|cx| cx.windows().len()),
            1,
            "round {round} opens one window at a time"
        );
        cx.executor().advance_clock(LINGER);
        cx.run_until_parked();
    }
    assert!(
        cx.update(|cx| cx.windows().is_empty()),
        "a warm-up window left open would hold the display and its memory"
    );
}
