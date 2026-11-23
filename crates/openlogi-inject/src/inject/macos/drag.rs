//! Trackpad gestures driven by a held button's drag: a two-finger swipe
//! (continuous, phased scroll with its gesture companion and momentum on
//! release) and a three-finger swipe (a Dock swipe whose progress follows the
//! hand). The event layouts follow Mac Mouse Fix's `GestureScrollSimulator`
//! and `TouchSimulator`, which have driven these gestures from a mouse for
//! years: <https://github.com/noah-nuebling/mac-mouse-fix>.
//!
//! Raw-XY diversion keeps the system pointer still, so the drag moves it
//! itself unless the user locked it, as Mac Mouse Fix does. Scroll events
//! stay pinned to where the drag began, so the view under the start point
//! keeps scrolling while the pointer travels.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use core_graphics::display::CGDisplay;
use objc2_core_foundation::{CFRetained, CGPoint};
use objc2_core_graphics::{CGEvent, CGEventField, CGEventTapLocation, CGEventType, CGMouseButton};
use openlogi_core::binding::HeldDrag;

use super::super::flick::FlickVelocity;
use super::spaces::{self, DockAxis};

/// Points of gesture travel per raw-XY sensor count. Raw XY reports the
/// sensor's own counts, unaccelerated; one count per point makes the content
/// follow the hand at the sensor resolutions MX mice ship with.
const POINTS_PER_COUNT: f64 = 1.0;

/// Pointer points per raw-XY sensor count while a drag moves the pointer.
/// Raw XY skips the system's pointer acceleration, so this is a fixed speed
/// near the default tracking speed.
const POINTER_POINTS_PER_COUNT: f64 = 0.5;

/// How much faster a two-finger swipe's gesture deltas run than its scroll
/// deltas. Mac Mouse Fix found 1.67 makes swiping between pages in Safari as
/// easy to trigger as on a trackpad.
const GESTURE_SCALE: f64 = 1.67;

/// Points per scroll line, `CGEventSource`'s default.
const POINTS_PER_LINE: f64 = 10.0;

/// Horizontal Dock-swipe progress per point, times the screen width: how far
/// one Space is in a swipe. Mac Mouse Fix measured `1 + 1 / (spaces - 1)` per
/// `width + 63` points; 1.5 sits between two and three Spaces.
const SPACE_OFFSET: f64 = 1.5;
const SPACE_SEPARATOR_WIDTH: f64 = 63.0;

/// A release later than this after the last motion starts no momentum: the
/// hand had stopped.
const MOMENTUM_MAX_IDLE: Duration = Duration::from_millis(50);
/// Weight of the newest raw-XY report in a three-finger drag's velocity: a
/// short average, so one stray report as the hand lets go cannot flip it.
const DOCK_VELOCITY_WEIGHT: f64 = 0.35;
/// A three-finger drag whose hand rested this long before release ends with
/// no exit velocity: the Dock then decides from how far it was dragged.
const DOCK_REST: Duration = Duration::from_millis(60);

/// Momentum decelerates as `dv/dt = -DRAG * v^DRAG_EXPONENT` (points/s), and
/// stops below `STOP_SPEED`. Mac Mouse Fix's constants for its swipes.
const DRAG: f64 = 30.0;
const DRAG_EXPONENT: f64 = 0.7;
const STOP_SPEED: f64 = 1.0;
const MOMENTUM_FRAME: Duration = Duration::from_micros(8_333);

/// Whether drags keep the pointer still; the host publishes the user's
/// choice through [`set_drag_pointer_lock`].
static POINTER_LOCK: AtomicBool = AtomicBool::new(false);

pub(in crate::inject) fn set_drag_pointer_lock(locked: bool) {
    POINTER_LOCK.store(locked, Ordering::Relaxed);
}

/// The phase a two-finger frame carries. Touch, momentum and rest frames are
/// distinct variants, so a frame can never pair a touch phase with a momentum
/// phase.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ScrollPhase {
    /// The hand is on the "trackpad".
    Touch(Touch),
    /// The glide after the hand let go.
    Momentum(Momentum),
    /// The pair a trackpad sends at rest after a glide.
    Rest(Rest),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Touch {
    Began,
    Changed,
    Ended,
    Cancelled,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Momentum {
    Begin,
    Continue,
    End,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Rest {
    MayBegin,
    Cancelled,
}

impl ScrollPhase {
    /// The scroll event's `(scroll phase, momentum phase)` fields: an
    /// `IOHIDEventPhaseBits` value and a `CGMomentumScrollPhase`.
    const fn fields(self) -> (i64, i64) {
        match self {
            Self::Touch(Touch::Began) => (1, 0),
            Self::Touch(Touch::Changed) => (2, 0),
            Self::Touch(Touch::Ended) => (4, 0),
            Self::Touch(Touch::Cancelled) | Self::Rest(Rest::Cancelled) => (8, 0),
            Self::Momentum(Momentum::Begin) => (0, 1),
            Self::Momentum(Momentum::Continue) => (0, 2),
            Self::Momentum(Momentum::End) => (0, 3),
            Self::Rest(Rest::MayBegin) => (128, 0),
        }
    }
}

/// The phase of a three-finger frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DockPhase {
    Began,
    Changed,
    Ended,
    Cancelled,
}

impl DockPhase {
    /// The `IOHIDEventPhaseBits` value the Dock reads.
    const fn bits(self) -> u32 {
        match self {
            Self::Began => 1,
            Self::Changed => 2,
            Self::Ended => 4,
            Self::Cancelled => 8,
        }
    }
}

/// The momentum glide a released two-finger drag launched, on its own thread.
/// Dropping it leaves the glide running; [`Self::stop`] ends it at the next
/// frame.
pub(in crate::inject) struct Glide {
    stop: Arc<AtomicBool>,
    done: Arc<AtomicBool>,
}

impl Glide {
    pub(in crate::inject) fn stop(&self) {
        self.stop.store(true, Ordering::Relaxed);
    }

    pub(in crate::inject) fn is_running(&self) -> bool {
        !self.done.load(Ordering::Relaxed)
    }
}

/// One held-button drag in progress: the gesture it plays, and the pointer
/// it moves.
pub(in crate::inject) struct Drag {
    gesture: Gesture,
    /// `None` while the pointer is locked for this drag.
    pointer: Option<Pointer>,
}

enum Gesture {
    Scroll(ScrollDrag),
    Dock(DockDrag),
}

impl Drag {
    /// Begin `kind` with the first motion, in sensor counts, whose report
    /// arrived `at`.
    pub(in crate::inject) fn begin(kind: HeldDrag, dx: f64, dy: f64, at: Instant) -> Self {
        let mut pointer = Pointer::current();
        let origin = pointer.at;
        let pointer = (!POINTER_LOCK.load(Ordering::Relaxed)).then(|| {
            pointer.moved(dx, dy);
            pointer
        });
        let (dx, dy) = (dx * POINTS_PER_COUNT, dy * POINTS_PER_COUNT);
        let gesture = match kind {
            HeldDrag::TwoFingerSwipe => Gesture::Scroll(ScrollDrag::begin(dx, dy, origin, at)),
            HeldDrag::ThreeFingerSwipe => Gesture::Dock(DockDrag::begin(dx, dy, at)),
        };
        Self { gesture, pointer }
    }

    /// Follow further motion, in sensor counts, whose report arrived `at`.
    pub(in crate::inject) fn moved(&mut self, dx: f64, dy: f64, at: Instant) {
        if let Some(pointer) = &mut self.pointer {
            pointer.moved(dx, dy);
        }
        let (dx, dy) = (dx * POINTS_PER_COUNT, dy * POINTS_PER_COUNT);
        match &mut self.gesture {
            Gesture::Scroll(drag) => drag.moved(dx, dy, at),
            Gesture::Dock(drag) => drag.moved(dx, dy, at),
        }
    }

    /// The button was released `at`: finish the gesture as a trackpad lift
    /// does, and hand back the glide a two-finger flick launched.
    pub(in crate::inject) fn release(self, at: Instant) -> Option<Glide> {
        match self.gesture {
            Gesture::Scroll(drag) => drag.release(at),
            Gesture::Dock(drag) => {
                drag.release(at);
                None
            }
        }
    }

    /// The press ended without a release (its capture session or binding
    /// went away): end the gesture with no glide, and let the Dock spring
    /// back.
    pub(in crate::inject) fn cancel(self) {
        match self.gesture {
            Gesture::Scroll(drag) => drag.cancel(),
            Gesture::Dock(drag) => drag.cancel(),
        }
    }
}

/// The system pointer a drag moves, kept inside the displays.
struct Pointer {
    at: CGPoint,
    /// The union of every display's bounds: `(min_x, min_y, max_x, max_y)`.
    bounds: (f64, f64, f64, f64),
}

impl Pointer {
    fn current() -> Self {
        let at = CGEvent::new(None).map_or(CGPoint::new(0.0, 0.0), |event| {
            CGEvent::location(Some(&event))
        });
        let bounds = CGDisplay::active_displays()
            .unwrap_or_default()
            .into_iter()
            .map(|id| CGDisplay::new(id).bounds())
            .fold(None, |union: Option<(f64, f64, f64, f64)>, rect| {
                let (x0, y0) = (rect.origin.x, rect.origin.y);
                let (x1, y1) = (x0 + rect.size.width, y0 + rect.size.height);
                Some(union.map_or((x0, y0, x1, y1), |(a, b, c, d)| {
                    (a.min(x0), b.min(y0), c.max(x1), d.max(y1))
                }))
            })
            .unwrap_or((at.x, at.y, at.x, at.y));
        Self { at, bounds }
    }

    /// Move the pointer by `dx`, `dy` sensor counts.
    fn moved(&mut self, dx: f64, dy: f64) {
        let (min_x, min_y, max_x, max_y) = self.bounds;
        let (dx, dy) = (dx * POINTER_POINTS_PER_COUNT, dy * POINTER_POINTS_PER_COUNT);
        let to = CGPoint::new(
            (self.at.x + dx).clamp(min_x, (max_x - 1.0).max(min_x)),
            (self.at.y + dy).clamp(min_y, (max_y - 1.0).max(min_y)),
        );
        self.at = to;
        let Some(event) =
            CGEvent::new_mouse_event(None, CGEventType::MouseMoved, to, CGMouseButton::Left)
        else {
            return;
        };
        #[expect(clippy::cast_possible_truncation, reason = "one report's travel")]
        let (delta_x, delta_y) = (dx.round() as i64, dy.round() as i64);
        CGEvent::set_integer_value_field(Some(&event), CGEventField::MouseEventDeltaX, delta_x);
        CGEvent::set_integer_value_field(Some(&event), CGEventField::MouseEventDeltaY, delta_y);
        tag(&event);
        CGEvent::post(CGEventTapLocation::HIDEventTap, Some(&event));
    }
}

/// A two-finger swipe: continuous scroll that follows the hand.
struct ScrollDrag {
    target: ScrollTarget,
    pixels: SubPixel,
    flick: FlickVelocity,
}

impl ScrollDrag {
    fn begin(dx: f64, dy: f64, origin: CGPoint, at: Instant) -> Self {
        let mut drag = Self {
            target: ScrollTarget {
                natural: spaces::natural_scrolling(),
                at: origin,
            },
            pixels: SubPixel::default(),
            flick: FlickVelocity::default(),
        };
        drag.post(dx, dy, Touch::Began, at);
        drag
    }

    fn moved(&mut self, dx: f64, dy: f64, at: Instant) {
        self.post(dx, dy, Touch::Changed, at);
    }

    fn post(&mut self, dx: f64, dy: f64, touch: Touch, at: Instant) {
        self.flick.sample(dx, dy, at);
        let (px, py) = self.pixels.take(dx, dy);
        if touch == Touch::Changed && px == 0.0 && py == 0.0 {
            return;
        }
        post_scroll_frame(px, py, ScrollPhase::Touch(touch), self.target);
    }

    fn release(self, at: Instant) -> Option<Glide> {
        post_scroll_frame(0.0, 0.0, ScrollPhase::Touch(Touch::Ended), self.target);
        let velocity = self.flick.exit(at, MOMENTUM_MAX_IDLE)?;
        let stop = Arc::new(AtomicBool::new(false));
        let done = Arc::new(AtomicBool::new(false));
        let (thread_stop, thread_done) = (Arc::clone(&stop), Arc::clone(&done));
        let target = self.target;
        let spawned = thread::Builder::new()
            .name("openlogi-drag-momentum".into())
            .spawn(move || {
                glide(velocity, &thread_stop, target);
                thread_done.store(true, Ordering::Relaxed);
            });
        match spawned {
            Ok(_) => Some(Glide { stop, done }),
            Err(error) => {
                tracing::warn!(?error, "two-finger drag: momentum thread failed to start");
                None
            }
        }
    }

    fn cancel(self) {
        post_scroll_frame(0.0, 0.0, ScrollPhase::Touch(Touch::Cancelled), self.target);
    }
}

/// Post the momentum glide after a two-finger swipe, as a trackpad driver
/// does: apps that leave momentum to the OS (Safari) keep moving, and the
/// glide ends with the MayBegin/Cancelled pair a trackpad sends at rest.
fn glide(velocity: (f64, f64), stop: &AtomicBool, target: ScrollTarget) {
    let speed = velocity.0.hypot(velocity.1);
    if speed <= STOP_SPEED {
        return;
    }
    let unit = (velocity.0 / speed, velocity.1 / speed);
    let mut speed = speed;
    let mut pixels = SubPixel::default();
    let mut phase = Momentum::Begin;
    let dt = MOMENTUM_FRAME.as_secs_f64();
    loop {
        thread::sleep(MOMENTUM_FRAME);
        if stop.load(Ordering::Relaxed) {
            post_scroll_frame(0.0, 0.0, ScrollPhase::Momentum(Momentum::End), target);
            return;
        }
        speed -= DRAG * speed.powf(DRAG_EXPONENT) * dt;
        if speed <= STOP_SPEED {
            break;
        }
        let (px, py) = pixels.take(unit.0 * speed * dt, unit.1 * speed * dt);
        post_scroll_frame(px, py, ScrollPhase::Momentum(phase), target);
        phase = Momentum::Continue;
    }
    post_scroll_frame(0.0, 0.0, ScrollPhase::Momentum(Momentum::End), target);
    post_scroll_frame(0.0, 0.0, ScrollPhase::Rest(Rest::MayBegin), target);
    post_scroll_frame(0.0, 0.0, ScrollPhase::Rest(Rest::Cancelled), target);
}

/// Whole-point deltas out of fractional travel, carrying the remainder.
#[derive(Default)]
struct SubPixel {
    x: f64,
    y: f64,
}

impl SubPixel {
    fn take(&mut self, dx: f64, dy: f64) -> (f64, f64) {
        self.x += dx;
        self.y += dy;
        let (px, py) = (self.x.trunc(), self.y.trunc());
        self.x -= px;
        self.y -= py;
        (px, py)
    }
}

/// Where a two-finger swipe's scroll lands: the point it began at, read with
/// the natural-scrolling flag a trackpad stamps.
#[derive(Clone, Copy)]
struct ScrollTarget {
    natural: bool,
    at: CGPoint,
}

/// One continuous scroll event plus, inside a touch or rest phase, the
/// gesture event that marks it as trackpad input. Positive deltas move the
/// content right and down, with the hand.
fn post_scroll_frame(px: f64, py: f64, phase: ScrollPhase, target: ScrollTarget) {
    let Some(scroll) = CGEvent::new(None) else {
        return;
    };
    CGEvent::set_type(Some(&scroll), CGEventType::ScrollWheel);
    let line = |points: f64| points / POINTS_PER_LINE;
    // Native continuous events report a sub-line delta as one line.
    #[expect(
        clippy::cast_possible_truncation,
        reason = "a frame scrolls a few lines"
    )]
    let line_int = |points: f64| {
        let lines = line(points);
        let whole = if lines.abs() <= 1.0 {
            lines.signum() * lines.abs().ceil()
        } else {
            lines.trunc()
        };
        whole as i64
    };
    #[expect(
        clippy::cast_possible_truncation,
        reason = "16.16 fixed point of a few lines"
    )]
    let fixed = |points: f64| (line(points) * 65_536.0).round() as i64;
    #[expect(clippy::cast_possible_truncation, reason = "whole points of one frame")]
    let point = |points: f64| points as i64;
    let (scroll_phase, momentum_phase) = phase.fields();
    for (field, value) in [
        (88, 1),                          // kCGScrollWheelEventIsContinuous
        (137, i64::from(target.natural)), // directionInvertedFromDevice
        (11, line_int(py)),
        (96, point(py)),
        (93, fixed(py)),
        (12, line_int(px)),
        (97, point(px)),
        (94, fixed(px)),
        (99, scroll_phase),
        (123, momentum_phase),
    ] {
        CGEvent::set_integer_value_field(Some(&scroll), CGEventField(field), value);
    }
    tag(&scroll);
    CGEvent::set_location(Some(&scroll), target.at);
    CGEvent::post(CGEventTapLocation::SessionEventTap, Some(&scroll));
    if matches!(phase, ScrollPhase::Momentum(_)) {
        return;
    }
    let Some(gesture) = CGEvent::new(None) else {
        return;
    };
    CGEvent::set_type(Some(&gesture), CGEventType(29));
    CGEvent::set_integer_value_field(Some(&gesture), CGEventField(110), 6); // kIOHIDEventTypeScroll
    // Native events carry -0.0 on a still axis.
    let gesture_delta = |points: f64| {
        if points == 0.0 {
            -0.0
        } else {
            points * GESTURE_SCALE
        }
    };
    CGEvent::set_double_value_field(Some(&gesture), CGEventField(116), gesture_delta(px));
    CGEvent::set_double_value_field(Some(&gesture), CGEventField(119), gesture_delta(py));
    CGEvent::set_integer_value_field(Some(&gesture), CGEventField(132), scroll_phase);
    tag(&gesture);
    CGEvent::set_location(Some(&gesture), target.at);
    CGEvent::post(CGEventTapLocation::SessionEventTap, Some(&gesture));
}

fn tag(event: &CFRetained<CGEvent>) {
    CGEvent::set_integer_value_field(
        Some(event),
        CGEventField::EventSourceUserData,
        super::super::SYNTHETIC_EVENT_USER_DATA,
    );
}

/// A three-finger swipe: a Dock swipe whose progress follows the hand. The
/// first motion picks the axis — sideways switches Spaces, vertical opens
/// Mission Control (up) or App Exposé (down) — and the release lets the Dock
/// finish or spring back from where the hand left it.
struct DockDrag {
    axis: DockAxis,
    scale: f64,
    natural: bool,
    progress: f64,
    /// Smoothed progress per report, signed like `progress`.
    velocity: f64,
    /// Arrival of the last report that moved the swipe.
    moved_at: Instant,
}

impl DockDrag {
    fn begin(dx: f64, dy: f64, at: Instant) -> Self {
        let bounds = CGDisplay::main().bounds();
        let (axis, scale) = if dx.abs() >= dy.abs() {
            (
                DockAxis::Horizontal,
                -SPACE_OFFSET / (bounds.size.width + SPACE_SEPARATOR_WIDTH),
            )
        } else {
            (DockAxis::Vertical, 1.0 / bounds.size.height)
        };
        let mut drag = Self {
            axis,
            scale,
            natural: spaces::natural_scrolling(),
            progress: 0.0,
            velocity: 0.0,
            moved_at: at,
        };
        drag.post(dx, dy, DockPhase::Began, at);
        drag
    }

    fn moved(&mut self, dx: f64, dy: f64, at: Instant) {
        self.post(dx, dy, DockPhase::Changed, at);
    }

    fn post(&mut self, dx: f64, dy: f64, phase: DockPhase, at: Instant) {
        let delta = match self.axis {
            DockAxis::Horizontal => dx,
            DockAxis::Vertical => dy,
        } * self.scale;
        if phase == DockPhase::Changed && delta == 0.0 {
            return;
        }
        self.progress += delta;
        self.velocity += DOCK_VELOCITY_WEIGHT * (delta - self.velocity);
        self.moved_at = self.moved_at.max(at);
        spaces::post_dock_drag(self.axis, phase.bits(), self.progress, None, self.natural);
    }

    fn release(self, at: Instant) {
        // A hand that came to rest lets the Dock decide from the distance; a
        // hand still moving back cancels the swipe, as on a trackpad. The
        // velocity is averaged, so a wobble as the button lets go does not.
        let velocity = if at.saturating_duration_since(self.moved_at) > DOCK_REST {
            0.0
        } else {
            self.velocity
        };
        let phase = if velocity != 0.0 && velocity.signum() != self.progress.signum() {
            DockPhase::Cancelled
        } else {
            DockPhase::Ended
        };
        tracing::debug!(
            axis = ?self.axis,
            progress = self.progress,
            velocity,
            ?phase,
            "three-finger drag released"
        );
        spaces::post_dock_drag(
            self.axis,
            phase.bits(),
            self.progress,
            Some(velocity * 100.0),
            self.natural,
        );
    }

    fn cancel(self) {
        spaces::post_dock_drag(
            self.axis,
            DockPhase::Cancelled.bits(),
            self.progress,
            Some(0.0),
            self.natural,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scroll_phases_never_mix_touch_and_momentum() {
        let all = [
            ScrollPhase::Touch(Touch::Began),
            ScrollPhase::Touch(Touch::Changed),
            ScrollPhase::Touch(Touch::Ended),
            ScrollPhase::Touch(Touch::Cancelled),
            ScrollPhase::Momentum(Momentum::Begin),
            ScrollPhase::Momentum(Momentum::Continue),
            ScrollPhase::Momentum(Momentum::End),
            ScrollPhase::Rest(Rest::MayBegin),
            ScrollPhase::Rest(Rest::Cancelled),
        ];
        for phase in all {
            let (touch, momentum) = phase.fields();
            assert!(touch == 0 || momentum == 0, "{phase:?} sets both phases");
            assert_eq!(
                momentum != 0,
                matches!(phase, ScrollPhase::Momentum(_)),
                "{phase:?}"
            );
        }
    }
}
