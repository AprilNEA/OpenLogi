//! What the online standalone lights report about themselves.
//!
//! A light reached over HID++ `0x1990` is watched by
//! [`crate::watchers::light`]. Its state is published for the GUI, and a
//! value changed on the light itself — its buttons or another program —
//! stands in for the saved value until the GUI saves it. Logi Options+ makes
//! such a change the new setting, so a reconnect or an unrelated save must
//! not undo it.

use openlogi_core::config::{Config, LightSettings};
use openlogi_core::device::{ChangedOnLight, LightState};
use openlogi_hid::{DeviceRoute, LightChange, LightCommand, LightControls, litra_model_for_route};
use openlogi_ipc::ObservedLight;
use tracing::{debug, info};

use super::Orchestrator;
use crate::watchers::light::WatchedLight;

impl Orchestrator {
    /// The online lights to watch: those with light controls on a Litra
    /// route.
    pub(super) fn watched_lights(&self) -> Vec<WatchedLight> {
        self.online_lights()
            .filter_map(|(_, route, _)| {
                Some(WatchedLight {
                    route: route.clone(),
                    model: litra_model_for_route(route)?,
                })
            })
            .collect()
    }

    /// Record what a watched light read as its watch began.
    pub fn light_read(&mut self, route: &DeviceRoute, state: LightState) {
        let Some(key) = self.online_light_key(route) else {
            return;
        };
        self.light_states.insert(key, (route.clone(), state));
        self.publish_lights();
    }

    /// Record a watched light's new state: one it announced, or one an
    /// OpenLogi write read back. A control changed on the light itself stands
    /// in for its saved value. The power of a
    /// camera-linked light is kept as a manual choice instead, as the GUI's
    /// power switch keeps it, so the next camera transition still applies.
    pub fn light_changed(&mut self, route: &DeviceRoute, change: LightChange) {
        let Some(key) = self.online_light_key(route) else {
            return;
        };
        let LightChange { state, on_light } = change;
        self.light_states
            .insert(key.clone(), (route.clone(), state));
        let camera_linked = self
            .config
            .light(&key)
            .is_some_and(|light| light.auto_camera);
        let changed = self.lights_changed_on_light.entry(key.clone()).or_default();
        if on_light.power {
            if camera_linked {
                self.manual_light_overrides.insert(key.clone(), state.on);
            } else {
                changed.enabled = Some(state.on);
            }
        }
        if on_light.brightness {
            let percent = state
                .brightness
                .zip(state.capabilities.brightness)
                .and_then(|(native, range)| range.percent_for_native(native));
            if let Some(percent) = percent {
                changed.brightness_percent = Some(percent);
            }
        }
        if on_light.temperature && state.temperature_kelvin.is_some() {
            changed.temperature_kelvin = state.temperature_kelvin;
        }
        let changed = *changed;
        if changed.is_empty() {
            self.lights_changed_on_light.remove(&key);
        }
        if on_light == LightControls::default() {
            debug!(device = %key, ?state, "a light reads back a new state");
        } else {
            info!(device = %key, ?on_light, ?changed, "a light was changed on the light itself");
        }
        self.publish_lights();
    }

    /// Forget the change made on the light for the control a GUI write just
    /// set: the light now holds what the user chose in OpenLogi.
    pub fn light_written(&mut self, route: &DeviceRoute, command: LightCommand) {
        let Some(key) = self.online_light_key(route) else {
            return;
        };
        let Some(changed) = self.lights_changed_on_light.get_mut(&key) else {
            return;
        };
        match command {
            LightCommand::Power(_) => changed.enabled = None,
            LightCommand::BrightnessPercent(_) | LightCommand::BrightnessNative(_) => {
                changed.brightness_percent = None;
            }
            LightCommand::TemperatureKelvin(_) => changed.temperature_kelvin = None,
        }
        if changed.is_empty() {
            self.lights_changed_on_light.remove(&key);
        }
        self.publish_lights();
    }

    /// Drop the changes made on a light that the new config saved or
    /// replaced. A value the save changed is the user's newer choice, and a
    /// value the config now holds needs no stand-in.
    pub(super) fn forget_saved_light_changes(&mut self, previous: &Config) {
        let config = &self.config;
        self.lights_changed_on_light.retain(|key, changed| {
            let (before, after) = (previous.light(key), config.light(key));
            forget_saved(
                &mut changed.enabled,
                before.map(|light| light.enabled),
                after.map(|light| light.enabled),
            );
            forget_saved(
                &mut changed.brightness_percent,
                before.map(|light| light.brightness_percent),
                after.map(|light| light.brightness_percent),
            );
            forget_saved(
                &mut changed.temperature_kelvin,
                before.and_then(|light| light.temperature_kelvin),
                after.and_then(|light| light.temperature_kelvin),
            );
            !changed.is_empty()
        });
    }

    /// Publish what each online light last read back, with the changes made
    /// on it. A light read on a route it no longer has is left out until its
    /// new watch reads it.
    pub(super) fn publish_lights(&self) {
        let lights = self
            .online_lights()
            .filter_map(|(dev, route, _)| {
                let (read_on, state) = self.light_states.get(&dev.config_key)?;
                (read_on == route).then(|| ObservedLight {
                    route: route.clone(),
                    state: *state,
                    changed_on_light: self
                        .lights_changed_on_light
                        .get(&dev.config_key)
                        .copied()
                        .unwrap_or_default(),
                })
            })
            .collect();
        self.observable.set_lights(lights);
    }

    /// The config key of the online light on `route`.
    fn online_light_key(&self, route: &DeviceRoute) -> Option<String> {
        self.online_lights()
            .find(|(_, light_route, _)| *light_route == route)
            .map(|(dev, _, _)| dev.config_key.clone())
    }
}

/// Put the values changed on the light in place of the saved ones.
pub(super) fn apply_changes_on_light(light: &mut LightSettings, changed: ChangedOnLight) {
    if let Some(enabled) = changed.enabled {
        light.enabled = enabled;
    }
    if let Some(percent) = changed.brightness_percent {
        light.brightness_percent = percent;
    }
    if let Some(kelvin) = changed.temperature_kelvin {
        light.temperature_kelvin = Some(kelvin);
    }
}

/// Forget one value changed on a light once the saved value changed or
/// equals it.
fn forget_saved<T: Copy + PartialEq>(changed: &mut Option<T>, before: Option<T>, after: Option<T>) {
    let adopted = changed.is_some() && *changed == after;
    if before != after || adopted {
        *changed = None;
    }
}
