//! Inline editors for the parameterised power-user actions, shown inside the
//! config panel (the side inspector) once one is selected from the list.
//!
//! Each editor reuses the shared [`compact_panel`] surface. Draft state lives on
//! the [`FunctionRowView`] so it survives re-rendering. Closing the editor
//! returns to the action list; the panel itself closes when the key is
//! deselected.
//!
//! [`compact_panel`]: crate::features::mouse::picker::compact_panel

#![expect(
    clippy::needless_pass_by_value,
    clippy::redundant_closure_for_method_calls,
    reason = "GPUI builders take owned Copy palette values; entity.update wants closures"
)]

use gpui::{
    App, Entity, FontWeight, IntoElement, ParentElement, RenderOnce, Styled, Window, div, px, svg,
};
use gpui_component::{
    Icon, IconName, Sizable as _,
    button::{Button, ButtonVariants},
    h_flex,
    input::InputState,
    v_flex,
};
use openlogi_core::binding::{Action, KeyCombo, WorkflowStep};
use openlogi_core::config::KeyTrigger;

use super::function_row::FunctionRowView;
use crate::features::mouse::picker::{compact_panel, divider, editor_scroll_list, title};
use crate::state::{AppState, DeviceRecord, StateEvent};
use crate::ui::components::{MenuRow, control_input};
use crate::ui::theme::{self, Palette, Typography as _};

/// Which power-user editor is showing for the selected key.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum PowerUserKind {
    TypeText,
    RunAppleScript,
    RunShellCommand,
    Workflow,
}

impl PowerUserKind {
    fn heading_key(self) -> &'static str {
        match self {
            Self::TypeText => "actions.type_text_heading",
            Self::RunAppleScript => "actions.run_applescript_heading",
            Self::RunShellCommand => "actions.run_shell_command_heading",
            Self::Workflow => "actions.workflow_heading",
        }
    }
}

pub(crate) fn text_editor_placeholder(kind: PowerUserKind) -> gpui::SharedString {
    match kind {
        PowerUserKind::TypeText => tr!("actions.type_text_placeholder"),
        PowerUserKind::RunAppleScript => "display dialog \"Hello\"".into(),
        PowerUserKind::RunShellCommand => "echo hello".into(),
        PowerUserKind::Workflow => "".into(),
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
) -> gpui::Div {
    match kind {
        PowerUserKind::Workflow => workflow_editor_card(trigger, workflow_draft, view, pal),
        _ => match text_state {
            Some(state) => text_editor_card(trigger, kind, state, view, pal),
            None => compact_panel(pal)
                .w(px(300.))
                .child(title(tr!("keyboard.editor_unavailable"), pal)),
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
) -> gpui::Div {
    let heading = tr!(kind.heading_key());
    let key_name = trigger.to_string();

    compact_panel(pal)
        .w(px(300.))
        .child(title(
            tr!("actions.action_key_summary", action => heading, key => key_name),
            pal,
        ))
        .child(divider(pal))
        .child(
            v_flex()
                .p_2()
                .gap_2()
                .child(div().child(control_input(&text_state).cleanable(true)))
                .child(editor_action_row(trigger, kind, view)),
        )
}

/// Cancel (back to list) + Save (commit the drafted text).
fn editor_action_row(
    trigger: KeyTrigger,
    kind: PowerUserKind,
    view: &Entity<FunctionRowView>,
) -> impl IntoElement {
    let view_save = view.clone();
    let trigger_save = trigger.clone();
    let view_cancel = view.clone();

    h_flex()
        .gap_2()
        .justify_end()
        .child(
            Button::new("editor-cancel")
                .ghost()
                .label(tr!("common.cancel"))
                .on_click(move |_e, _window, cx| {
                    view_cancel.update(cx, |v, vcx| v.close_editor(vcx));
                }),
        )
        .child(
            Button::new("editor-save")
                .primary()
                .label(tr!("common.save"))
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
                    AppState::update(cx, |state, cx| {
                        let key = state.current_record().map(DeviceRecord::device_key);
                        state.commit_keyboard_binding(trigger_save.clone(), Some(action));
                        if let Some(key) = key {
                            cx.emit(StateEvent::BindingsChanged(key));
                        }
                    });
                    view_save.update(cx, |v, vcx| v.close_editor(vcx));
                }),
        )
}

/// The Workflow editor: a list of steps with add/remove.
fn workflow_editor_card(
    trigger: KeyTrigger,
    steps: Vec<WorkflowStep>,
    view: &Entity<FunctionRowView>,
    pal: Palette,
) -> gpui::Div {
    let key_name = trigger.to_string();

    let rows = steps
        .into_iter()
        .enumerate()
        .map(|(idx, step)| WorkflowStepRow {
            idx,
            step,
            view: view.clone(),
        });

    compact_panel(pal)
        .w(px(320.))
        .child(title(
            tr!("actions.workflow_key_summary", key => key_name),
            pal,
        ))
        .child(divider(pal))
        .child(editor_scroll_list("workflow-steps", rows))
        .child(
            h_flex()
                .p_2()
                .gap_2()
                .justify_between()
                .child(
                    Button::new("wf-add-step")
                        .ghost()
                        .small()
                        .label(tr!("actions.add_workflow_step"))
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
                        .label(tr!("actions.save_workflow"))
                        .on_click({
                            let v = view.clone();
                            let trigger = trigger.clone();
                            move |_e, _window, cx| {
                                let steps = v.read(cx).workflow_draft().to_vec();
                                let action = Action::Workflow(steps);
                                AppState::update(cx, |state, cx| {
                                    let key = state.current_record().map(DeviceRecord::device_key);
                                    state.commit_keyboard_binding(trigger.clone(), Some(action));
                                    if let Some(key) = key {
                                        cx.emit(StateEvent::BindingsChanged(key));
                                    }
                                });
                                v.update(cx, |v, vcx| v.close_editor(vcx));
                            }
                        }),
                ),
        )
}

/// One Workflow step row: type chip + payload preview + remove button.
#[derive(IntoElement)]
struct WorkflowStepRow {
    idx: usize,
    step: WorkflowStep,
    view: Entity<FunctionRowView>,
}

impl RenderOnce for WorkflowStepRow {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let (type_label, glyph): (&'static str, &'static str) = match &self.step {
            WorkflowStep::TypeText(_) => ("Type Text", "action-icons/keyboard.svg"),
            WorkflowStep::PressKey(_) => ("Press Key", "action-icons/keyboard.svg"),
            WorkflowStep::Delay { .. } => ("Delay", "action-icons/chevrons-right.svg"),
            WorkflowStep::RunAppleScript(_) => ("AppleScript", "action-icons/terminal.svg"),
            WorkflowStep::RunShellCommand(_) => ("Shell", "action-icons/terminal.svg"),
        };
        let pal = theme::palette(cx);
        let view_remove = self.view;

        MenuRow::new(("wf-step", self.idx))
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
                            .text_caption()
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(pal.text_muted)
                            .child(type_label),
                    )
                    .child(div().flex_1().child(step_preview(&self.step, pal))),
            )
            .child(
                Icon::new(IconName::Close)
                    .size_3()
                    .text_color(pal.text_muted),
            )
            .on_click(move |_e, _w, cx| {
                view_remove.update(cx, |v, vcx| v.remove_workflow_step(self.idx, vcx));
            })
    }
}

fn step_preview(step: &WorkflowStep, pal: Palette) -> impl IntoElement {
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
        .text_caption()
        .text_color(pal.text_primary)
        .child(text)
}

fn key_combo_preview(combo: &KeyCombo) -> String {
    combo.rendered_label()
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
