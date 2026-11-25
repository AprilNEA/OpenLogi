//! Records a custom shortcut from the next key the user presses.

use gpui::{Context, FocusHandle, Keystroke, Modifiers, Subscription, Window};
use openlogi_core::binding::{BareModifiers, KeyCombo};

use crate::platform::os::{current_event_has_right_control, current_event_is_right_control};

#[cfg(test)]
mod tests;

/// What one key press recorded.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Recording {
    /// A chord a custom shortcut can send.
    Chord(KeyCombo),
    /// A modifier pressed and released on its own.
    Modifier(BareModifiers),
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
        // GPUI emits a synthetic modifier keystroke on release when no
        // ordinary key was pressed. The original event still carries Ctrl's side.
        if !keystroke.modifiers.modified() {
            let modifiers = [
                ("control", "Ctrl"),
                ("alt", "Alt"),
                ("shift", "Shift"),
                ("platform", platform_modifier_name()),
            ];
            if let Some((key, name)) = modifiers.into_iter().find(|(key, _)| *key == keystroke.key)
            {
                let name = if key == "control" && current_event_is_right_control() {
                    "RightCtrl"
                } else {
                    name
                };
                return name.parse().map_or(Self::Unsupported, Self::Modifier);
            }
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
    fn from(keystroke: &Keystroke) -> Self {
        let modifiers = keystroke.modifiers;
        if keystroke.key == "escape" && !modifiers.modified() {
            return Self::Cancelled;
        }
        let mut tokens: Vec<&str> = modifier_names(modifiers).collect();
        tokens.push(&keystroke.key);
        tokens
            .join("+")
            .parse::<KeyCombo>()
            .map_or(Self::Unsupported, Self::Chord)
    }
}

const fn platform_modifier_name() -> &'static str {
    if cfg!(target_os = "macos") {
        "Cmd"
    } else {
        "Super"
    }
}

fn modifier_names(modifiers: Modifiers) -> impl Iterator<Item = &'static str> {
    [
        (modifiers.platform, platform_modifier_name()),
        (modifiers.control, "Ctrl"),
        (modifiers.alt, "Alt"),
        (modifiers.shift, "Shift"),
    ]
    .into_iter()
    .filter_map(|(pressed, name)| pressed.then_some(name))
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
    /// Names of the currently pressed modifiers, in shortcut order.
    pub(crate) fn preview(modifiers: Modifiers) -> String {
        let right_control = modifiers.control && current_event_has_right_control();
        modifier_names(modifiers)
            .map(|name| {
                if right_control && name == "Ctrl" {
                    "RightCtrl"
                } else {
                    name
                }
            })
            .collect::<Vec<_>>()
            .join("+")
    }

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
