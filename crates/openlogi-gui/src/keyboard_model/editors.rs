//! Inline editors for the parameterised power-user actions, shown inside the
//! config panel (the side inspector) once one is selected from the list.
//!
//! Each editor reuses the shared [`menu_card`] surface. Draft state lives on
//! the [`FunctionRowView`] so it survives re-rendering. Closing the editor
//! returns to the action list; the panel itself closes when the key is
//! deselected.
//!
//! [`menu_card`]: crate::mouse_model::picker::menu_card

use std::rc::Rc;

use gpui::{
    AnyElement, BorrowAppContext as _, Context, Entity, FontWeight, IntoElement, ParentElement,
    StatefulInteractiveElement as _, Styled, div, px, svg,
};
use gpui_component::{
    Icon, IconName, Sizable as _,
    button::{Button, ButtonVariants},
    h_flex,
    input::Input,
    input::InputState,
    v_flex,
};
use openlogi_core::binding::{Action, KeyCombo, WorkflowStep};
use openlogi_core::config::KeyTrigger;

use crate::keyboard_model::function_row::FunctionRowView;
use crate::mouse_model::picker::{PickFn, divider, menu_card, menu_row, scroll_list, title};
use crate::state::AppState;
use crate::theme::Palette;

/// Which power-user editor is showing for the selected key.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum PowerUserKind {
    TypeText,
    RunAppleScript,
    RunShellCommand,
    Workflow,
}

impl PowerUserKind {
    fn heading(self) -> &'static str {
        match self {
            Self::TypeText => "Type Text",
            Self::RunAppleScript => "Run AppleScript",
            Self::RunShellCommand => "Run Shell Command",
            Self::Workflow => "Workflow",
        }
    }
}

pub(crate) fn text_editor_placeholder(kind: PowerUserKind) -> &'static str {
    match kind {
        PowerUserKind::TypeText => "Text to type…",
        PowerUserKind::RunAppleScript => "display dialog \"Hello\"",
        PowerUserKind::RunShellCommand => "echo hello",
        PowerUserKind::Workflow => "",
    }
}

pub(crate) fn text_editor_seed(action: Option<&Action>, kind: PowerUserKind) -> String {
    match (action, kind) {
        (Some(Action::TypeText(text)), PowerUserKind::TypeText)
        | (Some(Action::RunAppleScript(text)), PowerUserKind::RunAppleScript)
        | (Some(Action::RunShellCommand(text)), PowerUserKind::RunShellCommand) => text.clone(),
        _ => String::new(),
    }
}

pub(crate) fn workflow_editor_seed(action: Option<&Action>) -> Vec<WorkflowStep> {
    match action {
        Some(Action::Workflow(steps)) => steps.clone(),
        _ => Vec::new(),
    }
}

/// Render the editor card for `kind`, replacing the panel's action list.
pub fn editor_card(
    trigger: KeyTrigger,
    kind: PowerUserKind,
    text_state: Option<Entity<InputState>>,
    workflow_draft: Vec<WorkflowStep>,
    view: &Entity<FunctionRowView>,
    pal: Palette,
    cx: &mut Context<FunctionRowView>,
) -> AnyElement {
    match kind {
        PowerUserKind::Workflow => workflow_editor_card(trigger, workflow_draft, view, pal, cx),
        _ => match text_state {
            Some(state) => text_editor_card(trigger, kind, state, view, pal, cx),
            None => menu_card(pal)
                .w(px(300.))
                .child(title(tr!("Editor unavailable"), pal))
                .into_any_element(),
        },
    }
}

/// The TypeText / RunAppleScript / RunShellCommand editors share a single text
/// field; only the commit wrapping differs.
fn text_editor_card(
    trigger: KeyTrigger,
    kind: PowerUserKind,
    text_state: Entity<InputState>,
    view: &Entity<FunctionRowView>,
    pal: Palette,
    cx: &mut Context<FunctionRowView>,
) -> AnyElement {
    let heading = kind.heading();
    let key_name = trigger.to_string();

    menu_card(pal)
        .w(px(300.))
        .child(title(
            tr!("%{action} · %{key}", action => heading, key => key_name),
            pal,
        ))
        .child(divider(pal))
        .child(
            v_flex()
                .p_2()
                .gap_2()
                .child(div().child(Input::new(&text_state).cleanable(true)))
                .child(editor_action_row(trigger, kind, view, pal, cx)),
        )
        .into_any_element()
}

/// Cancel (back to list) + Save (commit the drafted text).
fn editor_action_row(
    trigger: KeyTrigger,
    kind: PowerUserKind,
    view: &Entity<FunctionRowView>,
    _pal: Palette,
    _cx: &mut Context<FunctionRowView>,
) -> AnyElement {
    let view_save = view.clone();
    let trigger_save = trigger.clone();
    let view_cancel = view.clone();

    h_flex()
        .gap_2()
        .justify_end()
        .child(
            Button::new("editor-cancel")
                .ghost()
                .label(tr!("Cancel"))
                .on_click(move |_e, _window, cx| {
                    view_cancel.update(cx, |v, vcx| v.close_editor(vcx));
                }),
        )
        .child(
            Button::new("editor-save")
                .primary()
                .label(tr!("Save"))
                .on_click(move |_e, _window, cx| {
                    let text = view_save
                        .read(cx)
                        .text_state()
                        .map(|s| s.read(cx).value().to_string())
                        .unwrap_or_default();
                    let action = match kind {
                        PowerUserKind::TypeText => Action::TypeText(text),
                        PowerUserKind::RunAppleScript => Action::RunAppleScript(text),
                        PowerUserKind::RunShellCommand => Action::RunShellCommand(text),
                        PowerUserKind::Workflow => return,
                    };
                    cx.update_global::<AppState, _>(|state, _| {
                        state.commit_keyboard_binding(trigger_save.clone(), Some(action));
                    });
                    view_save.update(cx, |v, vcx| v.close_editor(vcx));
                }),
        )
        .into_any_element()
}

/// The Workflow editor: a list of steps with add/remove.
fn workflow_editor_card(
    trigger: KeyTrigger,
    steps: Vec<WorkflowStep>,
    view: &Entity<FunctionRowView>,
    pal: Palette,
    cx: &mut Context<FunctionRowView>,
) -> AnyElement {
    let key_name = trigger.to_string();

    let mut rows: Vec<AnyElement> = Vec::new();
    for (idx, step) in steps.iter().enumerate() {
        rows.push(workflow_step_row(idx, step.clone(), view, pal, cx));
    }

    menu_card(pal)
        .w(px(320.))
        .child(title(tr!("Workflow · %{key}", key => key_name), pal))
        .child(divider(pal))
        .child(scroll_list("workflow-steps", rows))
        .child(
            h_flex()
                .p_2()
                .gap_2()
                .justify_between()
                .child(
                    Button::new("wf-add-step")
                        .ghost()
                        .small()
                        .label(tr!("+ Add Step"))
                        .on_click({
                            let v = view.clone();
                            move |_e, _w, cx| {
                                v.update(cx, |v, vcx| {
                                    v.push_workflow_step(
                                        WorkflowStep::TypeText(String::new()),
                                        vcx,
                                    );
                                });
                            }
                        }),
                )
                .child(
                    Button::new("wf-save")
                        .primary()
                        .label(tr!("Save Workflow"))
                        .on_click({
                            let v = view.clone();
                            let trigger = trigger.clone();
                            move |_e, _window, cx| {
                                let steps = v.read(cx).workflow_draft().to_vec();
                                let action = Action::Workflow(steps);
                                cx.update_global::<AppState, _>(|state, _| {
                                    state.commit_keyboard_binding(trigger.clone(), Some(action));
                                });
                                v.update(cx, |v, vcx| v.close_editor(vcx));
                            }
                        }),
                ),
        )
        .into_any_element()
}

/// One Workflow step row: type chip + payload preview + remove button.
fn workflow_step_row(
    idx: usize,
    step: WorkflowStep,
    view: &Entity<FunctionRowView>,
    pal: Palette,
    _cx: &mut Context<FunctionRowView>,
) -> AnyElement {
    let (type_label, glyph): (&'static str, &'static str) = match &step {
        WorkflowStep::TypeText(_) => ("Type Text", "action-icons/keyboard.svg"),
        WorkflowStep::PressKey(_) => ("Press Key", "action-icons/keyboard.svg"),
        WorkflowStep::Delay { .. } => ("Delay", "action-icons/chevrons-right.svg"),
        WorkflowStep::RunAppleScript(_) => ("AppleScript", "action-icons/terminal.svg"),
        WorkflowStep::RunShellCommand(_) => ("Shell", "action-icons/terminal.svg"),
    };
    let view_remove = view.clone();

    menu_row(("wf-step", idx), pal, false)
        .child(
            h_flex()
                .w_full()
                .items_center()
                .gap_2()
                .child(
                    svg()
                        .path(glyph)
                        .size_4()
                        .flex_none()
                        .text_color(pal.text_muted),
                )
                .child(
                    div()
                        .text_xs()
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(pal.text_muted)
                        .child(type_label),
                )
                .child(div().flex_1().child(step_preview(&step, pal))),
        )
        .child(
            Icon::new(IconName::Close)
                .size_3()
                .text_color(pal.text_muted),
        )
        .on_click(move |_e, _w, cx| {
            view_remove.update(cx, |v, vcx| v.remove_workflow_step(idx, vcx));
        })
        .into_any_element()
}

fn step_preview(step: &WorkflowStep, pal: Palette) -> AnyElement {
    let text: String = match step {
        WorkflowStep::TypeText(s) => {
            if s.is_empty() {
                "…".to_string()
            } else {
                format!("“{s}”")
            }
        }
        WorkflowStep::PressKey(k) => key_combo_preview(k),
        WorkflowStep::Delay { millis } => format!("{millis} ms"),
        WorkflowStep::RunAppleScript(s) | WorkflowStep::RunShellCommand(s) => {
            if s.is_empty() {
                "…".to_string()
            } else {
                s.clone()
            }
        }
    };
    div()
        .text_xs()
        .text_color(pal.text_primary)
        .child(text)
        .into_any_element()
}

fn key_combo_preview(combo: &KeyCombo) -> String {
    if !combo.display.is_empty() {
        combo.display.clone()
    } else if combo.key_code == 0 {
        "—".to_string()
    } else {
        format!("key 0x{:02X}", combo.key_code)
    }
}

#[allow(dead_code, reason = "kept for parity with the mouse picker")]
fn _silence_pickfn() -> PickFn {
    Rc::new(|_a: Action, _w: &mut gpui::Window, _cx: &mut gpui::App| {})
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_editor_seed_only_uses_matching_power_user_action() {
        assert_eq!(
            text_editor_seed(
                Some(&Action::RunAppleScript(
                    "tell app \"Finder\" to activate".into()
                )),
                PowerUserKind::RunAppleScript,
            ),
            "tell app \"Finder\" to activate"
        );
        assert_eq!(
            text_editor_seed(
                Some(&Action::RunShellCommand("echo nope".into())),
                PowerUserKind::RunAppleScript,
            ),
            ""
        );
    }

    #[test]
    fn workflow_editor_seed_only_uses_workflow_action() {
        let steps = vec![WorkflowStep::TypeText("hello".into())];
        assert_eq!(
            workflow_editor_seed(Some(&Action::Workflow(steps.clone()))),
            steps
        );
        assert!(
            workflow_editor_seed(Some(&Action::RunAppleScript(
                "display dialog \"Hello\"".into()
            )))
            .is_empty()
        );
    }
}
