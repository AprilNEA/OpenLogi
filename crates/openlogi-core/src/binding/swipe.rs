//! The swipe-gesture runtime machinery: travel thresholds, the
//! [`detect_swipe`] classifier, and the [`SwipeAccumulator`] state machine
//! shared by both gesture-capture paths. This is input processing, distinct
//! from the `Action` vocabulary the parent [`binding`](super) module defines.

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use super::{Action, GestureDirection};

/// Minimum dominant-axis travel (raw-XY units) before a held gesture commits to
/// a direction. Tuned to match Logitech Options+'s responsiveness.
pub const GESTURE_SWIPE_THRESHOLD: i32 = 50;
/// Maximum cross-axis travel allowed at the threshold, so only a reasonably
/// straight swipe commits. Grows with the dominant axis (`max(deadzone, 35%)`).
pub const GESTURE_SWIPE_DEADZONE: i32 = 40;
/// Net travel that keeps going the committed direction before that direction
/// fires again, so one continuous motion steps through several desktops like
/// Options+. Larger than [`GESTURE_SWIPE_THRESHOLD`] so the overshoot of a
/// single swipe doesn't double-fire; backtracking subtracts from it, so a
/// wobble never accumulates into a repeat.
///
/// A fast hand-width sweep on an MX Master 3 over Bluetooth is about 2000
/// raw-XY units. At 150 (3x the threshold) such a sweep repeated every
/// 30-40 ms and ran past four desktops; 600 gives about three repeats per hand
/// width (#1652).
// ponytail: fixed constant, make it a user setting if people want to tune it.
pub const GESTURE_REPEAT_DISTANCE: i32 = 12 * GESTURE_SWIPE_THRESHOLD;
/// How long the pointer must rest mid-hold to end a stroke: the next swipe then
/// starts fresh and may repeat the last direction at the normal threshold.
pub const GESTURE_REARM_PAUSE: std::time::Duration = std::time::Duration::from_millis(250);
/// Classify the *running* raw-XY travel of a held gesture button into a
/// directional swipe, the instant it commits — or `None` while it's still too
/// short or too diagonal.
///
/// The dominant axis must pass [`GESTURE_SWIPE_THRESHOLD`] while the cross axis
/// stays within `max(`[`GESTURE_SWIPE_DEADZONE`]`, 35% of dominant)`. Callers
/// fire the bound action the moment this returns `Some` — mid-swipe, like
/// Options+ — rather than waiting for the button release; a press that never
/// commits a direction is treated as [`GestureDirection::Click`] on release.
///
/// Coordinates follow the device's raw-XY convention (`+x` = right, `+y` =
/// down), so an upward swipe (negative `dy`) maps to [`GestureDirection::Up`].
#[must_use]
pub fn detect_swipe(dx: i32, dy: i32) -> Option<GestureDirection> {
    // Saturating throughout: a [`SwipeAccumulator`] hold that never commits (a
    // sustained diagonal) keeps summing travel, so `dx`/`dy` can reach the i32
    // bounds. `i32::MIN.abs()` would panic and a plain `dominant * 35` would
    // overflow — and a panic in the input-hook callback is exactly the freeze
    // hazard we must never hit. The clamp is inert in the normal range.
    let (abs_x, abs_y) = (dx.saturating_abs(), dy.saturating_abs());
    let dominant = abs_x.max(abs_y);
    if dominant < GESTURE_SWIPE_THRESHOLD {
        return None;
    }
    let cross_limit = GESTURE_SWIPE_DEADZONE.max(dominant.saturating_mul(35) / 100);
    if abs_x > abs_y {
        if abs_y > cross_limit {
            return None;
        }
        Some(if dx > 0 {
            GestureDirection::Right
        } else {
            GestureDirection::Left
        })
    } else {
        if abs_x > cross_limit {
            return None;
        }
        Some(if dy > 0 {
            GestureDirection::Down
        } else {
            GestureDirection::Up
        })
    }
}

/// Pure runtime response derived from effective directional bindings.
/// This policy is not serialized and never changes the user's configuration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GesturePolicy {
    enabled: [bool; 4],
    durations: [Duration; 4],
    repeat_distances: [i32; 4],
}

impl Default for GesturePolicy {
    fn default() -> Self {
        Self {
            enabled: [true; 4],
            durations: [Duration::ZERO; 4],
            repeat_distances: [GESTURE_REPEAT_DISTANCE; 4],
        }
    }
}

impl GesturePolicy {
    /// Resolve enabled directions and ordinary action pacing once per plan.
    #[must_use]
    pub fn for_bindings(bindings: &BTreeMap<GestureDirection, Action>) -> Self {
        let mut policy = Self {
            enabled: [false; 4],
            ..Self::default()
        };
        for (direction, action) in bindings {
            if let Some(index) = direction_index(*direction) {
                policy.enabled[index] = *action != Action::None;
                policy.durations[index] = ordinary_duration();
                if matches!(action, Action::VolumeUp | Action::VolumeDown) {
                    policy.repeat_distances[index] = GESTURE_SWIPE_THRESHOLD;
                    policy.durations[index] = volume_duration();
                }
            }
        }
        policy
    }
}

fn ordinary_duration() -> Duration {
    Duration::from_millis(if cfg!(target_os = "windows") {
        200
    } else {
        400
    })
}

fn volume_duration() -> Duration {
    Duration::from_millis(if cfg!(target_os = "windows") { 10 } else { 20 })
}

fn direction_index(direction: GestureDirection) -> Option<usize> {
    match direction {
        GestureDirection::Up => Some(0),
        GestureDirection::Down => Some(1),
        GestureDirection::Left => Some(2),
        GestureDirection::Right => Some(3),
        GestureDirection::Click => None,
    }
}

/// Report recognition shared by HID++ and OS hooks. A physical hold can emit
/// several directions; only new movement can trigger paced output.
#[derive(Debug, Default)]
pub struct SwipeAccumulator {
    policy: GesturePolicy,
    last_commit: Option<(GestureDirection, Instant)>,
    /// When the current hold began, or `None` when not holding. Gates a
    /// deliberate swipe against a quick click whose cursor happened to move.
    held_since: Option<Instant>,
    /// Accumulated raw-XY travel since the hold began (saturating, so an
    /// arbitrarily long hold can never overflow).
    dx: i32,
    dy: i32,
    /// Set once a direction has committed this hold, so the release isn't
    /// then also read as a click.
    fired: bool,
    /// The direction committed by the current stroke. Travel restarts from
    /// zero after each commit, so one hold can chain swipes (left, right,
    /// left...) the way Options+ does.
    last: Option<GestureDirection>,
    /// Recognized stroke waiting for pacing; only fresh continuation may emit it.
    pending: Option<GestureDirection>,
    /// Net travel continuing `last` since it committed; fires `last` again at
    /// [`GESTURE_REPEAT_DISTANCE`].
    repeat: i32,
    /// Signed sideways drift accompanying `repeat`, held to the same
    /// straightness rule as a fresh swipe so a diagonal can't repeat.
    repeat_cross: i32,
    /// When the last motion arrived, to detect the pause that ends a stroke.
    moved_at: Option<Instant>,
}

impl SwipeAccumulator {
    /// Begin a fresh hold, resetting the travel accumulator and commit state.
    pub fn begin(&mut self) {
        self.begin_at(Instant::now(), GesturePolicy::default());
    }

    /// Begin a physical hold using its effective runtime policy.
    pub fn begin_with_policy(&mut self, policy: GesturePolicy) {
        self.begin_at(Instant::now(), policy);
    }

    /// Begin a hold with explicit monotonic time for deterministic replay.
    pub fn begin_at(&mut self, now: Instant, policy: GesturePolicy) {
        self.policy = policy;
        self.last_commit = None;
        self.held_since = Some(now);
        self.dx = 0;
        self.dy = 0;
        self.fired = false;
        self.last = None;
        self.pending = None;
        self.repeat = 0;
        self.repeat_cross = 0;
        self.moved_at = None;
    }

    /// Whether a hold is in progress (between [`Self::begin`] and [`Self::end`]),
    /// so callers can do rising/falling-edge detection without a second flag.
    #[must_use]
    pub fn is_holding(&self) -> bool {
        self.held_since.is_some()
    }

    /// Feed a new raw-distance report using the host monotonic clock.
    pub fn accumulate(&mut self, dx: i32, dy: i32) -> Option<GestureDirection> {
        self.accumulate_at(Instant::now(), dx, dy)
    }

    /// Feed one report. Zero motion never rearms a stroke or releases paced output.
    pub fn accumulate_at(&mut self, now: Instant, dx: i32, dy: i32) -> Option<GestureDirection> {
        self.held_since?;
        if dx == 0 && dy == 0 {
            return None;
        }
        if self.pending.is_some_and(|dir| !continues(dir, dx, dy)) {
            // A turn abandons paced travel instead of releasing it in the old
            // direction. The new report begins its own deliberate stroke.
            self.dx = 0;
            self.dy = 0;
            self.pending = None;
        }
        let pending_repeat = self.pending.is_some()
            || self
                .last
                .and_then(direction_index)
                .is_some_and(|index| self.repeat >= self.policy.repeat_distances[index]);
        if !pending_repeat
            && self
                .moved_at
                .is_some_and(|t| now.saturating_duration_since(t) >= GESTURE_REARM_PAUSE)
        {
            // A rest ends the stroke: the next swipe may repeat the last
            // direction. Summed travel the new report heads against is an
            // abandoned partial swipe and is dropped; travel it continues is
            // kept, so a reversal paused halfway still completes.
            if (i64::from(self.dx) * i64::from(dx))
                .saturating_add(i64::from(self.dy) * i64::from(dy))
                < 0
            {
                self.dx = 0;
                self.dy = 0;
            }
            self.last = None;
            self.repeat = 0;
            self.repeat_cross = 0;
        }
        self.moved_at = Some(now);
        if let Some(dir) = self.last {
            // Net travel continuing the committed way fires it again every
            // GESTURE_REPEAT_DISTANCE, so a continuous motion keeps stepping.
            // Backtracking takes back that progress and its drift alike, so a
            // wobble never repeats and a corrected stroke isn't held to drift
            // it has undone.
            let (a, c) = (along(dir, dx, dy), across(dir, dx, dy));
            let earlier = self.repeat;
            let total = earlier.saturating_add(a);
            // Only backtracking to the start clears drift: a purely
            // sideways report at zero progress is still part of the stroke.
            self.repeat_cross = if a < 0 && total <= 0 {
                0
            } else {
                self.repeat_cross.saturating_add(c)
            };
            self.repeat = total.max(0);
            if continues(dir, dx, dy) {
                // The same straightness rule as a fresh swipe, seen in the
                // committed direction's frame (`Right` = along it).
                let straight =
                    detect_swipe(self.repeat, self.repeat_cross) == Some(GestureDirection::Right);
                let index = direction_index(dir)?;
                let distance = self.policy.repeat_distances[index];
                if self.repeat >= distance && straight {
                    // Continuing beyond a full repeat also undoes any partial
                    // reversal, even when output is still waiting for pacing.
                    self.dx = 0;
                    self.dy = 0;
                    if !self.ready(dir, now) {
                        self.bound_pending_repeat(distance);
                        return None;
                    }
                    // Carry the excess, so a report spanning two repeat
                    // distances fires again on the next report. The excess is
                    // the stroke's tail, so it keeps the tail's drift: this
                    // report's share first, then earlier travel's.
                    let excess = (self.repeat - distance).min(distance - 1);
                    let tail = excess.min(a);
                    let mut carried = i64::from(c) * i64::from(tail) / i64::from(a);
                    if excess > a && earlier > 0 {
                        carried += i64::from(self.repeat_cross.saturating_sub(c))
                            * i64::from(excess - a)
                            / i64::from(earlier);
                    }
                    self.repeat = excess;
                    self.repeat_cross = i32::try_from(carried).unwrap_or(if carried < 0 {
                        i32::MIN
                    } else {
                        i32::MAX
                    });
                    // The stroke went back past any partial reversal.
                    self.dx = 0;
                    self.dy = 0;
                    return self.emit(dir, now);
                }
                // Before any reversal this is the same stroke: drop it from
                // the reversal travel below.
                if reversal(dir, self.dx, self.dy) == 0 {
                    return None;
                }
            }
        }
        self.dx = self.dx.saturating_add(dx);
        self.dy = self.dy.saturating_add(dy);
        // Once reversing, backtracking subtracts, but travel never banks past
        // the turning point: a reversal counts from there at the normal
        // threshold, and a net-zero wobble never fires.
        match self.last {
            Some(GestureDirection::Right) => self.dx = self.dx.min(0),
            Some(GestureDirection::Left) => self.dx = self.dx.max(0),
            Some(GestureDirection::Down) => self.dy = self.dy.min(0),
            Some(GestureDirection::Up) => self.dy = self.dy.max(0),
            Some(GestureDirection::Click) | None => {}
        }
        if let Some(dir) = detect_swipe(self.dx, self.dy) {
            self.fired = true;
            if !self.ready(dir, now) {
                self.bound_pending_stroke(dir);
                return None;
            }
            self.fired = true;
            self.last = Some(dir);
            self.pending = None;
            self.repeat = 0;
            self.repeat_cross = 0;
            self.dx = 0;
            self.dy = 0;
            return self.emit(dir, now);
        }
        None
    }

    /// Keep one paced stroke with its direction and cross-axis ratio intact.
    fn bound_pending_stroke(&mut self, dir: GestureDirection) {
        let distance = i64::from(along(dir, self.dx, self.dy));
        self.dx = i32::try_from(i64::from(self.dx) * i64::from(GESTURE_SWIPE_THRESHOLD) / distance)
            .unwrap_or(self.dx);
        self.dy = i32::try_from(i64::from(self.dy) * i64::from(GESTURE_SWIPE_THRESHOLD) / distance)
            .unwrap_or(self.dy);
        self.pending = Some(dir);
    }

    /// Bound distance and sideways drift together, preserving the stroke's slope.
    fn bound_pending_repeat(&mut self, distance: i32) {
        // distance <= repeat, so scaling cannot enlarge drift.
        let scaled = i64::from(self.repeat_cross) * i64::from(distance) / i64::from(self.repeat);
        self.repeat_cross = i32::try_from(scaled).unwrap_or(self.repeat_cross);
        self.repeat = distance;
    }

    fn ready(&self, direction: GestureDirection, now: Instant) -> bool {
        self.last_commit.is_none_or(|(previous, at)| {
            previous != direction
                || direction_index(direction).is_some_and(|index| {
                    now.saturating_duration_since(at) >= self.policy.durations[index]
                })
        })
    }

    fn emit(&mut self, direction: GestureDirection, now: Instant) -> Option<GestureDirection> {
        self.last_commit = Some((direction, now));
        direction_index(direction)
            .filter(|&index| self.policy.enabled[index])
            .map(|_| direction)
    }

    /// End the current hold. Returns `true` when an in-progress hold ended
    /// without committing a swipe — the caller should fire the plain `Click`
    /// action — and `false` when a swipe already fired mid-motion, or when there
    /// was no hold to end (a stray release reports no click).
    pub fn end(&mut self) -> bool {
        self.end_at(Instant::now())
    }

    /// Release at an explicit instant. Long stationary holds remain clicks.
    pub fn end_at(&mut self, _now: Instant) -> bool {
        let was_click = self.held_since.is_some() && !self.fired;
        self.cancel();
        was_click
    }

    /// Cancel without generating a click or retaining pending movement.
    pub fn cancel(&mut self) {
        self.held_since = None;
        self.last_commit = None;
    }

    /// Backdate the physical hold for lifecycle regression tests without sleeping.
    #[doc(hidden)]
    pub fn backdate_hold_for_test(&mut self) {
        if self.held_since.is_some() {
            self.held_since = Instant::now().checked_sub(Duration::from_millis(800));
        }
    }

    /// Test-only seam: age the last motion past [`GESTURE_REARM_PAUSE`], as if
    /// the pointer had rested, without sleeping.
    #[doc(hidden)]
    pub fn rest_for_test(&mut self) {
        if let Some(moved_at) = self.moved_at {
            self.moved_at = moved_at.checked_sub(GESTURE_REARM_PAUSE * 2);
        }
    }
}

/// The signed component of one report along `direction` (positive continues it).
fn along(direction: GestureDirection, dx: i32, dy: i32) -> i32 {
    match direction {
        GestureDirection::Left => dx.saturating_neg(),
        GestureDirection::Right => dx,
        GestureDirection::Up => dy.saturating_neg(),
        GestureDirection::Down => dy,
        GestureDirection::Click => 0,
    }
}

/// The signed component of one report across `direction` (its sideways drift).
fn across(direction: GestureDirection, dx: i32, dy: i32) -> i32 {
    match direction {
        GestureDirection::Left | GestureDirection::Right => dy,
        GestureDirection::Up | GestureDirection::Down => dx,
        GestureDirection::Click => 0,
    }
}

/// How far summed travel has moved back against the committed `direction`.
fn reversal(direction: GestureDirection, dx: i32, dy: i32) -> i32 {
    match direction {
        GestureDirection::Left => dx,
        GestureDirection::Right => dx.saturating_neg(),
        GestureDirection::Up => dy,
        GestureDirection::Down => dy.saturating_neg(),
        GestureDirection::Click => 0,
    }
    .max(0)
}

/// Whether one motion report keeps heading in the already-committed direction
/// (dominant along it), rather than reversing or turning.
fn continues(direction: GestureDirection, dx: i32, dy: i32) -> bool {
    let (ax, ay) = (dx.saturating_abs(), dy.saturating_abs());
    match direction {
        GestureDirection::Left => dx < 0 && ax >= ay,
        GestureDirection::Right => dx > 0 && ax >= ay,
        GestureDirection::Up => dy < 0 && ay >= ax,
        GestureDirection::Down => dy > 0 && ay >= ax,
        GestureDirection::Click => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn configured() -> GesturePolicy {
        GesturePolicy::for_bindings(&BTreeMap::from([
            (GestureDirection::Right, Action::Copy),
            (GestureDirection::Left, Action::Paste),
            (GestureDirection::Up, Action::VolumeUp),
        ]))
    }

    #[test]
    fn paused_pending_stroke_requires_fresh_continuation() {
        let now = Instant::now();
        let mut policy = configured();
        policy.durations = [Duration::from_millis(400); 4];
        let mut acc = SwipeAccumulator::default();
        acc.begin_at(now, policy);
        assert_eq!(acc.accumulate_at(now, 50, 0), Some(GestureDirection::Right));
        assert_eq!(
            acc.accumulate_at(now + Duration::from_millis(250), 50, 0),
            None
        );
        assert_eq!(
            acc.accumulate_at(now + Duration::from_millis(400), 0, -1),
            None
        );
        assert_eq!(
            acc.accumulate_at(now + Duration::from_millis(401), 0, -49),
            Some(GestureDirection::Up)
        );
    }

    #[test]
    fn paused_pending_stroke_preserves_geometry() {
        let now = Instant::now();
        let mut policy = configured();
        policy.durations = [Duration::from_millis(400); 4];
        let mut acc = SwipeAccumulator::default();
        acc.begin_at(now, policy);
        assert_eq!(acc.accumulate_at(now, 50, 0), Some(GestureDirection::Right));
        assert_eq!(
            acc.accumulate_at(now + Duration::from_millis(250), 200, 60),
            None
        );
        assert_eq!(
            acc.accumulate_at(now + Duration::from_millis(400), 1, 0),
            Some(GestureDirection::Right)
        );
        assert_eq!(
            acc.accumulate_at(now + Duration::from_millis(401), 1, 0),
            None
        );
        assert!(!acc.end());
    }

    #[test]
    fn paced_progress_requires_fresh_motion_and_cannot_queue_a_burst() {
        let now = Instant::now();
        let due = now + ordinary_duration();
        let mut acc = SwipeAccumulator::default();
        acc.begin_at(now, configured());
        assert_eq!(acc.accumulate_at(now, 50, 0), Some(GestureDirection::Right));
        assert_eq!(acc.accumulate_at(now, i32::MAX, 0), None);
        assert_eq!(acc.accumulate_at(due, 0, 0), None);
        assert_eq!(acc.accumulate_at(due, 1, 0), Some(GestureDirection::Right));
        assert_eq!(acc.accumulate_at(due, 1, 0), None);
        assert_eq!(acc.accumulate_at(due + ordinary_duration(), 1, 0), None);
        // Bounding progress must retain a straight report's cross-axis ratio.
        acc.begin_at(now, configured());
        assert_eq!(acc.accumulate_at(now, 50, 0), Some(GestureDirection::Right));
        assert_eq!(acc.accumulate_at(now, i32::MAX, 1000), None);
        assert_eq!(acc.accumulate_at(due, 1, 0), Some(GestureDirection::Right));
    }

    #[test]
    fn deliberate_reversal_and_axis_change_bypass_repeat_cooldown() {
        let now = Instant::now();
        let mut acc = SwipeAccumulator::default();
        acc.begin_at(now, configured());
        for (dx, dy, expected) in [
            (50, 0, GestureDirection::Right),
            (-50, 0, GestureDirection::Left),
            (0, -50, GestureDirection::Up),
            (50, 0, GestureDirection::Right),
        ] {
            assert_eq!(acc.accumulate_at(now, dx, dy), Some(expected));
        }
    }

    #[test]
    fn unbound_and_disabled_swipes_consume_release_without_output() {
        for binding in [
            BTreeMap::new(),
            BTreeMap::from([(GestureDirection::Right, Action::None)]),
        ] {
            let now = Instant::now();
            let mut acc = SwipeAccumulator::default();
            acc.begin_at(now, GesturePolicy::for_bindings(&binding));
            assert_eq!(acc.accumulate_at(now, 50, 0), None);
            assert!(!acc.end_at(now));
            acc.begin_at(now, GesturePolicy::for_bindings(&binding));
            assert!(acc.end_at(now + Duration::from_secs(10)));
        }
    }

    #[test]
    fn rest_rearms_distance_but_does_not_bypass_same_direction_pacing() {
        let now = Instant::now();
        let mut acc = SwipeAccumulator::default();
        acc.begin_at(now, configured());
        assert_eq!(acc.accumulate_at(now, 50, 0), Some(GestureDirection::Right));
        let delay = ordinary_duration();
        let at = now + GESTURE_REARM_PAUSE;
        let expected = (GESTURE_REARM_PAUSE >= delay).then_some(GestureDirection::Right);
        assert_eq!(acc.accumulate_at(at, 50, 0), expected);
        if expected.is_none() {
            assert_eq!(
                acc.accumulate_at(now + delay, 1, 0),
                Some(GestureDirection::Right)
            );
        }
    }

    #[test]
    fn canceled_or_released_hold_has_no_pending_action() {
        let now = Instant::now();
        let mut acc = SwipeAccumulator::default();
        acc.begin_at(now, configured());
        assert_eq!(acc.accumulate_at(now, 50, 0), Some(GestureDirection::Right));
        assert_eq!(acc.accumulate_at(now, 600, 0), None);
        acc.cancel();
        assert_eq!(acc.accumulate_at(now + ordinary_duration(), 600, 0), None);
        assert!(!acc.end_at(now));
        acc.begin_at(now, configured());
        assert!(acc.end_at(now + Duration::from_secs(10)));
        assert_eq!(acc.accumulate_at(now, 50, 0), None);
    }

    #[test]
    fn paced_continuation_clears_an_abandoned_partial_reversal() {
        let now = Instant::now();
        let mut acc = SwipeAccumulator::default();
        acc.begin_at(now, configured());
        assert_eq!(acc.accumulate_at(now, 50, 0), Some(GestureDirection::Right));
        assert_eq!(acc.accumulate_at(now, -20, 0), None);
        assert_eq!(acc.accumulate_at(now, 600, 0), None);
        assert_eq!(acc.accumulate_at(now, -30, 0), None);
        assert_eq!(acc.accumulate_at(now, -20, 0), Some(GestureDirection::Left));
    }

    #[test]
    fn volume_progress_repeats_pauses_reverses_and_releases() {
        let now = Instant::now();
        let map = BTreeMap::from([
            (GestureDirection::Up, Action::VolumeUp),
            (GestureDirection::Down, Action::VolumeDown),
            (GestureDirection::Right, Action::NextDesktop),
        ]);
        let mut acc = SwipeAccumulator::default();
        acc.begin_at(now, GesturePolicy::for_bindings(&map));
        assert_eq!(acc.accumulate_at(now, 0, -50), Some(GestureDirection::Up));
        assert_eq!(acc.accumulate_at(now, 0, -5000), None);
        let due = now + volume_duration();
        assert_eq!(acc.accumulate_at(due, 0, 0), None);
        assert_eq!(acc.accumulate_at(due, 0, -1), Some(GestureDirection::Up));
        assert_eq!(acc.accumulate_at(due, 0, 25), None);
        assert_eq!(acc.accumulate_at(due, 0, 25), Some(GestureDirection::Down));
        // Cross-axis actions retain their ordinary policy and the same hold.
        assert_eq!(acc.accumulate_at(due, 50, 0), Some(GestureDirection::Right));
        assert_eq!(acc.accumulate_at(due, 600, 0), None);
        assert_eq!(acc.accumulate_at(due, 0, -50), Some(GestureDirection::Up));
        assert!(!acc.end_at(due));
        assert_eq!(acc.accumulate_at(due + ordinary_duration(), 0, -5000), None);
    }

    #[test]
    fn only_explicit_volume_bindings_get_volume_response() {
        let map = BTreeMap::from([
            (GestureDirection::Up, Action::VolumeUp),
            (GestureDirection::Down, Action::Copy),
        ]);
        let policy = GesturePolicy::for_bindings(&map);
        assert_eq!(policy.repeat_distances, [50, 600, 600, 600]);
        assert_eq!(policy.durations[0], volume_duration());
        assert_eq!(policy.durations[1], ordinary_duration());
    }

    // ── Gesture classification ────────────────────────────────────────────────

    #[test]
    fn detect_swipe_below_threshold_keeps_accumulating() {
        // Too little travel to commit — caller keeps summing raw-XY.
        assert_eq!(detect_swipe(40, 5), None);
        assert_eq!(detect_swipe(0, 0), None);
    }

    #[test]
    fn detect_swipe_commits_clean_direction() {
        assert_eq!(detect_swipe(120, 5), Some(GestureDirection::Right));
        assert_eq!(detect_swipe(-120, 5), Some(GestureDirection::Left));
        assert_eq!(detect_swipe(5, 120), Some(GestureDirection::Down));
        assert_eq!(detect_swipe(5, -120), Some(GestureDirection::Up));
    }

    #[test]
    fn detect_swipe_rejects_diagonal() {
        // Past the threshold but too diagonal (cross axis beyond the band).
        assert_eq!(detect_swipe(60, 60), None);
        assert_eq!(detect_swipe(-60, -60), None);
    }

    #[test]
    fn detect_swipe_threshold_and_cross_band_boundaries() {
        // The threshold bound is inclusive (`< THRESHOLD` rejects), so exactly at
        // it commits and one below does not.
        assert_eq!(
            detect_swipe(GESTURE_SWIPE_THRESHOLD, 0),
            Some(GestureDirection::Right)
        );
        assert_eq!(detect_swipe(GESTURE_SWIPE_THRESHOLD - 1, 0), None);

        // The cross-axis band is max(deadzone, 35% of dominant). For a large
        // dominant the 35% term wins (200 → 70): 69 commits, 71 is too diagonal.
        assert_eq!(detect_swipe(200, 69), Some(GestureDirection::Right));
        assert_eq!(detect_swipe(200, 71), None);
        // For a small dominant the 40-unit floor wins (100 → max(40, 35) = 40).
        assert_eq!(detect_swipe(100, 39), Some(GestureDirection::Right));
        assert_eq!(detect_swipe(100, 41), None);
    }

    #[test]
    fn detect_swipe_does_not_panic_on_extreme_values() {
        // Saturated accumulator travel can reach the i32 bounds. `i32::MIN.abs()`
        // panics and `dominant * 35` overflows — both must be clamped, not crash.
        assert_eq!(detect_swipe(i32::MAX, 0), Some(GestureDirection::Right));
        assert_eq!(detect_swipe(i32::MIN, 0), Some(GestureDirection::Left));
        assert_eq!(detect_swipe(0, i32::MAX), Some(GestureDirection::Down));
        assert_eq!(detect_swipe(0, i32::MIN), Some(GestureDirection::Up));
        // A diagonal at the extremes is still rejected, without panicking.
        assert_eq!(detect_swipe(i32::MIN, i32::MIN), None);
    }

    // ── SwipeAccumulator (the shared mid-swipe state machine) ─────────────────

    #[test]
    fn accumulator_first_direction_does_not_repeat_below_repeat_distance() {
        let mut acc = SwipeAccumulator::default();
        acc.begin();
        acc.backdate_hold_for_test();
        // A clear rightward swipe commits exactly once, mid-motion.
        assert_eq!(
            acc.accumulate(GESTURE_SWIPE_THRESHOLD + 10, 0),
            Some(GestureDirection::Right)
        );
        // Continuing the same swipe must not re-fire.
        assert_eq!(acc.accumulate(50, 0), None);
    }

    #[test]
    fn accumulator_chains_reversals_within_one_hold() {
        let mut acc = SwipeAccumulator::default();
        acc.begin();
        acc.backdate_hold_for_test();
        let t = GESTURE_SWIPE_THRESHOLD + 10;
        // Each reversal fires at the normal threshold (issue #1634).
        assert_eq!(acc.accumulate(-t, 0), Some(GestureDirection::Left));
        assert_eq!(acc.accumulate(t, 0), Some(GestureDirection::Right));
        assert_eq!(acc.accumulate(-t, 0), Some(GestureDirection::Left));
        assert!(!acc.end(), "a hold that swiped is not a click");
    }

    #[test]
    fn accumulator_continuous_swipe_keeps_stepping() {
        let mut acc = SwipeAccumulator::default();
        acc.begin();
        acc.backdate_hold_for_test();
        assert_eq!(
            acc.accumulate(GESTURE_SWIPE_THRESHOLD + 10, 0),
            Some(GestureDirection::Right)
        );
        // Keep going right without stopping, drifting a little upward: one more
        // Right per GESTURE_REPEAT_DISTANCE, and the drift never fires Up.
        let mut fired = Vec::new();
        for _ in 0..(2 * GESTURE_REPEAT_DISTANCE / 10) {
            fired.extend(acc.accumulate(10, -3));
        }
        assert_eq!(fired, [GestureDirection::Right, GestureDirection::Right]);
    }

    #[test]
    fn accumulator_diagonal_motion_does_not_repeat() {
        let mut acc = SwipeAccumulator::default();
        acc.begin();
        acc.backdate_hold_for_test();
        assert_eq!(
            acc.accumulate(GESTURE_SWIPE_THRESHOLD + 10, 0),
            Some(GestureDirection::Right)
        );
        // Each report leans right, but together they are too diagonal to be
        // a swipe, so they must not repeat Right.
        for _ in 0..(GESTURE_REPEAT_DISTANCE / 10 + 5) {
            assert_eq!(acc.accumulate(10, 9), None);
        }
    }

    #[test]
    fn accumulator_large_report_carries_into_the_next_repeat() {
        let mut acc = SwipeAccumulator::default();
        acc.begin();
        acc.backdate_hold_for_test();
        assert_eq!(
            acc.accumulate(GESTURE_SWIPE_THRESHOLD + 10, 0),
            Some(GestureDirection::Right)
        );
        // One report spanning two repeat distances: one Right now, and the
        // carried excess fires the second on the next report.
        assert_eq!(
            acc.accumulate(2 * GESTURE_REPEAT_DISTANCE, 0),
            Some(GestureDirection::Right)
        );
        assert_eq!(acc.accumulate(1, 0), Some(GestureDirection::Right));
    }

    #[test]
    fn accumulator_carried_travel_keeps_its_drift() {
        let mut acc = SwipeAccumulator::default();
        acc.begin();
        acc.backdate_hold_for_test();
        assert_eq!(
            acc.accumulate(GESTURE_SWIPE_THRESHOLD + 10, 0),
            Some(GestureDirection::Right)
        );
        assert_eq!(acc.accumulate(GESTURE_REPEAT_DISTANCE - 10, 0), None);
        // Straight enough overall to repeat, but the carried tail is this
        // diagonal report, so it must not fire again on a nudge.
        assert_eq!(
            acc.accumulate(
                GESTURE_REPEAT_DISTANCE + 10,
                GESTURE_REPEAT_DISTANCE * 2 / 5
            ),
            Some(GestureDirection::Right)
        );
        assert_eq!(acc.accumulate(1, 0), None);
    }

    #[test]
    fn accumulator_backtracking_takes_back_drift() {
        let mut acc = SwipeAccumulator::default();
        acc.begin();
        acc.backdate_hold_for_test();
        assert_eq!(
            acc.accumulate(GESTURE_SWIPE_THRESHOLD + 10, 0),
            Some(GestureDirection::Right)
        );
        // A repeat distance along with 40% of it as drift: too diagonal to
        // repeat yet.
        for _ in 0..(GESTURE_REPEAT_DISTANCE / 10) {
            assert_eq!(acc.accumulate(10, -4), None);
        }
        // Backing up undoes drift too, so the corrected stroke repeats.
        assert_eq!(acc.accumulate(-20, 40), None);
        assert_eq!(acc.accumulate(20, 0), Some(GestureDirection::Right));
    }

    #[test]
    fn accumulator_keeps_sideways_drift_at_zero_progress() {
        let mut acc = SwipeAccumulator::default();
        acc.begin();
        acc.backdate_hold_for_test();
        assert_eq!(
            acc.accumulate(GESTURE_SWIPE_THRESHOLD + 10, 0),
            Some(GestureDirection::Right)
        );
        // 1.2 repeat distances along: the last report's drift alone is
        // straight enough to repeat, but with the sideways report at zero
        // progress the stroke is too diagonal. The sideways report stays
        // under the threshold so it commits no Up of its own.
        let half = GESTURE_REPEAT_DISTANCE * 3 / 5;
        assert_eq!(acc.accumulate(0, -40), None);
        assert_eq!(acc.accumulate(half, 0), None);
        assert_eq!(
            acc.accumulate(half, -(GESTURE_REPEAT_DISTANCE * 2 / 5 - 20)),
            None
        );
    }

    #[test]
    fn accumulator_overshoot_alone_does_not_repeat() {
        let mut acc = SwipeAccumulator::default();
        acc.begin();
        acc.backdate_hold_for_test();
        assert_eq!(
            acc.accumulate(GESTURE_SWIPE_THRESHOLD + 10, 0),
            Some(GestureDirection::Right)
        );
        assert_eq!(acc.accumulate(GESTURE_REPEAT_DISTANCE - 1, 0), None);
    }

    #[test]
    fn accumulator_reverses_from_the_turning_point_after_an_overshoot() {
        let mut acc = SwipeAccumulator::default();
        acc.begin();
        acc.backdate_hold_for_test();
        assert_eq!(
            acc.accumulate(-(GESTURE_SWIPE_THRESHOLD + 10), 0),
            Some(GestureDirection::Left)
        );
        // Overshoot 100 units left, then turn: the normal threshold suffices.
        for _ in 0..10 {
            assert_eq!(acc.accumulate(-10, 0), None);
        }
        assert_eq!(
            acc.accumulate(GESTURE_SWIPE_THRESHOLD + 10, 0),
            Some(GestureDirection::Right)
        );
    }

    #[test]
    fn accumulator_reversal_survives_jitter_back_toward_the_last_swipe() {
        let mut acc = SwipeAccumulator::default();
        acc.begin();
        acc.backdate_hold_for_test();
        assert_eq!(
            acc.accumulate(GESTURE_SWIPE_THRESHOLD + 10, 0),
            Some(GestureDirection::Right)
        );
        // Left 30, a one-unit rightward jitter packet, then left 30 more.
        assert_eq!(acc.accumulate(-30, 0), None);
        assert_eq!(acc.accumulate(1, 0), None);
        assert_eq!(acc.accumulate(-30, 0), Some(GestureDirection::Left));
    }

    #[test]
    fn accumulator_reversal_survives_a_rest_halfway() {
        let mut acc = SwipeAccumulator::default();
        acc.begin();
        acc.backdate_hold_for_test();
        assert_eq!(
            acc.accumulate(GESTURE_SWIPE_THRESHOLD + 10, 0),
            Some(GestureDirection::Right)
        );
        assert_eq!(acc.accumulate(-30, 0), None);
        acc.rest_for_test();
        assert_eq!(acc.accumulate(-30, 0), Some(GestureDirection::Left));
    }

    #[test]
    fn accumulator_repeats_a_direction_after_a_rest() {
        let mut acc = SwipeAccumulator::default();
        acc.begin();
        acc.backdate_hold_for_test();
        let t = GESTURE_SWIPE_THRESHOLD + 10;
        assert_eq!(acc.accumulate(t, 0), Some(GestureDirection::Right));
        assert_eq!(acc.accumulate(t, 0), None, "still the same stroke");
        acc.rest_for_test();
        assert_eq!(acc.accumulate(t, 0), Some(GestureDirection::Right));
    }

    #[test]
    fn accumulator_repeat_after_a_rest_ignores_an_abandoned_reversal() {
        let mut acc = SwipeAccumulator::default();
        acc.begin();
        acc.backdate_hold_for_test();
        let t = GESTURE_SWIPE_THRESHOLD + 10;
        assert_eq!(acc.accumulate(t, 0), Some(GestureDirection::Right));
        assert_eq!(acc.accumulate(-30, 0), None);
        acc.rest_for_test();
        assert_eq!(
            acc.accumulate(GESTURE_SWIPE_THRESHOLD, 0),
            Some(GestureDirection::Right)
        );
    }

    #[test]
    fn accumulator_backtracking_counts_against_a_reversal() {
        let mut acc = SwipeAccumulator::default();
        acc.begin();
        acc.backdate_hold_for_test();
        let t = GESTURE_SWIPE_THRESHOLD + 10;
        assert_eq!(acc.accumulate(t, 0), Some(GestureDirection::Right));
        // Net Left travel is only 40.
        assert_eq!(acc.accumulate(-30, 0), None);
        assert_eq!(acc.accumulate(25, 0), None);
        assert_eq!(acc.accumulate(-35, 0), None);
    }

    #[test]
    fn accumulator_net_zero_wobble_after_a_swipe_never_fires() {
        let mut acc = SwipeAccumulator::default();
        acc.begin();
        acc.backdate_hold_for_test();
        let t = GESTURE_SWIPE_THRESHOLD + 10;
        assert_eq!(acc.accumulate(t, 0), Some(GestureDirection::Right));
        for _ in 0..20 {
            assert_eq!(acc.accumulate(-15, 0), None);
            assert_eq!(acc.accumulate(15, 0), None);
        }
    }

    #[test]
    fn accumulator_deliberate_travel_commits_without_a_hold_gate() {
        let mut acc = SwipeAccumulator::default();
        acc.begin();
        assert_eq!(acc.accumulate(49, 0), None);
        assert_eq!(acc.accumulate(1, 0), Some(GestureDirection::Right));
    }

    #[test]
    fn accumulator_end_reports_click_only_when_no_swipe_fired() {
        // A hold with only tiny drift never commits → end() is a click.
        let mut acc = SwipeAccumulator::default();
        acc.begin();
        acc.backdate_hold_for_test();
        assert_eq!(acc.accumulate(2, -1), None);
        assert!(acc.end(), "a hold that never swiped is a click");

        // A hold that committed a swipe → end() is not a click.
        acc.begin();
        acc.backdate_hold_for_test();
        assert!(acc.accumulate(GESTURE_SWIPE_THRESHOLD + 10, 0).is_some());
        assert!(!acc.end(), "a committed swipe must not also click");
    }

    #[test]
    fn accumulator_ignores_motion_when_not_holding() {
        let mut acc = SwipeAccumulator::default();
        assert!(!acc.is_holding());
        // Travel outside a hold is dropped, never committing a stray swipe.
        assert_eq!(acc.accumulate(GESTURE_SWIPE_THRESHOLD + 100, 0), None);
    }

    #[test]
    fn accumulator_sums_sub_threshold_deltas_until_they_commit() {
        // The whole reason for an accumulator (vs. detect_swipe on one delta):
        // several deltas each too small to commit on their own must sum across
        // the hold until the running total crosses the threshold, then commit.
        let mut acc = SwipeAccumulator::default();
        acc.begin();
        acc.backdate_hold_for_test();
        // Just under half the threshold: one or two steps never reach it, three do.
        let step = GESTURE_SWIPE_THRESHOLD / 2 - 1;
        assert_eq!(acc.accumulate(step, 0), None, "one step is sub-threshold");
        assert_eq!(acc.accumulate(step, 0), None, "two steps still under");
        assert_eq!(
            acc.accumulate(step, 0),
            Some(GestureDirection::Right),
            "the running sum finally crosses the threshold"
        );
    }

    #[test]
    fn accumulator_saturates_instead_of_overflowing() {
        // The doc promises an arbitrarily long hold can't overflow. A perfect
        // diagonal never commits, so travel keeps summing; feed deltas that would
        // overflow both an i32 sum and a naive cross-band multiply — both must
        // saturate, not panic (debug builds panic on overflow).
        let mut acc = SwipeAccumulator::default();
        acc.begin();
        acc.backdate_hold_for_test();
        assert_eq!(
            acc.accumulate(i32::MAX, i32::MAX),
            None,
            "a diagonal never commits"
        );
        assert_eq!(
            acc.accumulate(i32::MAX, i32::MAX),
            None,
            "the saturating sum must not panic"
        );
        acc.begin();
        assert_eq!(acc.accumulate(i32::MIN, i32::MIN), None);
        acc.rest_for_test();
        assert_eq!(acc.accumulate(i32::MIN, i32::MIN), None);
        // A clean axis on a fresh hold still commits with a saturated magnitude.
        acc.begin();
        acc.backdate_hold_for_test();
        assert_eq!(acc.accumulate(i32::MAX, 0), Some(GestureDirection::Right));
    }

    #[test]
    fn accumulator_begin_recovers_a_stale_hold() {
        // A missed release (e.g. focus loss between press and release) can leave
        // a dangling hold that already fired with travel in some direction. A
        // fresh begin() must wipe both the `fired` latch and the travel, so the
        // next press isn't poisoned by the old one.
        let mut acc = SwipeAccumulator::default();
        acc.begin();
        acc.backdate_hold_for_test();
        // Stale hold commits LEFT (negative dx) and latches `fired`.
        assert_eq!(
            acc.accumulate(-(GESTURE_SWIPE_THRESHOLD + 10), 0),
            Some(GestureDirection::Left)
        );
        // No end() — a dropped release, then a fresh press.
        acc.begin();
        acc.backdate_hold_for_test();
        // Had `fired` leaked this would be None; had the negative travel leaked it
        // would commit Left. Committing Right proves begin() reset both.
        assert_eq!(
            acc.accumulate(GESTURE_SWIPE_THRESHOLD + 10, 0),
            Some(GestureDirection::Right)
        );
    }

    #[test]
    fn accumulator_end_without_a_hold_is_not_a_click() {
        // end() in isolation (no begin) must not claim a click — there was no
        // hold — so a stray release can't be read as a press.
        let mut acc = SwipeAccumulator::default();
        assert!(!acc.end(), "a release with no hold is not a click");
        // A redundant second release after a real hold already ended is inert too.
        acc.begin();
        assert!(acc.end(), "the held release is a click");
        assert!(!acc.end(), "the redundant second release is not a click");
    }
}
