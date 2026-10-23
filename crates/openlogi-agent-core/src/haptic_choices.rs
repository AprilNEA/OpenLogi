//! The user's haptic-engine choices, counted per device.
//!
//! An Actions Ring session may turn a device's haptic engine on before its
//! first buzz. That arming must not undo a choice the user made after the
//! session opened. Every write of the user's haptic settings — from the
//! settings app or a changed `config.toml` — is counted here before it is
//! numbered in the device's write order, so an arming that checks its
//! [`ChoiceMark`] inside the device's transport turn either sees the choice
//! or runs before the user's write, which then lands last.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError};

use openlogi_hid::DeviceRoute;

/// How many times the user has set each device's haptic engine, by route.
#[derive(Clone, Debug, Default)]
pub struct HapticChoices(Arc<Mutex<HashMap<String, u64>>>);

/// The user's choices on one device at some moment.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChoiceMark {
    route: String,
    count: u64,
}

impl HapticChoices {
    /// The choices on `route` so far.
    #[must_use]
    pub fn mark(&self, route: &DeviceRoute) -> ChoiceMark {
        let route = route.to_string();
        let count = self.counts().get(&route).copied().unwrap_or(0);
        ChoiceMark { route, count }
    }

    /// Whether the user has made no choice on the marked device since
    /// `mark` was taken.
    #[must_use]
    pub fn unchanged_since(&self, mark: &ChoiceMark) -> bool {
        self.counts().get(&mark.route).copied().unwrap_or(0) == mark.count
    }

    /// Count a choice the user made on `route`.
    pub(crate) fn chose(&self, route: &DeviceRoute) {
        *self.counts().entry(route.to_string()).or_default() += 1;
    }

    fn counts(&self) -> std::sync::MutexGuard<'_, HashMap<String, u64>> {
        // Plain counters: a panic mid-update leaves at worst one choice
        // uncounted, never a state worth refusing to read.
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }
}
