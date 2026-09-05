//! Windows aggregate camera-use probe.
//!
//! Windows routes every camera acquisition through the Capability Access
//! Manager, which records one consent-store entry per client and stamps it
//! while the client holds the device. That is the same bookkeeping the shell's
//! own privacy indicator reads, so it covers packaged apps, plain Win32
//! executables, and services alike — without binding the policy to any
//! particular meeting or recording application, and without opening the
//! camera ourselves (which would take the device away from that client).

use std::io;

use winreg::RegKey;
use winreg::enums::{HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, KEY_READ};

/// Consent store listing every webcam client the Capability Access Manager has
/// seen. Present under both hives: `HKCU` holds the interactive user's clients,
/// `HKLM` the ones running as a service or as another account.
const WEBCAM_CONSENT_SUBKEY: &str =
    r"Software\Microsoft\Windows\CurrentVersion\CapabilityAccessManager\ConsentStore\webcam";

/// Non-packaged (plain Win32) clients are nested one level deeper, keyed by
/// executable path with the separators escaped as `#`.
const NON_PACKAGED_SUBKEY: &str = "NonPackaged";

/// FILETIME of the moment the client acquired the camera.
const LAST_USED_START: &str = "LastUsedTimeStart";
/// FILETIME of the moment the client released it. Windows zeroes this for as
/// long as the client holds the device, which is the in-use signal.
const LAST_USED_STOP: &str = "LastUsedTimeStop";

/// Report whether any client currently holds a camera. The error is the failing
/// Win32 status from the last hive that could not be opened; a hive that opens
/// answers on its own, so a machine policy hiding `HKLM` still reports normally.
pub(super) fn camera_in_use() -> Result<bool, i32> {
    let mut last_error = None;
    let mut read_any = false;
    for hive in [HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE] {
        match RegKey::predef(hive).open_subkey_with_flags(WEBCAM_CONSENT_SUBKEY, KEY_READ) {
            Ok(store) => {
                read_any = true;
                if store_has_active_client(&store) {
                    return Ok(true);
                }
            }
            Err(error) => last_error = Some(status_of(&error)),
        }
    }
    if read_any {
        Ok(false)
    } else {
        Err(last_error.unwrap_or(-1))
    }
}

/// Whether any client under one consent store holds the camera, including the
/// non-packaged clients nested one level below it.
fn store_has_active_client(store: &RegKey) -> bool {
    if any_client_holds_camera(store) {
        return true;
    }
    store
        .open_subkey_with_flags(NON_PACKAGED_SUBKEY, KEY_READ)
        .is_ok_and(|non_packaged| any_client_holds_camera(&non_packaged))
}

/// Whether any immediate child of `parent` holds the camera. A child that
/// cannot be opened or carries no usage stamps is skipped: the store also holds
/// grouping keys such as `NonPackaged`, which the caller walks separately.
fn any_client_holds_camera(parent: &RegKey) -> bool {
    parent
        .enum_keys()
        .filter_map(Result::ok)
        .filter_map(|name| parent.open_subkey_with_flags(name, KEY_READ).ok())
        .any(|client| client_holds_camera(&client))
}

/// Read one client's usage stamps. A missing stamp reads as zero, which is also
/// what a grouping key such as `NonPackaged` — it carries only `Value` and
/// `LastSetTime` — and a never-used client both look like.
fn client_holds_camera(client: &RegKey) -> bool {
    holds_camera(
        client.get_value::<u64, _>(LAST_USED_START).unwrap_or(0),
        client.get_value::<u64, _>(LAST_USED_STOP).unwrap_or(0),
    )
}

/// A client is holding the camera when it has started a session that has no
/// stop stamp yet. Both halves matter: granting an app permission creates its
/// entry with *neither* stamp written, so testing the stop stamp alone would
/// report every app that merely holds the permission as recording.
const fn holds_camera(started: u64, stopped: u64) -> bool {
    started != 0 && stopped == 0
}

/// The Win32 status behind a registry error, in the `i32` shape the watcher
/// logs for every platform.
fn status_of(error: &io::Error) -> i32 {
    error.raw_os_error().unwrap_or(-1)
}

#[cfg(test)]
mod tests {
    use super::holds_camera;

    /// An arbitrary acquisition stamp; only zero versus non-zero is read.
    const STARTED: u64 = 133_000_000_000_000_000;
    /// The matching release stamp, a few seconds later.
    const STOPPED: u64 = 133_000_000_050_000_000;

    #[test]
    fn a_started_session_with_no_stop_stamp_is_in_use() {
        assert!(holds_camera(STARTED, 0));
    }

    #[test]
    fn a_finished_session_is_not_in_use() {
        assert!(!holds_camera(STARTED, STOPPED));
    }

    #[test]
    fn permission_granted_but_never_used_is_not_in_use() {
        // Both stamps absent, which the caller reads as zero. Testing the stop
        // stamp alone would report every permitted app as recording.
        assert!(!holds_camera(0, 0));
    }
}
