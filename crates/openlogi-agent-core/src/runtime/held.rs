//! Synthetic output owned by accepted physical press capabilities.
//!
//! Every terminal event removes one owner. Mouse-button and keyboard-chord
//! guards release through RAII on cancellation, shutdown, or unwinding.

use std::collections::{HashMap, hash_map::Entry};

use openlogi_core::binding::{Action, Effect, HeldKeys, KeyCombo, MouseButton};
use openlogi_inject::{HeldChord, HeldMouseButton};

use super::{ActionDispatchTarget, ActionExecutor, PressToken};

/// The button handler's output seam. Native guards own OS output; test guards
/// record edges without touching the desktop input queue.
pub(super) trait ButtonOutput {
    type Chord;
    type Mouse;

    fn press_chord(&self, combo: &KeyCombo, keys: HeldKeys) -> Self::Chord;
    fn replace_chord(&self, held: &mut Self::Chord, combo: &KeyCombo, keys: HeldKeys);
    fn press_mouse(&self, button: MouseButton) -> Self::Mouse;
    fn replace_mouse(&self, held: &mut Self::Mouse, button: MouseButton);
    fn dispatch(
        &self,
        executor: &ActionExecutor,
        action: &Action,
        device_key: Option<&str>,
        target: ActionDispatchTarget,
    );
}

#[derive(Default)]
pub(super) struct NativeButtonOutput;

impl ButtonOutput for NativeButtonOutput {
    type Chord = HeldChord;
    type Mouse = HeldMouseButton;

    fn press_chord(&self, combo: &KeyCombo, keys: HeldKeys) -> Self::Chord {
        openlogi_inject::press_hold(combo, keys)
    }

    fn replace_chord(&self, held: &mut Self::Chord, combo: &KeyCombo, keys: HeldKeys) {
        held.replace(combo, keys);
    }

    fn press_mouse(&self, button: MouseButton) -> Self::Mouse {
        openlogi_inject::press_hold_mouse(button)
    }

    fn replace_mouse(&self, held: &mut Self::Mouse, button: MouseButton) {
        held.replace(button);
    }

    fn dispatch(
        &self,
        executor: &ActionExecutor,
        action: &Action,
        device_key: Option<&str>,
        target: ActionDispatchTarget,
    ) {
        executor.dispatch_to(action, device_key, target);
    }
}

/// One physical press owns at most one held output.
enum HeldAction<B: ButtonOutput> {
    Chord(B::Chord),
    Mouse(B::Mouse),
}

pub(super) struct HeldActions<B: ButtonOutput = NativeButtonOutput> {
    output: B,
    by_press: HashMap<PressToken, HeldAction<B>>,
}

impl<B: ButtonOutput + Default> Default for HeldActions<B> {
    fn default() -> Self {
        Self::new(B::default())
    }
}

impl<B: ButtonOutput> HeldActions<B> {
    pub(super) fn new(output: B) -> Self {
        Self {
            output,
            by_press: HashMap::new(),
        }
    }

    /// Single-action mouse bindings mirror physical down/up. Gesture commits
    /// and release-time clicks use `start_shortcut` instead.
    pub(super) fn start(
        &mut self,
        press: &PressToken,
        action: &Action,
        target: ActionDispatchTarget,
    ) -> bool {
        if let Effect::Click(button) = action.effect() {
            if target.resolve(action).is_some() {
                self.start_mouse(press, button);
            }
            true
        } else {
            self.start_shortcut(press, action, target)
        }
    }

    /// Preserve triggered held-key semantics without turning a gesture click
    /// into a mouse hold that lasts until the gesture ends.
    pub(super) fn start_shortcut(
        &mut self,
        press: &PressToken,
        action: &Action,
        target: ActionDispatchTarget,
    ) -> bool {
        let Some((combo, keys)) = action.held_chord() else {
            return false;
        };
        if target.resolve(action).is_none() {
            return true;
        }
        match self.by_press.entry(press.clone()) {
            Entry::Occupied(mut slot) => {
                if let HeldAction::Chord(held) = slot.get_mut() {
                    self.output.replace_chord(held, combo, keys);
                } else {
                    slot.insert(HeldAction::Chord(self.output.press_chord(combo, keys)));
                }
            }
            Entry::Vacant(slot) => {
                slot.insert(HeldAction::Chord(self.output.press_chord(combo, keys)));
            }
        }
        true
    }

    fn start_mouse(&mut self, press: &PressToken, button: MouseButton) {
        match self.by_press.entry(press.clone()) {
            Entry::Occupied(mut slot) => {
                if let HeldAction::Mouse(held) = slot.get_mut() {
                    self.output.replace_mouse(held, button);
                } else {
                    slot.insert(HeldAction::Mouse(self.output.press_mouse(button)));
                }
            }
            Entry::Vacant(slot) => {
                slot.insert(HeldAction::Mouse(self.output.press_mouse(button)));
            }
        }
    }

    pub(super) fn dispatch(
        &self,
        executor: &ActionExecutor,
        action: &Action,
        device_key: Option<&str>,
        target: ActionDispatchTarget,
    ) {
        self.output.dispatch(executor, action, device_key, target);
    }

    pub(super) fn end(&mut self, press: &PressToken) {
        self.by_press.remove(press);
    }

    #[cfg(test)]
    pub(super) fn contains(&self, press: &PressToken) -> bool {
        self.by_press.contains_key(press)
    }
}
