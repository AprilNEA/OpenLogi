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
#[cfg(not(target_os = "macos"))]
pub fn spawn(_period: Duration) -> mpsc::UnboundedReceiver<bool> {
    let (_tx, rx) = mpsc::unbounded_channel();
    rx
}

#[cfg(target_os = "macos")]
fn camera_in_use() -> Result<bool, i32> {
    macos::camera_in_use()
}

#[cfg(target_os = "macos")]
#[expect(
    unsafe_code,
    reason = "CoreMediaIO exposes the camera-running property through a C API"
)]
mod macos {
    use std::ffi::c_void;
    use std::mem::size_of;
    use std::ptr;

    type ObjectId = u32;
    type Selector = u32;
    type Scope = u32;
    type Element = u32;

    #[repr(C)]
    struct PropertyAddress {
        selector: Selector,
        scope: Scope,
        element: Element,
    }

    // CoreMediaIO constants are four-character codes from CMIOTypes.h.
    const SYSTEM_OBJECT: ObjectId = 1;
    const SCOPE_GLOBAL: Scope = u32::from_be_bytes(*b"glob");
    const ELEMENT_MASTER: Element = 0;
    const HARDWARE_DEVICES: Selector = u32::from_be_bytes(*b"dev#");
    const DEVICE_RUNNING_SOMEWHERE: Selector = u32::from_be_bytes(*b"gone");

    #[link(name = "CoreMediaIO", kind = "framework")]
    unsafe extern "C" {
        fn CMIOObjectGetPropertyDataSize(
            object_id: ObjectId,
            address: *const PropertyAddress,
            qualifier_data_size: u32,
            qualifier_data: *const c_void,
            data_size: *mut u32,
        ) -> i32;

        fn CMIOObjectGetPropertyData(
            object_id: ObjectId,
            address: *const PropertyAddress,
            qualifier_data_size: u32,
            qualifier_data: *const c_void,
            data_size: u32,
            data_used: *mut u32,
            data: *mut c_void,
        ) -> i32;
    }

    pub(super) fn camera_in_use() -> Result<bool, i32> {
        let devices_address = PropertyAddress {
            selector: HARDWARE_DEVICES,
            scope: SCOPE_GLOBAL,
            element: ELEMENT_MASTER,
        };
        let mut data_size = 0;
        // SAFETY: CoreMediaIO receives a valid system-object property address
        // and a writable UInt32 for the byte count; no qualifier is used.
        let status = unsafe {
            CMIOObjectGetPropertyDataSize(
                SYSTEM_OBJECT,
                &raw const devices_address,
                0,
                ptr::null(),
                &raw mut data_size,
            )
        };
        if status != 0 {
            return Err(status);
        }

        let object_size = size_of::<ObjectId>();
        let Some(device_count) = usize::try_from(data_size)
            .ok()
            .filter(|bytes| bytes % object_size == 0)
            .map(|bytes| bytes / object_size)
        else {
            return Err(-1);
        };
        if device_count == 0 {
            return Ok(false);
        }

        let mut devices = vec![0; device_count];
        let mut data_used = 0;
        // SAFETY: `devices` has the byte capacity reported by the preceding
        // size query and remains alive for the duration of the call.
        let status = unsafe {
            CMIOObjectGetPropertyData(
                SYSTEM_OBJECT,
                &raw const devices_address,
                0,
                ptr::null(),
                data_size,
                &raw mut data_used,
                devices.as_mut_ptr().cast(),
            )
        };
        if status != 0 {
            return Err(status);
        }
        let Some(used_count) = usize::try_from(data_used)
            .ok()
            .filter(|bytes| bytes % object_size == 0)
            .map(|bytes| bytes / object_size)
            .filter(|count| *count <= devices.len())
        else {
            return Err(-1);
        };
        devices.truncate(used_count);

        let running_address = PropertyAddress {
            selector: DEVICE_RUNNING_SOMEWHERE,
            scope: SCOPE_GLOBAL,
            element: ELEMENT_MASTER,
        };
        let mut last_error = None;
        let mut read_any = false;
        for device in devices {
            let mut running = 0_u32;
            let property_size = u32::try_from(size_of::<u32>()).unwrap_or(u32::MAX);
            let mut property_used = 0;
            // SAFETY: `running` and the size counter are valid writable
            // buffers; each device ID came from CoreMediaIO itself.
            let status = unsafe {
                CMIOObjectGetPropertyData(
                    device,
                    &raw const running_address,
                    0,
                    ptr::null(),
                    property_size,
                    &raw mut property_used,
                    (&raw mut running).cast(),
                )
            };
            if status != 0 {
                last_error = Some(status);
                continue;
            }
            if property_used != property_size {
                last_error = Some(-1);
                continue;
            }
            read_any = true;
            if running != 0 {
                return Ok(true);
            }
        }
        if read_any {
            Ok(false)
        } else {
            Err(last_error.unwrap_or(-1))
        }
    }
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
