//! The virtual HID keyboard synthesized keys type on, as the agent holds and
//! reports it.
//!
//! Only macOS has one ([`openlogi_inject::VirtualKeyboard`]). Elsewhere the
//! device type is uninhabited, the status is
//! [`VirtualKeyboardStatus::Unsupported`] and [`VirtualKeyboard::lost`]
//! never resolves, so the run loop needs no platform branches.

use std::time::Duration;

use openlogi_ipc::VirtualKeyboardStatus;

/// How often [`VirtualKeyboard::lost`] checks the device. The device goes
/// away only when it rejects a report, which the input worker that typed
/// finds out and nobody else hears of; this notices it within the Secure
/// Input watcher's period.
const CHECK_PERIOD: Duration = Duration::from_secs(2);

#[cfg(target_os = "macos")]
type Device = openlogi_inject::VirtualKeyboard;

/// No virtual keyboard off macOS.
#[cfg(not(target_os = "macos"))]
#[derive(Debug)]
enum Device {}

/// The process's virtual keyboard, if it has one. Dropping it removes the
/// device; keep it until every worker that can press a key has stopped, then
/// [`release`](Self::release) it.
#[derive(Debug, Default)]
pub(crate) struct VirtualKeyboard {
    device: Option<Device>,
}

impl VirtualKeyboard {
    /// Create the device, where the platform has one and this build may.
    pub(crate) fn install() -> Self {
        Self { device: create() }
    }

    /// Whether synthesized keys type on the device now.
    pub(crate) fn status(&self) -> VirtualKeyboardStatus {
        if self.device.as_ref().is_some_and(is_active) {
            VirtualKeyboardStatus::Active
        } else if cfg!(target_os = "macos") {
            VirtualKeyboardStatus::Unavailable
        } else {
            VirtualKeyboardStatus::Unsupported
        }
    }

    /// Resolve once the device has stopped carrying keys; never while there
    /// is none to lose. The run loop polls a fresh future on every turn, so
    /// each one checks at once and then once per [`CHECK_PERIOD`]. Replace a
    /// lost keyboard with [`Self::default`] so this stays pending.
    pub(crate) async fn lost(&self) {
        let Some(device) = &self.device else {
            return std::future::pending().await;
        };
        while is_active(device) {
            tokio::time::sleep(CHECK_PERIOD).await;
        }
    }

    /// Report every key the device holds released, then remove it. The exit
    /// path calls this, as `process::exit` runs no destructors.
    pub(crate) fn release(mut self) {
        // An assignment, not `drop`: off macOS there is nothing to drop.
        self.device = None;
    }
}

#[cfg(target_os = "macos")]
fn create() -> Option<Device> {
    openlogi_inject::VirtualKeyboard::install()
}

#[cfg(not(target_os = "macos"))]
const fn create() -> Option<Device> {
    None
}

#[cfg(target_os = "macos")]
fn is_active(device: &Device) -> bool {
    device.is_active()
}

#[cfg(not(target_os = "macos"))]
fn is_active(device: &Device) -> bool {
    match *device {}
}

#[cfg(test)]
mod tests {
    use openlogi_ipc::VirtualKeyboardStatus;

    use super::VirtualKeyboard;

    #[test]
    fn no_device_reports_the_platform_fallback() {
        let expected = if cfg!(target_os = "macos") {
            VirtualKeyboardStatus::Unavailable
        } else {
            VirtualKeyboardStatus::Unsupported
        };
        assert_eq!(VirtualKeyboard::default().status(), expected);
    }

    #[tokio::test(start_paused = true)]
    async fn no_device_is_never_lost() {
        let keyboard = VirtualKeyboard::default();
        let lost = tokio::time::timeout(std::time::Duration::from_secs(60), keyboard.lost());
        assert!(lost.await.is_err(), "nothing to lose must stay pending");
    }
}
