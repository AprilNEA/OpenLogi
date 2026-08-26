//! Typed key for `AppState`'s per-device UI-state side tables.

use std::borrow::Borrow;

/// Identifies one device across [`AppState`](super::AppState)'s per-device UI
/// caches: the DPI/SmartShift query state
/// ([`DeviceReads`](crate::services::device_reads::DeviceReads)) and the consolidated
/// per-device row
/// ([`DeviceRuntimeState`](super::device_runtime::DeviceRuntimeState)).
///
/// Wraps a device's config key — see
/// [`DeviceRecord::device_key`](super::devices::DeviceRecord::device_key) —
/// so a plain `String` computed for some unrelated purpose (a display name, a
/// model key, a capture id) can't be passed to one of these maps by
/// accident: every call site has to go through that one conversion point.
#[derive(
    Debug,
    Clone,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    Hash,
    Default,
    derive_more::Display,
    derive_more::From,
)]
#[from(forward)]
pub(crate) struct DeviceKey(String);

impl DeviceKey {
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
        assert_eq!(DeviceKey::from("a"), DeviceKey::from("a".to_string()));
        assert_ne!(DeviceKey::from("a"), DeviceKey::from("b"));
        assert!(DeviceKey::from("a") < DeviceKey::from("b"));
    }
}
