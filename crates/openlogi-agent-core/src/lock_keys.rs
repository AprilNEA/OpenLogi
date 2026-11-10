//! Agent-owned state for the transient lock-key indicator.
//!
//! A keyboard reports a lock key turning on or off over HID++ `0x4220`, and
//! the overlay shows which one for [`DISPLAY_LIFETIME`]. As with the backlight
//! indicator, the agent owns how long it shows, so an overlay that starts or
//! reconnects shows only a change that is still current.

use std::time::Duration;

use openlogi_core::config::AppSettings;
use openlogi_ipc::{Generation, LockKeyChange, LockKeyObservation};
use tokio::sync::watch;

use crate::indicator::Indicator;

/// How long the indicator shows one change: the backlight indicator's time.
pub const DISPLAY_LIFETIME: Duration = crate::indicator::DISPLAY_LIFETIME;

/// The lock-key indicator's cell.
#[derive(Clone)]
pub struct LockKeyIndicator {
    indicator: Indicator<LockKeyObservation>,
}

impl Default for LockKeyIndicator {
    fn default() -> Self {
        // Generation 1, not 0: 0 is the client sentinel for "I have seen
        // nothing", so a first `observe(0)` answers at once.
        let indicator = Indicator::new(LockKeyObservation {
            generation: 1,
            shown: None,
            language: None,
        });
        Self { indicator }
    }
}

impl LockKeyIndicator {
    /// Show a lock key a keyboard turned on or off, unless the user turned
    /// [`AppSettings::lock_key_notifications`] off or this platform gives the
    /// key no meaning. Answers whether the change is shown.
    ///
    /// Call it inside a Tokio runtime: a task ends the change when its time is
    /// up.
    #[must_use]
    pub fn announce(&self, change: LockKeyChange, settings: &AppSettings) -> bool {
        if !settings.lock_key_notifications || !change.key.is_indicated_here() {
            return false;
        }
        self.show(change, settings.language.clone());
        true
    }

    /// Replace the shown change, allowing a bounded cold start before the
    /// first delivery starts [`DISPLAY_LIFETIME`].
    ///
    /// `language` is the configured UI locale, or `None` to follow the
    /// overlay host's.
    fn show(&self, change: LockKeyChange, language: Option<String>) {
        self.indicator.show(LockKeyObservation {
            generation: 0,
            shown: Some(change),
            language,
        });
    }

    /// Long-poll the indicator. Answers as soon as it differs from generation
    /// `since`, or with the unchanged indicator after
    /// [`OBSERVE_HOLD`](openlogi_ipc::OBSERVE_HOLD).
    pub async fn observe(&self, since: Generation) -> LockKeyObservation {
        self.indicator.observe(since).await
    }

    /// Follow state changes without marking the indicator as delivered.
    #[must_use]
    pub fn subscribe(&self) -> watch::Receiver<LockKeyObservation> {
        self.indicator.subscribe()
    }
}

#[cfg(test)]
mod tests;
