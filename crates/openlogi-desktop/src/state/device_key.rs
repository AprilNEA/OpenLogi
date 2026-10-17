//! Typed key for `AppState`'s per-device UI-state side tables.

use std::borrow::Borrow;

/// Identifies one device across [`AppState`](super::AppState)'s per-device UI
/// caches: the DPI/SmartShift query state
/// ([`DeviceReads`](crate::services::device_reads::DeviceReads)) and the consolidated
/// per-device row
/// ([`DeviceSession`](super::device_session::DeviceSession)).
///
/// Wraps a device's config key, and is only ever made from one by
/// [`DeviceRecord::device_key`](super::devices::DeviceRecord::device_key) —
/// so a plain `String` computed for some unrelated purpose (a display name, a
/// model key, an inventory key, a capture id) can't be passed to one of these
/// maps by accident: there is no conversion from a string, and every call
/// site has to go through that one accessor.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, derive_more::Display)]
pub(crate) struct DeviceKey(String);

impl DeviceKey {
    /// The key of the record whose config key is `config_key`.
    pub(super) fn of_record(config_key: &str) -> Self {
        Self(config_key.to_owned())
    }

    /// Borrow the underlying key as a string slice.
    pub(crate) fn as_str(&self) -> &str {
        &self.0
    }
}

/// Lets a `BTreeMap<DeviceKey, _>` be read (`get`/`remove`/`contains_key`)
/// with a plain `&str` — most reads have a borrowed config key on hand
/// already and have no reason to allocate a throwaway `DeviceKey` just to
/// look something up.
impl Borrow<str> for DeviceKey {
    fn borrow(&self) -> &str {
        &self.0
    }
}

/// Tests name devices that no record stands for.
#[cfg(test)]
impl From<&str> for DeviceKey {
    fn from(key: &str) -> Self {
        Self(key.to_owned())
    }
}

#[cfg(test)]
mod tests {
    use super::DeviceKey;
    use std::collections::BTreeMap;

    #[test]
    fn borrowed_str_lookup_finds_an_owned_key() {
        let mut map = BTreeMap::new();
        map.insert(DeviceKey::from("2b034"), 7);
        assert_eq!(map.get("2b034"), Some(&7));
        assert_eq!(map.get("missing"), None);
    }

    #[test]
    fn equality_and_ordering_are_value_based() {
        let (a, again) = (DeviceKey::from("a"), DeviceKey::from("a"));
        assert_eq!(a, again);
        assert_ne!(DeviceKey::from("a"), DeviceKey::from("b"));
        assert!(DeviceKey::from("a") < DeviceKey::from("b"));
    }
}
