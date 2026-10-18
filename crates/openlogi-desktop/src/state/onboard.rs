//! A gaming mouse's onboard memory: where it takes its settings from, and
//! the profile stored on it.
//!
//! The *choice* is `config.toml`'s per-device `memory_mode`, which the agent
//! enforces on every connect: the firmware falls back to onboard mode by
//! itself after a power cycle, so a software choice has to be re-asserted.
//! The *reading* is the swr-backed device query of the mouse's live mode and
//! its active profile; the profile lives only on the mouse, so a save goes
//! straight to it and its read-back replaces the reading.

use openlogi_core::config::Lighting;
use openlogi_core::hid::{
    DeviceRoute, Dpi, MemoryMode, OnboardLighting, OnboardMemory, OnboardProfile,
    OnboardProfileChange, ReportRate, WriteError,
};
use tracing::{debug, warn};

use super::device_key::DeviceKey;
use super::events::StateEvents;
use super::load::OnboardLoad;
use super::{AppState, StateEvent};
use crate::services::ipc::OnboardWrite;

/// What OpenLogi applies to a mouse in software mode — its saved settings,
/// each `None` while never set, which leaves the mouse's own value alone.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SoftwareSettings {
    pub dpi: Option<Dpi>,
    pub report_rate: Option<ReportRate>,
    pub lighting: Option<Lighting>,
}

impl AppState {
    /// Whether the active device keeps profiles in its own memory (HID++
    /// `0x8100`).
    #[must_use]
    pub fn current_onboard_supported(&self) -> bool {
        self.current_record()
            .and_then(|record| record.capabilities)
            .is_some_and(|capabilities| capabilities.onboard_profiles)
    }

    /// Where the user chose the active mouse takes its settings from.
    #[must_use]
    pub fn current_memory_mode(&self) -> MemoryMode {
        self.current_record()
            .and_then(|record| record.persistent_config_key())
            .map(|key| self.config.memory_mode(key))
            .unwrap_or_default()
    }

    /// Whether the active mouse runs from its onboard memory, so the live
    /// DPI, report-rate and lighting controls would only be overwritten by
    /// its profile: those settings are edited in the stored profile instead.
    #[must_use]
    pub fn current_settings_on_mouse(&self) -> bool {
        self.current_onboard_supported() && self.current_memory_mode() == MemoryMode::Onboard
    }

    /// Whether OpenLogi's DPI, report rate and lighting reach the active
    /// device as they change. False while a mouse runs from its onboard
    /// memory: those values are then only saved, for when it is switched to
    /// OpenLogi — a live write would only cover the stored profile until its
    /// next power cycle, as a temporary LED effect does.
    #[must_use]
    pub fn applies_live(&self) -> bool {
        !self.current_settings_on_mouse()
    }

    /// What OpenLogi applies to the active mouse in software mode.
    #[must_use]
    pub(crate) fn current_software_settings(&self) -> SoftwareSettings {
        let record = self.current_record();
        let device = record
            .and_then(|record| record.persistent_config_key())
            .and_then(|key| self.config.devices.get(key));
        let route_key = record.map_or("", |record| record.route_key.as_str());
        SoftwareSettings {
            dpi: device.and_then(|d| d.effective_dpi(route_key)),
            report_rate: device.and_then(|d| d.report_rate),
            lighting: device
                .and_then(|d| d.effective_lighting(route_key))
                .cloned(),
        }
    }

    /// What is known of the active mouse's onboard memory.
    #[must_use]
    pub fn current_onboard_load(&self) -> OnboardLoad {
        self.current_record()
            .and_then(|record| self.pointer.reads.onboard_load(&record.device_key()))
            .cloned()
            .unwrap_or_default()
    }

    /// Whether a mode switch or save to the active mouse awaits its answer.
    #[must_use]
    pub fn current_onboard_writing(&self) -> bool {
        self.current_record()
            .is_some_and(|record| self.pointer.reads.onboard_writing(&record.device_key()))
    }

    /// Why the newest mode switch or save to the active mouse did not land.
    #[must_use]
    pub fn current_onboard_refusal(&self) -> Option<WriteError> {
        self.current_record()
            .and_then(|record| self.pointer.reads.onboard_refusal(&record.device_key()))
            .cloned()
    }

    /// Persist `mode` for the active mouse and switch it now. The agent
    /// re-applies the saved software settings on the reload this save
    /// triggers, and keeps the mouse in `mode` across reconnects.
    pub fn commit_memory_mode(&mut self, mode: MemoryMode) -> StateEvents {
        let mut events = self.for_current_device(StateEvent::OnboardChanged);
        if !self.current_onboard_supported() {
            return events;
        }
        let Some(record) = self.current_record() else {
            return events;
        };
        let device_key = record.device_key();
        let route = record.route.clone();
        // OpenLogi starts from what the mouse runs: a setting it never saved
        // takes the stored profile's value, so switching changes nothing the
        // user can see until they change it.
        let seed = (mode == MemoryMode::Software)
            .then(|| self.current_onboard_load())
            .and_then(|load| match load {
                OnboardLoad::Ready(memory) => memory.profile.clone(),
                _ => None,
            })
            .map(|profile| software_seed(&profile));
        if let Some(persistent_key) = record.persistent_config_key().map(str::to_string) {
            self.config.edit(|config| {
                config.set_memory_mode(&persistent_key, mode);
                if let Some(seed) = seed {
                    let device = config.devices.entry(persistent_key.clone()).or_default();
                    device.dpi = device.dpi.or(seed.dpi);
                    device.report_rate = device.report_rate.or(seed.report_rate);
                    if device.lighting.is_none() {
                        device.lighting = seed.lighting;
                    }
                }
            });
            if !self.persist_and_reload("memory mode") {
                return events;
            }
            // The pointer and lighting tabs change what they offer with it.
            events = events
                .and(StateEvent::DeviceConfigChanged(device_key.clone()))
                .and(StateEvent::LightingChanged(device_key.clone()));
        }
        let Some(route) = route else {
            return events;
        };
        if self.pointer.reads.begin_onboard_write(&device_key, &route) {
            self.send_ipc(crate::services::ipc::SetMemoryMode {
                route,
                mode,
                key: device_key,
            });
        }
        events
    }

    /// Save `change` into the profile the active mouse runs from its memory.
    pub fn commit_onboard_profile(&mut self, change: OnboardProfileChange) -> StateEvents {
        let events = self.for_current_device(StateEvent::OnboardChanged);
        let Some((key, route)) = self
            .current_record()
            .filter(|_| self.current_onboard_supported())
            .and_then(|record| Some((record.device_key(), record.route.clone()?)))
        else {
            return events;
        };
        if self.pointer.reads.begin_onboard_write(&key, &route) {
            self.send_ipc(crate::services::ipc::WriteOnboardProfile { route, change, key });
        }
        events
    }

    /// The mouse's answer to a mode switch or save: show its memory as read
    /// back, or keep the refusal for the panel and re-read the mouse.
    pub fn apply_onboard_written(
        &mut self,
        key: &DeviceKey,
        route: &DeviceRoute,
        write: OnboardWrite,
        result: Result<OnboardMemory, WriteError>,
    ) -> StateEvents {
        if let Err(error) = &result {
            warn!(%error, key = %key, ?write, "onboard write did not land");
        }
        if !self.pointer.reads.finish_onboard_write(key, route, result) {
            debug!(key = %key, "onboard answer for a connection no read is on — dropping it");
            return StateEvents::none();
        }
        StateEvent::OnboardChanged(key.clone()).into()
    }

    /// Re-run `key`'s exhausted onboard read.
    pub(crate) fn retry_onboard_read(&mut self, key: &DeviceKey) -> StateEvents {
        self.pointer.reads.retry_onboard(key);
        StateEvent::OnboardChanged(key.clone()).into()
    }

    pub(super) fn load_current_onboard(&mut self, cx: &mut gpui::Context<Self>) {
        let Some((key, route)) = self
            .current_record()
            .filter(|record| {
                record
                    .capabilities
                    .is_some_and(|capabilities| capabilities.onboard_profiles)
            })
            .and_then(|record| Some((record.device_key(), record.route.clone()?)))
        else {
            return;
        };
        self.pointer
            .reads
            .ensure_onboard(key, route, self.ipc_sender(), cx);
    }
}

/// The software settings that reproduce what `profile` runs: its default
/// DPI stage, its report rate, and its first LED zone where OpenLogi's single
/// lighting setting can express it — off, or one steady color.
fn software_seed(profile: &OnboardProfile) -> SoftwareSettings {
    let dpi = profile
        .dpi_slots
        .get(usize::from(profile.default_dpi_slot))
        .copied()
        .flatten();
    let lighting = match profile.lighting.first() {
        Some(OnboardLighting::Off) => Some(Lighting {
            enabled: false,
            ..Lighting::default()
        }),
        Some(OnboardLighting::Fixed(color)) => Some(Lighting {
            enabled: true,
            color: *color,
            brightness: 100,
        }),
        _ => None,
    };
    SoftwareSettings {
        dpi,
        report_rate: profile.report_rate,
        lighting,
    }
}

#[cfg(test)]
mod tests {
    use openlogi_core::color::Rgb;
    use openlogi_core::config::Lighting;
    use openlogi_core::hid::{Dpi, OnboardLighting, OnboardProfile, ReportRate};

    use super::software_seed;

    fn profile(lighting: OnboardLighting) -> OnboardProfile {
        OnboardProfile {
            number: 1,
            name: None,
            factory: false,
            report_rate: Some(ReportRate::new(1000)),
            dpi_slots: vec![Some(Dpi::new(400)), Some(Dpi::new(1600)), None],
            default_dpi_slot: 1,
            lighting: vec![lighting, OnboardLighting::Off],
        }
    }

    #[test]
    fn software_mode_starts_from_what_the_mouse_runs() {
        let seed = software_seed(&profile(OnboardLighting::Off));
        assert_eq!(seed.dpi, Some(Dpi::new(1600)));
        assert_eq!(seed.report_rate, Some(ReportRate::new(1000)));
        assert_eq!(seed.lighting.map(|lighting| lighting.enabled), Some(false));

        let red = Rgb::new(0xff, 0, 0);
        let seed = software_seed(&profile(OnboardLighting::Fixed(red)));
        assert_eq!(
            seed.lighting,
            Some(Lighting {
                enabled: true,
                color: red,
                brightness: 100,
            })
        );
    }

    #[test]
    fn an_effect_software_lighting_cannot_express_is_not_seeded() {
        let seed = software_seed(&profile(OnboardLighting::Cycle {
            period_ms: 8000,
            brightness: 100,
        }));
        assert_eq!(seed.lighting, None);
    }
}
