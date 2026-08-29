//! Per-device RGB keyboard lighting settings.

use openlogi_core::config::Lighting;
use openlogi_core::device_order::PhysicalDeviceKey;
use openlogi_core::hid::{LightingEffect, OnboardLed, OnboardLedMode};
use tracing::debug;

use super::AppState;
use super::load::{LightingLoad, Load};

impl AppState {
    /// The lighting config for the active device, or the onboard LED when none
    /// is stored, or the default when neither exists.
    #[must_use]
    pub fn lighting(&self) -> Lighting {
        self.current_record()
            .and_then(|record| {
                let key = record.persistent_config_key()?;
                self.lighting_for(key, &record.route_key)
            })
            .or_else(|| self.onboard_led().map(lighting_from_onboard))
            .unwrap_or_default()
    }

    /// Cached lighting catalog for the selected device.
    #[must_use]
    pub fn lighting_info(&self) -> LightingLoad {
        self.current_record().map_or(Load::Unknown, |record| {
            self.pointer.reads.lighting_status(&record.device_key())
        })
    }

    pub(super) fn load_current_lighting_info(&mut self, cx: &mut gpui::Context<Self>) {
        let Some((key, route)) = self.current_record().and_then(|record| {
            record
                .capabilities
                .as_ref()
                .is_some_and(|capabilities| capabilities.lighting)
                .then(|| {
                    record
                        .route
                        .clone()
                        .map(|route| (record.device_key(), route))
                })
                .flatten()
        }) else {
            return;
        };
        self.pointer
            .reads
            .ensure_lighting(key, route, self.ipc_sender(), cx);
    }

    /// Onboard LED records last read from firmware for the selected device.
    #[must_use]
    pub fn onboard_leds(&self) -> &[OnboardLed] {
        self.current_record()
            .and_then(|record| self.onboard_leds.get(&record.device_key()))
            .map_or(&[], Vec::as_slice)
    }

    /// The first onboard LED for the selected device, if firmware was read.
    #[must_use]
    pub fn onboard_led(&self) -> Option<OnboardLed> {
        self.onboard_leds().first().copied()
    }
    /// The stored lighting config for `key` on `route_key`, or `None` when
    /// unset (or overridden to unset on that link).
    #[must_use]
    pub fn lighting_for(&self, key: &str, route_key: &str) -> Option<Lighting> {
        if PhysicalDeviceKey::is_transient(key)
            || self
                .devices
                .records
                .iter()
                .any(|record| record.config_key == key && !record.is_persistent())
        {
            return None;
        }
        self.config
            .devices
            .get(key)
            .and_then(|device| device.effective_lighting(route_key))
            .cloned()
    }
    /// Persist a new lighting config for the active device and push it to the
    /// hardware (best-effort). No-op when no device is selected.
    pub fn commit_lighting(&mut self, lighting: Lighting) {
        let Some(record) = self.current_record() else {
            debug!("no active device — lighting change ignored");
            return;
        };
        let key = record.persistent_config_key().map(str::to_string);
        let target = record.route.clone();
        if let Some(key) = key {
            self.config
                .edit(|config| config.set_lighting(&key, lighting.clone()));
            // Lighting is pushed over `SetLighting`; a full agent reload would
            // rebuild hook maps and re-apply every volatile setting on each
            // slider tick.
            if !self.persist_config("lighting") {
                return;
            }
        } else {
            debug!("transient device lighting applied without persistence");
        }
        if let Some(route) = target {
            self.send_ipc(crate::services::ipc::Command::SetLighting(route, lighting));
        }
    }
}

fn lighting_from_onboard(led: OnboardLed) -> Lighting {
    Lighting {
        enabled: led.mode != OnboardLedMode::Off,
        color: led.color,
        brightness: if led.brightness == 0 {
            100
        } else {
            led.brightness
        },
        effect: LightingEffect::from_onboard(led.mode).unwrap_or_default(),
        ..Lighting::default()
    }
}
