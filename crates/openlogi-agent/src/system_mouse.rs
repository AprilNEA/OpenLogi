//! macOS host-wide primary mouse button state.
//!
//! The live IOHIDSystem parameter and the persistent CFPreferences value are
//! separate. A complete write updates and verifies both, matching the behavior
//! of System Settings and Logitech Options+.

#![expect(
    unsafe_code,
    reason = "IOKit and CFPreferences are C APIs; every call is isolated here behind typed ownership wrappers"
)]

use std::fmt;
use std::ptr::{self, NonNull};
use std::time::Duration;

use objc2_core_foundation::{
    CFBoolean, CFDictionary, CFNumber, CFPreferencesGetAppBooleanValue, CFPreferencesSetValue,
    CFPreferencesSynchronize, CFRetained, CFString, CFType, kCFPreferencesAnyApplication,
    kCFPreferencesCurrentHost, kCFPreferencesCurrentUser,
};
use objc2_io_kit::{
    IOHIDCopyCFTypeParameter, IOHIDSetCFTypeParameter, IOObjectRelease, IOServiceClose,
    IOServiceGetMatchingService, IOServiceMatching, IOServiceOpen, io_connect_t, io_service_t,
    kIOHIDParamConnectType, kIOHIDSystemClass, kIOReturnSuccess,
};
use openlogi_ipc::PrimaryMouseButton;
use tokio::sync::mpsc;
use tracing::{info, warn};

const HID_POINTER_BUTTON_MODE: &str = "HIDPointerButtonMode";
const PERSISTENT_SWAP_KEY: &str = "com.apple.mouse.swapLeftRightButton";
const LEFT_PRIMARY_MODE: i32 = 2;
const RIGHT_PRIMARY_MODE: i32 = 1;

/// A platform error suitable for logging or carrying over typed IPC.
#[derive(Debug)]
pub(crate) struct SystemMouseError(String);

impl SystemMouseError {
    fn new(message: impl Into<String>) -> Self {
        Self(message.into())
    }
}

impl fmt::Display for SystemMouseError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::error::Error for SystemMouseError {}

/// Read the live IOHIDSystem value used by the current input session.
pub(crate) fn read() -> Result<PrimaryMouseButton, SystemMouseError> {
    HidSystemConnection::open()?.read_primary_button()
}

/// Update the live input session and the persistent macOS preference, then
/// read both back before reporting success.
pub(crate) fn set(requested: PrimaryMouseButton) -> Result<PrimaryMouseButton, SystemMouseError> {
    let connection = HidSystemConnection::open()?;
    connection.write_primary_button(requested)?;
    write_persistent(requested)?;

    let live = connection.read_primary_button()?;
    if live != requested {
        return Err(SystemMouseError::new(format!(
            "IOHIDSystem verification returned {live:?} after requesting {requested:?}"
        )));
    }
    let persistent = read_persistent()?;
    if persistent != requested {
        return Err(SystemMouseError::new(format!(
            "CFPreferences verification returned {persistent:?} after requesting {requested:?}"
        )));
    }
    Ok(live)
}

/// Poll the externally mutable setting and publish edges. System Settings and
/// Options+ can change it without going through OpenLogi, so the GUI cannot
/// treat a successful OpenLogi write as the only source of truth.
pub(crate) fn spawn(period: Duration) -> mpsc::UnboundedReceiver<PrimaryMouseButton> {
    let (tx, rx) = mpsc::unbounded_channel();
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(period);
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        let mut last = None;
        let mut failed = false;
        loop {
            interval.tick().await;
            match read() {
                Ok(button) => {
                    if failed {
                        info!("macOS primary mouse button watcher recovered");
                    }
                    failed = false;
                    if last != Some(button) {
                        last = Some(button);
                        if tx.send(button).is_err() {
                            return;
                        }
                    }
                }
                Err(error) => {
                    if !failed {
                        warn!(%error, "could not read the macOS primary mouse button");
                    }
                    failed = true;
                }
            }
        }
    });
    rx
}

struct HidSystemService(io_service_t);

impl HidSystemService {
    fn find() -> Result<Self, SystemMouseError> {
        // SAFETY: `kIOHIDSystemClass` is a static, NUL-terminated C string from
        // the typed IOKit bindings. The returned dictionary has +1 ownership.
        let matching = unsafe { IOServiceMatching(kIOHIDSystemClass.as_ptr()) }
            .ok_or_else(|| SystemMouseError::new("IOServiceMatching returned null"))?;
        // SAFETY: CFMutableDictionary is an immutable-readable CFDictionary
        // subtype. IOKit consumes the +1 retain count in the next call.
        let matching = unsafe { CFRetained::cast_unchecked::<CFDictionary>(matching) };
        // SAFETY: The matching dictionary is valid and deliberately transferred
        // to IOKit, which consumes it regardless of the lookup result.
        let service = unsafe { IOServiceGetMatchingService(0, Some(matching)) };
        if service == 0 {
            return Err(SystemMouseError::new("IOHIDSystem service was not found"));
        }
        Ok(Self(service))
    }
}

impl Drop for HidSystemService {
    fn drop(&mut self) {
        let _ = IOObjectRelease(self.0);
    }
}

struct HidSystemConnection(io_connect_t);

impl HidSystemConnection {
    fn open() -> Result<Self, SystemMouseError> {
        let service = HidSystemService::find()?;
        let mut connection = 0;
        // SAFETY: `service` is a live IOHIDSystem service, `mach_task_self`
        // identifies this process, and `connection` is a valid out-pointer.
        let result = unsafe {
            IOServiceOpen(
                service.0,
                mach2::traps::mach_task_self(),
                kIOHIDParamConnectType,
                ptr::addr_of_mut!(connection),
            )
        };
        if result != kIOReturnSuccess {
            return Err(SystemMouseError::new(format!(
                "IOServiceOpen failed with 0x{result:08x}"
            )));
        }
        Ok(Self(connection))
    }

    fn read_primary_button(&self) -> Result<PrimaryMouseButton, SystemMouseError> {
        let key = CFString::from_str(HID_POINTER_BUTTON_MODE);
        let mut raw_value: *const CFType = ptr::null();
        // SAFETY: `self.0` is a live IOHIDSystem connection, `key` is non-null,
        // and `raw_value` is a valid out-pointer. Copy returns a +1 CF object.
        let result =
            unsafe { IOHIDCopyCFTypeParameter(self.0, Some(&key), ptr::addr_of_mut!(raw_value)) };
        if result != kIOReturnSuccess {
            return Err(SystemMouseError::new(format!(
                "IOHIDCopyCFTypeParameter failed with 0x{result:08x}"
            )));
        }
        let raw_value = NonNull::new(raw_value.cast_mut())
            .ok_or_else(|| SystemMouseError::new("HIDPointerButtonMode returned null"))?;
        // SAFETY: IOHIDCopyCFTypeParameter follows Core Foundation's Copy rule,
        // so this non-null pointer carries one retain count owned by the caller.
        let value = unsafe { CFRetained::from_raw(raw_value) };
        let number = value
            .downcast::<CFNumber>()
            .map_err(|_| SystemMouseError::new("HIDPointerButtonMode was not a CFNumber"))?;
        let mode = number
            .as_i32()
            .ok_or_else(|| SystemMouseError::new("HIDPointerButtonMode was not an i32"))?;
        decode_mode(mode)
    }

    fn write_primary_button(&self, button: PrimaryMouseButton) -> Result<(), SystemMouseError> {
        let key = CFString::from_str(HID_POINTER_BUTTON_MODE);
        let number = CFNumber::new_i32(encode_mode(button));
        // SAFETY: `self.0` is a live IOHIDSystem connection; both CF objects
        // remain valid for the duration of this synchronous call, and CFNumber
        // is a valid CFType parameter.
        let result = unsafe { IOHIDSetCFTypeParameter(self.0, Some(&key), Some(&number)) };
        if result != kIOReturnSuccess {
            return Err(SystemMouseError::new(format!(
                "IOHIDSetCFTypeParameter failed with 0x{result:08x}"
            )));
        }
        Ok(())
    }
}

impl Drop for HidSystemConnection {
    fn drop(&mut self) {
        let _ = IOServiceClose(self.0);
    }
}

fn read_persistent() -> Result<PrimaryMouseButton, SystemMouseError> {
    let key = CFString::from_str(PERSISTENT_SWAP_KEY);
    let mut valid = 0;
    // SAFETY: the key and application identifiers are valid CFStrings, and
    // `valid` is a live out-pointer for the duration of the call.
    let swapped = unsafe {
        CFPreferencesGetAppBooleanValue(
            &key,
            kCFPreferencesAnyApplication,
            ptr::addr_of_mut!(valid),
        )
    };
    if valid == 0 {
        return Err(SystemMouseError::new(
            "persistent mouse swap preference was absent or not a boolean",
        ));
    }
    Ok(if swapped {
        PrimaryMouseButton::Right
    } else {
        PrimaryMouseButton::Left
    })
}

fn write_persistent(button: PrimaryMouseButton) -> Result<(), SystemMouseError> {
    let key = CFString::from_str(PERSISTENT_SWAP_KEY);
    let value: &CFType = CFBoolean::new(button == PrimaryMouseButton::Right);
    // SAFETY: all CF references are valid for the call; CFBoolean is a valid
    // property-list value. This mirrors the CurrentUser/CurrentHost scope used
    // by System Settings and Options+.
    unsafe {
        CFPreferencesSetValue(
            &key,
            Some(value),
            kCFPreferencesAnyApplication,
            kCFPreferencesCurrentUser,
            kCFPreferencesCurrentHost,
        );
    }
    // SAFETY: these are immutable process-wide CFString constants exported by
    // Core Foundation and are valid for the full process lifetime.
    let synchronized = unsafe {
        CFPreferencesSynchronize(
            kCFPreferencesAnyApplication,
            kCFPreferencesCurrentUser,
            kCFPreferencesCurrentHost,
        )
    };
    if !synchronized {
        return Err(SystemMouseError::new(
            "CFPreferencesSynchronize rejected the mouse swap preference",
        ));
    }
    Ok(())
}

fn encode_mode(button: PrimaryMouseButton) -> i32 {
    match button {
        PrimaryMouseButton::Left => LEFT_PRIMARY_MODE,
        PrimaryMouseButton::Right => RIGHT_PRIMARY_MODE,
    }
}

fn decode_mode(mode: i32) -> Result<PrimaryMouseButton, SystemMouseError> {
    match mode {
        LEFT_PRIMARY_MODE => Ok(PrimaryMouseButton::Left),
        RIGHT_PRIMARY_MODE => Ok(PrimaryMouseButton::Right),
        other => Err(SystemMouseError::new(format!(
            "unrecognized HIDPointerButtonMode value {other}"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hid_pointer_modes_match_macos_values() {
        assert_eq!(encode_mode(PrimaryMouseButton::Left), 2);
        assert_eq!(encode_mode(PrimaryMouseButton::Right), 1);
        assert_eq!(decode_mode(2).ok(), Some(PrimaryMouseButton::Left));
        assert_eq!(decode_mode(1).ok(), Some(PrimaryMouseButton::Right));
        let Err(_) = decode_mode(0) else {
            panic!("unknown IOHID mode must be rejected");
        };
    }
}
