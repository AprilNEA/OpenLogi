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

/// What one scan of a consent store learned.
///
/// A client that cannot be read is not a client that is idle: reporting it as
/// idle lets a transient registry failure switch a linked light off while its
/// camera is still running. Keeping the two apart lets the watcher retain its
/// last state instead, which is what its error path exists for.
#[derive(Clone, Copy)]
enum Scan {
    /// A client holds the camera. No unreadable entry can contradict that.
    Holding,
    /// Every entry read cleanly, and none holds the camera.
    NoneHolding,
    /// An entry could not be read, so "none holds" is not a fact.
    Unreadable(i32),
}

impl Scan {
    /// Combine two scans of the same store, or two stores of the same host.
    /// Evidence of use outranks a failure, which outranks silence.
    fn merge(self, other: Self) -> Self {
        match (self, other) {
            (Self::Holding, _) | (_, Self::Holding) => Self::Holding,
            (Self::Unreadable(status), _) | (_, Self::Unreadable(status)) => {
                Self::Unreadable(status)
            }
            (Self::NoneHolding, Self::NoneHolding) => Self::NoneHolding,
        }
    }

    /// The watcher's answer for this scan. `Unreadable` becomes an error, which
    /// is what makes the watcher retain its last state instead of acting on a
    /// reading it does not have.
    const fn answer(self) -> Result<bool, i32> {
        match self {
            Self::Holding => Ok(true),
            Self::NoneHolding => Ok(false),
            Self::Unreadable(status) => Err(status),
        }
    }
}

/// Report whether any client currently holds a camera.
///
/// Every hive's scan reaches the answer through the same merge, so a hive that
/// could not be read cannot be silently outvoted by one that could: `HKLM` is
/// where services and other accounts are recorded, and answering from `HKCU`
/// alone would switch a linked light off while one of them is on a call.
pub(super) fn camera_in_use() -> Result<bool, i32> {
    let mut scan = Scan::NoneHolding;
    for hive in [HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE] {
        let hive_scan =
            match RegKey::predef(hive).open_subkey_with_flags(WEBCAM_CONSENT_SUBKEY, KEY_READ) {
                Ok(store) => scan_store(&store),
                // A hive with no consent store is silence, not failure: the store
                // is the Capability Access Manager's own bookkeeping, so its
                // absence means nothing was ever recorded there.
                Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
                // Anything else — an ACL, a policy hiding the key — leaves that
                // hive's clients unaccounted for.
                Err(error) => Scan::Unreadable(status_of(&error)),
            };
        if matches!(hive_scan, Scan::Holding) {
            return Ok(true);
        }
        scan = scan.merge(hive_scan);
    }
    scan.answer()
}

/// Scan one consent store, including the non-packaged clients nested one level
/// below it.
fn scan_store(store: &RegKey) -> Scan {
    let direct = scan_clients(store);
    if matches!(direct, Scan::Holding) {
        return Scan::Holding;
    }
    let nested = match store.open_subkey_with_flags(NON_PACKAGED_SUBKEY, KEY_READ) {
        Ok(non_packaged) => scan_clients(&non_packaged),
        // Not every store has non-packaged clients.
        Err(error) if error.kind() == io::ErrorKind::NotFound => Scan::NoneHolding,
        Err(error) => Scan::Unreadable(status_of(&error)),
    };
    direct.merge(nested)
}

/// Scan every immediate child of `parent`.
fn scan_clients(parent: &RegKey) -> Scan {
    let mut scan = Scan::NoneHolding;
    for name in parent.enum_keys() {
        let entry = match name {
            Ok(name) => match parent.open_subkey_with_flags(&name, KEY_READ) {
                Ok(client) => scan_client(&client),
                // A client that exited between the enumeration and the open
                // took its entry with it, and holds nothing.
                Err(error) if error.kind() == io::ErrorKind::NotFound => Scan::NoneHolding,
                Err(error) => Scan::Unreadable(status_of(&error)),
            },
            Err(error) => Scan::Unreadable(status_of(&error)),
        };
        if matches!(entry, Scan::Holding) {
            return Scan::Holding;
        }
        scan = scan.merge(entry);
    }
    scan
}

/// Scan one client entry.
fn scan_client(client: &RegKey) -> Scan {
    let started = match usage_stamp(client, LAST_USED_START) {
        Ok(stamp) => stamp,
        Err(status) => return Scan::Unreadable(status),
    };
    let stopped = match usage_stamp(client, LAST_USED_STOP) {
        Ok(stamp) => stamp,
        Err(status) => return Scan::Unreadable(status),
    };
    if holds_camera(started, stopped) {
        Scan::Holding
    } else {
        Scan::NoneHolding
    }
}

/// One usage stamp, or zero when the value is simply absent — a grouping key
/// such as `NonPackaged` carries neither stamp, and neither does a client that
/// has been granted the permission but never opened a camera. Any other
/// failure means the entry could not be read.
fn usage_stamp(client: &RegKey, name: &str) -> Result<u64, i32> {
    match client.get_value::<u64, _>(name) {
        Ok(stamp) => Ok(stamp),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(0),
        Err(error) => Err(status_of(&error)),
    }
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
    use super::{Scan, holds_camera};

    impl Scan {
        fn is_holding(self) -> bool {
            matches!(self, Self::Holding)
        }

        fn unreadable_status(self) -> Option<i32> {
            match self {
                Self::Unreadable(status) => Some(status),
                _ => None,
            }
        }
    }

    #[test]
    fn use_outranks_a_failure_which_outranks_silence() {
        // The order matters in both directions: merge is called with the
        // running total on either side depending on which entry came first.
        assert!(Scan::Holding.merge(Scan::Unreadable(5)).is_holding());
        assert!(Scan::Unreadable(5).merge(Scan::Holding).is_holding());
        assert!(Scan::Holding.merge(Scan::NoneHolding).is_holding());
        assert_eq!(
            Scan::NoneHolding
                .merge(Scan::Unreadable(5))
                .unreadable_status(),
            Some(5)
        );
        assert_eq!(
            Scan::Unreadable(5)
                .merge(Scan::NoneHolding)
                .unreadable_status(),
            Some(5)
        );
        assert!(!Scan::NoneHolding.merge(Scan::NoneHolding).is_holding());
        assert_eq!(
            Scan::NoneHolding
                .merge(Scan::NoneHolding)
                .unreadable_status(),
            None
        );
    }

    #[test]
    fn a_readable_hive_does_not_speak_for_one_that_failed() {
        // HKCU readable and idle, HKLM unreadable. HKLM is where services and
        // other accounts are recorded, so answering "no camera in use" from
        // HKCU alone can switch a light off while one of them is on a call.
        assert_eq!(
            Scan::NoneHolding.merge(Scan::Unreadable(5)).answer(),
            Err(5)
        );
        // Unless the readable hive established use, which nothing contradicts.
        assert_eq!(Scan::Holding.merge(Scan::Unreadable(5)).answer(), Ok(true));
    }

    #[test]
    fn silence_from_every_hive_is_an_answer_rather_than_a_failure() {
        // A hive with no consent store contributes nothing, and the aggregate
        // of nothing but silence is silence — the store is the Capability
        // Access Manager's own bookkeeping, so its absence everywhere means
        // nothing was recorded, not that a read failed.
        assert_eq!(Scan::NoneHolding.answer(), Ok(false));
    }

    #[test]
    fn an_unreadable_entry_does_not_read_as_an_idle_one() {
        // The distinction this type exists for: a client whose entry could not
        // be read must not answer "no camera in use", or a transient registry
        // failure switches a linked light off mid-call.
        let unreadable = Scan::NoneHolding.merge(Scan::Unreadable(5));
        assert!(!unreadable.is_holding());
        assert!(unreadable.unreadable_status().is_some());
    }

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
