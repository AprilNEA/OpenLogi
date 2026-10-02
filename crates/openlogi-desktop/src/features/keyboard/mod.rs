//! Keyboard remapper UI — the Keys tab.
//!
//! Mirrors [`crate::features::mouse`]: a hardware-style diagram whose clickable
//! hotspots (here, key-caps) each open the same action picker the mouse
//! buttons use. Which keys it shows is the asset's call: a Logi depot with
//! control markers names the keyboard's own HID++ controls, which bind per
//! device (and per app profile) through [`AppState::commit_binding`] and are
//! diverted by the agent while bound; a depot without them shows the OS-hook
//! F-row, whose bindings are global (`config.keyboard.bindings`) and commit
//! through [`AppState::commit_keyboard_binding`].
//!
//! [`AppState::commit_binding`]: crate::state::AppState::commit_binding
//! [`AppState::commit_keyboard_binding`]: crate::state::AppState::commit_keyboard_binding

pub mod disable_keys;
pub mod editors;
pub mod function_row;
