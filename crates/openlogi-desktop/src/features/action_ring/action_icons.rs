//! Action icon selection for the Actions Ring editor.

use openlogi_core::binding::{Action, ActionRingIcon};

/// Embedded Lucide asset for an action.
pub(crate) fn action_icon_path(action: &Action) -> &'static str {
    ActionRingIcon::for_action(action).asset_path()
}
