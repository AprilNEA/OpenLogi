//! Display helpers for the shared action vocabulary.

use gpui::SharedString;
use openlogi_core::binding::Action;

/// Localized label for an action, including its dynamic payload.
#[expect(
    clippy::expect_used,
    reason = "the preceding match arms handle every action without a static key"
)]
pub(crate) fn localized_action_label(action: &Action) -> SharedString {
    match action {
        Action::SetDpiPreset(index) => {
            tr!("pointer.dpi_preset", index => (index + 1).to_string())
        }
        Action::CustomShortcut(combo) => combo.rendered_label().into(),
        Action::HoldShortcut(combo) => {
            tr!("actions.hold_shortcut", chord => combo.rendered_label())
        }
        Action::TypeText(text) => tr!("actions.type_text_action", text => text.clone()),
        Action::RunAppleScript(_) => tr!("actions.run_applescript_heading"),
        Action::RunShellCommand(_) => tr!("actions.run_shell_command_heading"),
        Action::Workflow(steps) if steps.len() == 1 => {
            tr!("actions.workflow_step_count_singular")
        }
        Action::Workflow(steps) => {
            tr!("actions.workflow_step_count_plural", count => steps.len().to_string())
        }
        Action::OpenApplication(target) => {
            tr!("actions.open_named_target", name => target.display_name())
        }
        _ => tr!(action
            .translation_key()
            .expect("every payload-free action has a translation key")),
    }
}
