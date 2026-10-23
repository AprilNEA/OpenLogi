//! A button-output recorder that never posts to an OS input queue.

use openlogi_core::binding::{HeldKeys, KeyCombo, MouseButton};

use super::*;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum OutputEvent {
    MouseDown(MouseButton),
    MouseUp(MouseButton),
    ChordDown(KeyCombo, HeldKeys),
    ChordUp(KeyCombo, HeldKeys),
    Tap(Action),
}

#[derive(Clone, Default)]
pub(super) struct RecordingOutput(Arc<Mutex<Vec<OutputEvent>>>);

impl RecordingOutput {
    fn record(&self, event: OutputEvent) {
        self.0.lock().expect("test output lock").push(event);
    }

    pub(super) fn events(&self) -> Vec<OutputEvent> {
        self.0.lock().expect("test output lock").clone()
    }
}

pub(super) struct RecordedMouse {
    button: MouseButton,
    output: RecordingOutput,
}

impl Drop for RecordedMouse {
    fn drop(&mut self) {
        self.output.record(OutputEvent::MouseUp(self.button));
    }
}

pub(super) struct RecordedChord {
    combo: KeyCombo,
    keys: HeldKeys,
    output: RecordingOutput,
}

impl Drop for RecordedChord {
    fn drop(&mut self) {
        self.output
            .record(OutputEvent::ChordUp(self.combo.clone(), self.keys));
    }
}

impl ButtonOutput for RecordingOutput {
    type Chord = RecordedChord;
    type Mouse = RecordedMouse;

    fn press_chord(&self, combo: &KeyCombo, keys: HeldKeys) -> Self::Chord {
        self.record(OutputEvent::ChordDown(combo.clone(), keys));
        RecordedChord {
            combo: combo.clone(),
            keys,
            output: self.clone(),
        }
    }

    fn replace_chord(&self, held: &mut Self::Chord, combo: &KeyCombo, keys: HeldKeys) {
        self.record(OutputEvent::ChordUp(held.combo.clone(), held.keys));
        self.record(OutputEvent::ChordDown(combo.clone(), keys));
        held.combo = combo.clone();
        held.keys = keys;
    }

    fn press_mouse(&self, button: MouseButton) -> Self::Mouse {
        self.record(OutputEvent::MouseDown(button));
        RecordedMouse {
            button,
            output: self.clone(),
        }
    }

    fn replace_mouse(&self, held: &mut Self::Mouse, button: MouseButton) {
        if held.button != button {
            self.record(OutputEvent::MouseUp(held.button));
            self.record(OutputEvent::MouseDown(button));
            held.button = button;
        }
    }

    fn dispatch(
        &self,
        _executor: &ActionExecutor,
        action: &Action,
        _device_key: Option<&str>,
        _target: ActionDispatchTarget,
    ) {
        self.record(OutputEvent::Tap(action.clone()));
    }
}
