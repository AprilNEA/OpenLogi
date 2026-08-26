//! Device list refresh, transient adoption, and selection.

use std::collections::{BTreeMap, HashSet};

use openlogi_core::config::{Config, DeviceIdentity};
use openlogi_core::device::{DeviceInventory, StandaloneDevice};
use openlogi_core::device_order::PhysicalDeviceKey;
use tracing::debug;

use crate::services::assets::AssetResolver;
use crate::services::assets::sync::{AssetTarget, model_key};
use crate::state::devices::{
    DeviceRecord, adopt_transient_record, build_device_list, direct_key_prefix,
    fold_by_inventory_key, record_wire_pid, sort_device_list,
};

use super::device_key::DeviceKey;
use super::device_runtime::DeviceRuntimeState;
use super::load::Load;
use super::{AppState, INVENTORY_MISS_GRACE};

impl AppState {
    /// Every known device model that can be resolved to an asset depot.
    ///
    /// This reads the UI's merged device list rather than only the latest live
    /// inventory, so a temporarily incomplete probe can still download art for
    /// a device restored from its persisted identity.
    pub(crate) fn asset_models(&self) -> Vec<AssetTarget> {
        let mut seen = HashSet::new();
        self.devices
            .records
            .iter()
            .filter_map(|record| {
                let target = record
                    .registry_model_id
                    .clone()
                    .map(|registry_model_id| AssetTarget::Standalone { registry_model_id })
                    .or_else(|| {
                        record.model_info.clone().map(|model| AssetTarget::Hidpp {
                            model,
                            codename: record.codename.clone(),
                        })
                    })?;
                seen.insert(model_key(&target)).then_some(target)
            })
            .collect()
    }
    /// Replace the merged device catalog from a fresh inventory snapshot,
    /// preserving the active device by `config_key` when possible. If
    /// the previously-selected device disappeared, the selection falls back
    /// to index 0. Returns whether anything actually changed.
    ///
    /// No-op (returning `false`) when the rebuilt list equals the current one,
    /// so the caller skips the window refresh. The comparison is whole-record,
    /// which is what lets every input tier — the agent snapshot, the camera
    /// scan, and the asset cache — share one rebuild path without any of them
    /// needing to announce which fields it might have touched.
    pub fn refresh_inventories(
        &mut self,
        inventories: &[DeviceInventory],
        standalone: &[StandaloneDevice],
        cache: &AssetResolver,
        cameras: &[openlogi_camera::Camera],
    ) -> bool {
        let new_list = build_device_list(inventories, standalone, cache, &self.config, cameras);
        // Adoption runs before anything else touches the config. Only an
        // online record's identity was actually read this snapshot, so only an
        // online sighting can attribute its route to a device with confidence
        // — an offline route/identity pairing would be a guess. The agent
        // never writes config, so the GUI is the only adopter.
        //
        // Ordering is load-bearing: `persist_identities` keys off each
        // record's `config_key`, so running it first would write a bare
        // canonical entry beside the un-folded legacy one — and a bare entry
        // is exactly what `Config::resolve_device_key` must not let out-rank
        // the legacy entry holding the user's settings.
        let adopted = self.config.edit(|config| adopt_routes(config, &new_list));
        // The fold moved those settings to the canonical key, so records built
        // a moment ago still name the key it consumed. Rebuild, or
        // `persist_identities` would resurrect the entry adoption just
        // retired.
        //
        // The rebuild only fixes `new_list`; `merge_inventory_snapshot` below
        // can still re-inject a grace-kept record naming the consumed key
        // from `self.device_list`. What actually keeps that record from
        // getting a bare entry written back under it is that
        // `persist_identities`' `if !record.online { continue; }` (below)
        // skips it — a grace-kept record is never online. That guard existing
        // for its own reason is what this rebuild silently depends on.
        let new_list = if adopted {
            build_device_list(inventories, standalone, cache, &self.config, cameras)
        } else {
            new_list
        };
        let merged_list = self.merge_inventory_snapshot(new_list);
        // Capture any newly-probed identity before the unchanged-check can early
        // out: a device whose capabilities just resolved keeps the same
        // config_key + route, so that guard would otherwise skip the write.
        let identities_changed = self
            .config
            .edit(|config| persist_identities(config, &merged_list));
        if adopted {
            // One write covers both: a re-keyed entry needs the agent to
            // reload, and the identity update rides along with it. A failed
            // write rolls `self.config` back to the pre-fold, legacy-keyed
            // state (`persist_config`'s `restore_config_projections`), which
            // makes `merged_list` — built from the folded config — name a
            // `config_key` that no longer exists in `self.config`. Bail out
            // before it can be assigned to the record list: the list this
            // struct is still showing (built from the pre-fold config) is the
            // truthful one, and the next tick will retry the fold.
            if !self.persist_and_reload("adopt device route") {
                return false;
            }
        } else if identities_changed {
            self.persist_config("device identity");
        }
        // Whole-record equality, not a field allowlist. Most fields of a
        // `DeviceRecord` are rendered somewhere, so almost any of them
        // differing is a real change; an allowlist silently drops the fields
        // nobody thought to add — `battery` and the resolved `asset` were
        // both being swallowed here. Structural comparison also makes the
        // guard immune to new fields, which is what an allowlist can never
        // be.
        if merged_list == self.devices.records {
            return false;
        }

        let previous_key = self.current_record().map(DeviceRecord::inventory_key);
        let new_index = previous_key
            .as_deref()
            .and_then(|k| merged_list.iter().position(|r| r.inventory_key() == k))
            .unwrap_or(0);
        let connected_keys = merged_list
            .iter()
            .map(|r| r.config_key.as_str())
            .collect::<Vec<_>>();
        debug!(
            count = merged_list.len(),
            ?connected_keys,
            "inventory refreshed"
        );

        // A device that came back on a different route must re-run its device
        // queries — their subscriptions targeted the now-dead route.
        let rerouted: Vec<DeviceKey> = merged_list
            .iter()
            .filter(|new| {
                self.devices
                    .records
                    .iter()
                    .any(|old| old.config_key == new.config_key && old.route != new.route)
            })
            .map(DeviceRecord::device_key)
            .collect();

        self.devices.replace(merged_list, new_index);
        for key in &rerouted {
            self.pointer.reads.remove(key);
            if let Some(entry) = self.devices.runtime.get_mut(key) {
                entry.smartshift.pending_confirm = None;
                entry.smartshift.write_status = None;
            }
        }
        let present: HashSet<_> = self
            .devices
            .records
            .iter()
            .map(|record| record.config_key.as_str())
            .collect();
        self.pointer
            .reads
            .retain_present(|key| present.contains(key));
        // The active device may have changed (selection fell back to index 0
        // when the previous one vanished); re-seed the displayed DPI so it
        // tracks the now-current device rather than the old one.
        self.pointer.dpi = self.dpi_for_current();
        self.refresh_binding_projections();
        // Display state only — the agent runs its own inventory watcher and
        // rebuilds the live binding/DPI maps itself.
        true
    }
    pub(crate) fn merge_inventory_snapshot(
        &mut self,
        new_list: Vec<DeviceRecord>,
    ) -> Vec<DeviceRecord> {
        let mut by_key = fold_by_inventory_key(new_list);
        let mut adopted = self.adopt_transient_records(&mut by_key);
        let mut merged = Vec::with_capacity(by_key.len().max(self.devices.records.len()));

        for previous in &self.devices.records {
            let inv = previous.inventory_key();
            if let Some(record) = by_key.remove(&inv) {
                clear_inventory_misses(&mut self.devices.runtime, &inv);
                merged.push(record);
                continue;
            }

            if let Some(record) = adopted.remove(&inv) {
                clear_inventory_misses(&mut self.devices.runtime, &inv);
                merged.push(record);
                continue;
            }

            // An all-zero direct unit id is only a transient probe result. If
            // the next snapshot resolves a physical serial/unit key, retaining
            // this record through the normal miss grace would show both cards.
            if !previous.is_persistent() {
                clear_inventory_misses(&mut self.devices.runtime, &inv);
                continue;
            }

            // Cameras reappear under a new capture id after a port change —
            // do not grace-keep a stale cam-live entry beside the new one.
            if previous.kind == openlogi_core::device::DeviceKind::Camera {
                clear_inventory_misses(&mut self.devices.runtime, &inv);
                continue;
            }

            let entry = self
                .devices
                .runtime
                .entry(DeviceKey::from(inv.as_str()))
                .or_default();
            entry.inventory_misses = entry.inventory_misses.saturating_add(1);
            let misses = entry.inventory_misses;
            if misses <= INVENTORY_MISS_GRACE {
                debug!(
                    key = %inv,
                    misses,
                    "keeping device through transient inventory miss"
                );
                merged.push(previous.clone());
            }
        }

        for (key, record) in by_key {
            clear_inventory_misses(&mut self.devices.runtime, &key);
            merged.push(record);
        }
        // Adopted records whose known card was never in the previous list
        // (identity known only from config) still belong in the gallery.
        merged.extend(adopted.into_values());
        let live: HashSet<String> = merged.iter().map(DeviceRecord::inventory_key).collect();
        self.devices
            .runtime
            .retain(|key, _| live.contains(key.as_str()));
        // `merged` is `previous-order + newly-appeared`, so re-apply the
        // canonical route order or a new device would be stuck at the end of
        // the gallery permanently.
        sort_device_list(&mut merged);
        merged
    }
    /// Pair each transient direct record in the snapshot with the device it
    /// physically is. A transient key (`…:unit:00000000`) is a half-read probe
    /// of some existing device, not a new one (#482): when exactly one known
    /// card sharing its `direct:<vid>:<pid>` wire identity is not live online —
    /// so the half-read probe can only be that device — the transient record is
    /// folded into that card instead of surfacing beside it (or evicting it).
    /// With no such card the transient is dropped as probe noise when its wire
    /// product is already live online, and an ambiguous one (two known
    /// same-model cards absent) is left alone.
    ///
    /// The wire identity is compared through [`DeviceRecord::route_key`], not
    /// `config_key`: since Task 6 a persistent record's `config_key` may
    /// already be the device's own transport-free identity (`unit:…`) rather
    /// than a `direct:<vid>:<pid>:…` runtime key, so `direct_key_prefix` can
    /// no longer be relied on to parse it back out. The persisted link index
    /// backs an offline placeholder that has already been adopted once, but
    /// two same-model direct devices can only ever have *one* owner for a
    /// shared `direct:<vid>:<pid>` route (`Config::adopt_route` is
    /// exclusive), so the absent twin also needs a route-independent proof:
    /// [`record_wire_pid`] compares the HID++ model id the offline
    /// placeholder persisted against the one the half-read probe itself
    /// reports (`model_info` survives a half read even when the unit id
    /// does not).
    pub(crate) fn adopt_transient_records(
        &self,
        by_key: &mut BTreeMap<String, DeviceRecord>,
    ) -> BTreeMap<String, DeviceRecord> {
        let transient_keys: Vec<(String, Option<String>)> = by_key
            .values()
            .filter(|record| !record.is_persistent())
            .map(|record| (record.config_key.clone(), record_wire_pid(record)))
            .collect();
        let mut adopted = BTreeMap::new();
        for (key, transient_wire_pid) in transient_keys {
            let Some(prefix) = direct_key_prefix(&key) else {
                continue;
            };
            // A live record's own `route_key` proves wire compatibility
            // directly. An offline placeholder carries no live route, so
            // fall back first to its persisted link index (the route it was
            // last adopted on) and, failing that, to a wire-pid match
            // against the transient probe itself — the only proof available
            // for a same-model sibling that has never been adopted, since a
            // shared `direct:<vid>:<pid>` route can be indexed to at most one
            // device at a time.
            let same_wire = |record: &DeviceRecord| {
                record.is_persistent()
                    && (record.route_key == prefix
                        || self
                            .config
                            .devices
                            .get(record.config_key.as_str())
                            .is_some_and(|device| device.links.contains_key(prefix))
                        || transient_wire_pid.is_some()
                            && record_wire_pid(record) == transient_wire_pid)
            };
            // A live online sibling is accounted for and never a candidate,
            // but it must not discard the transient — the half-read probe may
            // be the *other* same-model device.
            let mut candidates: Vec<String> = by_key
                .iter()
                .filter(|(_, record)| same_wire(record) && !record.online)
                .map(|(k, _)| k.clone())
                .collect();
            for previous in &self.devices.records {
                if same_wire(previous)
                    && !by_key.contains_key(&previous.config_key)
                    && !candidates.contains(&previous.config_key)
                {
                    candidates.push(previous.config_key.clone());
                }
            }
            let [known_key] = candidates.as_slice() else {
                if candidates.is_empty()
                    && by_key
                        .iter()
                        .any(|(_, record)| same_wire(record) && record.online)
                {
                    by_key.remove(&key);
                }
                continue;
            };
            // Last tick's record carries the freshest identity; the offline
            // placeholder built from config is the fallback.
            let known = self
                .devices
                .records
                .iter()
                .find(|record| record.config_key == *known_key)
                .cloned()
                .or_else(|| by_key.get(known_key).cloned());
            let Some(known) = known else {
                continue;
            };
            let known_key = known_key.clone();
            by_key.remove(&known_key);
            if let Some(live) = by_key.remove(&key) {
                adopted.insert(known_key, adopt_transient_record(&known, live));
            }
        }
        adopted
    }
    /// Make the device at `idx` active. Out-of-range indices are silently
    /// ignored so callers can pass them straight through from UI events.
    /// Persists the new selection (by config key, not index — index isn't
    /// stable across restarts), reloads bindings for the new device, and
    /// pushes the new map into the hook-shared `Arc`. Returns the selected
    /// device key only when the selection changed.
    pub fn set_current_device(&mut self, idx: usize) -> Option<DeviceKey> {
        if !self.devices.select(idx) {
            return None;
        }
        let selected_key = self.current_record().map(DeviceRecord::device_key)?;
        // A device left in `Failed` (transient read errors exhausted its retry
        // budget) gets one fresh attempt each time it is re-selected.
        if let Some(key) = self.current_record().map(DeviceRecord::device_key) {
            if matches!(self.pointer.reads.dpi_load(&key), Some(Load::Failed(_))) {
                self.pointer.reads.retry_dpi(&key);
            }
            if matches!(
                self.pointer.reads.smartshift_load(&key),
                Some(Load::Failed(_))
            ) {
                self.retry_smartshift(&key);
            }
        }
        // The pointer editor value follows the active device; adopt the newly
        // selected device's known DPI so the panel doesn't keep showing the
        // previous device's number until a fresh read lands.
        self.pointer.dpi = self.dpi_for_current();
        self.refresh_binding_projections();
        let Some(key) = self
            .current_record()
            .and_then(DeviceRecord::persistent_config_key)
            .map(str::to_string)
        else {
            debug!("transient device selection not persisted");
            return Some(selected_key);
        };
        self.config
            .edit(|config| config.set_selected_device(Some(key)));
        // The agent owns the hook + device I/O; have it switch devices too.
        self.persist_and_reload("selected device");
        Some(selected_key)
    }
}

impl super::AppState {
    /// Forget an offline device: drop its persisted identity, custom name,
    /// and per-device settings, and remove its placeholder card. Live devices
    /// are never offered this — the next inventory snapshot would simply
    /// re-register them.
    pub(crate) fn forget_device(&mut self, record_key: &str) -> bool {
        let Some(index) = self
            .devices
            .records
            .iter()
            .position(|record| record.record_key() == record_key)
        else {
            return false;
        };
        let record = &self.devices.records[index];
        if record.online {
            return false;
        }
        let device_key = record.device_key();
        let config_key = record.persistent_config_key().map(str::to_string);

        // Dropping the config entry *is* the deletion, so the card only
        // follows once the write lands. A failed save restores the persisted
        // revision, so returning early keeps memory, disk, and the gallery in
        // agreement: the device honestly stays instead of vanishing until the
        // next inventory refresh resurrects it.
        if let Some(config_key) = config_key {
            self.config.edit(|config| config.remove_device(&config_key));
            if !self.persist_and_reload("device removed") {
                return false;
            }
        }

        let mut records = self.devices.records.clone();
        records.remove(index);
        let selected = match self.devices.selected_index() {
            Some(selected) if selected > index => selected - 1,
            Some(selected) if selected == index => 0,
            Some(selected) => selected,
            None => 0,
        };
        self.devices.replace(records, selected);
        self.devices.runtime.remove(&device_key);
        self.pointer.reads.remove(&device_key);
        true
    }
}

pub(super) fn persist_identities(config: &mut Config, list: &[DeviceRecord]) -> bool {
    let mut changed = false;
    for record in list {
        if !record.online {
            continue;
        }
        let Some(config_key) = record.persistent_config_key() else {
            continue;
        };
        let capabilities = record.capabilities.unwrap_or_default();
        if record.light_capabilities.is_none() && record.capabilities.is_none() {
            continue;
        }
        let identity = DeviceIdentity {
            display_name: record.model_name.clone(),
            kind: record.kind,
            capabilities,
            light_capabilities: record.light_capabilities,
            model_info: record.model_info.clone(),
            codename: record.codename.clone(),
            driver_id: record.driver_id.clone(),
            registry_model_id: record.registry_model_id.clone(),
        }
        .without_unit_identifiers();
        if config.device_identity(config_key) != Some(&identity) {
            config.set_device_identity(config_key, identity);
            changed = true;
        }
    }
    changed
}

/// Fold every online, persistent record's route into its canonical config
/// entry, so a device reached both ways in one snapshot resolves to the one
/// entry its settings actually live under.
///
/// A route two online records share cannot be attributed to either one —
/// `route_key` for a Direct route strips the device's own identity, so two
/// same-model direct devices seen online in the same snapshot report the
/// *same* route key. `Config::adopt_route` is exclusive per route, so
/// adopting it for one twin only gets it stolen back by the other on the
/// very next tick — a persist-and-reload storm that also churns the loser's
/// `LinkConfig` (including any per-link overrides) on every refresh. Such a
/// route is therefore skipped entirely; the route-derived key each twin
/// already falls back to is the correct answer when nothing can tell them
/// apart by route alone. Returns whether any entry changed.
///
/// The fold target is the record's *canonical* key, never its `config_key`:
/// those differ precisely in the case adoption exists to resolve — settings
/// still under a pre-schema-5 route key — so folding onto `config_key` would
/// fold the legacy entry onto itself and the branch would never converge.
pub(super) fn adopt_routes(config: &mut Config, list: &[DeviceRecord]) -> bool {
    let mut route_counts: BTreeMap<&str, usize> = BTreeMap::new();
    for record in list {
        if record.is_persistent() && record.online {
            *route_counts.entry(record.route_key.as_str()).or_insert(0) += 1;
        }
    }
    let mut adopted = false;
    for record in list {
        if !record.is_persistent() || !record.online {
            continue;
        }
        if route_counts
            .get(record.route_key.as_str())
            .is_some_and(|&count| count > 1)
        {
            continue;
        }
        let canonical = record
            .canonical_key
            .as_deref()
            .unwrap_or(&record.config_key);
        let Some(key) = PhysicalDeviceKey::parse(canonical) else {
            continue;
        };
        adopted |= config.adopt_route(&key, &record.route_key, record.capabilities);
    }
    adopted
}

/// Reset `key`'s consecutive-miss counter — the device was just confirmed
/// present (live, adopted, or freshly appeared) or is a kind that never earns
/// grace (transient, camera). Leaves the rest of the device's runtime row
/// untouched. A free function, not an `AppState` method, so callers can hold
/// it alongside a live borrow of the device catalog.
fn clear_inventory_misses(runtime: &mut BTreeMap<DeviceKey, DeviceRuntimeState>, key: &str) {
    if let Some(entry) = runtime.get_mut(key) {
        entry.inventory_misses = 0;
    }
}
