//! Keyboard remapper UI — the global function-key binding surface.
//!
//! Mirrors [`crate::mouse_model`]: a hardware-style diagram whose clickable
//! hotspots (here, function-row key-caps) each open the same action picker the
//! mouse buttons use. The key difference is scope — mouse bindings are
//! per-device under `config.devices[key].bindings`, while keyboard F-key
//! bindings are global (`config.keyboard.bindings`) and apply across all
//! keyboards, so the picker commits via [`AppState::commit_keyboard_binding`]
//! rather than [`AppState::commit_binding`].
//!
//! [`AppState::commit_binding`]: crate::state::AppState::commit_binding
//! [`AppState::commit_keyboard_binding`]: crate::state::AppState::commit_keyboard_binding

pub mod editors;
pub mod function_row;
