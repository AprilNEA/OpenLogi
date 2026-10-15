//! Report-by-report gesture recognition, shared by HID++ and OS capture.

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use super::{Action, GestureDirection};

const REST: Duration = Duration::from_millis(500);

/// Runtime response and action timing. This does not change the stored binding
/// schema; a capture plan derives it from the effective direction actions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GesturePolicy {
    start_threshold: i32,
    action_threshold: i32,
    durations: [Duration; 4],
    direction_locked: bool,
    horizontal: bool,
    vertical: bool,
}

impl Default for GesturePolicy {
    fn default() -> Self {
        Self {
            start_threshold: 50,
            action_threshold: 250,
            durations: [Duration::ZERO; 4],
            direction_locked: false,
            horizontal: true,
            vertical: true,
        }
    }
}

#[derive(Clone, Copy)]
enum CustomGestureTiming {
    Windows,
    /// macOS behavior; Linux explicitly adopts this compatible policy.
    MacosAndLinux,
}

impl CustomGestureTiming {
    const HOST: Self = if cfg!(target_os = "windows") {
        Self::Windows
    } else {
        Self::MacosAndLinux
    };

    fn duration(self, action: &Action) -> Duration {
        let volume = matches!(action, Action::VolumeUp | Action::VolumeDown);
        Duration::from_millis(match (self, volume) {
            (Self::Windows, true) => 10,
            (Self::Windows, false) => 200,
            (Self::MacosAndLinux, true) => 20,
            (Self::MacosAndLinux, false) => 400,
        })
    }
}

impl GesturePolicy {
    /// Generic custom bindings use Windows 10/200 ms or macOS 20/400 ms
    /// volume/other timing. Linux explicitly adopts the macOS policy.
    #[must_use]
    pub fn for_bindings(bindings: &BTreeMap<GestureDirection, Action>) -> Self {
        Self::for_bindings_with_timing(bindings, CustomGestureTiming::HOST)
    }

    fn for_bindings_with_timing(
        bindings: &BTreeMap<GestureDirection, Action>,
        timing: CustomGestureTiming,
    ) -> Self {
        let mut policy = Self::default();
        for (direction, action) in bindings {
            policy.set_duration(*direction, timing.duration(action));
        }
        policy
    }

    /// Override one direction's post-action duration. Click has no duration.
    pub fn set_duration(&mut self, direction: GestureDirection, duration: Duration) {
        if let Some(index) = direction_index(direction) {
            self.durations[index] = duration;
        }
    }

    /// Apply the literal speed-factor formula to any finite value. Negative
    /// factors make thresholds larger. Reject non-finite input without changing
    /// the policy; truncate and bound computed thresholds for integer safety.
    pub fn set_speed_factor(&mut self, factor: f64) -> bool {
        if !factor.is_finite() {
            return false;
        }
        self.start_threshold = bounded_count(50.0 * (1.0 - factor));
        self.action_threshold = bounded_count(250.0 * (1.0 - factor));
        true
    }

    /// Retain the recognized axis after each action when locking is enabled.
    pub fn set_direction_lock(&mut self, enabled: bool) {
        self.direction_locked = enabled;
    }

    /// Permit movement on the horizontal axis.
    pub fn set_horizontal_enabled(&mut self, enabled: bool) {
        self.horizontal = enabled;
    }

    /// Permit movement on the vertical axis.
    pub fn set_vertical_enabled(&mut self, enabled: bool) {
        self.vertical = enabled;
    }
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

#[expect(
    clippy::cast_possible_truncation,
    reason = "clamped finite input, truncation is the response rule"
)]
fn bounded_count(value: f64) -> i32 {
    value.clamp(f64::from(i32::MIN), f64::from(i32::MAX)) as i32
}

fn response(count: i32) -> i32 {
    super::accelerate_report_delta(count)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Axis {
    Horizontal,
    Vertical,
}

fn recognize(x: i32, y: i32, threshold: i32) -> Option<Axis> {
    let x = x.saturating_abs();
    let y = y.saturating_abs();
    if x > y && x >= threshold {
        Some(Axis::Horizontal)
    } else if y > x && y >= threshold {
        Some(Axis::Vertical)
    } else {
        None
    }
}

#[derive(Debug)]
struct Hold {
    started: Instant,
    last_report: Option<Instant>,
    blocked_until: Option<Instant>,
    first_report: bool,
    had_gesture: bool,
    axis: Option<Axis>,
    primary: [i32; 2],
    secondary: [i32; 2],
}

impl Hold {
    fn restart(&mut self) {
        self.axis = None;
        self.primary = [0; 2];
        self.secondary = [0; 2];
    }
}

/// One physical hold with recognition and action stages. It never schedules
/// stationary repeats; each output requires new movement on a later report.
#[derive(Debug, Default)]
pub struct SwipeAccumulator {
    policy: GesturePolicy,
    hold: Option<Hold>,
}

impl SwipeAccumulator {
    /// Begin a hold with the default response policy and zero action duration.
    pub fn begin(&mut self) {
        self.begin_at(Instant::now(), GesturePolicy::default());
    }

    /// Begin using a capture plan's runtime policy.
    pub fn begin_with_policy(&mut self, policy: GesturePolicy) {
        self.begin_at(Instant::now(), policy);
    }

    /// Explicit monotonic-time entry point for deterministic replay.
    pub fn begin_at(&mut self, now: Instant, policy: GesturePolicy) {
        self.policy = policy;
        self.hold = Some(Hold {
            started: now,
            last_report: None,
            blocked_until: None,
            first_report: true,
            had_gesture: false,
            axis: None,
            primary: [0; 2],
            secondary: [0; 2],
        });
    }

    /// Whether a physical hold is active.
    #[must_use]
    pub fn is_holding(&self) -> bool {
        self.hold.is_some()
    }

    /// Feed one progress report using the host's monotonic clock.
    pub fn accumulate(&mut self, dx: i32, dy: i32) -> Option<GestureDirection> {
        self.accumulate_at(Instant::now(), dx, dy)
    }

    /// Feed one progress report at an explicit monotonic instant. Discarded
    /// reports still count as input activity for the rest restart.
    pub fn accumulate_at(&mut self, now: Instant, dx: i32, dy: i32) -> Option<GestureDirection> {
        let hold = self.hold.as_mut()?;
        let rested = hold
            .last_report
            .is_some_and(|last| now.saturating_duration_since(last) > REST);
        hold.last_report = Some(now);
        if hold.first_report {
            hold.first_report = false;
            return None;
        }
        if hold.blocked_until.is_some_and(|until| now < until) {
            return None;
        }
        if rested && !self.policy.direction_locked {
            hold.restart();
        }
        let (x, y) = (i64::from(dx).abs(), i64::from(dy).abs());
        let movement = if x >= y * 2 && self.policy.horizontal {
            [response(dx), 0]
        } else if y >= x * 2 && self.policy.vertical {
            [0, response(dy)]
        } else {
            [0; 2]
        };
        let Some(axis) = hold.axis else {
            for (total, delta) in hold.primary.iter_mut().zip(movement) {
                *total = total.saturating_add(delta);
            }
            if let Some(axis) = recognize(
                hold.primary[0],
                hold.primary[1],
                self.policy.start_threshold,
            ) {
                hold.axis = Some(axis);
                hold.had_gesture = true;
                hold.primary = [0; 2];
                hold.secondary = [0; 2];
            }
            return None;
        };
        let selected = usize::from(axis == Axis::Vertical);
        hold.primary[selected] = hold.primary[selected].saturating_add(movement[selected]);
        if !self.policy.direction_locked {
            let cross = 1 - selected;
            hold.secondary[cross] = hold.secondary[cross].saturating_add(movement[cross]);
            if let Some(new_axis) = recognize(
                hold.secondary[0],
                hold.secondary[1],
                self.policy.start_threshold,
            ) {
                hold.axis = Some(new_axis);
                hold.primary = hold.secondary;
                hold.secondary = [0; 2];
                return None;
            }
        }
        let count = hold.primary[selected];
        if count.saturating_abs() <= self.policy.action_threshold {
            return None;
        }
        let direction = match (axis, count > 0) {
            (Axis::Horizontal, true) => GestureDirection::Right,
            (Axis::Horizontal, false) => GestureDirection::Left,
            (Axis::Vertical, true) => GestureDirection::Down,
            (Axis::Vertical, false) => GestureDirection::Up,
        };
        let duration = self.policy.durations[match axis {
            Axis::Horizontal => {
                if count > 0 {
                    3
                } else {
                    2
                }
            }
            Axis::Vertical => usize::from(count > 0),
        }];
        hold.blocked_until = now.checked_add(duration);
        if self.policy.direction_locked {
            hold.primary = [0; 2];
            hold.secondary = [0; 2];
        } else {
            hold.restart();
        }
        Some(direction)
    }

    /// Release; a click is legal only before recognition and within 500 ms.
    pub fn end(&mut self) -> bool {
        self.end_at(Instant::now())
    }

    /// Release at an explicit monotonic instant.
    pub fn end_at(&mut self, now: Instant) -> bool {
        self.hold.take().is_some_and(|hold| {
            !hold.had_gesture && now.saturating_duration_since(hold.started) <= REST
        })
    }

    /// Cancel without emitting a click.
    pub fn cancel(&mut self) {
        self.hold = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn started() -> (SwipeAccumulator, Instant) {
        let now = Instant::now();
        let mut acc = SwipeAccumulator::default();
        acc.begin_at(now, GesturePolicy::default());
        (acc, now)
    }

    #[test]
    fn first_report_and_recognition_are_not_actions_without_a_hold_gate() {
        let (mut acc, now) = started();
        assert_eq!(acc.accumulate_at(now, 10_000, 0), None);
        assert_eq!(acc.accumulate_at(now, 17, 0), None);
        assert_eq!(acc.accumulate_at(now, 52, 0), Some(GestureDirection::Right));
        assert!(!acc.end_at(now));
    }

    #[test]
    fn per_report_response_rejects_diagonals_and_does_not_power_the_sum() {
        let (mut acc, now) = started();
        acc.accumulate_at(now, 0, 0);
        acc.accumulate_at(now, 100, 51);
        assert_eq!(acc.hold.as_ref().unwrap().primary, [0, 0]);
        for _ in 0..4 {
            acc.accumulate_at(now, 5, 0);
        }
        assert_eq!(acc.hold.as_ref().unwrap().primary, [36, 0]);
        acc.accumulate_at(now, 10, 5); // inclusive 2:1 ratio
        assert_eq!(acc.hold.as_ref().unwrap().axis, Some(Axis::Horizontal));
        assert_eq!(response(i32::MIN), i32::MIN);
        assert_eq!(response(i32::MAX), i32::MAX);
    }

    #[test]
    fn recognition_includes_threshold_but_action_excludes_equality() {
        let (mut acc, now) = started();
        acc.accumulate_at(now, 0, 0);
        for _ in 0..49 {
            assert_eq!(acc.accumulate_at(now, 1, 0), None);
        }
        assert_eq!(acc.hold.as_ref().unwrap().axis, None);
        assert_eq!(acc.accumulate_at(now, 1, 0), None);
        assert_eq!(acc.hold.as_ref().unwrap().axis, Some(Axis::Horizontal));
        for _ in 0..250 {
            assert_eq!(acc.accumulate_at(now, 1, 0), None);
        }
        assert_eq!(acc.accumulate_at(now, 1, 0), Some(GestureDirection::Right));
        assert_eq!(acc.hold.as_ref().unwrap().axis, None);
        assert_eq!(recognize(50, 50, 50), None);
    }

    #[test]
    fn volume_repeats_and_reverses_only_after_new_recognition_and_duration() {
        let now = Instant::now();
        let mut acc = SwipeAccumulator::default();
        let actions = BTreeMap::from([
            (GestureDirection::Up, Action::VolumeUp),
            (GestureDirection::Down, Action::VolumeDown),
            (GestureDirection::Right, Action::Copy),
        ]);
        let host_durations = if cfg!(target_os = "windows") {
            (10, 200)
        } else {
            (20, 400)
        };
        for (policy, volume_ms, other_ms) in [
            (
                GesturePolicy::for_bindings_with_timing(&actions, CustomGestureTiming::Windows),
                10,
                200,
            ),
            (
                GesturePolicy::for_bindings_with_timing(
                    &actions,
                    CustomGestureTiming::MacosAndLinux,
                ),
                20,
                400,
            ),
            (
                GesturePolicy::for_bindings(&actions),
                host_durations.0,
                host_durations.1,
            ),
        ] {
            acc.begin_at(now, policy);
            acc.accumulate_at(now, 0, 0);
            let mut at = now;
            for sign in [-1, -1, 1, 1] {
                assert_eq!(acc.accumulate_at(at, 0, sign * 17), None);
                assert_eq!(
                    acc.accumulate_at(at, 0, sign * 52),
                    Some(if sign < 0 {
                        GestureDirection::Up
                    } else {
                        GestureDirection::Down
                    })
                );
                assert_eq!(
                    acc.accumulate_at(at + Duration::from_millis(volume_ms - 1), 0, i32::MAX),
                    None
                );
                at += Duration::from_millis(volume_ms);
            }
            // A non-volume action uses its own longer window, also strictly
            // excluding reports before expiry and accepting the exact boundary.
            assert_eq!(acc.accumulate_at(at, 17, 0), None);
            assert_eq!(acc.accumulate_at(at, 52, 0), Some(GestureDirection::Right));
            assert_eq!(
                acc.accumulate_at(at + Duration::from_millis(other_ms - 1), i32::MAX, 0),
                None
            );
            at += Duration::from_millis(other_ms);
            assert_eq!(acc.accumulate_at(at, -17, 0), None);
            assert_eq!(acc.accumulate_at(at, -52, 0), Some(GestureDirection::Left));
            assert!(!acc.end_at(at));
        }
    }

    #[test]
    fn a_large_report_cannot_create_excess_triggers() {
        let (mut acc, now) = started();
        acc.accumulate_at(now, 0, 0);
        assert_eq!(acc.accumulate_at(now, i32::MAX, 0), None);
        assert_eq!(
            acc.accumulate_at(now, i32::MAX, 0),
            Some(GestureDirection::Right)
        );
        assert_eq!(acc.accumulate_at(now, 0, 0), None);
        assert_eq!(acc.accumulate_at(now, 52, 0), None);
        assert_eq!(acc.accumulate_at(now, -52, 0), Some(GestureDirection::Left));
    }

    #[test]
    fn unlocked_axis_changes_preserve_the_correct_coordinate() {
        let (mut acc, now) = started();
        acc.accumulate_at(now, 0, 0);
        acc.accumulate_at(now, 17, 0);
        assert_eq!(acc.accumulate_at(now, 0, -17), None);
        assert_eq!(acc.hold.as_ref().unwrap().primary, [0, -52]);
        assert_eq!(acc.accumulate_at(now, 0, -45), Some(GestureDirection::Up));
        acc.accumulate_at(now, 0, 17);
        assert_eq!(acc.accumulate_at(now, -17, 0), None);
        assert_eq!(acc.hold.as_ref().unwrap().primary, [-52, 0]);
        assert_eq!(acc.accumulate_at(now, -45, 0), Some(GestureDirection::Left));
    }

    #[test]
    fn rest_restart_is_strictly_after_500_ms() {
        for (gap, expected) in [(500, Some(GestureDirection::Up)), (501, None)] {
            let (mut acc, now) = started();
            acc.accumulate_at(now, 0, 0);
            acc.accumulate_at(now, 0, -17);
            assert_eq!(
                acc.accumulate_at(now + Duration::from_millis(gap), 0, -52),
                expected
            );
            assert!(!acc.end_at(now + Duration::from_millis(gap)));
        }
    }

    #[test]
    fn release_click_deadline_and_cancellation() {
        for (duration, click) in [(500, true), (501, false)] {
            let (mut acc, now) = started();
            assert_eq!(acc.end_at(now + Duration::from_millis(duration)), click);
            assert!(!acc.end_at(now));
        }
        let (mut acc, now) = started();
        acc.accumulate_at(now, 0, 0);
        acc.accumulate_at(now, 17, 0);
        assert!(!acc.end_at(now));
        acc.begin_at(now, GesturePolicy::default());
        acc.cancel();
        assert!(!acc.end_at(now));
        assert_eq!(acc.accumulate_at(now, 100, 0), None);
    }

    #[test]
    fn finite_speed_factors_preserve_the_literal_threshold_formula() {
        let mut policy = GesturePolicy::default();
        assert!(policy.set_speed_factor(-0.5));
        assert_eq!((policy.start_threshold, policy.action_threshold), (75, 375));
        assert!(policy.set_speed_factor(1.5));
        assert_eq!(
            (policy.start_threshold, policy.action_threshold),
            (-25, -125)
        );
        assert!(!policy.set_speed_factor(f64::INFINITY));
        assert!(!policy.set_speed_factor(f64::NAN));
        assert_eq!(
            (policy.start_threshold, policy.action_threshold),
            (-25, -125)
        );
        assert!(policy.set_speed_factor(-f64::MAX));
        assert_eq!(
            (policy.start_threshold, policy.action_threshold),
            (i32::MAX, i32::MAX)
        );
    }

    #[test]
    fn locking_and_permitted_axes_are_explicit_policy() {
        let now = Instant::now();
        let mut policy = GesturePolicy::default();
        policy.set_direction_lock(true);
        policy.set_horizontal_enabled(false);
        assert!(!policy.set_speed_factor(f64::NAN));
        assert!(policy.set_speed_factor(0.5));
        assert_eq!((policy.start_threshold, policy.action_threshold), (25, 125));
        let mut acc = SwipeAccumulator::default();
        acc.begin_at(now, policy);
        acc.accumulate_at(now, 0, 0);
        acc.accumulate_at(now, 100, 0);
        assert_eq!(acc.hold.as_ref().unwrap().axis, None);
        acc.accumulate_at(now, 0, 17);
        assert_eq!(acc.accumulate_at(now, 0, -52), Some(GestureDirection::Up));
        assert_eq!(acc.hold.as_ref().unwrap().axis, Some(Axis::Vertical));
        assert_eq!(acc.accumulate_at(now, 100, 0), None);
        assert_eq!(acc.accumulate_at(now, 0, 52), Some(GestureDirection::Down));
    }
}
