//! Action icon selection for the Actions Ring editor.

use openlogi_core::binding::{Action, ActionRingIcon};

use openlogi_gui::action_ring::icons::ring_icon_path;

/// Embedded Lucide asset for an action.
pub(crate) fn action_icon_path(action: &Action) -> &'static str {
    ring_icon_path(ActionRingIcon::for_action(action))
}
