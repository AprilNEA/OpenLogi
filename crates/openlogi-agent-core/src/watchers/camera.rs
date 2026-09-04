//! Camera-use watcher used by standalone-light automation.
//!
//! CoreMediaIO exposes whether each camera device is running in any client.
//! Polling that read-only property covers physical webcams, virtual cameras,
//! capture cards, and SLR devices without coupling the policy to a particular
//! meeting or recording application.

use std::time::Duration;

#[cfg(target_os = "macos")]
use std::thread;
use tokio::sync::mpsc;
#[cfg(target_os = "macos")]
use tracing::{debug, info, warn};

#[cfg(target_os = "macos")]
mod macos;

/// CoreMediaIO can briefly report no running stream while a camera client
/// renegotiates or switches capture mode. Requiring two consecutive inactive
/// samples prevents that gap from turning linked lights off and back on.
#[cfg(any(target_os = "macos", test))]
const INACTIVE_CONFIRMATIONS: u8 = 2;

#[cfg(any(target_os = "macos", test))]
#[derive(Default)]
struct CameraDebouncer {
    emitted: Option<bool>,
    inactive_samples: u8,
}

#[cfg(any(target_os = "macos", test))]
impl CameraDebouncer {
    fn observe(&mut self, active: bool) -> Option<bool> {
        if active {
            self.inactive_samples = 0;
            return (self.emitted != Some(true)).then(|| {
                self.emitted = Some(true);
                true
            });
        }

        if self.emitted != Some(true) {
            return (self.emitted != Some(false)).then(|| {
                self.emitted = Some(false);
                false
            });
        }

        self.inactive_samples = self.inactive_samples.saturating_add(1);
        if self.inactive_samples < INACTIVE_CONFIRMATIONS {
            return None;
        }
        self.inactive_samples = 0;
        self.emitted = Some(false);
        Some(false)
    }

    fn retain_last_state_after_probe_error(&mut self) {
        self.inactive_samples = 0;
    }
}

/// Start the macOS camera-use watcher. The first successful sample is emitted
/// immediately; later samples are emitted only after a debounced state change.
/// Dropping the receiver stops the worker on its next attempted send.
#[cfg(target_os = "macos")]
#[must_use]
pub fn spawn(period: Duration) -> mpsc::UnboundedReceiver<bool> {
    let (tx, rx) = mpsc::unbounded_channel();
    let spawn_result = thread::Builder::new()
        .name("openlogi-camera-watcher".into())
        .spawn(move || {
            let mut debouncer = CameraDebouncer::default();
            loop {
                match camera_in_use() {
                    Ok(active) => {
                        if let Some(active) = debouncer.observe(active) {
                            info!(active, "camera usage state changed");
                            if tx.send(active).is_err() {
                                debug!("camera watcher receiver dropped â€” exiting");
                                return;
                            }
                        }
                    }
                    Err(error) => {
                        debouncer.retain_last_state_after_probe_error();
                        warn!(error, "camera state probe failed â€” retaining last state");
                    }
                }
                thread::sleep(period);
            }
        });
    if let Err(error) = spawn_result {
        warn!(error = %error, "could not spawn camera watcher");
    }
    rx
}

/// Return an inert watcher on platforms that do not yet expose a supported
/// aggregate camera-use provider. Camera-linked settings retain manual power.
#[cfg(not(target_os = "macos"))]
#[must_use]
pub fn spawn(_period: Duration) -> mpsc::UnboundedReceiver<bool> {
    let (_tx, rx) = mpsc::unbounded_channel();
    rx
}

#[cfg(target_os = "macos")]
fn camera_in_use() -> Result<bool, i32> {
    macos::camera_in_use()
}

#[cfg(test)]
mod tests {
    use super::CameraDebouncer;

    #[test]
    fn inactive_transition_requires_two_consecutive_samples() {
        let mut debouncer = CameraDebouncer::default();
        assert_eq!(debouncer.observe(false), Some(false));
        assert_eq!(debouncer.observe(true), Some(true));
        assert_eq!(debouncer.observe(false), None);
        assert_eq!(debouncer.observe(false), Some(false));
    }

    #[test]
    fn active_sample_cancels_pending_inactive_transition() {
        let mut debouncer = CameraDebouncer::default();
        assert_eq!(debouncer.observe(true), Some(true));
        assert_eq!(debouncer.observe(false), None);
        assert_eq!(debouncer.observe(true), None);
        assert_eq!(debouncer.observe(false), None);
        assert_eq!(debouncer.observe(false), Some(false));
    }

    #[test]
    fn probe_error_cancels_pending_inactive_transition() {
        let mut debouncer = CameraDebouncer::default();
        assert_eq!(debouncer.observe(true), Some(true));
        assert_eq!(debouncer.observe(false), None);
        debouncer.retain_last_state_after_probe_error();
        assert_eq!(debouncer.observe(false), None);
        assert_eq!(debouncer.observe(false), Some(false));
    }
}
