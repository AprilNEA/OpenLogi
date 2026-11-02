//! Records a custom shortcut from the next key the user presses.

use gpui::{Context, FocusHandle, Keystroke, Modifiers, Subscription, Window};
use openlogi_core::binding::KeyCombo;

#[cfg(test)]
mod tests;

/// What one key press recorded.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Recording {
    /// A chord a custom shortcut can send.
    Chord(KeyCombo),
    /// The recording stopped without a chord: Escape without modifiers, or
    /// focus left the recording area.
    Cancelled,
    /// A key a custom shortcut cannot send, such as a media key.
    Unsupported,
    /// Shift with a digit or symbol key, such as Shift+]. GPUI reports such
    /// a press as the character it types, without Shift (`}` for Shift+]),
    /// and which key types that character depends on the keyboard layout.
    /// The recorder cannot tell which chord was pressed, so it records none.
    ShiftedSymbol,
}

impl Recording {
    /// The recording for `keystroke` while the window saw `held` modifiers
    /// down.
    ///
    /// Every GPUI platform drops Shift from a digit or symbol key and reports
    /// the shifted character instead: Shift+] arrives as `}` and Shift+1 as
    /// `!`. Only `held`, which GPUI tracks from modifier changes, still shows
    /// Shift. Without this check such a press is either rejected or, on a
    /// layout whose shifted character is a key a chord can name (Shift+7
    /// types `/` on German), recorded as a different chord.
    pub(crate) fn from_press(keystroke: &Keystroke, held: Modifiers) -> Self {
        if held.shift && !keystroke.modifiers.shift && is_symbol(&keystroke.key) {
            return Self::ShiftedSymbol;
        }
        Self::from(keystroke)
    }
}

/// Whether GPUI names `key` by a single character that has no case: a digit
/// or a symbol. GPUI keeps Shift for letters and for named keys such as
/// `tab` or `pageup`.
fn is_symbol(key: &str) -> bool {
    let mut characters = key.chars();
    matches!(
        (characters.next(), characters.next()),
        (Some(character), None) if !character.is_alphabetic()
    )
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

/// A view that owns a [`ShortcutRecorder`].
pub(crate) trait RecordsShortcut: Sized + 'static {
    /// The recording ended with `recording`. The view must drop its
    /// [`ShortcutRecorder`] here, which stops the recording.
    fn shortcut_recorded(
        &mut self,
        recording: Recording,
        window: &mut Window,
        cx: &mut Context<Self>,
    );
}

/// A recording in progress, scoped to the element that tracks `focus`.
///
/// Starting it focuses that element. While focus stays inside it, the next
/// key press in the window is recorded ahead of the window's key bindings, so
/// a chord such as Cmd+W records instead of closing the window. When focus
/// leaves it, for example because the user clicks elsewhere or the editor is
/// no longer rendered, the recording ends as [`Recording::Cancelled`] and key
/// presses reach their usual target. Dropping it stops recording.
pub(crate) struct ShortcutRecorder {
    _interceptor: Subscription,
    _focus_out: Subscription,
}

impl ShortcutRecorder {
    /// Start recording for the view `cx` belongs to.
    pub(crate) fn start<V: RecordsShortcut>(
        focus: &FocusHandle,
        window: &mut Window,
        cx: &mut Context<V>,
    ) -> Self {
        window.focus(focus, cx);
        let target = window.window_handle();
        let view = cx.weak_entity();
        let area = focus.clone();
        let interceptor = cx.intercept_keystrokes(move |event, window, cx| {
            if window.window_handle() != target {
                return;
            }
            let recording = if area.contains_focused(window, cx) {
                cx.stop_propagation();
                Recording::from_press(&event.keystroke, window.modifiers())
            } else {
                Recording::Cancelled
            };
            view.update(cx, |view, cx| view.shortcut_recorded(recording, window, cx))
                .ok();
        });
        let focus_out = cx.on_focus_out(focus, window, |view, _, window, cx| {
            view.shortcut_recorded(Recording::Cancelled, window, cx);
        });
        Self {
            _interceptor: interceptor,
            _focus_out: focus_out,
        }
    }
}
