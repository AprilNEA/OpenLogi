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
    Binding, ButtonId, FlashBindingsState, FlashHost, GamingButtons, GamingLayout, RefusedSlot,
    SlotIntent,
};
use openlogi_core::bindings::button_bindings_for;
use openlogi_core::config::Config;
use openlogi_core::device::DeviceInventory;
use openlogi_core::hid::{Dpi, FlashProfileEdit, FlashProfiles, ProfileIndex, ReportRate};
use openlogi_hid::{DeviceRoute, G700DpiSave, HidppOperation, WriteError};

use super::devices::paired_config_key;
use super::{InventoryState, Orchestrator, SharedHandles};
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
                    FlashHost::CURRENT,
                );
                (device.route.clone(), (device.layout, intents))
            })
            .collect();
        let mut requested = HashMap::with_capacity(wanted.len());
        for (route, (layout, intents)) in wanted {
            let changed = self.flash_requested.get(&route) != Some(&intents);
            if changed || self.shared.flash_writes.failed(&route) {
                if changed {
                    log_refused_slots(layout, &intents);
                }
                self.shared
                    .flash_writes
                    .request(route.clone(), FlashJob::bindings(intents.clone()));
            }
            requested.insert(route, intents);
        }
        self.flash_requested = requested;
    }

    /// Whether `route` is a mouse whose flash holds its settings.
    #[must_use]
    pub fn is_flash_device(&self, route: &DeviceRoute) -> bool {
        self.flash_devices
            .iter()
            .any(|device| device.route == *route)
    }

    /// Note that `route` plays the profile at directory index `played`
    /// (`None`: its factory profile), as a read of it just found. When that
    /// is not the profile its last flash job left it on — a profile button
    /// on the mouse, or a selection from the settings app — its bindings are
    /// synced again, as on its return: the hook's agent buttons and the
    /// Buttons panel follow the profile it plays.
    pub fn note_played_profile(&self, route: &DeviceRoute, played: Option<u8>) {
        if !self.is_flash_device(route) {
            return;
        }
        match self.shared.flash_writes.played(route) {
            Some(last) if last != played => {
                tracing::info!(
                    ?last,
                    ?played,
                    "the G700 plays another profile — syncing it"
                );
                self.shared
                    .flash_writes
                    .request(route.clone(), FlashJob::default());
            }
            _ => {}
        }
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

impl SharedHandles {
    /// The onboard profiles of the flash mouse at `route`, read from it,
    /// with where its queued flash writes stand.
    pub async fn read_flash_profiles(
        &self,
        route: &DeviceRoute,
    ) -> Result<FlashProfiles, WriteError> {
        let mut profiles = self
            .device(route)
            .run(HidppOperation::ReadOnboard, |c| async move {
                openlogi_hid::read_g700_profiles_on(&c)
                    .await
                    .map_err(WriteError::from)
            })
            .await?;
        profiles.write = self.flash_writes.state(route).write;
        Ok(profiles)
    }

    /// The directory index of the profile the flash mouse at `route`
    /// plays, `None` for its factory profile: one register read.
    pub async fn read_played_flash_profile(
        &self,
        route: &DeviceRoute,
    ) -> Result<Option<u8>, WriteError> {
        self.device(route)
            .run(HidppOperation::ReadOnboard, |c| async move {
                openlogi_hid::read_g700_played_profile_on(&c)
                    .await
                    .map_err(WriteError::from)
            })
            .await
    }

    /// Apply `edit` to the flash mouse at `route`: a profile selection at
    /// once, anything else checked and queued with its other flash writes.
    pub async fn edit_flash_profiles(
        &self,
        route: &DeviceRoute,
        edit: FlashProfileEdit,
    ) -> Result<(), WriteError> {
        if let FlashProfileEdit::Activate(profile) = edit {
            return self.select_flash_profile(route, profile).await;
        }
        openlogi_hid::check_g700_edit(&edit).map_err(WriteError::from)?;
        self.flash_writes
            .request(route.clone(), FlashJob::edit(edit));
        Ok(())
    }

    async fn select_flash_profile(
        &self,
        route: &DeviceRoute,
        profile: ProfileIndex,
    ) -> Result<(), WriteError> {
        self.device(route)
            .run(HidppOperation::SelectOnboardProfile, |c| async move {
                openlogi_hid::activate_g700_profile_on(&c, profile)
                    .await
                    .map_err(WriteError::from)
            })
            .await
    }

    /// Set the flash mouse at `route` to `dpi` live, then save it to the
    /// played profile's DPI mode the mouse was in — the one whose value it
    /// ran at — once the edits settle ([`crate::flash_writes::DEBOUNCE`]).
    /// When the DPI it ran at does not read, the change stays live only.
    pub async fn set_flash_dpi(&self, route: &DeviceRoute, dpi: Dpi) -> Result<(), WriteError> {
        let from = self
            .device(route)
            .run(HidppOperation::ReadDpi, |c| async move {
                openlogi_hid::get_dpi_info_on(&c).await
            })
            .await
            .map(|info| info.current);
        self.set_dpi(route, dpi).await?;
        match from {
            Ok(from) if from != dpi => self
                .flash_writes
                .request(route.clone(), FlashJob::dpi(G700DpiSave { from, to: dpi })),
            Ok(_) => {}
            Err(error) => {
                tracing::warn!(%error, "the G700's DPI did not read before the change — kept live only");
            }
        }
        Ok(())
    }

    /// Save `rate`, which the flash mouse at `route` now reports at, to
    /// its played profile once the edits settle.
    pub fn save_flash_report_rate(&self, route: &DeviceRoute, rate: ReportRate) {
        self.flash_writes
            .request(route.clone(), FlashJob::rate(rate));
    }
}

/// Logs each slot whose configured action the agent may not run, once per
/// change of what the configuration asks.
fn log_refused_slots(layout: &GamingLayout, intents: &[SlotIntent]) {
    for (button, intent) in layout.slots.iter().zip(intents) {
        match intent {
            SlotIntent::Refused(RefusedSlot::PrimaryButton) => tracing::warn!(
                ?button,
                "a primary button keeps its native click: an action only OpenLogi runs is refused there"
            ),
            SlotIntent::Refused(RefusedSlot::HookBlind) => tracing::info!(
                ?button,
                "this platform's hook cannot see agent buttons — the slot keeps its flash binding"
            ),
            SlotIntent::Keep | SlotIntent::Native(_) | SlotIntent::Agent => {}
        }
    }
}

#[cfg(test)]
mod tests;
