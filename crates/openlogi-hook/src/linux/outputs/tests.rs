use super::Logical;
use crate::DisplayBounds;

#[test]
fn bounds_need_a_position_and_a_positive_size() {
    let both = Logical {
        position: Some((2048, 0)),
        size: Some((3440, 1440)),
    };
    assert_eq!(
        both.bounds(),
        Some(DisplayBounds {
            x: 2048.0,
            y: 0.0,
            width: 3440.0,
            height: 1440.0,
        })
    );
    assert_eq!(
        Logical {
            position: None,
            size: Some((10, 10)),
        }
        .bounds(),
        None
    );
    assert_eq!(
        Logical {
            position: Some((0, 0)),
            size: None,
        }
        .bounds(),
        None
    );
    assert_eq!(
        Logical {
            position: Some((0, 0)),
            size: Some((0, 10)),
        }
        .bounds(),
        None
    );
}

#[test]
fn a_negative_position_is_kept() {
    let left_of_origin = Logical {
        position: Some((-1920, -200)),
        size: Some((1920, 1080)),
    };

    assert_eq!(
        left_of_origin.bounds().map(|bounds| (bounds.x, bounds.y)),
        Some((-1920.0, -200.0))
    );
}
