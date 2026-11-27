use super::{RANGE, absolute_position};
use crate::{DesktopPoint, DisplayBounds, WarpError};

fn display(x: f64, y: f64, width: f64, height: f64) -> DisplayBounds {
    DisplayBounds {
        x,
        y,
        width,
        height,
    }
}

fn point(x: f64, y: f64) -> DesktopPoint {
    DesktopPoint { x, y }
}

/// A wide monitor with a smaller laptop panel lower and to its left, as KDE
/// laid them out in a real session.
fn stepped() -> [DisplayBounds; 2] {
    [
        display(2048.0, 0.0, 3440.0, 1440.0),
        display(0.0, 160.0, 2048.0, 1280.0),
    ]
}

#[test]
fn points_map_linearly_over_the_bounding_box() {
    // The bounding box is 5488 x 1440. These are the three targets a live
    // test moved the cursor to; KWin put it on each exactly.
    assert_eq!(
        absolute_position(point(1024.0, 800.0), &stepped()).expect("on the panel"),
        (12228, 36408)
    );
    assert_eq!(
        absolute_position(point(3768.0, 720.0), &stepped()).expect("on the monitor"),
        (44996, 32768)
    );
    assert_eq!(
        absolute_position(point(5400.0, 100.0), &stepped()).expect("near the right edge"),
        (64484, 4551)
    );
}

#[test]
fn the_corners_of_the_box_reach_the_ends_of_the_range() {
    let displays = [display(0.0, 0.0, 1920.0, 1080.0)];

    assert_eq!(
        absolute_position(point(0.0, 0.0), &displays).expect("top left"),
        (0, 0)
    );
    // The far edge is outside the half-open display, so the last point inside
    // is one pixel short of the end of the range: 65535 / 1920 = 34 units
    // across and 65535 / 1080 = 61 units down.
    let (x, y) = absolute_position(point(1919.0, 1079.0), &displays).expect("bottom right");
    assert!(x > RANGE - 35 && x < RANGE, "x was {x}");
    assert!(y > RANGE - 62 && y < RANGE, "y was {y}");
}

#[test]
fn an_origin_other_than_zero_is_subtracted() {
    let displays = [
        display(-1920.0, -200.0, 1920.0, 1080.0),
        display(0.0, 0.0, 2560.0, 1440.0),
    ];

    // The leftmost, topmost point of the layout is the range's origin.
    assert_eq!(
        absolute_position(point(-1920.0, -200.0), &displays).expect("the origin"),
        (0, 0)
    );
}

#[test]
fn a_point_outside_every_display_is_refused() {
    // The empty corner above the laptop panel lies inside the bounding box
    // but on no display.
    assert!(matches!(
        absolute_position(point(100.0, 50.0), &stepped()),
        Err(WarpError::OutsideDisplays)
    ));
    assert!(matches!(
        absolute_position(point(9000.0, 100.0), &stepped()),
        Err(WarpError::OutsideDisplays)
    ));
    assert!(matches!(
        absolute_position(point(10.0, 10.0), &[]),
        Err(WarpError::OutsideDisplays)
    ));
}
