//! Logical mouse button identifiers and the action vocabulary each one can
//! bind to. Lives in `openlogi-core` because the [`config`](crate::config)
//! schema serializes these directly — the GUI re-exports them.
//!
//! When [`Action`] gains new variants, keep the existing variant names stable:
//! the TOML config keys/values use the enum variant identifiers verbatim, so
//! renames are migration events.
//!
//! Modules are split by domain so concurrent feature work (new buttons, new
//! actions, defaults, gesture maps) rarely edits the same file.

mod action;
mod action_ring;
mod application_target;
mod button;
mod category;
mod defaults;
mod effect;
mod gesture;
mod key_combo;
mod swipe;
mod value;

#[cfg(test)]
mod tests;

pub use action::{Action, WorkflowStep};
pub use action_ring::{
    ActionRingConfig, ActionRingEntry, ActionRingIcon, ActionRingLayout, ActionRingSlot,
    RingAction, RingActionError,
};
pub use application_target::{ApplicationTarget, ApplicationTargetError};
pub use button::ButtonId;
pub use category::Category;
pub use defaults::{default_binding, default_binding_for, default_gesture_binding};
pub use effect::{Effect, MediaKey, MouseButton, NativeAction, Script, Shortcut};
pub use gesture::GestureDirection;
pub use key_combo::{KeyCombo, KeyComboParseError, KeyboardUsage, KeyboardUsageError};
pub use swipe::{
    GESTURE_HOLD_FOR_SWIPE, GESTURE_SWIPE_DEADZONE, GESTURE_SWIPE_THRESHOLD, SwipeAccumulator,
    detect_swipe,
};
pub use value::{Binding, LONG_PRESS_THRESHOLD, LongPressBinding};
