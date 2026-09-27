//! What a Keys-panel selection binds.
//!
//! The panel edits two kinds of binding through one action list: a global
//! function-key trigger, which the OS hook remaps on every keyboard, and one of
//! the selected keyboard's own hotkeys, a per-device binding the agent captures
//! by diverting that control over HID++. The editors only ever read and commit
//! through a [`KeyTarget`], so they never branch on which kind is open.

use gpui::SharedString;
use openlogi_core::binding::{Action, ButtonId};
use openlogi_core::config::KeyTrigger;

use crate::state::{AppState, StateEvents};

/// The binding a Keys-panel selection edits.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum KeyTarget {
    /// A function-row key, bound globally in `config.keyboard.bindings`.
    Function(KeyTrigger),
    /// A divertable keyboard hotkey, bound per device like a mouse button.
    Hotkey(ButtonId),
}

impl KeyTarget {
    /// The key's user-facing name, e.g. "F1" or "Calculator Key".
    pub(crate) fn name(&self) -> SharedString {
        match self {
            Self::Function(trigger) => trigger.to_string().into(),
            Self::Hotkey(button) => tr!(button.translation_key()),
        }
    }

    /// The action currently bound to this key, if any.
    pub(crate) fn current(&self, state: &AppState) -> Option<Action> {
        match self {
            Self::Function(trigger) => state.keyboard_bindings().get(trigger).cloned(),
            Self::Hotkey(button) => hotkey_binding(state, *button).cloned(),
        }
    }

    /// Bind `action` to this key. For a hotkey, [`Action::None`] hands the
    /// control back to the keyboard's firmware: an unbound key is never
    /// diverted.
    pub(crate) fn commit(&self, state: &mut AppState, action: Action) -> StateEvents {
        match self {
            Self::Function(trigger) => state.commit_keyboard_binding(trigger.clone(), Some(action)),
            Self::Hotkey(button) => state.commit_binding(*button, action),
        }
    }
}

/// What `button` is bound to on the selected keyboard, or `None` while it
/// keeps its firmware function. A hotkey's default, [`Action::None`], reads
/// as unbound like an F-key with no entry: the key is simply not diverted.
pub(crate) fn hotkey_binding(state: &AppState, button: ButtonId) -> Option<&Action> {
    state
        .button_bindings()
        .get(&button)
        .filter(|action| **action != Action::None)
}

#[cfg(test)]
mod tests {
    use openlogi_core::binding::{Action, ButtonId};
    use openlogi_core::config::{FunctionKey, KeyModifiers, KeyTrigger};

    use super::KeyTarget;
    use crate::state::tests::state_with_a_known_mouse;

    #[test]
    fn hotkeys_bind_per_device_and_function_keys_bind_globally() {
        let mut state = state_with_a_known_mouse();
        let hotkey = KeyTarget::Hotkey(ButtonId::KeyCalculator);
        let f1 = KeyTarget::Function(KeyTrigger {
            keycode: FunctionKey::ALL[1].keycode(),
            modifiers: KeyModifiers::default(),
        });

        let _ = hotkey.commit(&mut state, Action::TypeText("hello".into()));
        assert_eq!(
            hotkey.current(&state),
            Some(Action::TypeText("hello".into()))
        );
        assert!(
            state.keyboard_bindings().is_empty(),
            "a hotkey must not land in the global F-key map"
        );

        let _ = f1.commit(&mut state, Action::Copy);
        assert_eq!(f1.current(&state), Some(Action::Copy));
        assert_eq!(
            state.button_bindings().get(&ButtonId::KeyCalculator),
            Some(&Action::TypeText("hello".into())),
            "an F-key commit must leave the device's hotkeys alone"
        );

        // Binding a hotkey to nothing hands it back to the firmware, which
        // reads as unbound.
        let _ = hotkey.commit(&mut state, Action::None);
        assert_eq!(hotkey.current(&state), None);
    }
}
