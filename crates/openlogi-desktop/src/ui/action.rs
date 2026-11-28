//! Display helpers for the shared action vocabulary.

use gpui::SharedString;
use openlogi_core::binding::Action;
use openlogi_ipc::ModuleStatus;

/// [`localized_action_label`], with a key module's action named as
/// `modules`, the agent's list in `AppState`, names it.
pub(crate) fn action_label(action: &Action, modules: &[ModuleStatus]) -> SharedString {
    let Action::Module(id) = action else {
        return localized_action_label(action);
    };
    modules
        .iter()
        .filter_map(|status| status.module.as_ref())
        .find(|module| module.id == *id)
        .map_or_else(
            || id.to_string().into(),
            |module| module.name.clone().into(),
        )
}

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
        Action::TapKeyHoldingModifiers(combo) => match combo.rendered_modifiers() {
            Some(modifiers) => tr!(
                "actions.hold_modifiers_tap_key",
                modifiers => modifiers,
                key => combo.rendered_key()
            ),
            None => tr!("actions.tap_key", key => combo.rendered_key()),
        },
        Action::HoldModifier(mods) => tr!("actions.hold_shortcut", chord => mods.rendered_label()),
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
        // A module's name is the agent's to report: see `action_label`.
        Action::Module(id) => id.to_string().into(),
        _ => tr!(action
            .translation_key()
            .expect("every payload-free action has a translation key")),
    }
}
