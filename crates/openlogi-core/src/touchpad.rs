//! Platform-free raw-touchpad frames and gesture recognition.
//!
//! Device code validates and normalizes HID++ reports into [`TouchFrame`]s.
//! The recognizer consumes only integer micrometres and microseconds, so its
//! thresholds are independent of a touchpad's native coordinate range.

use crate::binding::ButtonId;

#[cfg(test)]
mod tests;

const TAP_MAX_DURATION_US: u64 = 250_000;
const TAP_MAX_TRAVEL_UM: u64 = 3_000;
const SWIPE_MIN_DISTANCE_UM: u64 = 10_000;
const SWIPE_MIN_SPEED_UM_PER_SECOND: u64 = 50_000;
const HORIZONTAL_SWIPE_MIN_DURATION_US: u64 = 50_000;
const VERTICAL_SWIPE_MIN_DURATION_US: u64 = 35_000;
const FLICK_MIN_MOTION_FRAMES: u8 = 3;
const FLICK_MIN_DISTANCE_UM: u64 = 15_000;
const SWIPE_CROSS_AXIS_FLOOR_UM: u64 = 3_000;
const PINCH_MIN_SPREAD_CHANGE_UM: u64 = 8_000;
const PINCH_MIN_SPREAD_PERCENT: u64 = 8;
// Real swipes keep the spread within ~2 mm while pinching hands drift the
// centroid past the spread change itself, so only two-finger pinches need
// the dominance gate (130 Hz Casa Touch captures).
const MOTION_DOMINANCE_NUMERATOR: u64 = 3;
const MOTION_DOMINANCE_DENOMINATOR: u64 = 2;

/// One normalized touch contact.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct TouchContact {
    /// Controller-assigned contact identifier.
    pub id: u8,
    /// Horizontal position from the left edge, in micrometres.
    pub x_um: u32,
    /// Vertical position from the top edge, in micrometres.
    pub y_um: u32,
}

/// One complete, normalized touchpad frame.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TouchFrame {
    /// Monotonic frame time, in microseconds.
    pub timestamp_us: u64,
    /// Whether the physical switch beneath the surface is pressed.
    pub button: bool,
    contacts: Box<[TouchContact]>,
}

impl TouchFrame {
    /// Build a frame, sorting contacts by ID and rejecting duplicate IDs.
    pub fn new(
        timestamp_us: u64,
        button: bool,
        mut contacts: Vec<TouchContact>,
    ) -> Result<Self, TouchFrameError> {
        contacts.sort_unstable_by_key(|contact| contact.id);
        if contacts.windows(2).any(|pair| pair[0].id == pair[1].id) {
            return Err(TouchFrameError::DuplicateContactId);
        }
        Ok(Self {
            timestamp_us,
            button,
            contacts: contacts.into_boxed_slice(),
        })
    }

    /// Contacts in stable finger-ID order.
    #[must_use]
    pub fn contacts(&self) -> &[TouchContact] {
        &self.contacts
    }
}

/// Invalid normalized frame input.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum TouchFrameError {
    /// Two contacts in one frame carried the same controller ID.
    #[error("touchpad frame contains a duplicate contact id")]
    DuplicateContactId,
}

/// Observable result of feeding one frame to [`TouchpadGestureRecognizer`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GestureRecognition {
    /// No gesture has committed yet.
    Pending,
    /// A custom gesture committed as its binding trigger and should fire once.
    Gesture(ButtonId),
    /// One frame of two-finger scrolling: the centroid's travel since the
    /// previous frame, in micrometres. Streaming raw reports switches the pad
    /// out of its firmware scroll translation, so the host owns this stroke
    /// and must synthesize the scroll itself; the first `Scroll` of a stroke
    /// follows the activation travel, never includes it.
    Scroll {
        /// Centroid travel to the right, in micrometres.
        dx_um: i64,
        /// Centroid travel towards the bottom edge, in micrometres.
        dy_um: i64,
    },
}

/// Pure recognizer for one touchpad stream.
#[derive(Debug, Default)]
pub struct TouchpadGestureRecognizer {
    state: StrokeState,
}

#[derive(Debug, Default)]
enum StrokeState {
    #[default]
    Idle,
    Tracking(Stroke),
    Committed,
    Cancelled,
}

#[derive(Debug)]
struct Stroke {
    starts: Box<[TouchContact]>,
    latest: Box<[TouchContact]>,
    started_at_us: u64,
    last_at_us: u64,
    start_spread_um: u64,
    max_contact_travel_um: u64,
    motion_frames: u8,
    previous_centroid: Point,
    scrolling: bool,
}

#[derive(Clone, Copy, Debug)]
struct Point {
    x: i64,
    y: i64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ContactUpdate {
    Stable,
    Rebased,
}

impl TouchpadGestureRecognizer {
    /// Feed one complete frame. A gesture is returned at most once per stroke.
    pub fn update(&mut self, frame: &TouchFrame) -> GestureRecognition {
        let count = frame.contacts.len();
        if count == 0 {
            let _ = self.end();
            return GestureRecognition::Pending;
        }
        if count > 4 {
            self.state = StrokeState::Cancelled;
            return GestureRecognition::Pending;
        }

        match &mut self.state {
            StrokeState::Idle => {
                if count >= 2 {
                    self.state = StrokeState::Tracking(Stroke::new(frame));
                }
                GestureRecognition::Pending
            }
            StrokeState::Tracking(stroke) => {
                let Some(update) = stroke.advance(frame) else {
                    self.state = StrokeState::Cancelled;
                    return GestureRecognition::Pending;
                };
                if update == ContactUpdate::Rebased {
                    return GestureRecognition::Pending;
                }
                if let Some((dx_um, dy_um)) = stroke.scroll_delta() {
                    return GestureRecognition::Scroll { dx_um, dy_um };
                }
                let recognition = stroke.classify();
                if !matches!(recognition, GestureRecognition::Pending) {
                    self.state = match recognition {
                        GestureRecognition::Gesture(_) => StrokeState::Committed,
                        GestureRecognition::Scroll { .. } | GestureRecognition::Pending => {
                            unreachable!(
                                "a returned scroll or pending recognition keeps the stroke tracking"
                            )
                        }
                    };
                }
                recognition
            }
            StrokeState::Committed | StrokeState::Cancelled => GestureRecognition::Pending,
        }
    }

    /// End the current stroke, returning a tap when it stayed short and still.
    pub fn end(&mut self) -> Option<ButtonId> {
        let state = std::mem::take(&mut self.state);
        match state {
            StrokeState::Tracking(stroke) if stroke.is_tap() => stroke.tap_gesture(),
            StrokeState::Idle
            | StrokeState::Tracking(_)
            | StrokeState::Committed
            | StrokeState::Cancelled => None,
        }
    }

    /// Cancel the current stroke without producing a tap.
    pub fn cancel(&mut self) {
        self.state = StrokeState::Cancelled;
    }
}

impl Stroke {
    fn new(frame: &TouchFrame) -> Self {
        let centroid = centroid(&frame.contacts);
        Self {
            starts: frame.contacts.clone(),
            latest: frame.contacts.clone(),
            started_at_us: frame.timestamp_us,
            last_at_us: frame.timestamp_us,
            start_spread_um: spread(&frame.contacts, centroid),
            max_contact_travel_um: 0,
            motion_frames: 0,
            previous_centroid: centroid,
            scrolling: false,
        }
    }

    fn advance(&mut self, frame: &TouchFrame) -> Option<ContactUpdate> {
        let same_contacts = contact_ids(&frame.contacts).eq(contact_ids(&self.latest));
        if same_contacts {
            self.record(frame);
            self.motion_frames = self.motion_frames.saturating_add(1);
            return Some(ContactUpdate::Stable);
        }

        // A rising contact count re-anchors the stroke even when it does not
        // return to the count the stroke started with: a 2→1→2 lift-and-
        // re-land must resume as a fresh stroke, not fall through to the
        // cancel below and freeze the recognizer until the watchdog ends it.
        let fingers_landed = frame.contacts.len() > self.latest.len()
            && self
                .latest
                .iter()
                .all(|contact| has_contact(&frame.contacts, contact.id));
        if fingers_landed {
            *self = Self::new(frame);
            return Some(ContactUpdate::Rebased);
        }

        let fingers_lifted = frame.contacts.len() < self.latest.len()
            && frame
                .contacts
                .iter()
                .all(|contact| has_contact(&self.latest, contact.id));
        if !fingers_lifted {
            return None;
        }

        self.record(frame);
        Some(ContactUpdate::Rebased)
    }

    fn record(&mut self, frame: &TouchFrame) {
        self.last_at_us = frame.timestamp_us;
        self.max_contact_travel_um = self.max_contact_travel_um.max(
            frame
                .contacts
                .iter()
                .filter_map(|current| {
                    self.starts
                        .iter()
                        .find(|start| start.id == current.id)
                        .map(|start| contact_distance(*start, *current))
                })
                .max()
                .unwrap_or(0),
        );
        self.latest.clone_from(&frame.contacts);
    }

    /// Stream the centroid delta of one frame once this stroke scrolls.
    ///
    /// Scrolling claims the stroke when centroid travel passes the tap limit
    /// and dominates spread change — past that point the stroke is content
    /// motion, not a pinch or a tap. The claim is sticky: a stroke that
    /// scrolled never re-classifies into a pinch, matching how a zoom chord
    /// must be deliberate from the start rather than grown out of a scroll.
    fn scroll_delta(&mut self) -> Option<(i64, i64)> {
        if self.starts.len() != 2 || self.latest.len() != 2 {
            return None;
        }
        let current = centroid(&self.latest);
        if !self.scrolling {
            let geometry = self.current_geometry();
            let centroid_distance = vector_length(geometry.dx, geometry.dy);
            let spread_change = geometry.spread_um.abs_diff(self.start_spread_um);
            if centroid_distance <= TAP_MAX_TRAVEL_UM
                || !dominates(centroid_distance, spread_change)
            {
                self.previous_centroid = current;
                return None;
            }
            self.scrolling = true;
        }
        let delta = (
            current.x - self.previous_centroid.x,
            current.y - self.previous_centroid.y,
        );
        self.previous_centroid = current;
        Some(delta)
    }

    fn classify(&self) -> GestureRecognition {
        if self.latest.len() < 2 {
            return GestureRecognition::Pending;
        }
        let current = self.current_geometry();
        let centroid_distance = vector_length(current.dx, current.dy);
        let spread_change = current.spread_um.abs_diff(self.start_spread_um);
        let finger_count = self.starts.len();

        if !self.scrolling
            && self.latest.len() == finger_count
            && matches!(finger_count, 2 | 4)
            && spread_change >= self.pinch_threshold()
            && (finger_count == 4 || dominates(spread_change, centroid_distance))
        {
            return GestureRecognition::Gesture(
                self.pinch_gesture(current.spread_um >= self.start_spread_um),
            );
        }

        if matches!(finger_count, 3 | 4)
            && let Some(gesture) = self.swipe_gesture(current.dx, current.dy)
        {
            return GestureRecognition::Gesture(gesture);
        }

        GestureRecognition::Pending
    }

    fn current_geometry(&self) -> Geometry {
        let count = i64::try_from(self.latest.len()).unwrap_or(1);
        let (dx, dy) = self.latest.iter().fold((0, 0), |(dx, dy), current| {
            let start = self
                .starts
                .iter()
                .find(|start| start.id == current.id)
                .unwrap_or(current);
            (
                dx + i64::from(current.x_um) - i64::from(start.x_um),
                dy + i64::from(current.y_um) - i64::from(start.y_um),
            )
        });
        let centroid = centroid(&self.latest);
        Geometry {
            dx: dx / count,
            dy: dy / count,
            spread_um: spread(&self.latest, centroid),
        }
    }

    fn pinch_threshold(&self) -> u64 {
        PINCH_MIN_SPREAD_CHANGE_UM.max(
            self.start_spread_um
                .saturating_mul(PINCH_MIN_SPREAD_PERCENT)
                / 100,
        )
    }

    fn pinch_gesture(&self, outward: bool) -> ButtonId {
        match (self.starts.len(), outward) {
            (2, false) => ButtonId::TouchpadTwoFingerPinchIn,
            (2, true) => ButtonId::TouchpadTwoFingerPinchOut,
            (4, false) => ButtonId::TouchpadFourFingerPinchIn,
            (4, true) => ButtonId::TouchpadFourFingerPinchOut,
            _ => unreachable!("pinches require two or four fingers"),
        }
    }

    fn swipe_gesture(&self, dx: i64, dy: i64) -> Option<ButtonId> {
        let (abs_x, abs_y) = (dx.unsigned_abs(), dy.unsigned_abs());
        let (dominant, cross, min_duration) = if abs_x > abs_y {
            (abs_x, abs_y, HORIZONTAL_SWIPE_MIN_DURATION_US)
        } else {
            (abs_y, abs_x, VERTICAL_SWIPE_MIN_DURATION_US)
        };
        let duration = self.last_at_us.saturating_sub(self.started_at_us);
        let cross_limit = SWIPE_CROSS_AXIS_FLOOR_UM.max(dominant.saturating_mul(40) / 100);
        let duration_met = duration >= min_duration;
        let flick =
            self.motion_frames >= FLICK_MIN_MOTION_FRAMES && dominant >= FLICK_MIN_DISTANCE_UM;
        if dominant < SWIPE_MIN_DISTANCE_UM
            || cross > cross_limit
            || !(duration_met || flick)
            || dominant.saturating_mul(1_000_000)
                < SWIPE_MIN_SPEED_UM_PER_SECOND.saturating_mul(duration)
        {
            return None;
        }
        match (self.starts.len(), abs_x > abs_y, dx > 0, dy > 0) {
            (3, true, true, _) => Some(ButtonId::TouchpadThreeFingerSwipeRight),
            (3, true, false, _) => Some(ButtonId::TouchpadThreeFingerSwipeLeft),
            (3, false, _, true) => Some(ButtonId::TouchpadThreeFingerSwipeDown),
            (3, false, _, false) => Some(ButtonId::TouchpadThreeFingerSwipeUp),
            (4, true, true, _) => Some(ButtonId::TouchpadFourFingerSwipeRight),
            (4, true, false, _) => Some(ButtonId::TouchpadFourFingerSwipeLeft),
            (4, false, _, true) => Some(ButtonId::TouchpadFourFingerSwipeDown),
            (4, false, _, false) => Some(ButtonId::TouchpadFourFingerSwipeUp),
            _ => None,
        }
    }

    fn is_tap(&self) -> bool {
        self.last_at_us.saturating_sub(self.started_at_us) <= TAP_MAX_DURATION_US
            && self.max_contact_travel_um <= TAP_MAX_TRAVEL_UM
    }

    fn tap_gesture(&self) -> Option<ButtonId> {
        match self.starts.len() {
            2 => Some(ButtonId::TouchpadTwoFingerTap),
            3 => Some(ButtonId::TouchpadThreeFingerTap),
            4 => Some(ButtonId::TouchpadFourFingerTap),
            _ => None,
        }
    }
}

fn contact_ids(contacts: &[TouchContact]) -> impl Iterator<Item = u8> + '_ {
    contacts.iter().map(|contact| contact.id)
}

fn has_contact(contacts: &[TouchContact], id: u8) -> bool {
    contacts.iter().any(|contact| contact.id == id)
}

#[derive(Clone, Copy)]
struct Geometry {
    dx: i64,
    dy: i64,
    spread_um: u64,
}

fn centroid(contacts: &[TouchContact]) -> Point {
    let count = i64::try_from(contacts.len()).unwrap_or(1);
    Point {
        x: contacts
            .iter()
            .map(|contact| i64::from(contact.x_um))
            .sum::<i64>()
            / count,
        y: contacts
            .iter()
            .map(|contact| i64::from(contact.y_um))
            .sum::<i64>()
            / count,
    }
}

fn spread(contacts: &[TouchContact], centre: Point) -> u64 {
    let count = u64::try_from(contacts.len()).unwrap_or(1);
    contacts
        .iter()
        .map(|contact| {
            vector_length(
                i64::from(contact.x_um) - centre.x,
                i64::from(contact.y_um) - centre.y,
            )
        })
        .sum::<u64>()
        / count
}

fn contact_distance(a: TouchContact, b: TouchContact) -> u64 {
    vector_length(
        i64::from(b.x_um) - i64::from(a.x_um),
        i64::from(b.y_um) - i64::from(a.y_um),
    )
}

fn vector_length(dx: i64, dy: i64) -> u64 {
    dx.unsigned_abs()
        .saturating_pow(2)
        .saturating_add(dy.unsigned_abs().saturating_pow(2))
        .isqrt()
}

fn dominates(candidate: u64, other: u64) -> bool {
    candidate.saturating_mul(MOTION_DOMINANCE_DENOMINATOR)
        > other.saturating_mul(MOTION_DOMINANCE_NUMERATOR)
}
