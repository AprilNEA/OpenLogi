//! Action-derived icon selection for the settings GUI.

use openlogi_core::binding::{Action, ActionRingIcon};

use openlogi_gui::action_ring_icons::ring_icon_path;

/// Embedded Lucide asset for an action.
pub(crate) fn action_icon_path(action: &Action) -> &'static str {
    ring_icon_path(ActionRingIcon::for_action(action))
}
