//! What the online standalone lights report about themselves.
//!
//! A light reached over HID++ `0x1990` is watched by
//! [`crate::watchers::light`]. Its state is published for the GUI, and a
//! value changed on the light itself — its buttons or another program —
//! stands in for the saved value until the GUI saves it. Logi Options+ makes
//! such a change the new setting, so a reconnect or an unrelated save must
//! not undo it.

use std::collections::BTreeSet;

use openlogi_core::config::Config;
use openlogi_core::device::{ChangedOnLight, LightState};
use openlogi_core::hid::WriteError;
use openlogi_hid::{DeviceRoute, LightChange, LightCommand, LightControls, litra_model_for_route};
use openlogi_ipc::ObservedLight;
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

/// The values a light reported as changed on it for each control, since the
/// change of that control began.
#[derive(Debug, Default)]
pub(super) struct ReportedOnLight {
    enabled: BTreeSet<bool>,
    brightness_percent: BTreeSet<u8>,
    temperature_kelvin: BTreeSet<u16>,
}

impl ReportedOnLight {
    /// Keep the reports in step with `changed`: forget the controls it no
    /// longer holds, and note each value it holds now.
    fn follow(&mut self, changed: ChangedOnLight) {
        follow(&mut self.enabled, changed.enabled);
        follow(&mut self.brightness_percent, changed.brightness_percent);
        follow(&mut self.temperature_kelvin, changed.temperature_kelvin);
    }
}

fn follow<T: Ord>(reported: &mut BTreeSet<T>, changed: Option<T>) {
    match changed {
        Some(value) => {
            reported.insert(value);
        }
        None => reported.clear(),
    }
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
        self.sync_reported(&key);
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
        self.sync_reported(&key);
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
        let changed = self.lights_changed_on_light.entry(key.clone()).or_default();
        changed.enabled = changed.enabled.or(taken.enabled);
        changed.brightness_percent = changed.brightness_percent.or(taken.brightness_percent);
        changed.temperature_kelvin = changed.temperature_kelvin.or(taken.temperature_kelvin);
        self.sync_reported(&key);
        self.publish_lights();
    }

    /// Drop the changes made on a light that the new config saved or
    /// replaced. A value the config now holds needs no stand-in, and any
    /// other newly saved value is the user's newer choice. A save of a value
    /// the light reported earlier is the GUI acknowledging that report, so
    /// a change the light made since then stands.
    pub(super) fn forget_saved_light_changes(&mut self, previous: &Config) {
        let config = &self.config;
        let reported = &self.lights_reported_on_light;
        self.lights_changed_on_light.retain(|key, changed| {
            let (before, after) = (previous.light(key), config.light(key));
            let reported = reported.get(key);
            forget_saved(
                &mut changed.enabled,
                reported.map(|reported| &reported.enabled),
                before.map(|light| light.enabled),
                after.map(|light| light.enabled),
            );
            forget_saved(
                &mut changed.brightness_percent,
                reported.map(|reported| &reported.brightness_percent),
                before.map(|light| light.brightness_percent),
                after.map(|light| light.brightness_percent),
            );
            forget_saved(
                &mut changed.temperature_kelvin,
                reported.map(|reported| &reported.temperature_kelvin),
                before.and_then(|light| light.temperature_kelvin),
                after.and_then(|light| light.temperature_kelvin),
            );
            !changed.is_empty()
        });
        let changed = &self.lights_changed_on_light;
        self.lights_reported_on_light
            .retain(|key, reported| match changed.get(key) {
                Some(changed) => {
                    reported.follow(*changed);
                    true
                }
                None => false,
            });
    }

    /// Keep the reported values of the light `key` in step with the change
    /// it holds now.
    fn sync_reported(&mut self, key: &str) {
        match self.lights_changed_on_light.get(key) {
            Some(changed) => self
                .lights_reported_on_light
                .entry(key.to_owned())
                .or_default()
                .follow(*changed),
            None => {
                self.lights_reported_on_light.remove(key);
            }
        }
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

/// Forget one value changed on a light once the config holds it, or once a
/// save replaced it with a value the light did not report earlier.
fn forget_saved<T: Copy + Ord>(
    changed: &mut Option<T>,
    reported: Option<&BTreeSet<T>>,
    before: Option<T>,
    after: Option<T>,
) {
    let Some(value) = *changed else {
        return;
    };
    let adopted = after == Some(value);
    let acknowledged_earlier =
        after.is_some_and(|saved| reported.is_some_and(|reported| reported.contains(&saved)));
    if adopted || (before != after && !acknowledged_earlier) {
        *changed = None;
    }
}
