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
use openlogi_core::binding::{Action, RingSlot};

use crate::app_menu::{CloseWindow, Minimize, Zoom};
use crate::state::AppState;
use crate::theme;
use crate::windows::{self, AuxWindow, WindowRegistry};

/// Which payload action this editor edits.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PayloadKind {
    /// [`Action::Run`] — a URL, file, or program (`||` separates arguments).
    Run,
    /// [`Action::PasteText`] — a text snippet typed at the cursor.
    PasteText,
}

impl PayloadKind {
    /// The dialog / flyout-row title (an i18n key).
    pub fn title_key(self) -> &'static str {
        match self {
            PayloadKind::Run => "Run Command",
            PayloadKind::PasteText => "Paste Text",
        }
    }

    fn placeholder_key(self) -> &'static str {
        match self {
            PayloadKind::Run => "Command or URL",
            PayloadKind::PasteText => "Text to paste",
        }
    }

    fn hint_key(self) -> &'static str {
        match self {
            PayloadKind::Run => {
                "A URL, file, or program. After a program path, use || to separate \
                 arguments; %VAR% environment variables expand."
            }
            PayloadKind::PasteText => "Typed at the cursor exactly as written.",
        }
    }

    /// The payload of `action` when it already is this kind — the editor
    /// seeds its input with it so reopening a configured slot edits in
    /// place.
    pub fn payload_of(self, action: &Action) -> Option<&str> {
        match (self, action) {
            (PayloadKind::Run, Action::Run(payload)) => Some(payload),
            (PayloadKind::PasteText, Action::PasteText(text)) => Some(text),
            _ => None,
        }
    }

    /// Wrap `payload` in this kind's [`Action`] variant. (With an empty
    /// payload it also serves as the icon-lookup sample for the flyout row.)
    pub fn action(self, payload: String) -> Action {
        match self {
            PayloadKind::Run => Action::Run(payload),
            PayloadKind::PasteText => Action::PasteText(payload),
        }
    }
}

/// Standalone payload-editor window root view.
pub struct RingActionEditorView {
    focus_handle: FocusHandle,
    #[allow(dead_code, reason = "held to keep the appearance observer alive")]
    appearance_obs: Option<Subscription>,
    slot: RingSlot,
    kind: PayloadKind,
    input: Entity<InputState>,
}

impl AuxWindow for RingActionEditorView {
    fn set_appearance_obs(&mut self, sub: Subscription) {
        self.appearance_obs = Some(sub);
    }
}

/// Open the editor for `slot`, seeded with the slot's current payload when it
/// already holds an action of `kind`. An editor left open for another slot is
/// closed first — focusing it would edit the wrong slot.
pub fn open(slot: RingSlot, kind: PayloadKind, cx: &mut App) {
    if let Some(handle) = cx
        .default_global::<WindowRegistry>()
        .ring_action_editor
        .take()
    {
        let _ = handle.update(cx, |_, window, _| window.remove_window());
    }

    let seed: String = cx
        .try_global::<AppState>()
        .and_then(|state| {
            state
                .ring_slots_for_current()
                .into_iter()
                .find(|(candidate, _)| *candidate == slot)
                .and_then(|(_, action)| kind.payload_of(&action).map(str::to_owned))
        })
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
                slot,
                kind,
                input,
            }
        },
        cx,
    );
}

impl Render for RingActionEditorView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let pal = theme::palette(cx);
        let (slot, kind, input) = (self.slot, self.kind, self.input.clone());

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
                            .child(div().text_sm().text_color(pal.text_muted).child(format!(
                                "{}  {}",
                                self.slot.glyph(),
                                tr!(self.slot.label())
                            ))),
                    )
                    .child(Input::new(&self.input))
                    .child(
                        div()
                            .text_sm()
                            .text_color(pal.text_muted)
                            .child(tr!(self.kind.hint_key())),
                    )
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
                                    .on_click(move |_, window, cx| {
                                        let payload =
                                            input.read(cx).value().trim().to_owned();
                                        if !payload.is_empty() {
                                            let action = kind.action(payload);
                                            cx.update_global::<AppState, _>(|state, _| {
                                                state.commit_ring_binding(slot, action);
                                            });
                                        }
                                        window.remove_window();
                                    }),
                            ),
                    ),
            )
    }
}
