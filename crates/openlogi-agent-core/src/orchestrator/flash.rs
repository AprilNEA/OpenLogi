//! The mice whose onboard flash holds their button bindings (the G700): which
//! are present, what the configuration asks their flash to hold, and the
//! bindings the hook runs for the slots the agent takes.
//!
//! What goes to the flash and what to the agent is
//! `openlogi_core::binding::flash`'s decision; the writes are
//! [`crate::flash_writes`]'s. This module only connects the two to the
//! inventory and the configuration.

use std::collections::{BTreeMap, HashMap};

use openlogi_core::binding::flash::flash_intents;
use openlogi_core::binding::{
    Binding, ButtonId, FlashBindingsState, GamingButtons, GamingLayout, ModifierHost,
};
use openlogi_core::bindings::button_bindings_for;
use openlogi_core::config::Config;
use openlogi_core::device::DeviceInventory;
use openlogi_hid::{DeviceRoute, WriteError};

use super::devices::paired_config_key;
use super::{InventoryState, Orchestrator};
use crate::flash_writes::FlashJob;

/// One mouse whose flash holds its bindings.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct FlashDevice {
    /// The configuration entry its bindings are stored under.
    pub(super) config_key: String,
    pub(super) route: DeviceRoute,
    pub(super) online: bool,
    pub(super) layout: &'static GamingLayout,
}

/// The flash-bindings mice `inventories` lists, keyed as `config` keys them.
///
/// A device the configuration cannot key ([`paired_config_key`]) is left
/// out: nothing could be stored for it.
pub(super) fn flash_devices(config: &Config, inventories: &[DeviceInventory]) -> Vec<FlashDevice> {
    let mut devices = Vec::new();
    for inv in inventories {
        for paired in &inv.paired {
            let Some(layout) = GamingButtons::flash_layout_for(paired.capabilities.as_ref()) else {
                continue;
            };
            let Some(route) = DeviceRoute::for_slot(inv, paired.slot) else {
                continue;
            };
            let Some(config_key) = paired_config_key(config, Some(&route), paired) else {
                continue;
            };
            devices.push(FlashDevice {
                config_key: config_key.into_string(),
                route,
                online: paired.online,
                layout,
            });
        }
    }
    devices
}

impl Orchestrator {
    /// Re-derive the flash-bindings mice from the latest inventory and the
    /// configuration, republish the hook's maps if they changed, and ask the
    /// flash of each online one for what the configuration now wants.
    pub(super) fn refresh_flash_devices(&mut self) {
        let devices = match &self.inventory {
            InventoryState::Ready { inventories, .. } => flash_devices(&self.config, inventories),
            InventoryState::Pending | InventoryState::Unavailable => Vec::new(),
        };
        if devices != self.flash_devices {
            self.flash_devices = devices;
            self.publish_hook_maps(self.hook_maps_for(self.current_device()));
        }
        self.request_flash_writes();
    }

    /// Ask the flash-write task to bring each online, enabled flash mouse to
    /// what the configuration wants of it, when that changed since the last
    /// request or the mouse came back. A mouse that leaves, sleeps or is
    /// disabled is forgotten, so its return syncs it again: the flash may
    /// have been rewritten elsewhere meanwhile.
    fn request_flash_writes(&mut self) {
        let wanted: HashMap<DeviceRoute, _> = self
            .flash_devices
            .iter()
            .filter(|device| device.online && self.config.device_enabled(&device.config_key))
            .map(|device| {
                let intents = flash_intents(
                    &self.config,
                    &device.config_key,
                    device.layout,
                    ModifierHost::CURRENT,
                );
                (device.route.clone(), intents)
            })
            .collect();
        for (route, intents) in &wanted {
            if self.flash_requested.get(route) != Some(intents) {
                self.shared.flash_writes.request(
                    route.clone(),
                    FlashJob {
                        bindings: intents.clone(),
                    },
                );
            }
        }
        self.flash_requested = wanted;
    }

    /// What `route`'s flash bindings were last found to be and where their
    /// write stands, from the flash-write task's record;
    /// [`WriteError::DeviceNotFound`] for a route that is no flash-bindings
    /// mouse.
    pub fn read_flash_bindings(
        &self,
        route: &DeviceRoute,
    ) -> Result<FlashBindingsState, WriteError> {
        if !self
            .flash_devices
            .iter()
            .any(|device| device.route == *route)
        {
            return Err(WriteError::DeviceNotFound);
        }
        Ok(self.shared.flash_writes.state(route))
    }

    /// The bindings the hook runs for the agent-run slots of the first
    /// online, enabled flash mouse, for the app in front. The hook cannot
    /// tell two such mice apart, so a second one shares the first's.
    pub(super) fn flash_hook_bindings(&self) -> BTreeMap<ButtonId, Binding> {
        let Some(device) = self
            .flash_devices
            .iter()
            .find(|device| device.online && self.config.device_enabled(&device.config_key))
        else {
            return BTreeMap::new();
        };
        let (app, _) = self.mouse_context();
        let mut bindings = button_bindings_for(
            &self.config,
            Some(&device.config_key),
            app,
            Some(device.layout),
        );
        bindings.retain(|button, _| {
            device
                .layout
                .slots
                .iter()
                .any(|slot| slot.button == *button)
        });
        bindings
    }
}

#[cfg(test)]
mod tests;
