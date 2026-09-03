//! Keyboard remapper UI for function-row and dedicated Logitech keyboard controls.
//!
//! Mirrors [`crate::features::mouse`]: a hardware-style diagram whose clickable
//! hotspots each open the same action picker the mouse buttons use. Ordinary
//! Esc/F-key bindings are global under `config.keyboard.bindings`; dedicated
//! HID++ keyboard controls stay per-device under `config.devices[key].bindings`.
//!
//! [`AppState::commit_binding`]: crate::state::AppState::commit_binding
//! [`AppState::commit_keyboard_binding`]: crate::state::AppState::commit_keyboard_binding

pub mod editors;
pub mod function_row;
