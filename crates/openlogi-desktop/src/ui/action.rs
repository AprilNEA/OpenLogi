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
            rust_i18n::t!("pointer.dpi_preset", index => (index + 1).to_string()).into()
        }
        Action::CustomShortcut(combo) => combo.rendered_label().into(),
        Action::HoldShortcut(combo) => {
            rust_i18n::t!("actions.hold_shortcut", chord => combo.rendered_label()).into()
        }
        Action::TypeText(text) => {
            rust_i18n::t!("actions.type_text_action", text => text.clone()).into()
        }
        Action::RunAppleScript(_) => rust_i18n::t!("actions.run_applescript_heading").into(),
        Action::RunShellCommand(_) => rust_i18n::t!("actions.run_shell_command_heading").into(),
        Action::Workflow(steps) if steps.len() == 1 => {
            rust_i18n::t!("actions.workflow_step_count_singular").into()
        }
        Action::Workflow(steps) => {
            rust_i18n::t!("actions.workflow_step_count_plural", count => steps.len().to_string())
                .into()
        }
        Action::OpenApplication(target) => {
            rust_i18n::t!("actions.open_named_target", name => target.display_name()).into()
        }
        _ => rust_i18n::t!(
            action
                .translation_key()
                .expect("every payload-free action has a translation key")
        )
        .into(),
    }
}
