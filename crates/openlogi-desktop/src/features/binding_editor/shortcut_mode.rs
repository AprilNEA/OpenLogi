//! How a recorded key combination fires, shared by every editor that binds
//! one: the mouse inspector and the Keys tab's key panel.

use gpui::SharedString;
use openlogi_core::binding::{Action, KeyCombo};

/// Whether a custom shortcut is tapped, held, or held at its modifiers.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum ShortcutMode {
    /// Press and release the whole chord at once
    /// ([`Action::CustomShortcut`]).
    #[default]
    Tap,
    /// Hold the whole chord for as long as the button is held
    /// ([`Action::HoldShortcut`]).
    Hold,
    /// Hold the chord's modifiers while the button is held and tap its key
    /// once, as an app switcher needs ([`Action::TapKeyHoldingModifiers`]).
    HoldModifiers,
}

impl ShortcutMode {
    /// Every mode, in the order the editors offer them.
    pub(crate) const ALL: [Self; 3] = [Self::Tap, Self::Hold, Self::HoldModifiers];

    /// The action that fires `combo` this way.
    pub(crate) fn action(self, combo: KeyCombo) -> Action {
        match self {
            Self::Tap => Action::CustomShortcut(combo),
            Self::Hold => Action::HoldShortcut(combo),
            Self::HoldModifiers => Action::TapKeyHoldingModifiers(combo),
        }
    }

    /// The mode and chord of `action`, when it fires a key combination.
    pub(crate) fn of(action: &Action) -> Option<(Self, &KeyCombo)> {
        match action {
            Action::CustomShortcut(combo) => Some((Self::Tap, combo)),
            Action::HoldShortcut(combo) => Some((Self::Hold, combo)),
            Action::TapKeyHoldingModifiers(combo) => Some((Self::HoldModifiers, combo)),
            _ => None,
        }
    }

    /// The mode's button label.
    pub(crate) fn label(self) -> SharedString {
        match self {
            Self::Tap => tr!("actions.shortcut_tap"),
            Self::Hold => tr!("actions.shortcut_hold"),
            Self::HoldModifiers => tr!("actions.shortcut_hold_modifiers"),
        }
    }

    /// The stable id fragment of the mode's button.
    pub(crate) const fn id(self) -> &'static str {
        match self {
            Self::Tap => "tap",
            Self::Hold => "hold",
            Self::HoldModifiers => "hold-modifiers",
        }
    }
}

/// Why a custom shortcut editor shows an error.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ShortcutError {
    /// The typed text, or the recorded key, is not a chord a shortcut can
    /// send.
    Unrecognized,
    /// Record saw Shift with a digit or symbol key, which it cannot record
    /// (see [`Recording::ShiftedSymbol`]). Typing the chord still works.
    ///
    /// [`Recording::ShiftedSymbol`]: super::Recording::ShiftedSymbol
    ShiftedSymbol,
}

impl ShortcutError {
    /// The error line the editor shows.
    pub(crate) fn message(self) -> SharedString {
        match self {
            Self::Unrecognized => tr!("action_ring.custom_action_invalid_input"),
            Self::ShiftedSymbol => tr!("actions.shifted_symbol_unrecordable"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chord(text: &str) -> KeyCombo {
        text.parse().expect("a valid chord")
    }

    /// Each mode makes its own action and reads back from it, so an editor
    /// shows the mode the key is bound with.
    #[test]
    fn every_mode_round_trips_through_its_action() {
        for mode in ShortcutMode::ALL {
            let action = mode.action(chord("Ctrl+Shift+P"));
            assert_eq!(
                ShortcutMode::of(&action),
                Some((mode, &chord("Ctrl+Shift+P")))
            );
        }
        assert_eq!(
            ShortcutMode::Hold.action(chord("F5")),
            Action::HoldShortcut(chord("F5"))
        );
        assert_eq!(
            ShortcutMode::HoldModifiers.action(chord("Alt+Tab")),
            Action::TapKeyHoldingModifiers(chord("Alt+Tab"))
        );
        assert_eq!(ShortcutMode::of(&Action::Copy), None);
    }
}
