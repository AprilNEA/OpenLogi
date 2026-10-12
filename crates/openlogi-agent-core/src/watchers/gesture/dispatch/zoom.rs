//! Continuous thumb-wheel zoom, scoped to exact capture-session incarnations.

use std::collections::{HashMap, VecDeque};
use std::time::{Duration, Instant};

use openlogi_core::binding::Action;
use openlogi_core::config::ThumbwheelSensitivity;
use openlogi_inject::{ZoomContext, ZoomGesture, ZoomPhase};

use crate::runtime::HidppSessionId;

// A stop report marks inactivity. Delay the terminal frame so a brief pause
// does not split a continuing sequence into separate gestures.
const END_DELAY: Duration = Duration::from_millis(500);
const INACTIVITY: Duration = Duration::from_millis(100);
const IDLE: Duration = INACTIVITY.saturating_add(END_DELAY);

/// Output seam permits deterministic lifecycle tests without native injection.
pub(super) trait Output: Sized {
    fn begin(context: ZoomContext, delta: f64) -> Option<Self>;
    fn change(&mut self, delta: f64, cursor: (f64, f64));
    fn finish(&mut self, phase: ZoomPhase);
}

impl Output for ZoomGesture {
    fn begin(context: ZoomContext, delta: f64) -> Option<Self> {
        Self::begin(context, delta)
    }
    fn change(&mut self, delta: f64, cursor: (f64, f64)) {
        self.change(delta, cursor);
    }
    fn finish(&mut self, phase: ZoomPhase) {
        self.finish(phase);
    }
}

/// Application/target identity cannot leak between successive gestures.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Target {
    pub(super) app_id: String,
    pub(super) pointer: Option<openlogi_hook::PointerTarget>,
    pub(super) foreground_pid: Option<i32>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Admission {
    Accepting,
    Paused,
    Shutdown,
}

pub(super) struct Motion<'a> {
    pub(super) action: &'a Action,
    pub(super) sensitivity: ThumbwheelSensitivity,
    pub(super) magnitude: f64,
    pub(super) target: Target,
    pub(super) context: ZoomContext,
    pub(super) now: Instant,
    pub(super) admission: super::WheelAdmission,
}

struct Active<O: Output> {
    output: Option<O>,
    target: Target,
    sensitivity: ThumbwheelSensitivity,
    history: VecDeque<f64>,
    deadline: Instant,
    next_check: Instant,
}

impl<O: Output> Drop for Active<O> {
    fn drop(&mut self) {
        if let Some(mut output) = self.output.take() {
            output.finish(ZoomPhase::Cancelled);
        }
    }
}

/// One owner per exact device session; stale teardown cannot end a successor.
pub(super) struct Sessions<O: Output = ZoomGesture> {
    active: HashMap<HidppSessionId, Active<O>>,
    admission: Admission,
}

impl<O: Output> Default for Sessions<O> {
    fn default() -> Self {
        Self {
            active: HashMap::new(),
            admission: Admission::Accepting,
        }
    }
}

impl<O: Output> Sessions<O> {
    pub(super) fn cancel(&mut self, session: &HidppSessionId) {
        self.active.remove(session);
    }

    pub(super) fn stop(&mut self, session: &HidppSessionId, now: Instant) {
        if let Some(active) = self.active.get_mut(session) {
            active.deadline = now + END_DELAY;
        }
    }

    pub(super) fn cancel_all(&mut self) {
        self.active.clear();
    }

    pub(super) fn pause(&mut self) {
        if self.admission != Admission::Shutdown {
            self.admission = Admission::Paused;
        }
        self.cancel_all();
    }
    pub(super) fn resume(&mut self) {
        if self.admission == Admission::Paused {
            self.admission = Admission::Accepting;
        }
    }

    pub(super) fn shutdown(&mut self) {
        self.admission = Admission::Shutdown;
        self.cancel_all();
    }

    pub(super) fn deadline(&self) -> Option<Instant> {
        self.active
            .values()
            .map(|active| active.deadline.min(active.next_check))
            .min()
    }

    pub(super) fn expire(
        &mut self,
        now: Instant,
        app_id: Option<&str>,
        permitted: bool,
        foreground_pid: Option<i32>,
    ) {
        self.active.retain(|_, active| {
            if !permitted
                || app_id != Some(active.target.app_id.as_str())
                || foreground_pid != active.target.foreground_pid
            {
                return false;
            }
            if now >= active.deadline {
                if let Some(mut output) = active.output.take() {
                    output.finish(ZoomPhase::Ended);
                }
                return false;
            }
            active.next_check = now + INACTIVITY;
            true
        });
    }

    pub(super) fn motion(&mut self, session: &HidppSessionId, motion: Motion<'_>) -> bool {
        let Motion {
            action,
            sensitivity,
            magnitude,
            target,
            context,
            now,
            admission,
        } = motion;
        let Some(direction) = direction(action) else {
            self.cancel(session);
            return false;
        };
        if admission == super::WheelAdmission::Draining {
            self.cancel(session);
            return true;
        }
        if self.admission != Admission::Accepting {
            return true;
        }
        if !magnitude.is_finite() || magnitude <= 0.0 {
            return true;
        }
        if self
            .active
            .get(session)
            .is_some_and(|active| now >= active.deadline)
            && let Some(mut active) = self.active.remove(session)
            && let Some(mut output) = active.output.take()
        {
            output.finish(ZoomPhase::Ended);
        }
        if self
            .active
            .get(session)
            .is_some_and(|active| active.target != target || active.sensitivity != sensitivity)
        {
            self.cancel(session);
        }
        // Acceleration precedes sensitivity and the constant-weight four-report
        // filter. HID++ counts are already the native zoom input units.
        let speed = slider_speed(sensitivity);
        let scale = 0.003_125 + 0.021_875 * (speed + 1.0) / 2.0;
        let delta = (magnitude.powf(1.5)).trunc() * direction * scale;
        if let Some(active) = self.active.get_mut(session) {
            let delta = smooth(&mut active.history, delta);
            if let Some(output) = &mut active.output {
                output.change(delta, (context.x, context.y));
            }
            active.deadline = now + IDLE;
        } else {
            let mut history = VecDeque::with_capacity(4);
            let delta = smooth(&mut history, delta);
            if let Some(output) = O::begin(context, delta) {
                self.active.insert(
                    session.clone(),
                    Active {
                        output: Some(output),
                        target,
                        sensitivity,
                        history,
                        deadline: now + IDLE,
                        next_check: now + INACTIVITY,
                    },
                );
            }
        }
        true
    }
}

fn direction(action: &Action) -> Option<f64> {
    match action {
        Action::ZoomIn => Some(1.0),
        Action::ZoomOut => Some(-1.0),
        _ => None,
    }
}

fn smooth(history: &mut VecDeque<f64>, delta: f64) -> f64 {
    if history.len() == 4 {
        history.pop_front();
    }
    history.push_back(delta);
    history.iter().sum::<f64>() / f64::from(u32::try_from(history.len()).unwrap_or(4))
}

// OpenLogi's existing slider has default14 rather than the reference's signed
// speed0. Piecewise interpolation preserves that default and both endpoints.
fn slider_speed(sensitivity: ThumbwheelSensitivity) -> f64 {
    let value = f64::from(sensitivity.into_inner());
    let default = f64::from(ThumbwheelSensitivity::DEFAULT.into_inner());
    if value <= default {
        (value - default) / (default - f64::from(ThumbwheelSensitivity::MIN.into_inner()))
    } else {
        (value - default) / (f64::from(ThumbwheelSensitivity::MAX.into_inner()) - default)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    thread_local! { static EVENTS: RefCell<Vec<(ZoomPhase, f64)>> = const { RefCell::new(Vec::new()) }; }
    struct Recorder;
    impl Output for Recorder {
        fn begin(_: ZoomContext, delta: f64) -> Option<Self> {
            EVENTS.with_borrow_mut(|events| {
                events.extend([(ZoomPhase::Began, 0.0), (ZoomPhase::Changed, delta)]);
            });
            Some(Self)
        }
        fn change(&mut self, delta: f64, _: (f64, f64)) {
            EVENTS.with_borrow_mut(|events| events.push((ZoomPhase::Changed, delta)));
        }
        fn finish(&mut self, phase: ZoomPhase) {
            EVENTS.with_borrow_mut(|events| events.push((phase, 0.0)));
        }
    }
    fn target(app: &str) -> Target {
        Target {
            app_id: app.into(),
            pointer: None,
            foreground_pid: Some(42),
        }
    }
    fn context() -> ZoomContext {
        ZoomContext {
            x: 100.25,
            y: 201.5,
            process_id: None,
            window_id: None,
        }
    }
    fn events() -> Vec<(ZoomPhase, f64)> {
        EVENTS.with_borrow_mut(std::mem::take)
    }
    fn motion(
        sessions: &mut Sessions<Recorder>,
        id: &HidppSessionId,
        action: &Action,
        magnitude: f64,
        now: Instant,
    ) {
        sessions.motion(
            id,
            Motion {
                action,
                sensitivity: ThumbwheelSensitivity::DEFAULT,
                magnitude,
                target: target("app-a"),
                context: context(),
                now,
                admission: super::super::WheelAdmission::Active,
            },
        );
    }

    #[test]
    fn slow_motion_and_reversal_are_fractional_without_threshold_or_cooldown() {
        events();
        let now = Instant::now();
        let mut sessions = Sessions::<Recorder>::default();
        let id = HidppSessionId::with_epoch("mouse-a", 1);
        motion(&mut sessions, &id, &Action::ZoomIn, 1.0, now);
        motion(&mut sessions, &id, &Action::ZoomOut, 1.0, now);
        let recorded = events();
        assert_eq!(recorded.len(), 3);
        assert!((recorded[1].1 - 0.014_062_5).abs() < f64::EPSILON);
        assert!(recorded[2].1.abs() < f64::EPSILON);
        sessions.expire(now + IDLE, Some("app-a"), true, Some(42));
        assert_eq!(events(), [(ZoomPhase::Ended, 0.0)]);
    }

    #[test]
    fn stale_session_cleanup_cannot_cancel_replacement_and_new_start_has_no_history() {
        events();
        let now = Instant::now();
        let mut sessions = Sessions::<Recorder>::default();
        let old = HidppSessionId::with_epoch("mouse-a", 1);
        let new = HidppSessionId::with_epoch("mouse-a", 2);
        motion(&mut sessions, &old, &Action::ZoomIn, 4.0, now);
        sessions.cancel(&old);
        motion(&mut sessions, &new, &Action::ZoomOut, 1.0, now);
        events();
        sessions.cancel(&old);
        assert!(events().is_empty());
        assert_eq!(sessions.active.len(), 1);
        sessions.cancel(&new);
        assert_eq!(events(), [(ZoomPhase::Cancelled, 0.0)]);
    }

    #[test]
    fn focus_permission_disconnect_and_shutdown_each_cancel_zero_once() {
        for reason in 0..4 {
            events();
            let now = Instant::now();
            let mut sessions = Sessions::<Recorder>::default();
            let id = HidppSessionId::with_epoch("mouse-a", 1);
            motion(&mut sessions, &id, &Action::ZoomIn, 1.0, now);
            events();
            match reason {
                0 => sessions.expire(now, Some("app-b"), true, Some(42)),
                1 => sessions.expire(now, Some("app-a"), false, Some(42)),
                2 => sessions.cancel(&id),
                _ => sessions.cancel_all(),
            }
            sessions.cancel_all();
            assert_eq!(events(), [(ZoomPhase::Cancelled, 0.0)]);
        }
    }

    #[test]
    fn speed_mapping_anchors_existing_slider_default_and_endpoints() {
        assert!((slider_speed(ThumbwheelSensitivity::MIN) + 1.0).abs() < f64::EPSILON);
        assert!(slider_speed(ThumbwheelSensitivity::DEFAULT).abs() < f64::EPSILON);
        assert!((slider_speed(ThumbwheelSensitivity::MAX) - 1.0).abs() < f64::EPSILON);
    }
    #[test]
    fn stop_delay_resumes_same_gesture_and_expiry_resets_filter() {
        events();
        let now = Instant::now();
        let mut sessions = Sessions::<Recorder>::default();
        let id = HidppSessionId::with_epoch("mouse-a", 1);
        motion(&mut sessions, &id, &Action::ZoomIn, 4.0, now);
        events();
        sessions.stop(&id, now);
        sessions.expire(now + END_DELAY / 2, Some("app-a"), true, Some(42));
        motion(
            &mut sessions,
            &id,
            &Action::ZoomIn,
            1.0,
            now + END_DELAY / 2,
        );
        let continued = events();
        assert_eq!(continued.len(), 1);
        assert_eq!(continued[0].0, ZoomPhase::Changed);
        assert!((continued[0].1 - 0.063_281_25).abs() < f64::EPSILON);
        sessions.expire(now + IDLE + END_DELAY / 2, Some("app-a"), true, Some(42));
        assert_eq!(events(), [(ZoomPhase::Ended, 0.0)]);
        motion(&mut sessions, &id, &Action::ZoomIn, 1.0, now + IDLE * 2);
        let resumed = events();
        assert_eq!(resumed[0], (ZoomPhase::Began, 0.0));
        assert!((resumed[1].1 - 0.014_062_5).abs() < f64::EPSILON);
    }

    #[test]
    fn shutdown_rejects_late_drained_motion_and_unknown_stop_is_inert() {
        events();
        let now = Instant::now();
        let mut sessions = Sessions::<Recorder>::default();
        let id = HidppSessionId::with_epoch("mouse-a", 1);
        sessions.stop(&id, now);
        assert!(events().is_empty());
        motion(&mut sessions, &id, &Action::ZoomIn, 1.0, now);
        events();
        sessions.shutdown();
        assert_eq!(events(), [(ZoomPhase::Cancelled, 0.0)]);
        motion(&mut sessions, &id, &Action::ZoomIn, 1.0, now);
        assert!(events().is_empty());
        assert!(sessions.deadline().is_none());
    }

    #[test]
    fn filter_has_no_startup_padding_and_keeps_only_four_reports() {
        let mut history = VecDeque::new();
        let outputs: Vec<_> = [1.0, 3.0, 5.0, 7.0, -7.0]
            .into_iter()
            .map(|delta| smooth(&mut history, delta))
            .collect();
        for (actual, expected) in outputs.into_iter().zip([1.0, 2.0, 3.0, 4.0, 2.0]) {
            assert!((actual - expected).abs() < f64::EPSILON);
        }
    }

    fn assert_configuration_restart(
        new_target: Target,
        sensitivity: ThumbwheelSensitivity,
        expected: f64,
    ) {
        events();
        let now = Instant::now();
        let mut sessions = Sessions::<Recorder>::default();
        let id = HidppSessionId::with_epoch("mouse-a", 1);
        motion(&mut sessions, &id, &Action::ZoomIn, 4.0, now);
        events();
        sessions.motion(
            &id,
            Motion {
                action: &Action::ZoomIn,
                sensitivity,
                magnitude: 1.0,
                target: new_target,
                context: context(),
                now,
                admission: super::super::WheelAdmission::Active,
            },
        );
        let recorded = events();
        assert_eq!(recorded[0], (ZoomPhase::Cancelled, 0.0));
        assert_eq!(recorded[1], (ZoomPhase::Began, 0.0));
        assert!((recorded[2].1 - expected).abs() < f64::EPSILON);
    }

    #[test]
    fn target_only_change_cancels_before_begin_and_clears_filter_history() {
        assert_configuration_restart(target("app-b"), ThumbwheelSensitivity::DEFAULT, 0.014_062_5);
    }

    #[test]
    fn sensitivity_only_change_cancels_before_begin_and_clears_filter_history() {
        assert_configuration_restart(target("app-a"), ThumbwheelSensitivity::MIN, 0.003_125);
    }

    #[test]
    fn paused_capture_rejects_late_motion_until_resumed() {
        events();
        let now = Instant::now();
        let mut sessions = Sessions::<Recorder>::default();
        let id = HidppSessionId::with_epoch("mouse-a", 1);
        motion(&mut sessions, &id, &Action::ZoomIn, 1.0, now);
        events();
        sessions.pause();
        assert_eq!(events(), [(ZoomPhase::Cancelled, 0.0)]);
        motion(&mut sessions, &id, &Action::ZoomIn, 1.0, now);
        assert!(events().is_empty());
        assert!(sessions.deadline().is_none());
        sessions.resume();
        motion(&mut sessions, &id, &Action::ZoomIn, 1.0, now);
        assert_eq!(events()[0], (ZoomPhase::Began, 0.0));
        sessions.shutdown();
        events();
        sessions.resume();
        motion(&mut sessions, &id, &Action::ZoomIn, 1.0, now);
        assert!(events().is_empty());
    }

    #[test]
    fn retiring_capture_cancels_zoom_and_rejects_drained_wheel_motion() {
        events();
        let now = Instant::now();
        let mut sessions = Sessions::<Recorder>::default();
        let id = HidppSessionId::with_epoch("mouse-a", 1);
        motion(&mut sessions, &id, &Action::ZoomIn, 1.0, now);
        events();
        sessions.motion(
            &id,
            Motion {
                action: &Action::ZoomIn,
                sensitivity: ThumbwheelSensitivity::DEFAULT,
                magnitude: 1.0,
                target: target("app-a"),
                context: context(),
                now,
                admission: super::super::WheelAdmission::Draining,
            },
        );
        assert_eq!(events(), [(ZoomPhase::Cancelled, 0.0)]);
        assert!(sessions.deadline().is_none());
    }
}
