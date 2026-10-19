//! Agent-owned state for the transient lock-key indicator.
//!
//! A keyboard reports a lock key turning on or off over HID++ `0x4220`, and
//! the overlay shows which one for [`DISPLAY_LIFETIME`]. As with the backlight
//! indicator, the agent owns how long it shows, so an overlay that starts or
//! reconnects shows only a change that is still current.

use std::time::Duration;

use openlogi_ipc::{Generation, LockKeyChange, LockKeyObservation, OBSERVE_HOLD};
use tokio::sync::watch;

/// How long the indicator shows one change: the backlight indicator's time.
pub const DISPLAY_LIFETIME: Duration = crate::backlight_overlay::DISPLAY_LIFETIME;

/// The lock-key indicator's cell.
#[derive(Clone)]
pub struct LockKeyIndicator {
    published: watch::Sender<LockKeyObservation>,
}

impl Default for LockKeyIndicator {
    fn default() -> Self {
        // Generation 1, not 0: 0 is the client sentinel for "I have seen
        // nothing", so a first `observe(0)` answers at once.
        let (published, _) = watch::channel(LockKeyObservation {
            generation: 1,
            shown: None,
            language: None,
        });
        Self { published }
    }
}

impl LockKeyIndicator {
    /// Show `change` for [`DISPLAY_LIFETIME`], replacing the change on show.
    ///
    /// `language` is the configured UI locale, or `None` to follow the
    /// overlay host's. Call it inside a Tokio runtime: a task ends the change
    /// when its time is up.
    pub fn show(&self, change: LockKeyChange, language: Option<String>) {
        let mut generation = 0;
        self.published.send_modify(|observation| {
            observation.generation = observation.generation.wrapping_add(1);
            observation.shown = Some(change);
            observation.language = language;
            generation = observation.generation;
        });
        let published = self.published.clone();
        tokio::spawn(async move {
            tokio::time::sleep(DISPLAY_LIFETIME).await;
            // Compared under the cell's lock: a newer change moved the
            // generation, and its own task ends it.
            published.send_if_modified(|observation| {
                if observation.generation != generation {
                    return false;
                }
                observation.generation = observation.generation.wrapping_add(1);
                observation.shown = None;
                true
            });
        });
    }

    /// Long-poll the indicator. Answers as soon as it differs from generation
    /// `since`, or with the unchanged indicator after [`OBSERVE_HOLD`].
    pub async fn observe(&self, since: Generation) -> LockKeyObservation {
        let mut receiver = self.published.subscribe();
        let changed = receiver.wait_for(|observation| observation.generation != since);
        match tokio::time::timeout(OBSERVE_HOLD, changed).await {
            Ok(Ok(observation)) => observation.clone(),
            Ok(Err(_)) | Err(_) => self.published.borrow().clone(),
        }
    }
}

#[cfg(test)]
mod tests;
