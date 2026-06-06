//! Binding-map construction: overlay the stored per-device (and per-app)
//! bindings on top of the built-in defaults.
//!
//! Keyed by `config_key` (`Option<&str>`) rather than any UI device record so
//! both the agent and the GUI can build the effective map from a [`Config`].

use std::collections::BTreeMap;

use openlogi_core::binding::{
    Action, ButtonId, GestureDirection, default_binding, default_gesture_binding,
};
use openlogi_core::config::Config;

/// Effective button bindings for the device `config_key`, with `app_bundle`'s
/// per-app overlay applied. Unset buttons fall back to [`default_binding`].
#[must_use]
pub fn bindings_for(
    config: &Config,
    config_key: Option<&str>,
    app_bundle: Option<&str>,
) -> BTreeMap<ButtonId, Action> {
    let stored = config_key
        .map(|key| config.effective_bindings(key, app_bundle))
        .unwrap_or_default();
    let mut bindings: BTreeMap<ButtonId, Action> = ButtonId::ALL
        .iter()
        .copied()
        .map(|b| (b, default_binding(b)))
        .collect();
    for (k, v) in stored {
        bindings.insert(k, v);
    }
    bindings
}

/// Effective gesture bindings for the device `config_key`. Unset directions
/// fall back to [`default_gesture_binding`].
#[must_use]
pub fn gesture_bindings_for(
    config: &Config,
    config_key: Option<&str>,
) -> BTreeMap<GestureDirection, Action> {
    let stored = config_key
        .map(|key| config.gesture_bindings_for(key))
        .unwrap_or_default();
    let mut bindings: BTreeMap<GestureDirection, Action> = GestureDirection::ALL
        .iter()
        .copied()
        .map(|d| (d, default_gesture_binding(d)))
        .collect();
    for (k, v) in stored {
        bindings.insert(k, v);
    }
    bindings
}
