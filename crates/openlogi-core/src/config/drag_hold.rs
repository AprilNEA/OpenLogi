//! Drag-hold mode: whether holding Back or Forward and moving the mouse plays
//! a two- or three-finger trackpad swipe, and the edit that switches it.
//!
//! Like wheel-hold, the mode is the binding's shape ([`Binding::DragHold`]),
//! never a separate flag. Switching it keeps the button's click action, so a
//! press without dragging does what it did.
use super::Config;
use crate::binding::{Binding, ButtonId, DragHoldBinding, HeldDrag, default_binding};

impl Config {
    /// The swipe `button` of `device_key` plays when held and dragged, or
    /// `None` when it is not stored as a drag-hold binding. No button drags
    /// by default.
    #[must_use]
    pub fn drag_hold(&self, device_key: &str, button: ButtonId) -> Option<HeldDrag> {
        match self.stored_binding(device_key, button) {
            Some(Binding::DragHold(binding)) => Some(binding.drag()),
            _ => None,
        }
    }

    /// Set the swipe `button` of `device_key` plays when held and dragged, or
    /// turn drag-hold off with `None`, keeping the action its press runs.
    ///
    /// On: a button in gesture mode first leaves it through
    /// [`Self::set_gesture_mode`], which stashes its direction map so turning
    /// gestures on again restores it, and keeps its `Click` action. The
    /// resulting click action becomes the drag-hold press. A wheel-hold or
    /// long-press binding gives way the same way, keeping its click.
    ///
    /// Off: demote to a [`Binding::Single`] of the click action.
    ///
    /// A no-op for a button that cannot drag ([`ButtonId::supports_drag_hold`])
    /// and when the mode already matches.
    pub fn set_drag_hold(&mut self, device_key: &str, button: ButtonId, drag: Option<HeldDrag>) {
        if !button.supports_drag_hold() || self.drag_hold(device_key, button) == drag {
            return;
        }
        if drag.is_some() && self.is_gesture_mode(device_key, button) {
            self.set_gesture_mode(device_key, button, false);
        }
        let click = self
            .stored_binding(device_key, button)
            .map_or_else(|| default_binding(button), Binding::click_action);
        let binding = match drag {
            Some(drag) => Binding::DragHold(DragHoldBinding::new(click, drag)),
            None => Binding::Single(click),
        };
        self.set_binding(device_key, button, binding);
    }
}
