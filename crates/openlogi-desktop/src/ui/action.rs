//! Display helpers for the shared action vocabulary.

use gpui::SharedString;
use openlogi_core::binding::Action;

/// Localized label for an action, including its dynamic payload.
pub(crate) fn localized_action_label(action: &Action) -> SharedString {
    match action {
        Action::SetDpiPreset(index) => {
            tr!("DPI Preset %{index}", index => (index + 1).to_string())
        }
        Action::CustomShortcut(combo) => combo.rendered_label().into(),
        Action::HoldShortcut(combo) => {
            tr!("Hold %{chord}", chord => combo.rendered_label())
        }
        Action::TapKeyHoldingModifiers(combo) => match combo.rendered_modifiers() {
            Some(modifiers) => tr!(
                "Hold %{modifiers}, tap %{key}",
                modifiers => modifiers,
                key => combo.rendered_key()
            ),
            None => tr!("Tap %{key}", key => combo.rendered_key()),
        },
        _ => tr!(action.label()),
    }
}
