//! Camera-use watcher used by standalone-light automation.
//!
//! The policy needs one aggregate fact — is *any* camera in use right now — so
//! each platform provides that fact from whatever the OS already tracks for its
//! own privacy indicator, rather than the watcher opening cameras or watching a
//! list of meeting applications. macOS reads CoreMediaIO's device-running
//! property; Windows reads the Capability Access Manager's consent store; Linux,
//! which keeps no such bookkeeping, reads which processes hold a V4L2 node open.
//! A platform with no such provider gets an inert watcher and manual light power.
//!
//! Each platform module exports the same `Probe`: a provider that needs state
//! between samples (Linux's inotify wake) keeps it there, and the stateless
//! ones are unit structs.

use std::time::Duration;

#[cfg(any(target_os = "macos", target_os = "windows", target_os = "linux"))]
use std::thread;
use tokio::sync::mpsc;
#[cfg(any(target_os = "macos", target_os = "windows", target_os = "linux"))]
use tracing::{debug, info, warn};

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "windows")]
mod windows;

#[cfg(target_os = "linux")]
use linux::Probe;
#[cfg(target_os = "macos")]
use macos::Probe;
#[cfg(target_os = "windows")]
use windows::Probe;

/// A provider can briefly report no running stream while a camera client
/// renegotiates or switches capture mode. Requiring two consecutive inactive
/// samples prevents that gap from turning linked lights off and back on.
#[cfg(any(target_os = "macos", target_os = "windows", target_os = "linux", test))]
const INACTIVE_CONFIRMATIONS: u8 = 2;

#[cfg(any(target_os = "macos", target_os = "windows", target_os = "linux", test))]
#[derive(Default)]
struct CameraDebouncer {
    emitted: Option<bool>,
    inactive_samples: u8,
}

#[cfg(any(target_os = "macos", target_os = "windows", target_os = "linux", test))]
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

/// Start the camera-use watcher. The first successful sample is emitted
/// immediately; later samples are emitted only after a debounced state change.
/// Dropping the receiver stops the worker on its next attempted send.
#[cfg(any(target_os = "macos", target_os = "windows", target_os = "linux"))]
#[must_use]
pub fn spawn(period: Duration) -> mpsc::UnboundedReceiver<bool> {
    let (tx, rx) = mpsc::unbounded_channel();
    let spawn_result = thread::Builder::new()
        .name("openlogi-camera-watcher".into())
        .spawn(move || {
            let mut probe = Probe::new();
            let mut debouncer = CameraDebouncer::default();
            loop {
                match probe.sample() {
                    Ok(active) => {
                        if let Some(active) = debouncer.observe(active) {
                            info!(active, "camera usage state changed");
                            if tx.send(active).is_err() {
                                debug!("camera watcher receiver dropped — exiting");
                                return;
                            }
                        }
                    }
                    Err(error) => {
                        debouncer.retain_last_state_after_probe_error();
                        warn!(error, "camera state probe failed — retaining last state");
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
#[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
#[must_use]
pub fn spawn(_period: Duration) -> mpsc::UnboundedReceiver<bool> {
    let (_tx, rx) = mpsc::unbounded_channel();
    rx
}

/// The OS error behind an I/O failure, in the `i32` shape the watcher logs for
/// every platform — logged rather than interpreted. `-1` stands for a failure
/// that carries no OS code.
#[cfg(any(target_os = "windows", target_os = "linux"))]
fn status_of(error: &std::io::Error) -> i32 {
    error.raw_os_error().unwrap_or(-1)
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
