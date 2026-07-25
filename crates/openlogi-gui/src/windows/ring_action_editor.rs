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
    button::{Button, ButtonVariants as _},
    h_flex,
    input::{Input, InputState},
    v_flex,
};
use openlogi_core::binding::{Action, KeyCombo, RingSlot};

use crate::app_menu::{CloseWindow, Minimize, Zoom};
use crate::state::AppState;
use crate::theme;
use crate::windows::{self, AuxWindow, WindowRegistry};

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
}

impl EditTarget {
    /// The action currently at this target, resolved against the selected
    /// device's ring.
    pub fn current_action(self, state: &AppState) -> Option<Action> {
        let slots = state.ring_slots_for_current();
        match self {
            EditTarget::Slot(slot) => slots
                .into_iter()
                .find(|(candidate, _)| *candidate == slot)
                .map(|(_, action)| action),
            EditTarget::FolderSlot { folder, sub } => slots
                .into_iter()
                .find(|(candidate, _)| *candidate == folder)
                .and_then(|(_, action)| match action {
                    Action::Folder(items) => items.get(&sub).cloned(),
                    _ => None,
                }),
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
        match (self, action) {
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
    /// Set when Save could not parse the input (Shortcut only); renders an
    /// inline error until the next attempt.
    invalid: bool,
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

    let seed: String = cx
        .try_global::<AppState>()
        .and_then(|state| target.current_action(state))
        .and_then(|action| kind.payload_of(&action))
        .unwrap_or_default();

    windows::open_or_focus(
        |reg| &mut reg.ring_action_editor,
        tr!(kind.title_key()),
        Size::new(px(420.), px(240.)),
        move |window, cx| {
            let focus_handle = cx.focus_handle();
            let input = cx.new(|cx| {
                InputState::new(window, cx)
                    .placeholder(tr!(kind.placeholder_key()))
                    .default_value(seed)
            });
            // Type straight away — the input is the whole dialog.
            input.update(cx, |state, cx| state.focus(window, cx));
            RingActionEditorView {
                focus_handle,
                appearance_obs: None,
                target,
                kind,
                input,
                invalid: false,
            }
        },
        cx,
    );
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
                    .child(Input::new(&self.input))
                    .child(
                        div()
                            .text_sm()
                            .text_color(pal.text_muted)
                            .child(tr!(self.kind.hint_key())),
                    )
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
                    .child(
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
                                    // An emptied input closes without
                                    // committing — clearing a slot is what
                                    // the plain rows ("Do Nothing") are for.
                                    // Unparseable input (Shortcut) keeps the
                                    // dialog open with an inline error.
                                    .on_click(move |_, window, cx| {
                                        let payload =
                                            input.read(cx).value().trim().to_owned();
                                        if payload.is_empty() {
                                            window.remove_window();
                                            return;
                                        }
                                        match kind.to_action(payload) {
                                            Some(action) => {
                                                target.commit(action, cx);
                                                window.remove_window();
                                            }
                                            None => {
                                                editor.update(cx, |view, vcx| {
                                                    view.invalid = true;
                                                    vcx.notify();
                                                });
                                            }
                                        }
                                    }),
                            ),
                    ),
            )
    }
}
