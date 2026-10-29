//! Records a custom shortcut from the next key the user presses.

use gpui::{App, Keystroke, Subscription, Window};
use openlogi_core::binding::KeyCombo;

#[cfg(test)]
mod tests;

/// What one key press recorded.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Recording {
    /// A chord a custom shortcut can send.
    Chord(KeyCombo),
    /// Escape without modifiers, which stops recording without a chord.
    Cancelled,
    /// A key a custom shortcut cannot send, such as a media key.
    Unsupported,
}

impl From<&Keystroke> for Recording {
    /// Lone modifier presses never get here: GPUI reports them as modifier
    /// changes, not as key presses.
    fn from(keystroke: &Keystroke) -> Self {
        let modifiers = keystroke.modifiers;
        if keystroke.key == "escape" && !modifiers.modified() {
            return Self::Cancelled;
        }
        let mut tokens = Vec::with_capacity(5);
        if modifiers.platform {
            // GPUI's platform modifier is Command on macOS and the logo key
            // elsewhere, which a chord writes as `Super`.
            tokens.push(if cfg!(target_os = "macos") {
                "Cmd"
            } else {
                "Super"
            });
        }
        if modifiers.control {
            tokens.push("Ctrl");
        }
        if modifiers.alt {
            tokens.push("Alt");
        }
        if modifiers.shift {
            tokens.push("Shift");
        }
        tokens.push(&keystroke.key);
        tokens
            .join("+")
            .parse::<KeyCombo>()
            .map_or(Self::Unsupported, Self::Chord)
    }
}

/// A recording in progress. It takes the next key press in the window that
/// started it, ahead of the window's key bindings, so a chord such as Cmd+W
/// records instead of closing the window. Dropping it stops recording.
pub(crate) struct ShortcutRecorder {
    _interceptor: Subscription,
}

impl ShortcutRecorder {
    /// Start recording. `on_recorded` runs once, for the first key press.
    pub(crate) fn start(
        window: &Window,
        cx: &mut App,
        on_recorded: impl FnOnce(Recording, &mut Window, &mut App) + 'static,
    ) -> Self {
        let target = window.window_handle();
        let mut on_recorded = Some(on_recorded);
        let interceptor = cx.intercept_keystrokes(move |event, window, cx| {
            if window.window_handle() != target {
                return;
            }
            let Some(on_recorded) = on_recorded.take() else {
                return;
            };
            cx.stop_propagation();
            on_recorded(Recording::from(&event.keystroke), window, cx);
        });
        Self {
            _interceptor: interceptor,
        }
    }
}
