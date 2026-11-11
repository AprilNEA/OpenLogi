//! The key panel's custom shortcut: a key combination the selected key sends
//! — tapped, held, or held at its modifiers — recorded from the keyboard or
//! typed, the way the mouse inspector's Custom shortcut editor works. Every
//! key the Keys tab shows can carry one: keypad and dialpad keys, the dial's
//! and the roller's directions, and a keyboard's keys.

use gpui::{
    AppContext as _, Context, Entity, InteractiveElement, ParentElement, Styled, Subscription,
    Window, div, prelude::FluentBuilder as _, px,
};
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{InputEvent, InputState};
use gpui_kit::component::{IconName, Selectable as _, Sizable as _, h_flex, v_flex};
use openlogi_core::binding::{Action, KeyCombo};

use super::{FunctionRowView, KeyTarget, commit_key_action};
use crate::features::binding_editor::{
    Recording, RecordsShortcut, ShortcutError, ShortcutMode, ShortcutRecorder, editor_section,
};
use crate::state::AppState;
use crate::ui::components::{control_button, control_input};
use crate::ui::theme::{Palette, Typography as _};

/// The shortcut editor's state for the selected key: the chord field, the
/// mode a new chord is bound with, the recording in progress, and the
/// field's error. Dropped with the selection, which also stops a recording.
pub(super) struct ShortcutDraft {
    input: Entity<InputState>,
    mode: ShortcutMode,
    recorder: Option<ShortcutRecorder>,
    error: Option<ShortcutError>,
    _input_changed: Subscription,
}

/// The mode the editor shows for a key bound to `current`: the mode of its
/// shortcut, or tapping when it has none.
pub(super) fn shown_mode(current: Option<&Action>) -> ShortcutMode {
    current
        .and_then(ShortcutMode::of)
        .map_or(ShortcutMode::Tap, |(mode, _)| mode)
}

/// The action switching a key bound to `current` to `mode` makes: the same
/// chord fired the new way, or `None` when the key carries no shortcut (the
/// mode is then only the draft's, for the next chord).
pub(super) fn rebound_in_mode(current: Option<&Action>, mode: ShortcutMode) -> Option<Action> {
    let (bound, combo) = current.and_then(ShortcutMode::of)?;
    (bound != mode).then(|| mode.action(combo.clone()))
}

/// The chord text in the field as the action it binds in `mode`, or the
/// error the field shows.
pub(super) fn typed_shortcut(text: &str, mode: ShortcutMode) -> Result<Action, ShortcutError> {
    text.trim()
        .parse::<KeyCombo>()
        .map(|combo| mode.action(combo))
        .map_err(|_| ShortcutError::Unrecognized)
}

impl FunctionRowView {
    /// Make sure the selected key has a shortcut editor, its field and mode
    /// seeded from the shortcut the key is bound to now.
    pub(super) fn ensure_shortcut_draft(
        &mut self,
        current: Option<&Action>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.shortcut.is_some() {
            return;
        }
        let seed = current
            .and_then(ShortcutMode::of)
            .map(|(_, combo)| combo.rendered_label());
        let input = cx.new(|cx| {
            let mut input = InputState::new(window, cx)
                .placeholder(tr!("action_ring.shortcut_e_g_cmd_plus_shift_plus_p"));
            if let Some(seed) = seed {
                input.set_value(seed, window, cx);
            }
            input
        });
        let input_changed = cx.subscribe(&input, |view, _, event: &InputEvent, cx| {
            if matches!(event, InputEvent::Change)
                && let Some(draft) = view.shortcut.as_mut()
            {
                draft.error = None;
                cx.notify();
            }
        });
        self.shortcut = Some(ShortcutDraft {
            input,
            mode: shown_mode(current),
            recorder: None,
            error: None,
            _input_changed: input_changed,
        });
    }

    /// Start recording the shortcut, or stop the recording in progress.
    fn toggle_shortcut_recording(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let area = self.shortcut_record_focus.clone();
        let Some(draft) = self.shortcut.as_mut() else {
            return;
        };
        if draft.recorder.take().is_none() {
            draft.recorder = Some(ShortcutRecorder::start(&area, window, cx));
        }
        cx.notify();
    }

    /// Fire the key's shortcut `mode`'s way: the mode a new chord is bound
    /// with, and — when the key already sends a chord — the chord it sends.
    fn set_shortcut_mode(
        &mut self,
        target: &KeyTarget,
        current: Option<&Action>,
        mode: ShortcutMode,
        cx: &mut Context<Self>,
    ) {
        if let Some(draft) = self.shortcut.as_mut() {
            draft.mode = mode;
        }
        if let Some(action) = rebound_in_mode(current, mode) {
            AppState::apply(cx, |state| commit_key_action(state, target, Some(action)));
        }
        cx.notify();
    }

    /// Bind the chord in the field, or show why it cannot be bound.
    fn add_typed_shortcut(&mut self, target: &KeyTarget, cx: &mut Context<Self>) {
        let Some(draft) = self.shortcut.as_mut() else {
            return;
        };
        match typed_shortcut(&draft.input.read(cx).value(), draft.mode) {
            Ok(action) => {
                draft.error = None;
                AppState::apply(cx, |state| commit_key_action(state, target, Some(action)));
            }
            Err(error) => draft.error = Some(error),
        }
        cx.notify();
    }

    /// Unbind the key's shortcut, back to its own function.
    fn clear_shortcut(&mut self, target: &KeyTarget, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(draft) = self.shortcut.as_mut() {
            draft.error = None;
            draft
                .input
                .update(cx, |input, cx| input.set_value("", window, cx));
        }
        AppState::apply(cx, |state| commit_key_action(state, target, None));
        cx.notify();
    }

    /// The Custom shortcut section for `target`, bound to `current` in the
    /// scope the panel edits; `None` before its editor exists.
    pub(super) fn shortcut_section(
        &self,
        target: &KeyTarget,
        current: Option<&Action>,
        view: &Entity<Self>,
        pal: Palette,
    ) -> Option<gpui::Div> {
        let draft = self.shortcut.as_ref()?;
        let recording = draft.recorder.is_some();
        let bound = current.and_then(ShortcutMode::of);
        let mode_buttons = ShortcutMode::ALL.into_iter().map(|mode| {
            let view = view.clone();
            let target = target.clone();
            let current = current.cloned();
            control_button(format!("key-shortcut-{}", mode.id()))
                .debug_selector(move || format!("key-shortcut-{}", mode.id()))
                .selected(draft.mode == mode)
                .label(mode.label())
                .on_click(move |_, _, cx| {
                    view.update(cx, |view, cx| {
                        view.set_shortcut_mode(&target, current.as_ref(), mode, cx);
                    });
                })
        });
        let record = {
            let view = view.clone();
            div().track_focus(&self.shortcut_record_focus).child(
                Button::new("key-shortcut-record")
                    .debug_selector(|| "key-shortcut-record".into())
                    .compact()
                    .selected(recording)
                    .label(if recording {
                        tr!("common.cancel")
                    } else {
                        tr!("actions.record_shortcut")
                    })
                    .on_click(move |_, window, cx| {
                        view.update(cx, |view, cx| view.toggle_shortcut_recording(window, cx));
                    }),
            )
        };
        let add = {
            let view = view.clone();
            let target = target.clone();
            Button::new("key-shortcut-add")
                .debug_selector(|| "key-shortcut-add".into())
                .compact()
                .label(tr!("common.add"))
                .on_click(move |_, _, cx| {
                    view.update(cx, |view, cx| view.add_typed_shortcut(&target, cx));
                })
        };
        Some(
            v_flex()
                .px_2()
                .pb_1()
                .gap_1()
                .child(editor_section(tr!("action_ring.custom_shortcut"), pal).px_0())
                .when_some(bound, |section, (_, combo)| {
                    section.child(bound_combo(combo, target, view, pal))
                })
                .child(h_flex().gap_2().flex_wrap().children(mode_buttons))
                // The chord field gets the panel's full width: the buttons'
                // translated labels would squeeze it to a few characters.
                .child(
                    div()
                        .debug_selector(|| "key-shortcut-input".into())
                        .child(control_input(&draft.input).cleanable(true)),
                )
                .child(h_flex().gap_2().child(record).child(add))
                .when(recording, |section| {
                    section.child(
                        div()
                            .debug_selector(|| "key-shortcut-recording".into())
                            .text_caption()
                            .text_color(pal.text_muted)
                            .child(tr!("actions.press_the_shortcut_to_record")),
                    )
                })
                .when_some(draft.error, |section, error| {
                    section.child(
                        div()
                            .debug_selector(|| "key-shortcut-error".into())
                            .text_caption()
                            .text_color(pal.text_danger)
                            .child(error.message()),
                    )
                }),
        )
    }
}

/// The chord the key sends, as key caps, with the button that unbinds it.
fn bound_combo(
    combo: &KeyCombo,
    target: &KeyTarget,
    view: &Entity<FunctionRowView>,
    pal: Palette,
) -> gpui::Div {
    let label = combo.rendered_label();
    let caps = label.split('+').map(|key| {
        div()
            .px_1p5()
            .min_w(px(22.))
            .h(px(22.))
            .flex()
            .items_center()
            .justify_center()
            .rounded(pal.control_radius)
            .border_1()
            .border_b_2()
            .border_color(pal.border)
            .bg(pal.control)
            .text_caption()
            .text_color(pal.text_primary)
            .child(key.to_owned())
    });
    let view = view.clone();
    let target = target.clone();
    h_flex()
        .debug_selector(|| "key-shortcut-bound".into())
        .gap_1()
        .items_center()
        .flex_wrap()
        .children(caps)
        .child(
            Button::new("key-shortcut-clear")
                .debug_selector(|| "key-shortcut-clear".into())
                .ghost()
                .xsmall()
                .icon(IconName::Close)
                .tooltip(tr!("common.clear"))
                .on_click(move |_, window, cx| {
                    view.update(cx, |view, cx| view.clear_shortcut(&target, window, cx));
                }),
        )
}

impl RecordsShortcut for FunctionRowView {
    /// A recorded chord fills the field, and Add binds it in the chosen
    /// mode — as in the mouse inspector. A key the recorder cannot turn into
    /// a chord shows the field's error and leaves the field as it was.
    fn shortcut_recorded(
        &mut self,
        recording: Recording,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(draft) = self.shortcut.as_mut() else {
            return;
        };
        draft.recorder = None;
        match recording {
            Recording::Chord(combo) => {
                draft.input.update(cx, |input, cx| {
                    input.set_value(combo.rendered_label(), window, cx);
                });
                draft.error = None;
            }
            Recording::Cancelled => {}
            Recording::Unsupported => draft.error = Some(ShortcutError::Unrecognized),
            Recording::ShiftedSymbol => draft.error = Some(ShortcutError::ShiftedSymbol),
        }
        cx.notify();
    }
}
