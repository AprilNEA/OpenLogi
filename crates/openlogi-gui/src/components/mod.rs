//! Feature-level UI widgets built on gpui-component.
//!
//! These are product-specific panels rather than generic primitives. Each
//! widget owns its local state; cross-widget coordination happens through
//! [`crate::state::AppState`].

pub mod dpi_panel;
