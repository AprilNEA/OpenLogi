use super::{DesktopPoint, DisplayBounds};

const DISPLAY: DisplayBounds = DisplayBounds {
    x: -1280.0,
    y: 0.0,
    width: 1280.0,
    height: 800.0,
};

fn point(x: f64, y: f64) -> DesktopPoint {
    DesktopPoint { x, y }
}

#[test]
fn the_left_and_top_edges_are_on_the_display() {
    assert!(DISPLAY.contains(point(-1280.0, 0.0)));
    assert!(DISPLAY.contains(point(-0.5, 799.5)));
}

#[test]
fn the_right_and_bottom_edges_belong_to_the_next_display() {
    // A display to the right starts at x = 0, so x = 0 must not land here as
    // well, or a warp to the shared edge would be ambiguous.
    assert!(!DISPLAY.contains(point(0.0, 400.0)));
    assert!(!DISPLAY.contains(point(-640.0, 800.0)));
}

#[test]
fn points_beyond_the_display_are_outside() {
    assert!(!DISPLAY.contains(point(-1280.5, 400.0)));
    assert!(!DISPLAY.contains(point(-640.0, -0.5)));
    assert!(!DISPLAY.contains(point(f64::NAN, 400.0)));
}
