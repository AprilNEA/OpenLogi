//! Windows: read the Logitech receivers' USB lower filters and the LampArray
//! service registrations out of the registry.
//!
//! Everything here is a read. `Enum\USB` device-instance keys and
//! `Services` registrations are readable by an ordinary user, so the check
//! needs no elevation and cannot change the host.
//!
//! A device instance whose key cannot be opened is skipped rather than
//! fatal: the `Enum` tree holds historical instances for hardware that is
//! long gone, and those can deny access. The `Enum\USB` key itself failing
//! is fatal instead — that would mean the host was never really inspected,
//! and reporting "no receiver carries a filter" from an unread tree would be
//! a false all-clear.

use anyhow::{Context, Result};
use winreg::RegKey;
use winreg::enums::{HKEY_LOCAL_MACHINE, KEY_READ};

use super::{HostState, LAMPARRAY_DRIVER, LAMPARRAY_SERVICE, ReceiverFilters, Registration};

/// `HKLM\SYSTEM\CurrentControlSet\Enum\USB` — where USB device instances and
/// the lower filters attached to them live.
const ENUM_USB: &str = r"SYSTEM\CurrentControlSet\Enum\USB";

/// `HKLM\SYSTEM\CurrentControlSet\Services` — where driver and service
/// registrations live.
const SERVICES: &str = r"SYSTEM\CurrentControlSet\Services";

/// The `VID_046D&PID_` prefix every Logitech USB product key carries.
const LOGITECH_PRODUCT_PREFIX: &str = "VID_046D&PID_";

/// The registry value carrying a device instance's lower filters.
const LOWER_FILTERS: &str = "LowerFilters";

/// The registry value saying how a service or driver loads.
const START: &str = "Start";

/// Read the host state. Always applicable on Windows.
pub(super) fn collect() -> Result<Option<HostState>> {
    Ok(Some(HostState {
        receivers: receivers()?,
        driver: registration(LAMPARRAY_DRIVER),
        service: registration(LAMPARRAY_SERVICE),
    }))
}

/// Every Logitech receiver device instance, with its lower filters.
fn receivers() -> Result<Vec<ReceiverFilters>> {
    let root = RegKey::predef(HKEY_LOCAL_MACHINE)
        .open_subkey_with_flags(ENUM_USB, KEY_READ)
        .context("read HKLM\\SYSTEM\\CurrentControlSet\\Enum\\USB")?;
    let mut found = Vec::new();
    for product in root.enum_keys().flatten() {
        if !is_logitech_product_key(&product) {
            continue;
        }
        let Ok(product_key) = root.open_subkey_with_flags(&product, KEY_READ) else {
            continue;
        };
        for instance in product_key.enum_keys().flatten() {
            let Ok(instance_key) = product_key.open_subkey_with_flags(&instance, KEY_READ) else {
                continue;
            };
            // A missing `LowerFilters` is the normal case for a device with no
            // lower filter attached; a value of another type is not something
            // this check can read, and reads as "no filter" too.
            let lower_filters = instance_key
                .get_value::<Vec<String>, _>(LOWER_FILTERS)
                .unwrap_or_default();
            found.push(ReceiverFilters {
                product_key: product.clone(),
                instance,
                lower_filters,
            });
        }
    }
    Ok(found)
}

/// How a driver or service is registered, if it is at all.
fn registration(name: &str) -> Registration {
    let Ok(root) = RegKey::predef(HKEY_LOCAL_MACHINE).open_subkey_with_flags(SERVICES, KEY_READ)
    else {
        return Registration::default();
    };
    let Ok(key) = root.open_subkey_with_flags(name, KEY_READ) else {
        return Registration::default();
    };
    Registration {
        present: true,
        start: key.get_value::<u32, _>(START).ok(),
    }
}

/// Whether a key under `Enum\USB` names a Logitech device product key.
///
/// `Enum\USB` also holds the interface-level and LampArray pseudo-keys
/// (`VID_046D&PID_C53F&MI_00`, `VID_046D&PID_C53F&LAMPARRAY`); only the bare
/// `VID_046D&PID_XXXX` key holds real device instances.
fn is_logitech_product_key(name: &str) -> bool {
    let Some(product_id) = name.strip_prefix(LOGITECH_PRODUCT_PREFIX) else {
        return false;
    };
    product_id.len() == 4 && product_id.bytes().all(|byte| byte.is_ascii_hexdigit())
}

#[cfg(test)]
mod tests {
    use super::is_logitech_product_key;

    #[test]
    fn only_a_bare_product_key_names_a_device_product() {
        assert!(is_logitech_product_key("VID_046D&PID_C53F"));
        assert!(is_logitech_product_key("VID_046D&PID_c53f"));
    }

    /// `Enum\USB` also holds the interface-level and LampArray pseudo-keys;
    /// only the bare product key holds real device instances.
    #[test]
    fn interface_and_lamparray_pseudo_keys_are_not_product_keys() {
        assert!(!is_logitech_product_key("VID_046D&PID_C53F&MI_00"));
        assert!(!is_logitech_product_key("VID_046D&PID_C53F&LAMPARRAY"));
    }

    #[test]
    fn another_vendor_is_not_a_logitech_product_key() {
        assert!(!is_logitech_product_key("VID_05AC&PID_024F"));
        assert!(!is_logitech_product_key("VID_046D"));
    }

    #[test]
    fn a_malformed_product_id_is_rejected() {
        assert!(!is_logitech_product_key("VID_046D&PID_C53"));
        assert!(!is_logitech_product_key("VID_046D&PID_C53FF"));
        assert!(!is_logitech_product_key("VID_046D&PID_C53Z"));
    }
}
