//! Display helpers for the shared action vocabulary.

use std::sync::LazyLock;

use gpui::SharedString;
use openlogi_core::binding::{Action, Host};

/// The host the agent dispatches on, read once: the GUI runs in the same
/// session, so it sees the same OS and `$XDG_CURRENT_DESKTOP`.
pub(crate) fn host() -> Host {
    static HOST: LazyLock<Host> = LazyLock::new(Host::current);
    *HOST
}

/// Whether binding `action` does anything on this host. The pickers offer
/// only these; an existing binding to anything else keeps its row, marked by
/// [`localized_action_label`].
pub(crate) fn is_available(action: &Action) -> bool {
    action.is_available_on(host())
}

/// Localized label for an action, including its dynamic payload, under the
/// name this host uses for it. A binding this host cannot perform says so,
/// rather than looking like it works.
pub(crate) fn localized_action_label(action: &Action) -> SharedString {
    let label = action_label(action);
    if is_available(action) {
        label
    } else {
        tr!("actions.unavailable_here", action => label)
    }
}

#[expect(
    clippy::expect_used,
    reason = "the preceding match arms handle every action without a static key"
)]
fn action_label(action: &Action) -> SharedString {
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
            .translation_key_on(host())
            .expect("every payload-free action has a translation key")),
    }
}
