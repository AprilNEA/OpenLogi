//! What the online standalone lights report about themselves.
//!
//! A light reached over HID++ `0x1990` is watched by
//! [`crate::watchers::light`]. Its state is published for the GUI, and a
//! value changed on the light itself — its buttons or another program —
//! stands in for the saved value until the GUI saves it. Logi Options+ makes
//! such a change the new setting, so a reconnect or an unrelated save must
//! not undo it.

use openlogi_core::config::Config;
use openlogi_core::device::{ChangedOnLight, LightState};
use openlogi_core::hid::WriteError;
use openlogi_hid::{DeviceRoute, LightChange, LightCommand, LightControls, litra_model_for_route};
use openlogi_ipc::{ChosenLight, ObservedLight};
use tokio::sync::Mutex;
use tracing::{debug, info};

use super::Orchestrator;
use crate::watchers::light::WatchedLight;

/// The change made on a light that [`Orchestrator::light_writing`] took for a
/// GUI write. It keeps the light's config key, so a failed write puts the
/// change back even when the light went offline during the write.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TakenLightChange {
    /// Config key of the light the change was taken from.
    pub key: String,
    /// The values taken.
    pub changed: ChangedOnLight,
}

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

    /// Take the change made on the light for the control a GUI write is about
    /// to set: from here on the light is given the user's choice in OpenLogi,
    /// so no state published during the write still offers the older value as
    /// the setting. A change the light announces after this is newer than the
    /// write and stands. Returns what was taken, if anything, for
    /// [`Self::light_write_failed`].
    pub fn light_writing(
        &mut self,
        route: &DeviceRoute,
        command: LightCommand,
    ) -> Option<TakenLightChange> {
        let key = self.online_light_key(route)?;
        let changed = self.lights_changed_on_light.get_mut(&key)?;
        let mut taken = ChangedOnLight::default();
        match command {
            LightCommand::Power(_) => taken.enabled = changed.enabled.take(),
            LightCommand::BrightnessPercent(_) | LightCommand::BrightnessNative(_) => {
                taken.brightness_percent = changed.brightness_percent.take();
            }
            LightCommand::TemperatureKelvin(_) => {
                taken.temperature_kelvin = changed.temperature_kelvin.take();
            }
        }
        if changed.is_empty() {
            self.lights_changed_on_light.remove(&key);
        }
        if taken.is_empty() {
            return None;
        }
        self.publish_lights();
        Some(TakenLightChange {
            key,
            changed: taken,
        })
    }

    /// Put back what [`Self::light_writing`] took once the write failed: the
    /// light may still hold the value changed on it. That holds as well for a
    /// light that went offline during the write, so its reconnect writes the
    /// change back too. A newer change the light announced during the write
    /// is kept instead.
    pub fn light_write_failed(&mut self, taken: TakenLightChange) {
        let TakenLightChange {
            key,
            changed: taken,
        } = taken;
        if taken.is_empty() {
            return;
        }
        let changed = self.lights_changed_on_light.entry(key).or_default();
        changed.enabled = changed.enabled.or(taken.enabled);
        changed.brightness_percent = changed.brightness_percent.or(taken.brightness_percent);
        changed.temperature_kelvin = changed.temperature_kelvin.or(taken.temperature_kelvin);
        self.publish_lights();
    }

    /// Drop the changes made on a light that the new config holds or
    /// replaced. A value the config now holds needs no stand-in. Whether
    /// another saved value replaces a change depends on `source`.
    pub(super) fn forget_saved_light_changes(
        &mut self,
        previous: &Config,
        source: ConfigSource<'_>,
    ) {
        let config = &self.config;
        self.lights_changed_on_light.retain(|key, changed| {
            let (before, after) = (previous.light(key), config.light(key));
            let replaced =
                |differs: bool, removed: bool, chose: fn(&ChosenLight) -> bool| match source {
                    ConfigSource::Loaded => differs,
                    ConfigSource::Saved { chosen } => {
                        removed
                            || chosen
                                .iter()
                                .any(|light| light.device == *key && chose(light))
                    }
                };
            let (enabled_before, enabled_after) = (
                before.map(|light| light.enabled),
                after.map(|light| light.enabled),
            );
            forget_saved(
                &mut changed.enabled,
                enabled_after,
                replaced(
                    enabled_before != enabled_after,
                    removed(enabled_before, enabled_after),
                    |light| light.power,
                ),
            );
            let (brightness_before, brightness_after) = (
                before.map(|light| light.brightness_percent),
                after.map(|light| light.brightness_percent),
            );
            forget_saved(
                &mut changed.brightness_percent,
                brightness_after,
                replaced(
                    brightness_before != brightness_after,
                    removed(brightness_before, brightness_after),
                    |light| light.brightness,
                ),
            );
            let (temperature_before, temperature_after) = (
                before.and_then(|light| light.temperature_kelvin),
                after.and_then(|light| light.temperature_kelvin),
            );
            forget_saved(
                &mut changed.temperature_kelvin,
                temperature_after,
                replaced(
                    temperature_before != temperature_after,
                    removed(temperature_before, temperature_after),
                    |light| light.temperature,
                ),
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

/// Run `write`, a GUI write of `command` to the light on `route`, with the
/// change made on the light for that control taken before it starts and put
/// back if it fails. The GUI saves the light's published changes as its
/// settings and receives the snapshots answered during a write after the
/// write's own result, so a snapshot from inside the write must already show
/// the control as the user's choice.
///
/// # Errors
///
/// Returns the write's error.
pub async fn write_light(
    orchestrator: &Mutex<Orchestrator>,
    route: &DeviceRoute,
    command: LightCommand,
    write: impl Future<Output = Result<(), WriteError>>,
) -> Result<(), WriteError> {
    let taken = orchestrator.lock().await.light_writing(route, command);
    let written = write.await;
    if let (Err(_), Some(taken)) = (&written, taken) {
        orchestrator.lock().await.light_write_failed(taken);
    }
    written
}

/// Where the config a reload loads comes from, which decides whether a
/// saved value replaces a change made on a light.
#[derive(Debug, Clone, Copy)]
pub enum ConfigSource<'a> {
    /// A file the agent did not see saved: at startup, or after a hand edit
    /// or a CLI edit. A saved value that differs from the one before is a
    /// new choice and replaces the change.
    Loaded,
    /// A save of the GUI. The GUI writes a light it changes, and the write
    /// takes the change first, so a saved value that differs is the GUI's
    /// copy of an earlier report and the change stands. Only the controls
    /// in `chosen`, which the user set without a write, give way, and the
    /// values the save removed: a copy of a report always holds a value.
    Saved {
        /// The controls the user set without a write, by light.
        chosen: &'a [ChosenLight],
    },
}

/// Whether a reload removed a saved value.
fn removed<T: Copy>(before: Option<T>, after: Option<T>) -> bool {
    before.is_some() && after.is_none()
}

/// Forget one value changed on a light once the config holds it, or once
/// the saved value replaced it.
fn forget_saved<T: Copy + PartialEq>(changed: &mut Option<T>, after: Option<T>, replaced: bool) {
    let Some(value) = *changed else {
        return;
    };
    if after == Some(value) || replaced {
        *changed = None;
    }
}
