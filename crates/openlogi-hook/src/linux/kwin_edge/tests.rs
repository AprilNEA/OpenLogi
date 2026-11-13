use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc;

use super::{KwinSide, Service, parse_displays, parse_point, parse_side};
use crate::{DesktopPoint, DisplayBounds};

fn display(x: f64, y: f64, width: f64, height: f64) -> DisplayBounds {
    DisplayBounds {
        x,
        y,
        width,
        height,
    }
}

#[test]
fn sides_parse_by_name() {
    assert_eq!(parse_side("left"), Some(KwinSide::Left));
    assert_eq!(parse_side("right"), Some(KwinSide::Right));
    assert_eq!(parse_side("top"), Some(KwinSide::Top));
    assert_eq!(parse_side("bottom"), Some(KwinSide::Bottom));
    assert_eq!(parse_side("Left"), None);
    assert_eq!(parse_side(""), None);
}

#[test]
fn points_need_two_finite_numbers() {
    assert_eq!(
        parse_point("0,760"),
        Some(DesktopPoint { x: 0.0, y: 760.0 })
    );
    assert_eq!(
        parse_point(" -3.5 , 12 "),
        Some(DesktopPoint { x: -3.5, y: 12.0 })
    );
    assert_eq!(parse_point("12"), None);
    assert_eq!(parse_point("a,b"), None);
    assert_eq!(parse_point("NaN,1"), None);
    assert_eq!(parse_point("inf,1"), None);
}

#[test]
fn displays_parse_and_skip_bad_entries() {
    assert_eq!(
        parse_displays("2048,0,3440,1440;0,160,2048,1280"),
        [
            display(2048.0, 0.0, 3440.0, 1440.0),
            display(0.0, 160.0, 2048.0, 1280.0)
        ]
    );
    // A short entry, a long one and a non-number are dropped; the rest stay.
    assert_eq!(
        parse_displays("1,2,3;0,0,10,10;1,2,3,4,5;x,0,1,1"),
        [display(0.0, 0.0, 10.0, 10.0)]
    );
    assert!(parse_displays("").is_empty());
}

fn service(
    queue: usize,
) -> (
    Service,
    mpsc::Receiver<super::KwinCrossing>,
    Arc<AtomicUsize>,
) {
    let (events, receiver) = mpsc::sync_channel(queue);
    let wakes = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&wakes);
    let service = Service {
        events,
        wake: Arc::new(move || {
            counter.fetch_add(1, Ordering::Relaxed);
        }),
    };
    (service, receiver, wakes)
}

#[test]
fn a_crossing_is_queued_and_wakes_the_owner() {
    let (service, receiver, wakes) = service(4);

    service.crossed("left", "0,760", "0,160,2048,1280");

    let crossing = receiver.try_recv().expect("a queued crossing");
    assert_eq!(crossing.side, KwinSide::Left);
    assert_eq!(crossing.position, DesktopPoint { x: 0.0, y: 760.0 });
    assert_eq!(crossing.displays, [display(0.0, 160.0, 2048.0, 1280.0)]);
    assert_eq!(wakes.load(Ordering::Relaxed), 1);
}

#[test]
fn a_malformed_call_is_dropped_without_waking() {
    let (service, receiver, wakes) = service(4);

    service.crossed("sideways", "0,760", "");
    service.crossed("left", "nope", "");

    assert_eq!(receiver.try_recv(), Err(mpsc::TryRecvError::Empty));
    assert_eq!(wakes.load(Ordering::Relaxed), 0);
}

#[test]
fn a_full_queue_drops_instead_of_blocking() {
    let (service, receiver, wakes) = service(1);

    service.crossed("left", "0,1", "");
    service.crossed("left", "0,2", "");

    assert_eq!(receiver.try_recv().expect("the first").position.y, 1.0);
    assert_eq!(receiver.try_recv(), Err(mpsc::TryRecvError::Empty));
    assert_eq!(wakes.load(Ordering::Relaxed), 1);
}
