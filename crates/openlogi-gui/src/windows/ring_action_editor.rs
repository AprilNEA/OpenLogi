//! Payload editor for the Action Ring's parameterized actions.
//!
//! `Run` and `PasteText` carry free text a picker row can't capture, so the
//! ring flyout's "Run Command…" / "Paste Text…" rows open this small dialog —
//! the same shape as the Options+ Run editor: one input, a hint line, Cancel
//! / Save. Saving commits the action to the slot through the same
//! [`AppState::commit_ring_binding`] path the plain rows use.

use gpui::{
    App, AppContext as _, BorrowAppContext as _, Context, Entity, FocusHandle, FontWeight,
    InteractiveElement, IntoElement, ParentElement as _, Render, Size, Styled as _, Subscription,
    Window, div, prelude::FluentBuilder as _, px,
};
use gpui_component::{
    // Named (not `as _`) because the record button selects its variant with
    // `.when(cond, ButtonVariants::danger)`, which needs the trait's path.
    button::{Button, ButtonVariants},
    h_flex,
    input::{Input, InputState},
    v_flex,
};
use openlogi_core::binding::{Action, ButtonId, GestureDirection, KeyCombo, RingSlot};

use crate::app_menu::{CloseWindow, Minimize, Zoom};
use crate::chord_recorder::{ChordRecorder, Poll};
use crate::state::AppState;
use crate::theme;
use crate::windows::{self, AuxWindow, WindowRegistry};

/// How often the chord recorder samples the keyboard while listening.
const RECORD_TICK: std::time::Duration = std::time::Duration::from_millis(30);
/// Give up listening after this long with nothing pressed, so an abandoned
/// dialog doesn't leave a timer running forever.
const RECORD_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(15);

/// Where a ring edit lands: a top-level slot, or a sub-slot inside the
/// folder at `folder`. Carried by the payload dialog and the editor's
/// action panel so both commit through the right path.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EditTarget {
    /// A top-level Action Ring slot.
    Slot(RingSlot),
    /// A position inside the folder at the top-level slot `folder`.
    FolderSlot {
        /// The top-level slot holding the folder.
        folder: RingSlot,
        /// The position inside that folder.
        sub: RingSlot,
    },
    /// One direction of the gesture button (hold + swipe).
    Gesture(GestureDirection),
    /// A plain button's single action — the DPI toggle, Middle, Back, …
    Button(ButtonId),
}

impl EditTarget {
    /// The action currently at this target, resolved against the selected
    /// device's ring.
    pub fn current_action(self, state: &AppState) -> Option<Action> {
        match self {
            EditTarget::Slot(slot) => state
                .ring_slots_for_current()
                .into_iter()
                .find(|(candidate, _)| *candidate == slot)
                .map(|(_, action)| action),
            EditTarget::FolderSlot { folder, sub } => state
                .ring_slots_for_current()
                .into_iter()
                .find(|(candidate, _)| *candidate == folder)
                .and_then(|(_, action)| match action {
                    Action::Folder(items) => items.get(&sub).cloned(),
                    _ => None,
                }),
            EditTarget::Gesture(direction) => state.gesture_bindings.get(&direction).cloned(),
            EditTarget::Button(button) => state.button_bindings.get(&button).cloned(),
        }
    }

    /// Commit `action` to this target through the matching [`AppState`]
    /// path.
    pub fn commit(self, action: Action, cx: &mut App) {
        cx.update_global::<AppState, _>(|state, _| match self {
            EditTarget::Slot(slot) => state.commit_ring_binding(slot, action),
            EditTarget::FolderSlot { folder, sub } => {
                state.commit_ring_folder_binding(folder, sub, action);
            }
            EditTarget::Gesture(direction) => state.commit_gesture_binding(direction, action),
            EditTarget::Button(button) => state.commit_binding(button, action),
        });
    }

    /// Compass caption for dialog subtitles: `↗ Top Right` or
    /// `↗ Top Right › ↓ Bottom`.
    pub fn caption(self) -> String {
        match self {
            EditTarget::Slot(slot) => format!("{}  {}", slot.glyph(), tr!(slot.label())),
            EditTarget::FolderSlot { folder, sub } => format!(
                "{}  {}  ›  {}  {}",
                folder.glyph(),
                tr!(folder.label()),
                sub.glyph(),
                tr!(sub.label())
            ),
            EditTarget::Gesture(direction) => {
                format!("{}  {}", direction.glyph(), tr!(direction.label()))
            }
            EditTarget::Button(button) => tr!(button.label()).to_string(),
        }
    }
}

/// Which payload action this editor edits.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PayloadKind {
    /// [`Action::Run`] — a URL, file, or program (`||` separates arguments).
    Run,
    /// [`Action::PasteText`] — a text snippet typed at the cursor.
    PasteText,
    /// [`Action::CustomShortcut`] — a chord entered as text
    /// (`"Win+Ctrl+Space"`) and parsed by [`KeyCombo::parse`].
    Shortcut,
}

impl PayloadKind {
    /// The dialog / flyout-row title (an i18n key).
    pub fn title_key(self) -> &'static str {
        match self {
            PayloadKind::Run => "Run Command",
            PayloadKind::PasteText => "Paste Text",
            PayloadKind::Shortcut => "Keyboard Shortcut",
        }
    }

    fn placeholder_key(self) -> &'static str {
        match self {
            PayloadKind::Run => "Command or URL",
            PayloadKind::PasteText => "Text to paste",
            PayloadKind::Shortcut => "e.g. Ctrl+Shift+P",
        }
    }

    fn hint_key(self) -> &'static str {
        match self {
            PayloadKind::Run => {
                "A URL, file, or program. After a program path, use || to separate \
                 arguments; %VAR% environment variables expand."
            }
            PayloadKind::PasteText => "Typed at the cursor exactly as written.",
            PayloadKind::Shortcut => {
                "Modifier names joined with +, then one key: Ctrl+Shift+P, \
                 Win+Ctrl+Space, Alt+F4."
            }
        }
    }

    /// Caption above this kind's payload input.
    fn field_label_key(self) -> &'static str {
        match self {
            PayloadKind::Run => "Command",
            PayloadKind::PasteText => "Text",
            PayloadKind::Shortcut => "Shortcut",
        }
    }

    /// The vendored icon shown on this kind's editor row.
    pub fn icon_path(self) -> &'static str {
        match self {
            PayloadKind::Run => "action-icons/square-arrow-right.svg",
            PayloadKind::PasteText => "action-icons/clipboard-paste.svg",
            PayloadKind::Shortcut => "action-icons/keyboard.svg",
        }
    }

    /// The editable payload of `action` when it already is this kind — the
    /// editor seeds its input with it so reopening a configured slot edits
    /// in place.
    pub fn payload_of(self, action: &Action) -> Option<String> {
        // Look through a user label: naming a Run action doesn't stop it
        // being the Run action this dialog edits.
        match (self, action.inner()) {
            (PayloadKind::Run, Action::Run(payload)) => Some(payload.clone()),
            (PayloadKind::PasteText, Action::PasteText(text)) => Some(text.clone()),
            (PayloadKind::Shortcut, Action::CustomShortcut(combo)) => Some(combo.rendered_label()),
            _ => None,
        }
    }

    /// Parse `payload` into this kind's [`Action`]. `None` means the input
    /// is not valid for this kind (only `Shortcut` can fail).
    pub fn to_action(self, payload: String) -> Option<Action> {
        match self {
            PayloadKind::Run => Some(Action::Run(payload)),
            PayloadKind::PasteText => Some(Action::PasteText(payload)),
            PayloadKind::Shortcut => KeyCombo::parse(&payload).map(Action::CustomShortcut),
        }
    }
}

/// Standalone payload-editor window root view.
pub struct RingActionEditorView {
    focus_handle: FocusHandle,
    #[allow(dead_code, reason = "held to keep the appearance observer alive")]
    appearance_obs: Option<Subscription>,
    target: EditTarget,
    kind: PayloadKind,
    input: Entity<InputState>,
    /// The user's label for this action ("Wispr Voice"), shown on the ring
    /// instead of the raw payload. Empty means "use the payload's own
    /// label".
    name_input: Entity<InputState>,
    /// Set when Save could not parse the input (Shortcut only); renders an
    /// inline error until the next attempt.
    invalid: bool,
    /// Live chord recording (Shortcut only): the chord held so far, or an
    /// empty string right after "Press Keys" and before the first key.
    listening: Option<String>,
}

impl AuxWindow for RingActionEditorView {
    fn set_appearance_obs(&mut self, sub: Subscription) {
        self.appearance_obs = Some(sub);
    }
}

/// Open the editor for `target`, seeded with its current payload when it
/// already holds an action of `kind`. An editor left open for another target
/// is closed first — focusing it would edit the wrong slot.
pub fn open(target: EditTarget, kind: PayloadKind, cx: &mut App) {
    if let Some(handle) = cx
        .default_global::<WindowRegistry>()
        .ring_action_editor
        .take()
    {
        let _ = handle.update(cx, |_, window, _| window.remove_window());
    }

    let current = cx
        .try_global::<AppState>()
        .and_then(|state| target.current_action(state));
    // Seed both fields from the slot, but only when it already holds this
    // kind — a name belongs to the action it was given to.
    let seed: String = current
        .as_ref()
        .and_then(|action| kind.payload_of(action))
        .unwrap_or_default();
    let name_seed: String = current
        .as_ref()
        .filter(|action| kind.payload_of(action).is_some())
        .and_then(|action| action.display_name().map(str::to_owned))
        .unwrap_or_default();

    windows::open_or_focus(
        |reg| &mut reg.ring_action_editor,
        tr!(kind.title_key()),
        Size::new(px(440.), px(320.)),
        move |window, cx| {
            let focus_handle = cx.focus_handle();
            let input = cx.new(|cx| {
                InputState::new(window, cx)
                    .placeholder(tr!(kind.placeholder_key()))
                    .default_value(seed)
            });
            let name_input = cx.new(|cx| {
                InputState::new(window, cx)
                    .placeholder(tr!("Optional — shown on the ring"))
                    .default_value(name_seed)
            });
            // Type straight away — the payload is the point of the dialog.
            input.update(cx, |state, cx| state.focus(window, cx));
            RingActionEditorView {
                focus_handle,
                appearance_obs: None,
                target,
                kind,
                input,
                name_input,
                invalid: false,
                listening: None,
            }
        },
        cx,
    );
}

/// Start listening for a physical chord on `editor`, sampling until the
/// user releases a real chord (committed straight to `target` — recording
/// *is* the save), presses Escape alone, or the timeout elapses.
fn start_listening(editor: &Entity<RingActionEditorView>, target: EditTarget, cx: &mut App) {
    editor.update(cx, |view, vcx| {
        view.invalid = false;
        view.listening = Some(String::new());
        vcx.notify();
    });

    let editor = editor.clone();
    cx.spawn(async move |cx| {
        let mut recorder = ChordRecorder::default();
        let mut idle_ticks = 0_u32;
        let max_idle = RECORD_TIMEOUT.as_millis() / RECORD_TICK.as_millis().max(1);
        loop {
            cx.background_executor().timer(RECORD_TICK).await;
            let done = cx.update(|cx| {
                // The dialog closed (or a different one opened) — stop.
                let still_listening = editor.read_with(cx, |view, _| view.listening.is_some());
                if !still_listening {
                    return true;
                }
                match recorder.poll() {
                    Poll::Idle => {
                        idle_ticks += 1;
                        if u128::from(idle_ticks) >= max_idle {
                            stop_listening(&editor, cx);
                            return true;
                        }
                        false
                    }
                    Poll::Holding(chord) => {
                        idle_ticks = 0;
                        editor.update(cx, |view, vcx| {
                            view.listening = Some(chord);
                            vcx.notify();
                        });
                        false
                    }
                    Poll::Cancelled => {
                        stop_listening(&editor, cx);
                        true
                    }
                    Poll::Done(chord) => {
                        // The recorder emits canonical chord text, so this
                        // parse is the same one a typed chord takes. Any
                        // name already typed in the dialog rides along.
                        let name = editor.read_with(cx, |view, cx| {
                            view.name_input.read(cx).value().trim().to_owned()
                        });
                        if let Some(action) = PayloadKind::Shortcut.to_action(chord) {
                            target.commit(action.with_name(&name), cx);
                            close_editor_window(cx);
                        } else {
                            stop_listening(&editor, cx);
                        }
                        true
                    }
                }
            });
            if done {
                break;
            }
        }
    })
    .detach();
}

/// The "Press Keys" button plus its live readout. While listening the row
/// shows the chord as it is held (or a prompt before the first key); the
/// captured chord commits and closes on release, so recording *is* saving.
fn record_row(
    listening: Option<&str>,
    editor: &Entity<RingActionEditorView>,
    target: EditTarget,
    pal: crate::theme::Palette,
) -> impl IntoElement {
    let editor_click = editor.clone();
    let is_listening = listening.is_some();
    let readout: gpui::SharedString = match listening {
        Some("") => tr!("Press a shortcut…"),
        Some(chord) => chord.to_owned().into(),
        None => tr!("Or record it: hold the keys, then let go."),
    };

    h_flex()
        .items_center()
        .gap_3()
        .child(
            Button::new("ring-editor-record")
                .when(is_listening, ButtonVariants::danger)
                .when(!is_listening, Button::outline)
                .label(if is_listening {
                    tr!("Listening…")
                } else {
                    tr!("Press Keys")
                })
                .on_click(move |_, _, cx| {
                    let listening = editor_click.read_with(cx, |view, _| view.listening.is_some());
                    if listening {
                        stop_listening(&editor_click, cx);
                    } else {
                        start_listening(&editor_click, target, cx);
                    }
                }),
        )
        .child(
            div()
                .text_sm()
                .when(is_listening, |s| s.font_weight(FontWeight::SEMIBOLD))
                .text_color(if is_listening {
                    pal.text_primary
                } else {
                    pal.text_muted
                })
                .child(readout),
        )
}

/// Leave listening mode without committing.
fn stop_listening(editor: &Entity<RingActionEditorView>, cx: &mut App) {
    editor.update(cx, |view, vcx| {
        view.listening = None;
        vcx.notify();
    });
}

/// Close the payload dialog from outside its own event handlers (the
/// recorder's timer has no `&mut Window`), via the registry handle.
fn close_editor_window(cx: &mut App) {
    if let Some(handle) = cx
        .default_global::<WindowRegistry>()
        .ring_action_editor
        .take()
    {
        let _ = handle.update(cx, |_, window, _| window.remove_window());
    }
}

impl Render for RingActionEditorView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let pal = theme::palette(cx);
        let (target, kind, input) = (self.target, self.kind, self.input.clone());
        let editor = cx.entity();

        v_flex()
            .size_full()
            .bg(pal.bg)
            .text_color(pal.text_primary)
            .track_focus(&self.focus_handle)
            .on_action(|_: &CloseWindow, window, _| window.remove_window())
            .on_action(|_: &Minimize, window, _| window.minimize_window())
            .on_action(|_: &Zoom, window, _| window.zoom_window())
            .when(cfg!(target_os = "linux"), |this| {
                this.child(windows::aux_title_bar(tr!(self.kind.title_key()), cx))
            })
            .child(
                v_flex()
                    .flex_1()
                    .w_full()
                    .gap_3()
                    .p_5()
                    .child(
                        h_flex()
                            .items_center()
                            .gap_2()
                            .child(
                                div()
                                    .text_lg()
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .child(tr!(self.kind.title_key())),
                            )
                            .child(
                                div()
                                    .text_sm()
                                    .text_color(pal.text_muted)
                                    .child(self.target.caption()),
                            ),
                    )
                    // Recording is the primary path for a shortcut; the
                    // text field stays as the editable / accessible form.
                    .when(
                        kind == PayloadKind::Shortcut && crate::chord_recorder::is_supported(),
                        |this| {
                            this.child(record_row(
                                self.listening.as_deref(),
                                &editor,
                                target,
                                pal,
                            ))
                        },
                    )
                    .child(field_label(tr!(self.kind.field_label_key()), pal))
                    .child(Input::new(&self.input))
                    .child(
                        div()
                            .text_sm()
                            .text_color(pal.text_muted)
                            .child(tr!(self.kind.hint_key())),
                    )
                    .child(field_label(tr!("Name"), pal))
                    .child(Input::new(&self.name_input))
                    .when(self.invalid, |this| {
                        this.child(
                            div()
                                .text_sm()
                                .text_color(gpui::rgb(0x00d6_4541))
                                .child(tr!(
                                    "That shortcut could not be read — use the Ctrl+Shift+P shape."
                                )),
                        )
                    })
                    .child(button_row(
                        &input,
                        &self.name_input,
                        &editor,
                        target,
                        kind,
                    )),
            )
    }
}

/// Small uppercase caption above an input.
fn field_label(text: gpui::SharedString, pal: crate::theme::Palette) -> impl IntoElement {
    div()
        .text_xs()
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(pal.text_muted)
        .child(text)
}

/// Cancel / Save. An emptied input closes without committing — clearing a
/// slot is what the plain rows ("Do Nothing") are for — and input this kind
/// can't parse (Shortcut) keeps the dialog open with an inline error
/// instead of dropping the edit.
fn button_row(
    input: &Entity<InputState>,
    name_input: &Entity<InputState>,
    editor: &Entity<RingActionEditorView>,
    target: EditTarget,
    kind: PayloadKind,
) -> impl IntoElement {
    let (input, name_input, editor) = (input.clone(), name_input.clone(), editor.clone());
    h_flex()
        .w_full()
        .justify_end()
        .gap_3()
        .pt_1()
        .child(
            Button::new("ring-editor-cancel")
                .outline()
                .label(tr!("Cancel"))
                .on_click(|_, window, _| window.remove_window()),
        )
        .child(
            Button::new("ring-editor-save")
                .primary()
                .label(tr!("Save"))
                .on_click(move |_, window, cx| {
                    let payload = input.read(cx).value().trim().to_owned();
                    if payload.is_empty() {
                        window.remove_window();
                        return;
                    }
                    let name = name_input.read(cx).value().trim().to_owned();
                    match kind.to_action(payload) {
                        Some(action) => {
                            target.commit(action.with_name(&name), cx);
                            window.remove_window();
                        }
                        None => editor.update(cx, |view, vcx| {
                            view.invalid = true;
                            vcx.notify();
                        }),
                    }
                }),
        )
}
