//! The custom shortcut editor the mouse inspector and the keyboard key panel
//! share: a Tap/Hold switch, the chord field, Record, Add, and the field's
//! inline error.
//!
//! The view that shows it owns a [`ShortcutDraft`] and exposes it through
//! [`EditsShortcut`]; [`shortcut_editor`] renders the draft and writes back to
//! it through that view.

use gpui::{
    App, AppContext as _, Context, Entity, FocusHandle, InteractiveElement as _, IntoElement,
    ParentElement, Styled, Window, div, prelude::FluentBuilder as _,
};
use gpui_kit::component::{
    Selectable as _,
    button::Button,
    h_flex,
    input::{InputEvent, InputState},
    v_flex,
};
use openlogi_core::binding::{Action, KeyCombo};

use super::{PickFn, Recording, RecordsShortcut, ShortcutRecorder, editor_section};
use crate::ui::components::{control_button, control_input};
use crate::ui::theme::{Palette, Typography as _};

#[cfg(test)]
mod tests;

/// Why the custom shortcut editor shows an error.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ShortcutError {
    /// The typed text, or the recorded key, is not a chord a shortcut can
    /// send.
    Unrecognized,
    /// Record saw Shift with a digit or symbol key, which it cannot record
    /// (see [`Recording::ShiftedSymbol`]). Typing the chord still works.
    ShiftedSymbol,
}

/// Whether the custom shortcut editor commits a tapped or a held chord.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ShortcutMode {
    Tap,
    Hold,
}

impl ShortcutMode {
    /// The action that sends `combo` in this mode.
    pub(crate) fn action(self, combo: KeyCombo) -> Action {
        match self {
            Self::Tap => Action::CustomShortcut(combo),
            Self::Hold => Action::HoldShortcut(combo),
        }
    }

    /// The chord and mode of a bound custom shortcut, or `None` for any other
    /// action.
    pub(crate) fn of(action: &Action) -> Option<(&KeyCombo, Self)> {
        match action {
            Action::CustomShortcut(combo) => Some((combo, Self::Tap)),
            Action::HoldShortcut(combo) => Some((combo, Self::Hold)),
            _ => None,
        }
    }
}

/// The custom shortcut editor's draft: the chord field, the chosen mode, the
/// field's error, and the recording in progress.
pub(crate) struct ShortcutDraft {
    input: Entity<InputState>,
    mode: ShortcutMode,
    error: Option<ShortcutError>,
    /// The recording in progress, if any. Dropping it stops recording.
    recorder: Option<ShortcutRecorder>,
    /// The Record button's area; a recording lasts while focus stays in it.
    record_focus: FocusHandle,
}

/// A view that shows a [`shortcut_editor`] for the draft it owns.
pub(crate) trait EditsShortcut: RecordsShortcut {
    /// The view's custom shortcut draft, or `None` while it has none: a view
    /// may create the draft only when its editor opens and drop it when the
    /// editor closes.
    fn shortcut_draft(&mut self) -> Option<&mut ShortcutDraft>;
}

impl ShortcutDraft {
    /// An empty draft in Tap mode. Editing its field clears the field's
    /// error, so a corrected chord no longer reads as invalid.
    pub(crate) fn new<V: EditsShortcut>(window: &mut Window, cx: &mut Context<V>) -> Self {
        let input = cx.new(|cx| InputState::new(window, cx).placeholder(Self::placeholder()));
        cx.subscribe(&input, |view, _, event: &InputEvent, cx| {
            if matches!(event, InputEvent::Change)
                && let Some(draft) = view.shortcut_draft()
            {
                draft.error = None;
                cx.notify();
            }
        })
        .detach();
        Self {
            input,
            mode: ShortcutMode::Tap,
            error: None,
            recorder: None,
            record_focus: cx.focus_handle(),
        }
    }

    fn placeholder() -> gpui::SharedString {
        tr!("action_ring.shortcut_e_g_cmd_plus_shift_plus_p")
    }

    /// The chord field.
    #[cfg(test)]
    pub(crate) fn input(&self) -> &Entity<InputState> {
        &self.input
    }

    /// The mode Add commits the chord in.
    #[cfg(test)]
    pub(crate) fn mode(&self) -> ShortcutMode {
        self.mode
    }

    /// The field's error, if the last Add or recording failed.
    #[cfg(test)]
    pub(crate) fn error(&self) -> Option<ShortcutError> {
        self.error
    }

    /// Whether Record is waiting for the next key press.
    pub(crate) fn is_recording(&self) -> bool {
        self.recorder.is_some()
    }

    /// Commit the chord in `mode` from now on.
    pub(crate) fn set_mode(&mut self, mode: ShortcutMode) {
        self.mode = mode;
    }

    /// Re-stamp the field's placeholder after a language switch.
    pub(crate) fn localize(&self, window: &mut Window, cx: &mut App) {
        crate::ui::components::localize_placeholder(&self.input, Self::placeholder(), window, cx);
    }

    /// Fill the draft from `bound`: the chord and mode of a bound custom
    /// shortcut, or an empty Tap draft for anything else. Clears the error
    /// and stops a recording.
    pub(crate) fn reset(&mut self, bound: Option<&Action>, window: &mut Window, cx: &mut App) {
        let (text, mode) = bound
            .and_then(ShortcutMode::of)
            .map_or((String::new(), ShortcutMode::Tap), |(combo, mode)| {
                (combo.rendered_label(), mode)
            });
        self.input
            .update(cx, |input, cx| input.set_value(text, window, cx));
        self.mode = mode;
        self.error = None;
        self.recorder = None;
    }

    /// Stop the recording in progress, if any.
    pub(crate) fn stop_recording(&mut self) {
        self.recorder = None;
    }

    /// Start recording, or stop the recording in progress.
    pub(crate) fn toggle_recording<V: RecordsShortcut>(
        &mut self,
        window: &mut Window,
        cx: &mut Context<V>,
    ) {
        if self.recorder.take().is_none() {
            let area = self.record_focus.clone();
            self.recorder = Some(ShortcutRecorder::start(&area, window, cx));
        }
        cx.notify();
    }

    /// End the recording with `recording`: a chord fills the field, and Add
    /// commits it in the chosen mode. A key the recorder cannot turn into a
    /// chord shows the field's error and leaves the field as it was.
    pub(crate) fn recorded(&mut self, recording: Recording, window: &mut Window, cx: &mut App) {
        self.recorder = None;
        match recording {
            Recording::Chord(combo) => {
                self.input.update(cx, |input, cx| {
                    input.set_value(combo.rendered_label(), window, cx);
                });
                self.error = None;
            }
            Recording::Cancelled => {}
            Recording::Unsupported => self.error = Some(ShortcutError::Unrecognized),
            Recording::ShiftedSymbol => self.error = Some(ShortcutError::ShiftedSymbol),
        }
    }

    /// The action Add commits for the field's text, or the field's error when
    /// the text is not a chord.
    fn submit(&mut self, cx: &App) -> Option<Action> {
        let Ok(combo) = self.input.read(cx).value().parse::<KeyCombo>() else {
            self.error = Some(ShortcutError::Unrecognized);
            return None;
        };
        Some(self.mode.action(combo))
    }
}

/// The "Custom shortcut" editor for `view`'s draft, matching the Action Ring
/// editor's `shortcut_editor` (`features/action_ring/editor.rs`). Record fills
/// the field from the next key press; Add hands the chord, in the chosen
/// mode, to `on_pick`. `id_prefix` names the debug selectors
/// `{id_prefix}-shortcut-{tap,hold}` and `{id_prefix}-custom-shortcut-*`.
pub(crate) fn shortcut_editor<V: EditsShortcut>(
    id_prefix: &'static str,
    draft: &ShortcutDraft,
    view: &Entity<V>,
    on_pick: &PickFn,
    pal: Palette,
) -> impl IntoElement {
    let mode = draft.mode;
    let recording = draft.is_recording();
    let mode_button = |option: ShortcutMode| {
        let (id, label) = match option {
            ShortcutMode::Tap => ("tap", tr!("actions.shortcut_tap")),
            ShortcutMode::Hold => ("hold", tr!("actions.shortcut_hold")),
        };
        let view = view.clone();
        control_button(format!("{id_prefix}-shortcut-{id}"))
            .debug_selector(move || format!("{id_prefix}-shortcut-{id}"))
            .selected(mode == option)
            .label(label)
            .on_click(move |_, _, cx| {
                view.update(cx, |view, cx| {
                    if let Some(draft) = view.shortcut_draft() {
                        draft.set_mode(option);
                        cx.notify();
                    }
                });
            })
    };
    let tap_button = mode_button(ShortcutMode::Tap);
    let hold_button = mode_button(ShortcutMode::Hold);
    let record_view = view.clone();
    let add_view = view.clone();
    let on_pick = on_pick.clone();
    v_flex()
        .gap_1()
        .child(editor_section(tr!("action_ring.custom_shortcut"), pal))
        .child(h_flex().gap_2().child(tap_button).child(hold_button))
        .child(
            h_flex()
                .gap_2()
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .debug_selector(move || format!("{id_prefix}-custom-shortcut-input"))
                        .child(control_input(&draft.input).cleanable(true)),
                )
                .child(
                    div().track_focus(&draft.record_focus).child(
                        Button::new(format!("{id_prefix}-custom-shortcut-record"))
                            .debug_selector(move || format!("{id_prefix}-custom-shortcut-record"))
                            .compact()
                            .selected(recording)
                            .label(if recording {
                                tr!("common.cancel")
                            } else {
                                tr!("actions.record_shortcut")
                            })
                            .on_click(move |_, window, cx| {
                                record_view.update(cx, |view, cx| {
                                    if let Some(draft) = view.shortcut_draft() {
                                        draft.toggle_recording(window, cx);
                                    }
                                });
                            }),
                    ),
                )
                .child(
                    Button::new(format!("{id_prefix}-custom-shortcut-add"))
                        .debug_selector(move || format!("{id_prefix}-custom-shortcut-add"))
                        .compact()
                        .label(tr!("common.add"))
                        .on_click(move |_, window, cx| {
                            let action = add_view.update(cx, |view, cx| {
                                let action = view.shortcut_draft()?.submit(cx);
                                cx.notify();
                                action
                            });
                            if let Some(action) = action {
                                (on_pick)(action, window, cx);
                            }
                        }),
                ),
        )
        .when(recording, |editor| {
            editor.child(
                div()
                    .debug_selector(move || format!("{id_prefix}-custom-shortcut-recording"))
                    .text_caption()
                    .text_color(pal.text_muted)
                    .child(tr!("actions.press_the_shortcut_to_record")),
            )
        })
        .when_some(draft.error, |editor, error| {
            editor.child(
                div()
                    .debug_selector(move || format!("{id_prefix}-custom-shortcut-error"))
                    .text_caption()
                    .text_color(pal.text_danger)
                    .child(match error {
                        ShortcutError::Unrecognized => {
                            tr!("action_ring.custom_action_invalid_input")
                        }
                        ShortcutError::ShiftedSymbol => tr!("actions.shifted_symbol_unrecordable"),
                    }),
            )
        })
}
