//! Resolving a device's configuration key from its route and its own identity.
//!
//! A device's entry is keyed by what it *is* — its unit id or serial — so
//! settings follow the device when it moves between a receiver and a cable.
//! When the device is asleep its identity is unreadable and only the route is
//! known, so the entry's `links` table doubles as a route index.

#[cfg(test)]
use crate::binding::{Action, ButtonId};
use crate::config::{Config, DeviceConfig};
#[cfg(test)]
use crate::config::{LightSettings, Lighting, LinkConfig};
use crate::device::Capabilities;
use crate::device_order::{DeviceIdentity, DeviceStableId, PhysicalDeviceKey};
#[cfg(test)]
use crate::hid::Dpi;

impl DeviceIdentity {
    /// This identity as a transport-free configuration key, or `None` when the
    /// device reported no identity worth keying on.
    #[must_use]
    pub fn config_key(&self) -> Option<String> {
        self.is_physical().then(|| self.key())
    }
}

/// The key a device's settings ultimately belong under: its own identity when
/// it reported one, else the route-derived key that is the best available
/// stand-in.
///
/// This is the *destination* of the schema-5 model, independent of what is on
/// disk right now. [`Config::resolve_device_key`] may legitimately answer with
/// a different, pre-upgrade key while the settings still live there; this is
/// the key those settings are folded onto by [`Config::adopt_route`].
#[must_use]
pub fn canonical_device_key(
    stable: &DeviceStableId,
    identity: Option<&DeviceIdentity>,
) -> Option<PhysicalDeviceKey> {
    identity
        .and_then(DeviceIdentity::config_key)
        .and_then(|key| PhysicalDeviceKey::parse(&key))
        .or_else(|| stable.physical_key())
}

impl Config {
    /// Restore the route index's one-owner invariant after loading and schema
    /// migration.
    ///
    /// A hand-edited file can put one route in several entries, and a key
    /// migration can add a link that an existing entry already claims. Device
    /// keys are stored in a [`BTreeMap`](std::collections::BTreeMap), so the
    /// lexicographically first key keeps its complete
    /// [`LinkConfig`](crate::config::LinkConfig) and later claims are removed.
    /// This matches the owner an ambiguous index resolved
    /// to before the repair, when [`Self::resolve_device_key`] used the first
    /// matching map entry.
    ///
    /// Only the route index is repaired. Device entries are neither renamed
    /// nor consumed, so [`Self::selected_device`] and
    /// [`DeviceConfig::host_switch_targets`] keep referring to the same
    /// physical configuration entries.
    #[cfg(feature = "fs")]
    pub(super) fn repair_duplicate_routes(&mut self) {
        let mut owners = std::collections::BTreeMap::<String, String>::new();
        for (device_key, device) in &mut self.devices {
            device.links.retain(|route_key, _| {
                let Some(owner) = owners.get(route_key) else {
                    owners.insert(route_key.clone(), device_key.clone());
                    return true;
                };
                tracing::warn!(
                    %route_key,
                    %owner,
                    duplicate_owner = %device_key,
                    "route is indexed by multiple devices; keeping the first owner"
                );
                false
            });
        }
    }

    /// The configuration key to *read and write* for a device reached by
    /// `stable`, given whatever `identity` the current probe could read.
    ///
    /// Prefers the device's own identity — that is the whole point of the
    /// schema-5 model — but only once the settings are actually there. A
    /// `receiver:`/`raw:` entry is not renamed by the load-time migration
    /// (nothing on disk says which device occupies a pairing slot), so
    /// immediately after an upgrade every such device's settings still live
    /// under its route-derived key while its identity key holds nothing.
    /// Answering with the identity key there would silently apply defaults to
    /// every receiver-paired device until the GUI next ran
    /// [`Self::adopt_route`] — which happens only when the user opens the GUI,
    /// and the agent runs unattended from login. So the pre-upgrade key wins
    /// for exactly as long as it is the one holding the settings.
    ///
    /// "Holding the settings" is [`DeviceConfig::holds_settings`], not mere
    /// existence: an entry carrying only the probed identity and the `links`
    /// route index is metadata OpenLogi wrote itself, and must not out-rank a
    /// legacy entry holding the user's actual bindings and DPI.
    ///
    /// Failing both, and for a device whose identity is unreadable (asleep),
    /// the route is resolved through the persisted `links` index, then finally
    /// through the route-derived key that every entry used before this
    /// indirection existed.
    #[must_use]
    pub fn resolve_device_key(
        &self,
        stable: &DeviceStableId,
        identity: Option<&DeviceIdentity>,
    ) -> Option<PhysicalDeviceKey> {
        if let Some(key) = identity
            .and_then(DeviceIdentity::config_key)
            .and_then(|key| PhysicalDeviceKey::parse(&key))
        {
            let holds_settings = |key: &str| {
                self.devices
                    .get(key)
                    .is_some_and(DeviceConfig::holds_settings)
            };
            let legacy = stable
                .physical_key()
                .filter(|legacy| legacy.as_str() != key.as_str());
            if let Some(legacy) = legacy
                && !holds_settings(key.as_str())
                && holds_settings(legacy.as_str())
            {
                return Some(legacy);
            }
            return Some(key);
        }
        let route = stable.route_key();
        if let Some(key) = self
            .devices
            .iter()
            .find(|(_, device)| device.links.contains_key(&route))
            .map(|(key, _)| key.as_str())
            .and_then(PhysicalDeviceKey::parse)
        {
            return Some(key);
        }
        stable.physical_key()
    }

    /// Record that the device keyed `canonical` was reached by `route_key`,
    /// folding in any entry still keyed by that route.
    ///
    /// `capabilities` is what the device just measured on this route, and it
    /// is what makes the per-link table a record of the hardware rather than
    /// a leftover of migration: a G502 that answers `0x2121` over its
    /// receiver and not over USB only differs in the config once both links
    /// have been sighted. Stored on every online sighting, so a link whose
    /// capabilities genuinely change stops reporting the old ones. `None`
    /// leaves whatever the link already recorded — an unprobed sighting is
    /// not evidence the capability went away.
    ///
    /// Returns whether anything actually changed — a route was newly
    /// registered in the entry's `links` index, its measured capabilities
    /// differ from what was recorded, a stale link pointing a re-paired
    /// route at its previous device was removed, or a legacy entry
    /// was folded in. Callers use this to decide whether the mutation needs
    /// persisting; a `false` means the entry already recorded this exact
    /// route and there is nothing new to write. Called on an online
    /// sighting, where the device's identity is known and the route can
    /// therefore be attributed to it with confidence.
    ///
    /// Consuming a legacy entry is a rename, and a device key lives in three
    /// places — the `devices` map, [`Config::selected_device`], and every
    /// entry of some keyboard's [`DeviceConfig::host_switch_targets`]. All
    /// three are re-pointed here, exactly as
    /// [`Config::migrate_transport_scoped_keys`] does for the keys it renames
    /// at load; leaving either reference behind would drop the carousel
    /// selection and silently unlink a host-switch target.
    pub fn adopt_route(
        &mut self,
        canonical: &PhysicalDeviceKey,
        route_key: &str,
        capabilities: Option<Capabilities>,
    ) -> bool {
        let mut changed = false;
        // A route names one device at a time. Re-pairing a different unit into
        // a slot must move the index, not leave the slot pointing at both.
        for (key, device) in &mut self.devices {
            if key != canonical.as_str() && device.links.remove(route_key).is_some() {
                changed = true;
            }
        }
        let legacy = (route_key != canonical.as_str())
            .then(|| self.devices.remove(route_key))
            .flatten();
        let device = self
            .devices
            .entry(canonical.as_str().to_string())
            .or_default();
        if !device.links.contains_key(route_key) {
            changed = true;
        }
        let link = device.links.entry(route_key.to_string()).or_default();
        // Only a probe that actually answered may overwrite the record, and
        // only when it says something new — rewriting an identical value on
        // every poll would persist the config on every tick.
        if capabilities.is_some() && link.capabilities != capabilities {
            link.capabilities = capabilities;
            changed = true;
        }
        let Some(legacy) = legacy else {
            return changed;
        };
        fold(device, legacy, route_key);
        self.repoint_references(route_key, canonical.as_str());
        true
    }

    /// Rewrite every reference to `old` — the carousel selection and any
    /// host-switch target — to `new`, after `old`'s entry was consumed.
    fn repoint_references(&mut self, old: &str, new: &str) {
        if self.selected_device.as_deref() == Some(old) {
            self.selected_device = Some(new.to_string());
        }
        for device in self.devices.values_mut() {
            for target in &mut device.host_switch_targets {
                if target == old {
                    *target = new.to_string();
                }
            }
        }
    }
}

/// Merge a legacy route-keyed entry into its device's canonical entry.
///
/// A value the canonical side does not have is simply taken. A value both
/// sides have and disagree on is not a conflict to resolve but a difference to
/// preserve: the canonical value stays the device default and the legacy one
/// becomes an override on its own link, when the field has an override slot
/// to preserve it in; otherwise the canonical value wins and the disagreement
/// is logged rather than silently discarded.
///
/// Routes `legacy` itself indexed come along, so folding is closed over the
/// `links` map: a route the canonical entry already knows keeps the canonical
/// link (its overrides are the ones expressed against the canonical
/// defaults). At runtime that map is always empty — `links` did not exist
/// before schema 5, so a route-keyed entry on disk cannot carry one — but
/// [`Config::migrate_transport_scoped_keys`] folds two *renamed* entries that
/// each already carry the route they were renamed out of.
pub(super) fn fold(device: &mut DeviceConfig, mut legacy: DeviceConfig, route_key: &str) {
    for (key, value) in std::mem::take(&mut legacy.links) {
        device.links.entry(key).or_insert(value);
    }
    fold_maps(device, &mut legacy, route_key);

    let link = device.links.entry(route_key.to_string()).or_default();
    if let Some(capabilities) = legacy
        .identity
        .as_ref()
        .map(|identity| identity.capabilities)
    {
        // Only where the link has no measurement of its own: the legacy
        // entry's copy can be arbitrarily old, and the sighting that
        // triggered this fold just measured the same route.
        link.capabilities.get_or_insert(capabilities);
    }

    // Values with a per-link override slot: canonical stays the device
    // default and a disagreeing legacy value survives as an override scoped
    // to the route it was set on.
    macro_rules! fold_field {
        ($field:ident) => {
            match (&device.$field, legacy.$field) {
                (None, Some(value)) => device.$field = Some(value),
                (Some(ours), Some(theirs)) if *ours != theirs => {
                    link.overrides.$field = Some(theirs);
                }
                _ => {}
            }
        };
    }
    fold_field!(dpi);
    fold_field!(scroll_resolution);
    fold_field!(lighting);
    fold_field!(smartshift);

    // `invert_scroll` is a bare bool, so `false` is indistinguishable from
    // never-set: only a legacy `true` is evidence the user chose anything.
    // Recording an override for a legacy `false` would fire far more often
    // wrongly (a device that simply never had the setting touched) than
    // rightly.
    if device.invert_scroll != legacy.invert_scroll && legacy.invert_scroll {
        link.overrides.invert_scroll = Some(legacy.invert_scroll);
    }

    // Scalar values with no per-link override slot: take the legacy value
    // when the canonical side is unset. A genuine disagreement keeps the
    // canonical value — there is nowhere to park the legacy one — and is
    // logged so the loss is visible instead of silent.
    macro_rules! fold_option_field {
        ($field:ident) => {
            match (&device.$field, legacy.$field) {
                (None, Some(value)) => device.$field = Some(value),
                (Some(ours), Some(theirs)) if *ours != theirs => {
                    tracing::warn!(
                        %route_key,
                        field = stringify!($field),
                        "value differs between merged entries; keeping the canonical one"
                    );
                }
                _ => {}
            }
        };
    }
    fold_option_field!(light);
    fold_option_field!(camera_controls);
    fold_option_field!(camera_profile);
    fold_option_field!(thumbwheel_sensitivity);
    fold_option_field!(fn_lock);
    // The user-assigned alias. Without this a legacy entry carrying a name
    // folded into a canonical entry with none would drop it silently — the
    // one field here a user typed by hand, so the loss is the most visible.
    fold_option_field!(custom_name);

    if device.identity.is_none() {
        device.identity = legacy.identity.take();
    }

    // Collections with no natural per-item override: take the legacy value
    // wholesale when the canonical side is empty (i.e. never configured).
    // Merging two curated lists is not a safe default — a DPI cycle is
    // ordered and a host-switch list is positional — so a populated
    // canonical side wins, and the list it displaces is logged.
    macro_rules! fold_if_empty {
        ($field:ident) => {
            if device.$field.is_empty() {
                device.$field = legacy.$field;
            } else if !legacy.$field.is_empty() && device.$field != legacy.$field {
                tracing::warn!(
                    %route_key,
                    field = stringify!($field),
                    "list differs between merged entries; keeping the canonical one"
                );
            }
        };
    }
    fold_if_empty!(dpi_presets);
    fold_if_empty!(host_switch_targets);

    // `ActionRingConfig` has its own notion of "unset". Two configured rings
    // cannot be merged — the slots are positional — so the canonical one wins
    // and the displaced one is logged, as elsewhere.
    if device.action_ring.is_default() {
        device.action_ring = legacy.action_ring.clone();
    } else if !legacy.action_ring.is_default() && device.action_ring != legacy.action_ring {
        tracing::warn!(
            %route_key,
            field = "action_ring",
            "ring differs between merged entries; keeping the canonical one"
        );
    }

    // `enabled` defaults to `true` and is only ever persisted when `false`,
    // so a legacy `false` is a deliberate "leave this device alone" choice
    // that must not be lost under a canonical entry that never opted out.
    if device.enabled && !legacy.enabled {
        device.enabled = false;
    }
}

/// The map-valued halves of [`fold`], split out to keep that function inside
/// the workspace's line budget.
///
/// Maps merge key by key: a legacy entry is taken only where the canonical map
/// has no entry for that key. A genuine conflict on a shared key keeps the
/// canonical entry and is logged, not silently dropped.
fn fold_maps(device: &mut DeviceConfig, legacy: &mut DeviceConfig, route_key: &str) {
    macro_rules! fold_map_field {
        ($field:ident) => {
            for (key, value) in std::mem::take(&mut legacy.$field) {
                if let Some(existing) = device.$field.get(&key) {
                    if *existing != value {
                        tracing::warn!(
                            %route_key,
                            field = stringify!($field),
                            key = ?key,
                            "entry differs between merged entries; keeping the canonical one"
                        );
                    }
                } else {
                    device.$field.insert(key, value);
                }
            }
        };
    }
    fold_map_field!(bindings);
    fold_map_field!(disabled_gestures);
    fold_map_field!(per_app_bindings);
    fold_map_field!(camera_profiles);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::device_order::{DeviceIdentity, DeviceStableId};

    fn cabled() -> DeviceStableId {
        DeviceStableId::Direct {
            vendor_id: 0x046d,
            product_id: 0xc08d,
            identity: DeviceIdentity::Unit([0x6b, 0xe9, 0xd3, 0x00]),
        }
    }

    fn on_receiver() -> DeviceStableId {
        DeviceStableId::Bolt {
            receiver_uid: "82839805".to_string(),
            slot: 1,
        }
    }

    #[test]
    fn a_known_unit_keys_the_entry_directly() {
        let config = Config::default();
        let unit = DeviceIdentity::Unit([0x6b, 0xe9, 0xd3, 0x00]);
        let key = config
            .resolve_device_key(&cabled(), Some(&unit))
            .expect("a non-zero unit is a physical identity");
        assert_eq!(key.as_str(), "unit:6be9d300");
    }

    #[test]
    fn the_same_unit_resolves_alike_on_either_route() {
        // The whole point: one mouse, two routes, one entry.
        let config = Config::default();
        let unit = DeviceIdentity::Unit([0x6b, 0xe9, 0xd3, 0x00]);
        assert_eq!(
            config.resolve_device_key(&cabled(), Some(&unit)),
            config.resolve_device_key(&on_receiver(), Some(&unit)),
        );
    }

    fn litra() -> DeviceStableId {
        DeviceStableId::RawHid {
            vendor_id: 0x046d,
            product_id: 0xc901,
            usage_page: 0xff43,
            usage_id: 0x0202,
            identity: "serial:beam-1".to_string(),
        }
    }

    #[test]
    fn settings_still_under_the_pre_upgrade_key_are_read_from_it() {
        // The upgrade path the agent lives through: the load-time migration
        // deliberately leaves `receiver:` keys alone, and only the GUI folds
        // them. Answering with `unit:6be9d300` here would hand the agent an
        // empty entry, so every receiver-paired device would silently revert
        // to default bindings and DPI until the user next opened the GUI.
        let mut config = Config::default();
        config.devices.insert(
            "receiver:82839805:slot:1".to_string(),
            DeviceConfig {
                dpi: Some(Dpi::new(3200)),
                ..DeviceConfig::default()
            },
        );

        let unit = DeviceIdentity::Unit([0x6b, 0xe9, 0xd3, 0x00]);
        let key = config
            .resolve_device_key(&on_receiver(), Some(&unit))
            .expect("resolves");
        assert_eq!(key.as_str(), "receiver:82839805:slot:1");
        assert_eq!(
            config.devices[key.as_str()].dpi,
            Some(Dpi::new(3200)),
            "the settings are where the key says they are"
        );
    }

    #[test]
    fn a_standalone_raw_entry_keeps_its_light_settings() {
        // A Litra has no migration at all: its key would move from
        // `raw:…:serial:beam-1` to `serial:beam-1` the moment the identity
        // won, orphaning the light settings the user configured.
        let mut config = Config::default();
        config.devices.insert(
            "raw:046d:c901:ff43:0202:serial:beam-1".to_string(),
            DeviceConfig {
                light: Some(LightSettings::default()),
                ..DeviceConfig::default()
            },
        );

        let serial = DeviceIdentity::Serial("beam-1".to_string());
        let key = config
            .resolve_device_key(&litra(), Some(&serial))
            .expect("resolves");
        assert_eq!(key.as_str(), "raw:046d:c901:ff43:0202:serial:beam-1");
    }

    #[test]
    fn a_canonical_entry_holding_only_metadata_does_not_shadow_the_legacy_one() {
        // `persist_identities` and the `links` index write entries on their
        // own. Neither is a user setting, so neither may out-rank the entry
        // that actually holds the bindings.
        let mut config = Config::default();
        config.devices.insert(
            "receiver:82839805:slot:1".to_string(),
            DeviceConfig {
                dpi: Some(Dpi::new(3200)),
                ..DeviceConfig::default()
            },
        );
        let mut bare = DeviceConfig::default();
        bare.links.insert(
            "receiver:82839805:slot:1".to_string(),
            LinkConfig::default(),
        );
        config.devices.insert("unit:6be9d300".to_string(), bare);

        let unit = DeviceIdentity::Unit([0x6b, 0xe9, 0xd3, 0x00]);
        let key = config
            .resolve_device_key(&on_receiver(), Some(&unit))
            .expect("resolves");
        assert_eq!(key.as_str(), "receiver:82839805:slot:1");
    }

    #[test]
    fn a_canonical_entry_with_settings_wins_over_a_legacy_one() {
        // Once adoption (or a plain first-run write) has put real settings
        // under the identity key, that is the answer — the legacy entry is a
        // leftover the next fold will consume.
        let mut config = Config::default();
        config.devices.insert(
            "receiver:82839805:slot:1".to_string(),
            DeviceConfig {
                dpi: Some(Dpi::new(3200)),
                ..DeviceConfig::default()
            },
        );
        config.devices.insert(
            "unit:6be9d300".to_string(),
            DeviceConfig {
                dpi: Some(Dpi::new(800)),
                ..DeviceConfig::default()
            },
        );

        let unit = DeviceIdentity::Unit([0x6b, 0xe9, 0xd3, 0x00]);
        let key = config
            .resolve_device_key(&on_receiver(), Some(&unit))
            .expect("resolves");
        assert_eq!(key.as_str(), "unit:6be9d300");
    }

    #[test]
    fn the_route_key_is_the_legacy_entry_key_for_every_variant_but_direct() {
        // `adopt_route` finds a legacy entry with `devices.remove(route_key)`,
        // which only works because a Bolt/RawHid/Unknown route key *is* the
        // key such an entry was written under. A Direct route key deliberately
        // is not: its identity moved into the entry key, which is exactly why
        // the load-time migration can rename direct entries and
        // cannot rename the others. Three doc comments say so; this asserts it.
        for stable in [on_receiver(), litra()] {
            let physical = stable.physical_key().expect("physical");
            assert_eq!(
                stable.route_key(),
                physical.as_str(),
                "{stable:?}: the route key must find the legacy entry"
            );
        }
        let unknown = DeviceStableId::Unknown {
            slot: 2,
            identity: DeviceIdentity::Unit([0x6b, 0xe9, 0xd3, 0x00]),
        };
        assert_eq!(
            unknown.route_key(),
            unknown.physical_key().expect("physical").as_str()
        );

        let direct = cabled();
        assert_ne!(
            direct.route_key(),
            direct.physical_key().expect("physical").as_str(),
            "a direct route key drops the identity that keys the entry"
        );
    }

    #[test]
    fn adoption_repoints_the_selection_and_any_host_switch_target() {
        // Adoption is a rename, and a device key lives in three places. A
        // stale `selected_device` silently jumps the carousel to the first
        // device; a stale host-switch target silently drops out of the group.
        let mut config = Config::default();
        config.devices.insert(
            "receiver:82839805:slot:1".to_string(),
            DeviceConfig {
                dpi: Some(Dpi::new(800)),
                ..DeviceConfig::default()
            },
        );
        config.devices.insert(
            "receiver:82839805:slot:2".to_string(),
            DeviceConfig {
                host_switch_targets: vec!["receiver:82839805:slot:1".to_string()],
                ..DeviceConfig::default()
            },
        );
        config.selected_device = Some("receiver:82839805:slot:1".to_string());

        let canonical = PhysicalDeviceKey::parse("unit:6be9d300").expect("valid");
        assert!(config.adopt_route(&canonical, "receiver:82839805:slot:1", None));

        assert_eq!(config.selected_device.as_deref(), Some("unit:6be9d300"));
        assert_eq!(
            config.devices["receiver:82839805:slot:2"].host_switch_targets,
            vec!["unit:6be9d300".to_string()],
            "the keyboard still switches the mouse it was linked to"
        );
    }

    #[test]
    fn an_asleep_device_resolves_through_its_indexed_route() {
        // Offline: no unit id was readable, only the receiver slot. The links
        // table recorded on an earlier online sighting is the only way back
        // to the entry.
        let mut config = Config::default();
        let mut device = DeviceConfig::default();
        device.links.insert(
            "receiver:82839805:slot:1".to_string(),
            LinkConfig::default(),
        );
        config.devices.insert("unit:6be9d300".to_string(), device);

        let key = config
            .resolve_device_key(&on_receiver(), None)
            .expect("the indexed route resolves");
        assert_eq!(key.as_str(), "unit:6be9d300");
    }

    #[test]
    fn an_unindexed_route_falls_back_to_todays_key() {
        let config = Config::default();
        let key = config
            .resolve_device_key(&on_receiver(), None)
            .expect("receiver keys are physical");
        assert_eq!(key.as_str(), "receiver:82839805:slot:1");
    }

    #[test]
    fn an_all_zero_unit_over_a_receiver_keeps_its_route_key() {
        // Not every device reports a unit id. Such a device simply never
        // correlates across transports rather than colliding on `unit:00000000`.
        let config = Config::default();
        let zero = DeviceIdentity::Unit([0; 4]);
        let key = config
            .resolve_device_key(&on_receiver(), Some(&zero))
            .expect("falls back rather than failing");
        assert_eq!(key.as_str(), "receiver:82839805:slot:1");
    }

    #[test]
    fn a_direct_device_with_no_identity_stays_non_persistent() {
        let config = Config::default();
        let anonymous = DeviceStableId::Direct {
            vendor_id: 0x046d,
            product_id: 0xc08d,
            identity: DeviceIdentity::Unit([0; 4]),
        };
        assert_eq!(config.resolve_device_key(&anonymous, None), None);
    }

    #[test]
    fn folding_takes_a_custom_name_the_canonical_entry_lacks() {
        // The alias is the one field in an entry the user typed by hand. A
        // legacy entry carrying it must not lose it to a canonical entry that
        // was created by, say, a first DPI write and never named.
        let mut config = Config::default();
        let legacy = DeviceConfig {
            custom_name: Some("Desk mouse".to_string()),
            ..DeviceConfig::default()
        };
        config
            .devices
            .insert("receiver:82839805:slot:1".to_string(), legacy);
        config
            .devices
            .insert("unit:6be9d300".to_string(), DeviceConfig::default());

        let canonical = PhysicalDeviceKey::parse("unit:6be9d300").expect("valid");
        assert!(config.adopt_route(&canonical, "receiver:82839805:slot:1", None));

        assert_eq!(
            config.devices["unit:6be9d300"].custom_name.as_deref(),
            Some("Desk mouse"),
            "the user's alias survives the fold"
        );
    }

    #[test]
    fn folding_keeps_the_canonical_custom_name_when_both_are_named() {
        // Two names cannot be merged and there is no per-link slot for one, so
        // the canonical entry wins. `fold` logs the loss rather than dropping
        // it silently; this pins which of the two survives.
        let mut config = Config::default();
        let legacy = DeviceConfig {
            custom_name: Some("Old name".to_string()),
            ..DeviceConfig::default()
        };
        let canonical_entry = DeviceConfig {
            custom_name: Some("Current name".to_string()),
            ..DeviceConfig::default()
        };
        config
            .devices
            .insert("receiver:82839805:slot:1".to_string(), legacy);
        config
            .devices
            .insert("unit:6be9d300".to_string(), canonical_entry);

        let canonical = PhysicalDeviceKey::parse("unit:6be9d300").expect("valid");
        assert!(config.adopt_route(&canonical, "receiver:82839805:slot:1", None));

        assert_eq!(
            config.devices["unit:6be9d300"].custom_name.as_deref(),
            Some("Current name"),
            "the canonical alias wins a genuine conflict"
        );
    }

    #[test]
    fn adopting_a_route_folds_the_legacy_entry_in() {
        // The lighting configured over the receiver has to survive onto the
        // cable — that is the user-visible bug this whole change exists for.
        let mut config = Config::default();
        let legacy = DeviceConfig {
            lighting: Some(Lighting::default()),
            ..DeviceConfig::default()
        };
        config
            .devices
            .insert("receiver:82839805:slot:1".to_string(), legacy);
        config
            .devices
            .insert("unit:6be9d300".to_string(), DeviceConfig::default());

        let canonical = PhysicalDeviceKey::parse("unit:6be9d300").expect("valid");
        assert!(config.adopt_route(&canonical, "receiver:82839805:slot:1", None));

        let device = &config.devices["unit:6be9d300"];
        assert!(device.lighting.is_some(), "lighting moved to device level");
        assert!(device.links.contains_key("receiver:82839805:slot:1"));
        assert!(
            !config.devices.contains_key("receiver:82839805:slot:1"),
            "the legacy entry is consumed, not left behind"
        );
    }

    #[test]
    fn a_sighting_records_what_the_link_measured() {
        // Without this the per-link table only ever gets capabilities from a
        // migration fold, so the "supported on your other connection" notice
        // could never appear for anyone who installed at schema 5.
        let mut config = Config::default();
        let canonical = PhysicalDeviceKey::parse("unit:6be9d300").expect("valid");
        let wired = Capabilities {
            hires_wheel: false,
            ..Capabilities::default()
        };
        let wireless = Capabilities {
            hires_wheel: true,
            ..Capabilities::default()
        };

        assert!(config.adopt_route(&canonical, "direct:046d:c08d", Some(wired)));
        assert!(config.adopt_route(&canonical, "receiver:82839805:slot:1", Some(wireless)));

        let links = &config.devices["unit:6be9d300"].links;
        assert_eq!(links["direct:046d:c08d"].capabilities, Some(wired));
        assert_eq!(
            links["receiver:82839805:slot:1"].capabilities,
            Some(wireless)
        );

        // Re-sighting the same route with the same answer is not a change:
        // reporting one would persist the config on every poll.
        assert!(!config.adopt_route(&canonical, "direct:046d:c08d", Some(wired)));
        // An unprobed sighting is not evidence the capability went away.
        assert!(!config.adopt_route(&canonical, "direct:046d:c08d", None));
        assert_eq!(
            config.devices["unit:6be9d300"].links["direct:046d:c08d"].capabilities,
            Some(wired)
        );
    }

    #[test]
    fn a_conflicting_value_becomes_a_per_link_override() {
        // Both sides set a DPI and they disagree. Neither is wrong — the user
        // set each one — so the legacy value survives as an override on its
        // own link rather than being overwritten.
        let mut config = Config::default();
        let legacy = DeviceConfig {
            dpi: Some(Dpi::new(800)),
            ..DeviceConfig::default()
        };
        config
            .devices
            .insert("receiver:82839805:slot:1".to_string(), legacy);
        let canonical_entry = DeviceConfig {
            dpi: Some(Dpi::new(1600)),
            ..DeviceConfig::default()
        };
        config
            .devices
            .insert("unit:6be9d300".to_string(), canonical_entry);

        let canonical = PhysicalDeviceKey::parse("unit:6be9d300").expect("valid");
        config.adopt_route(&canonical, "receiver:82839805:slot:1", None);

        let device = &config.devices["unit:6be9d300"];
        assert_eq!(device.dpi, Some(Dpi::new(1600)), "canonical stays default");
        assert_eq!(
            device.links["receiver:82839805:slot:1"].overrides.dpi,
            Some(Dpi::new(800)),
            "the legacy value survives on its own link"
        );
    }

    #[test]
    fn adopting_an_unseen_route_registers_it_and_reports_a_change() {
        // No legacy entry to fold in, but a new route in the links index is
        // itself a change worth persisting — the return value means "did
        // anything change", not "was a legacy entry folded".
        let mut config = Config::default();
        config
            .devices
            .insert("unit:6be9d300".to_string(), DeviceConfig::default());
        let canonical = PhysicalDeviceKey::parse("unit:6be9d300").expect("valid");

        assert!(config.adopt_route(&canonical, "direct:046d:c08d", None));
        assert!(
            config.devices["unit:6be9d300"]
                .links
                .contains_key("direct:046d:c08d")
        );
    }

    #[test]
    fn adopting_an_already_indexed_route_reports_no_change() {
        // The route is already recorded and there is no legacy entry to
        // fold — calling `adopt_route` again must be a safe no-op that
        // reports nothing changed, so a caller polling every refresh does
        // not persist and reload on every tick.
        let mut config = Config::default();
        let mut device = DeviceConfig::default();
        device
            .links
            .insert("direct:046d:c08d".to_string(), LinkConfig::default());
        config.devices.insert("unit:6be9d300".to_string(), device);
        let canonical = PhysicalDeviceKey::parse("unit:6be9d300").expect("valid");

        assert!(!config.adopt_route(&canonical, "direct:046d:c08d", None));
    }

    #[test]
    fn a_re_paired_slot_points_at_the_new_unit() {
        // Unpair mouse A from slot 1, pair mouse B into it. Today B inherits
        // A's bindings because the key *is* the slot. It must not.
        let mut config = Config::default();
        let mut first = DeviceConfig::default();
        first.links.insert(
            "receiver:82839805:slot:1".to_string(),
            LinkConfig::default(),
        );
        config.devices.insert("unit:aaaaaaaa".to_string(), first);
        config
            .devices
            .insert("unit:bbbbbbbb".to_string(), DeviceConfig::default());

        let second = PhysicalDeviceKey::parse("unit:bbbbbbbb").expect("valid");
        config.adopt_route(&second, "receiver:82839805:slot:1", None);

        assert!(
            !config.devices["unit:aaaaaaaa"]
                .links
                .contains_key("receiver:82839805:slot:1"),
            "the route no longer points at the previous unit"
        );
        assert!(
            config.devices["unit:bbbbbbbb"]
                .links
                .contains_key("receiver:82839805:slot:1")
        );
    }

    #[test]
    fn per_app_bindings_merge_key_by_key() {
        // A route-keyed entry could carry an app overlay the canonical side
        // never bound — the fold must not silently drop it.
        let mut config = Config::default();
        let legacy = DeviceConfig {
            per_app_bindings: std::collections::BTreeMap::from([(
                "com.example.App".to_string(),
                std::collections::BTreeMap::from([(ButtonId::MiddleClick, Action::Copy)]),
            )]),
            ..DeviceConfig::default()
        };
        config
            .devices
            .insert("receiver:82839805:slot:1".to_string(), legacy);
        config
            .devices
            .insert("unit:6be9d300".to_string(), DeviceConfig::default());

        let canonical = PhysicalDeviceKey::parse("unit:6be9d300").expect("valid");
        config.adopt_route(&canonical, "receiver:82839805:slot:1", None);

        let device = &config.devices["unit:6be9d300"];
        assert_eq!(
            device.per_app_bindings.get("com.example.App"),
            Some(&std::collections::BTreeMap::from([(
                ButtonId::MiddleClick,
                Action::Copy
            )])),
            "the app overlay survives adoption"
        );
    }

    #[test]
    fn light_is_taken_when_the_canonical_side_has_none() {
        // `light` (standalone-light settings) has no per-link override slot,
        // unlike `lighting` — it must still be taken rather than dropped.
        let mut config = Config::default();
        let legacy = DeviceConfig {
            light: Some(LightSettings::default()),
            ..DeviceConfig::default()
        };
        config
            .devices
            .insert("receiver:82839805:slot:1".to_string(), legacy);
        config
            .devices
            .insert("unit:6be9d300".to_string(), DeviceConfig::default());

        let canonical = PhysicalDeviceKey::parse("unit:6be9d300").expect("valid");
        config.adopt_route(&canonical, "receiver:82839805:slot:1", None);

        assert_eq!(
            config.devices["unit:6be9d300"].light,
            Some(LightSettings::default()),
            "the standalone-light setting is not silently discarded"
        );
    }

    #[test]
    fn a_legacy_disabled_flag_disables_the_canonical_entry() {
        // `enabled` defaults to `true` and is only ever persisted as `false`,
        // so a legacy `false` is a deliberate "leave this device alone"
        // choice that adoption must not silently override.
        let mut config = Config::default();
        let legacy = DeviceConfig {
            enabled: false,
            ..DeviceConfig::default()
        };
        config
            .devices
            .insert("receiver:82839805:slot:1".to_string(), legacy);
        config
            .devices
            .insert("unit:6be9d300".to_string(), DeviceConfig::default());

        let canonical = PhysicalDeviceKey::parse("unit:6be9d300").expect("valid");
        config.adopt_route(&canonical, "receiver:82839805:slot:1", None);

        assert!(
            !config.devices["unit:6be9d300"].enabled,
            "the legacy opt-out survives adoption"
        );
    }
}
