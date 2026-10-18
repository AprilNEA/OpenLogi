//! The button worker owns pan capture; the OS hook only queues motion.

use std::sync::{Arc, Mutex, PoisonError};

use openlogi_core::scroll::ScrollDelta;

use super::{PressToken, button::ButtonInputHandle};

struct PanState {
    press: PressToken,
    /// Wheel ticks accumulated behind at most one queued notification.
    pending: Option<ScrollDelta>,
}

/// A published capability, never an independent physical-button lifecycle.
#[derive(Clone, Default)]
pub(super) struct PanInput(Arc<Mutex<Option<PanState>>>);

impl PanInput {
    #[cfg(test)]
    pub(super) fn is_capturing(&self) -> bool {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .is_some()
    }

    pub(super) fn try_motion(&self, buttons: &ButtonInputHandle, dx: i32, dy: i32) -> bool {
        let Ok(mut state) = self.0.try_lock() else {
            return false;
        };
        let Some(active) = state.as_mut() else {
            return false;
        };
        if !buttons.accepts_press(&active.press) {
            return false;
        }
        let delta = pan_delta(dx, dy);
        if let Some(pending) = active.pending.as_mut() {
            *pending = ScrollDelta::wheel_ticks(pending.x() + delta.x(), pending.y() + delta.y());
        } else {
            if !buttons.try_pan_motion(&active.press) {
                return false;
            }
            active.pending = Some(delta);
        }
        true
    }
}

/// The button worker alone changes capture identity and clears it on unwind.
/// The hook may only accumulate motion under that published identity.
#[derive(Default)]
pub(super) struct PanOwner {
    input: PanInput,
}

impl PanOwner {
    pub(super) fn input(&self) -> PanInput {
        self.input.clone()
    }

    pub(super) fn start(&mut self, press: &PressToken) {
        *self.input.0.lock().unwrap_or_else(PoisonError::into_inner) = Some(PanState {
            press: press.clone(),
            pending: None,
        });
    }

    pub(super) fn end(&mut self, press: &PressToken) {
        let mut active = self.input.0.lock().unwrap_or_else(PoisonError::into_inner);
        if active.as_ref().is_some_and(|active| active.press == *press) {
            *active = None;
        }
    }

    pub(super) fn take_delta(&mut self, press: &PressToken) -> Option<ScrollDelta> {
        let mut state = self.input.0.lock().unwrap_or_else(PoisonError::into_inner);
        let active = state.as_mut()?;
        (active.press == *press)
            .then(|| active.pending.take())
            .flatten()
    }
}

impl Drop for PanOwner {
    fn drop(&mut self) {
        *self.input.0.lock().unwrap_or_else(PoisonError::into_inner) = None;
    }
}

/// Grab-and-drag direction: content follows the hand. Fractional wheel ticks
/// preserve slow motion on Windows through the injector's existing quantizer.
pub(super) fn pan_delta(dx: i32, dy: i32) -> ScrollDelta {
    ScrollDelta::wheel_ticks(-f64::from(dx) / 40.0, f64::from(dy) / 40.0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use openlogi_core::binding::ButtonId;

    #[test]
    fn pan_follows_the_hand_on_both_axes() {
        assert_eq!(pan_delta(40, 80), ScrollDelta::wheel_ticks(-1.0, 2.0));
        assert_eq!(pan_delta(-20, -10), ScrollDelta::wheel_ticks(0.5, -0.25));
    }

    #[test]
    fn pending_motion_fails_open_after_invalidation_and_cannot_cross_presses() {
        let mut runtime = super::super::button::ButtonRuntimeOwner::spawn(|_| {}).unwrap();
        let buttons = runtime.input();
        let mut owner = PanOwner::default();
        let input = owner.input();
        let press = || {
            buttons
                .try_hook_down_with_target(
                    ButtonId::Back,
                    None,
                    super::super::ActionDispatchTarget::Keyboard,
                )
                .unwrap()
        };
        let first = press();
        owner.start(&first);
        assert!(input.try_motion(&buttons, 40, 80));
        buttons.invalidate_all();
        assert!(!input.try_motion(&buttons, 40, 80));
        let second = press();
        owner.start(&second);
        assert_eq!(owner.take_delta(&first), None);
        assert_eq!(owner.take_delta(&second), None);
        assert!(input.try_motion(&buttons, -40, -80));
        assert_eq!(owner.take_delta(&first), None);
        assert_eq!(
            owner.take_delta(&second),
            Some(ScrollDelta::wheel_ticks(1.0, -2.0))
        );
        let lock = input.0.lock().unwrap();
        assert!(
            !input.try_motion(&buttons, 1, 1),
            "the hook must never wait on a contended capture lock"
        );
        drop(lock);
        assert!(runtime.shutdown());
    }

    #[test]
    fn stale_release_cannot_end_a_new_pan_and_drop_clears_capture() {
        let mut owner = PanOwner::default();
        let input = owner.input();
        let first = PressToken::hook_for_test(1, ButtonId::MiddleClick);
        let second = PressToken::hook_for_test(2, ButtonId::MiddleClick);
        owner.start(&first);
        owner.start(&second);
        owner.end(&first);
        assert_eq!(
            input.0.lock().unwrap().as_ref().map(|active| &active.press),
            Some(&second)
        );
        owner.end(&second);
        assert!(input.0.lock().unwrap().is_none());
        owner.start(&first);
        drop(owner);
        assert!(input.0.lock().unwrap().is_none());
    }
}
