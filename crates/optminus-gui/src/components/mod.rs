//! Reusable UI primitives built on gpui-component.
//!
//! Modules are added phase by phase per UI.md. Each component is a small,
//! self-contained entity or render-once element; cross-component coordination
//! happens through [`crate::state::AppState`].

pub mod dpi_panel;
