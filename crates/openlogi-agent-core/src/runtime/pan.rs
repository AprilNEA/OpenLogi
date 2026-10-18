//! The button worker owns pan capture; the OS hook only queues motion.

use std::sync::{Arc, Mutex, PoisonError};

use openlogi_core::scroll::ScrollDelta;

use super::PressToken;

/// A published capability, never an independent physical-button lifecycle.
#[derive(Clone, Default)]
pub(super) struct PanInput(Arc<Mutex<Option<PressToken>>>);

impl PanInput {
    pub(super) fn try_press(&self) -> Option<PressToken> {
        self.0.try_lock().ok()?.clone()
    }
}

/// Exactly one writer, owned by the button worker and cleared on unwind.
#[derive(Default)]
pub(super) struct PanOwner {
    input: PanInput,
}

impl PanOwner {
    pub(super) fn input(&self) -> PanInput {
        self.input.clone()
    }

    pub(super) fn start(&mut self, press: &PressToken) {
        *self.input.0.lock().unwrap_or_else(PoisonError::into_inner) = Some(press.clone());
    }

    pub(super) fn end(&mut self, press: &PressToken) {
        let mut active = self.input.0.lock().unwrap_or_else(PoisonError::into_inner);
        if active.as_ref() == Some(press) {
            *active = None;
        }
    }

    pub(super) fn owns(&self, press: &PressToken) -> bool {
        self.input
            .0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .as_ref()
            == Some(press)
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
    fn stale_release_cannot_end_a_new_pan_and_drop_clears_capture() {
        let mut owner = PanOwner::default();
        let input = owner.input();
        let first = PressToken::hook_for_test(1, ButtonId::MiddleClick);
        let second = PressToken::hook_for_test(2, ButtonId::MiddleClick);
        owner.start(&first);
        owner.start(&second);
        owner.end(&first);
        assert_eq!(input.try_press(), Some(second.clone()));
        owner.end(&second);
        assert!(input.try_press().is_none());
        owner.start(&first);
        drop(owner);
        assert!(input.try_press().is_none());
    }
}
