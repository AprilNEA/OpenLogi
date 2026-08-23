//! Device list refresh, transient adoption, and selection.

use std::collections::{BTreeMap, HashSet};

use openlogi_core::config::{Config, DeviceIdentity};
use openlogi_core::device::{DeviceInventory, StandaloneDevice};
use tracing::debug;

use crate::services::assets::AssetResolver;
use crate::services::assets::sync::{AssetTarget, model_key};
use crate::state::devices::{
    DeviceRecord, adopt_transient_record, build_device_list, direct_key_prefix, sort_device_list,
};

use super::device_key::DeviceKey;
use super::device_ui::DeviceUiState;
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
        self.device_list
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
    /// Replace [`Self::device_list`] from a fresh inventory snapshot,
    /// preserving the carousel selection by `config_key` when possible. If
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
        let merged_list = self.merge_inventory_snapshot(new_list);
        // Capture any newly-probed identity before the unchanged-check can early
        // out: a device whose capabilities just resolved keeps the same
        // config_key + route, so that guard would otherwise skip the write.
        if persist_identities(&mut self.config, &merged_list) {
            self.persist_config("device identity");
        }
        // Whole-record equality, not a field allowlist. Every field of a
        // `DeviceRecord` is rendered somewhere, so any of them differing is a
        // real change; an allowlist silently drops the fields nobody thought to
        // add — `battery` and the resolved `asset` were both being swallowed
        // here. Structural comparison also makes the guard immune to new
        // fields, which is what an allowlist can never be.
        if merged_list == self.device_list {
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

        // A device that came back on a different route must re-discover DPI —
        // its cached status/attempts were keyed to the now-dead route.
        let rerouted: Vec<DeviceKey> = merged_list
            .iter()
            .filter(|new| {
                self.device_list
                    .iter()
                    .any(|old| old.config_key == new.config_key && old.route != new.route)
            })
            .map(DeviceRecord::device_key)
            .collect();

        self.device_list = merged_list;
        for key in &rerouted {
            self.reads.dpi.remove(key);
            self.reads.smartshift.remove(key);
            if let Some(entry) = self.device_ui.get_mut(key) {
                entry.smartshift_pending_confirm = None;
                entry.smartshift_write_status = None;
            }
        }
        let present = |key: &str| {
            self.device_list
                .iter()
                .any(|r| r.config_key.as_str() == key)
        };
        self.reads.dpi.retain_present(present);
        self.reads.smartshift.retain_present(present);
        self.current_device = new_index;
        // The active device may have changed (selection fell back to index 0
        // when the previous one vanished); re-seed the displayed DPI so it
        // tracks the now-current device rather than the old one.
        self.dpi = self.dpi_for_current();
        self.button_bindings = self.bindings_for_current();
        self.gesture_bindings = self.current_gesture_maps();
        // Display state only — the agent runs its own inventory watcher and
        // rebuilds the live binding/DPI maps itself.
        true
    }
    pub(crate) fn merge_inventory_snapshot(
        &mut self,
        new_list: Vec<DeviceRecord>,
    ) -> Vec<DeviceRecord> {
        let mut by_key = new_list
            .into_iter()
            .map(|record| (record.inventory_key(), record))
            .collect::<BTreeMap<_, _>>();
        let mut adopted = self.adopt_transient_records(&mut by_key);
        let mut merged = Vec::with_capacity(by_key.len().max(self.device_list.len()));

        for previous in &self.device_list {
            let inv = previous.inventory_key();
            if let Some(record) = by_key.remove(&inv) {
                clear_inventory_misses(&mut self.device_ui, &inv);
                merged.push(record);
                continue;
            }

            if let Some(record) = adopted.remove(&inv) {
                clear_inventory_misses(&mut self.device_ui, &inv);
                merged.push(record);
                continue;
            }

            // An all-zero direct unit id is only a transient probe result. If
            // the next snapshot resolves a physical serial/unit key, retaining
            // this record through the normal miss grace would show both cards.
            if !previous.is_persistent() {
                clear_inventory_misses(&mut self.device_ui, &inv);
                continue;
            }

            // Cameras reappear under a new capture id after a port change —
            // do not grace-keep a stale cam-live entry beside the new one.
            if previous.kind == openlogi_core::device::DeviceKind::Camera {
                clear_inventory_misses(&mut self.device_ui, &inv);
                continue;
            }

            let entry = self
                .device_ui
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
            clear_inventory_misses(&mut self.device_ui, &key);
            merged.push(record);
        }
        // Adopted records whose known card was never in the previous list
        // (identity known only from config) still belong in the carousel.
        merged.extend(adopted.into_values());
        let live: HashSet<String> = merged.iter().map(DeviceRecord::inventory_key).collect();
        self.device_ui.retain(|key, _| live.contains(key.as_str()));
        // `merged` is `previous-order + newly-appeared`, so re-apply the
        // canonical route order or a new device would be stuck at the end of
        // the carousel permanently.
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
    pub(crate) fn adopt_transient_records(
        &self,
        by_key: &mut BTreeMap<String, DeviceRecord>,
    ) -> BTreeMap<String, DeviceRecord> {
        let transient_keys: Vec<String> = by_key
            .values()
            .filter(|record| !record.is_persistent())
            .map(|record| record.config_key.clone())
            .collect();
        let mut adopted = BTreeMap::new();
        for key in transient_keys {
            let Some(prefix) = direct_key_prefix(&key) else {
                continue;
            };
            let same_wire = |key: &str, record: &DeviceRecord| {
                record.is_persistent() && direct_key_prefix(key) == Some(prefix)
            };
            // A live online sibling is accounted for and never a candidate,
            // but it must not discard the transient — the half-read probe may
            // be the *other* same-model device.
            let mut candidates: Vec<String> = by_key
                .iter()
                .filter(|(k, record)| same_wire(k, record) && !record.online)
                .map(|(k, _)| k.clone())
                .collect();
            for previous in &self.device_list {
                if same_wire(&previous.config_key, previous)
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
                        .any(|(k, record)| same_wire(k, record) && record.online)
                {
                    by_key.remove(&key);
                }
                continue;
            };
            // Last tick's record carries the freshest identity; the offline
            // placeholder built from config is the fallback.
            let known = self
                .device_list
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
    /// Switch the carousel to `idx`. Out-of-range indices are silently
    /// ignored so callers can pass them straight through from UI events.
    /// Persists the new selection (by config key, not index — index isn't
    /// stable across restarts), reloads bindings for the new device, and
    /// pushes the new map into the hook-shared `Arc`.
    pub fn set_current_device(&mut self, idx: usize) {
        if idx >= self.device_list.len() || idx == self.current_device {
            return;
        }
        self.current_device = idx;
        // A device left in `Failed` (transient read errors exhausted its retry
        // budget) gets one fresh attempt each time it is re-selected.
        if let Some(key) = self.current_record().map(DeviceRecord::device_key) {
            if matches!(self.reads.dpi.get(&key), Some(Load::Failed(_))) {
                self.reads.dpi.retry(&key);
            }
            if matches!(self.reads.smartshift.get(&key), Some(Load::Failed(_))) {
                self.retry_smartshift(&key);
            }
        }
        // `self.dpi` is the active device's value; adopt the newly-selected
        // device's known DPI so the panel doesn't keep showing the previous
        // device's number until a fresh read lands.
        self.dpi = self.dpi_for_current();
        self.button_bindings = self.bindings_for_current();
        self.gesture_bindings = self.current_gesture_maps();
        let Some(key) = self
            .current_record()
            .and_then(DeviceRecord::persistent_config_key)
            .map(str::to_string)
        else {
            debug!("transient device selection not persisted");
            return;
        };
        self.config.set_selected_device(Some(key));
        // The agent owns the hook + device I/O; have it switch devices too.
        self.persist_and_reload("selected device");
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
            display_name: record.display_name.clone(),
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

/// Reset `key`'s consecutive-miss counter — the device was just confirmed
/// present (live, adopted, or freshly appeared) or is a kind that never earns
/// grace (transient, camera). Leaves the rest of the device's UI row
/// untouched. A free function, not an `AppState` method, so callers can hold
/// it alongside a live borrow of `self.device_list`.
fn clear_inventory_misses(device_ui: &mut BTreeMap<DeviceKey, DeviceUiState>, key: &str) {
    if let Some(entry) = device_ui.get_mut(key) {
        entry.inventory_misses = 0;
    }
}
