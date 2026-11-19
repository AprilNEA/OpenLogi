//! Copying settings onto a device from another device or from a file, and
//! exporting a device's settings to a file. The rules of a transfer live in
//! `openlogi_core::config::transfer`; this module picks the devices,
//! previews, and saves.

use gpui::App;
use openlogi_core::config::{
    Config, ImportedDevice, SettingsTransfer, TransferItem, TransferTarget,
};
use openlogi_core::hid::SmartShiftStatus;

use super::devices::DeviceRecord;
use super::events::StateEvents;
use super::pointer::DpiWrite;
use super::scroll::scroll_inversion_available;
use super::{AppState, StateEvent};

/// Settings a device can take: another device's, or a device's read from a
/// file.
#[derive(Debug, Clone, PartialEq)]
pub struct TransferSource {
    /// The name the source is shown under.
    pub name: String,
    origin: Origin,
}

#[derive(Debug, Clone, PartialEq)]
enum Origin {
    /// A device in this config, by key. Read when the transfer runs.
    Device(String),
    /// A device read from a file.
    File(Box<ImportedDevice>),
}

impl AppState {
    /// The devices of the card `record_key`'s kind that have settings of
    /// their own, offline ones included, in device-list order.
    #[must_use]
    pub fn transfer_sources(&self, record_key: &str) -> Vec<TransferSource> {
        let Some((target, target_key)) = self.transfer_record(record_key) else {
            return Vec::new();
        };
        self.devices()
            .iter()
            .filter(|record| record.kind == target.kind)
            .filter_map(|record| {
                let key = record.persistent_config_key()?;
                (key != target_key && self.config.devices.contains_key(key)).then(|| {
                    TransferSource {
                        name: record.display_name.clone(),
                        origin: Origin::Device(key.to_owned()),
                    }
                })
            })
            .collect()
    }

    /// The devices in `file` of the card `record_key`'s kind, named the way
    /// the file names them.
    #[must_use]
    pub fn file_transfer_sources(&self, record_key: &str, file: &Config) -> Vec<TransferSource> {
        let Some((target, _)) = self.transfer_record(record_key) else {
            return Vec::new();
        };
        file.devices
            .iter()
            .filter(|(_, device)| {
                device
                    .identity
                    .as_ref()
                    .is_none_or(|identity| identity.kind == target.kind)
            })
            .filter_map(|(key, device)| {
                Some(TransferSource {
                    name: device
                        .custom_name
                        .clone()
                        .or_else(|| device.identity.as_ref().map(|i| i.display_name.clone()))
                        .unwrap_or_else(|| key.clone()),
                    origin: Origin::File(Box::new(file.imported_device(key)?)),
                })
            })
            .collect()
    }

    /// What copying `source` onto the card `record_key`'s device would
    /// change and leave out. `None` when either side is gone.
    #[must_use]
    pub fn settings_transfer_preview(
        &self,
        record_key: &str,
        source: &TransferSource,
    ) -> Option<SettingsTransfer> {
        let (target, target_key) = self.transfer_record(record_key)?;
        let facts = TransferTarget {
            kind: Some(target.kind),
            capabilities: target.capabilities,
            invertible_wheel: scroll_inversion_available(target),
        };
        match &source.origin {
            Origin::Device(key) => self.config.settings_transfer(key, target_key, facts),
            Origin::File(device) => Some(
                self.config
                    .settings_transfer_from_file(device, target_key, facts),
            ),
        }
    }

    /// Copy `source` onto the card `record_key`'s device and save. The
    /// transfer is computed again here, so what is saved matches the config
    /// as it is now, not as it was when the preview opened.
    ///
    /// A reload applies most settings to a connected device but not its DPI
    /// or SmartShift, so those are written to it here, the way their panels
    /// write them. Until the device takes the DPI, which is snapped to the
    /// values it was read to support, the file keeps the target's own DPI
    /// and its per-link overrides; taking it saves it on every link. An
    /// offline device gets both from the agent when it connects.
    pub fn commit_settings_transfer(
        &mut self,
        record_key: &str,
        source: &TransferSource,
    ) -> StateEvents {
        let Some(mut transfer) = self.settings_transfer_preview(record_key, source) else {
            return StateEvents::none();
        };
        if transfer.is_empty() {
            return StateEvents::none();
        }
        let Some((target, target_key)) = self.transfer_record(record_key) else {
            return StateEvents::none();
        };
        let device = target.device_key();
        let route = target.addressable_route().cloned();
        let target_key = target_key.to_owned();
        let copied = |item: TransferItem| transfer.changed.contains(&item);
        let dpi = transfer.settings.dpi.filter(|_| copied(TransferItem::Dpi));
        let smartshift = transfer
            .settings
            .smartshift
            .filter(|_| copied(TransferItem::SmartShift));
        if route.is_some() && dpi.is_some() {
            let before = self.config.devices.get(&target_key);
            transfer.settings.dpi = before.and_then(|settings| settings.dpi);
            for (route_key, link) in &mut transfer.settings.links {
                link.overrides.dpi = before
                    .and_then(|settings| settings.links.get(route_key))
                    .and_then(|link| link.overrides.dpi);
            }
        }
        self.config
            .edit(|config| config.apply_settings_transfer(&target_key, transfer));
        let saved = self.persist_and_reload("settings copied onto a device");
        self.restore_config_projections();
        if saved && let Some(route) = route {
            if let Some(dpi) = dpi {
                let dpi = self
                    .dpi_capabilities_for(&device)
                    .map_or(dpi, |supported| supported.nearest(dpi));
                self.write_dpi(device.clone(), route.clone(), dpi, DpiWrite::Copied);
            }
            if let Some(smartshift) = smartshift {
                self.write_smartshift(
                    device.clone(),
                    Some(route),
                    SmartShiftStatus::from(smartshift),
                );
            }
        }
        StateEvents::from(StateEvent::SettingsChanged)
            .and(StateEvent::DeviceConfigChanged(device.clone()))
            .and(StateEvent::BindingsChanged(device.clone()))
            .and(StateEvent::DpiChanged(device.clone()))
            .and(StateEvent::SmartShiftChanged(device))
    }

    /// Copy `source` onto the card `record_key`'s device
    /// ([`Self::commit_settings_transfer`]) and start the read that confirms
    /// a SmartShift written to it. Not an [`AppState::apply`] call because
    /// the confirming read is a query subscription, which only the entity's
    /// own context can open.
    pub(crate) fn copy_settings(cx: &mut App, record_key: &str, source: &TransferSource) {
        Self::update(cx, |state, cx| {
            let events = state.commit_settings_transfer(record_key, source);
            let target = state.transfer_record(record_key).and_then(|(record, _)| {
                Some((record.device_key(), record.addressable_route()?.clone()))
            });
            if let Some((key, route)) = target {
                state.confirm_smartshift(&key, route, cx);
            }
            events.emit(cx);
        });
    }

    /// The card `record_key`'s device settings as a config file of their
    /// own. `None` when the device has no settings yet.
    #[must_use]
    pub fn device_export(&self, record_key: &str) -> Option<Config> {
        let (_, key) = self.transfer_record(record_key)?;
        self.config.device_export(key)
    }

    /// The card `record_key`'s record and config key, for a device whose
    /// settings persist.
    fn transfer_record(&self, record_key: &str) -> Option<(&DeviceRecord, &str)> {
        let record = self
            .devices()
            .iter()
            .find(|record| record.record_key() == record_key)?;
        Some((record, record.persistent_config_key()?))
    }
}
