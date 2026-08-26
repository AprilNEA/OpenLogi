//! Optimistic standalone-light state and IPC result handling.

use std::collections::BTreeMap;

use openlogi_core::config::LightSettings;
use openlogi_core::hid::{DeviceRoute, LightCommand, WriteError};
use tracing::debug;

use super::AppState;
use super::device_key::DeviceKey;
use super::device_runtime::DeviceRuntimeState;

const fn camera_policy_applies(light: LightSettings) -> bool {
    cfg!(target_os = "macos") && light.auto_camera
}

/// Result state of the latest standalone-light command for one device.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LightCommandStatus {
    /// The command has been queued for the agent.
    Pending,
    /// The device or agent rejected the command.
    Failed(String),
    /// No route is available because the device is offline.
    Offline,
}

/// Process-session state shared by standalone-light devices.
#[derive(Debug, Default)]
pub(super) struct LightingState {
    camera_active: bool,
    next_request_id: u64,
}

/// Runtime state scoped to one standalone light.
#[derive(Debug, Default)]
pub(super) struct LightDeviceState {
    /// Transient manual power choice while camera automation is enabled.
    manual_override: Option<bool>,
    /// Session-only settings for a device without a persistent identity.
    volatile_settings: Option<LightSettings>,
    /// The in-flight optimistic write, if any.
    pending: Option<PendingLightCommand>,
    /// Visible outcome of the latest write for this device.
    status: Option<(u64, LightCommandStatus)>,
}

#[derive(Debug)]
struct PendingLightCommand {
    request_id: u64,
    pending: u16,
    settings: Option<LightSettings>,
    persistent_key: Option<String>,
    rollback_settings: LightSettings,
    previous_volatile: Option<LightSettings>,
    manual_override_rollback: Option<ManualOverrideRollback>,
    successful_commands: Vec<LightCommand>,
    failure: Option<String>,
    superseded: Vec<SupersededLightCommand>,
}

#[derive(Debug)]
struct SupersededLightCommand {
    request_id: u64,
    pending: u16,
    successful_commands: Vec<LightCommand>,
    failure: Option<String>,
}

#[derive(Debug, Clone, Copy)]
struct ManualOverrideRollback {
    previous: Option<bool>,
}

impl AppState {
    /// Effective power state for the selected standalone light.
    #[must_use]
    pub fn light_enabled(&self) -> bool {
        self.current_record()
            .is_some_and(|record| self.light_enabled_for(&record.device_key()))
    }

    /// Effective power state for any standalone-light key.
    #[must_use]
    pub fn light_enabled_for(&self, key: &DeviceKey) -> bool {
        let light = self.light_for(key);
        if camera_policy_applies(light) {
            self.devices
                .runtime
                .get(key)
                .and_then(|entry| entry.light.manual_override)
                .unwrap_or(self.lighting.camera_active)
        } else {
            light.enabled
        }
    }

    /// Update the runtime camera state used by camera-linked light rendering.
    /// A real transition clears every transient manual override.
    pub fn set_camera_active(&mut self, active: bool) -> bool {
        let changed = self.lighting.camera_active != active;
        if changed {
            for entry in self.devices.runtime.values_mut() {
                entry.light.manual_override = None;
            }
        }
        self.lighting.camera_active = active;
        changed
    }

    /// Whether the selected light is currently governed by a supported camera
    /// automation provider. The persisted setting remains portable, while
    /// platforms without a provider retain normal manual power behaviour.
    #[must_use]
    pub fn camera_automation_active(&self) -> bool {
        camera_policy_applies(self.light())
    }

    /// Latest light-command status for the selected device, if one exists.
    #[must_use]
    pub fn light_command_status(&self) -> Option<LightCommandStatus> {
        let key = self.current_record()?.device_key();
        self.devices
            .runtime
            .get(&key)
            .and_then(|entry| entry.light.status.as_ref())
            .map(|(_, status)| status.clone())
    }

    fn begin_light_command(&mut self, key: &DeviceKey, online: bool) -> u64 {
        self.lighting.next_request_id = self.lighting.next_request_id.wrapping_add(1);
        let request_id = self.lighting.next_request_id;
        let entry = self.devices.runtime.entry(key.clone()).or_default();
        if online {
            entry.light.pending = Some(PendingLightCommand {
                request_id,
                pending: 0,
                settings: None,
                persistent_key: None,
                rollback_settings: LightSettings::default(),
                previous_volatile: None,
                manual_override_rollback: None,
                successful_commands: Vec::new(),
                failure: None,
                superseded: Vec::new(),
            });
            entry.light.status = Some((request_id, LightCommandStatus::Pending));
        } else {
            entry.light.status = Some((request_id, LightCommandStatus::Offline));
        }
        request_id
    }

    fn queue_light_command(
        &mut self,
        key: &DeviceKey,
        request_id: u64,
        route: DeviceRoute,
        command: LightCommand,
    ) {
        if let Some(pending) = self
            .devices
            .runtime
            .get_mut(key)
            .and_then(|entry| entry.light.pending.as_mut())
            && pending.request_id == request_id
        {
            pending.pending = pending.pending.saturating_add(1);
        }
        if !self.send_ipc(crate::services::ipc::Command::SetLight(
            route,
            command,
            key.to_string(),
            request_id,
        )) {
            self.apply_light_command_result(
                key.to_string(),
                request_id,
                command,
                Err(WriteError::AgentUnavailable),
            );
        }
    }

    fn supersede_light_command(&mut self, key: &DeviceKey) -> Vec<SupersededLightCommand> {
        let Some(pending) = self
            .devices
            .runtime
            .get_mut(key)
            .and_then(|entry| entry.light.pending.take())
        else {
            return Vec::new();
        };
        let mut superseded = pending.superseded;
        superseded.push(SupersededLightCommand {
            request_id: pending.request_id,
            pending: pending.pending,
            successful_commands: pending.successful_commands,
            failure: pending.failure,
        });
        superseded
    }

    /// Consume an asynchronous light-write result from the IPC client.
    /// Results from an older request are ignored so a slow failed write cannot
    /// overwrite the status of a newer slider release.
    pub fn apply_light_command_result(
        &mut self,
        key: String,
        request_id: u64,
        command: LightCommand,
        result: Result<(), WriteError>,
    ) -> bool {
        let key = DeviceKey::from(key);
        let Some(pending) = self
            .devices
            .runtime
            .get_mut(&key)
            .and_then(|entry| entry.light.pending.as_mut())
        else {
            return false;
        };
        if pending.request_id == request_id {
            pending.pending = pending.pending.saturating_sub(1);
            match result {
                Ok(()) => pending.successful_commands.push(command),
                Err(error) => {
                    pending.failure.get_or_insert_with(|| error.to_string());
                }
            }
        } else if let Some(superseded) = pending
            .superseded
            .iter_mut()
            .find(|superseded| superseded.request_id == request_id)
        {
            superseded.pending = superseded.pending.saturating_sub(1);
            match result {
                Ok(()) => superseded.successful_commands.push(command),
                Err(error) => {
                    superseded.failure.get_or_insert_with(|| error.to_string());
                }
            }
        } else {
            return false;
        }
        if pending.pending != 0 || pending.superseded.iter().any(|batch| batch.pending != 0) {
            // A sibling may still be queued after the first failure. Keep the
            // request alive so later successes are reflected in the reconciled
            // GUI/config state instead of being discarded as stale.
            return true;
        }

        let Some(pending) = self
            .devices
            .runtime
            .get_mut(&key)
            .and_then(|entry| entry.light.pending.take())
        else {
            return false;
        };
        self.finish_light_command(key, request_id, pending);
        true
    }

    /// Reconcile one fully settled light request with runtime and persisted state.
    fn finish_light_command(
        &mut self,
        key: DeviceKey,
        request_id: u64,
        pending: PendingLightCommand,
    ) {
        let failure = pending.failure.clone().or_else(|| {
            pending
                .superseded
                .iter()
                .find_map(|batch| batch.failure.clone())
        });
        let successful_commands = pending
            .superseded
            .iter()
            .flat_map(|batch| batch.successful_commands.iter().copied())
            .chain(pending.successful_commands.iter().copied())
            .collect::<Vec<_>>();
        let manual_override_rollback = pending.manual_override_rollback;
        if let Some(error) = failure {
            if successful_commands.is_empty() {
                self.devices
                    .runtime
                    .entry(key.clone())
                    .or_default()
                    .light
                    .volatile_settings = pending.previous_volatile;
                restore_manual_override(&mut self.devices.runtime, &key, manual_override_rollback);
            } else {
                let mut accepted = pending.rollback_settings;
                for &command in &successful_commands {
                    apply_light_command(&mut accepted, command);
                }
                if let Some(persistent_key) = pending.persistent_key {
                    if let Some(entry) = self.devices.runtime.get_mut(&key) {
                        entry.light.volatile_settings = None;
                    }
                    self.config
                        .edit(|config| config.set_light(&persistent_key, accepted));
                    self.persist_and_reload("partial light");
                } else {
                    self.devices
                        .runtime
                        .entry(key.clone())
                        .or_default()
                        .light
                        .volatile_settings = Some(accepted);
                }
                if !successful_commands
                    .iter()
                    .any(|command| matches!(command, LightCommand::Power(_)))
                {
                    restore_manual_override(
                        &mut self.devices.runtime,
                        &key,
                        manual_override_rollback,
                    );
                }
            }
            self.devices.runtime.entry(key).or_default().light.status =
                Some((request_id, LightCommandStatus::Failed(error)));
        } else {
            if let (Some(settings), Some(persistent_key)) =
                (pending.settings, pending.persistent_key)
            {
                self.config
                    .edit(|config| config.set_light(&persistent_key, settings));
                if let Some(entry) = self.devices.runtime.get_mut(&key) {
                    entry.light.volatile_settings = None;
                }
                self.persist_and_reload("light");
            }
            // Successful writes are reflected by the controls themselves; do
            // not leave a persistent success banner in the panel.
            if let Some(entry) = self.devices.runtime.get_mut(&key) {
                entry.light.status = None;
            }
        }
    }

    /// The standalone-light settings for the active device, or defaults when
    /// no light config has been stored yet.
    #[must_use]
    pub fn light(&self) -> LightSettings {
        self.current_record()
            .map_or_else(LightSettings::default, |record| {
                self.light_for(&record.device_key())
            })
    }

    /// The standalone-light settings for any persistent or runtime device key.
    #[must_use]
    pub fn light_for(&self, key: &DeviceKey) -> LightSettings {
        self.devices
            .runtime
            .get(key)
            .and_then(|entry| entry.light.volatile_settings)
            .or_else(|| self.config.light(key.as_str()))
            .unwrap_or_default()
    }

    /// The rollback point for a new optimistic light write: inherited from an
    /// in-flight command this one supersedes (so a chain of edits always
    /// rolls back to the last *accepted* value, never a still-pending one),
    /// or freshly computed from `fallback` and the current volatile settings.
    fn light_write_rollback(
        &self,
        key: &DeviceKey,
        fallback: LightSettings,
    ) -> (LightSettings, Option<LightSettings>) {
        self.devices
            .runtime
            .get(key)
            .and_then(|entry| entry.light.pending.as_ref())
            .map_or_else(
                || {
                    (
                        fallback,
                        self.devices
                            .runtime
                            .get(key)
                            .and_then(|entry| entry.light.volatile_settings),
                    )
                },
                |pending| (pending.rollback_settings, pending.previous_volatile),
            )
    }

    /// The manual-override rollback for a `commit_light` write: inherited
    /// from an in-flight command this one supersedes, or — only when the
    /// camera-linked auto mode itself just flipped — the override in effect
    /// right now.
    fn light_mode_override_rollback(
        &self,
        key: &DeviceKey,
        camera_mode_changed: bool,
    ) -> Option<ManualOverrideRollback> {
        self.devices
            .runtime
            .get(key)
            .and_then(|entry| entry.light.pending.as_ref())
            .and_then(|pending| pending.manual_override_rollback)
            .or_else(|| {
                camera_mode_changed.then(|| ManualOverrideRollback {
                    previous: self
                        .devices
                        .runtime
                        .get(key)
                        .and_then(|entry| entry.light.manual_override),
                })
            })
    }

    /// Persist and apply standalone-light settings through the agent-owned
    /// raw-HID path. Online persistent changes are committed only after every
    /// advertised device command succeeds; failures roll optimistic state back.
    pub fn commit_light(&mut self, light: LightSettings) {
        let Some((runtime_key, key, route, online, capabilities)) =
            self.current_record().map(|record| {
                (
                    record.device_key(),
                    record.persistent_config_key().map(str::to_string),
                    record.route.clone(),
                    record.online,
                    record.light_capabilities,
                )
            })
        else {
            debug!("no active device — light change ignored");
            return;
        };
        let previous = self.light_for(&runtime_key);
        let camera_mode_changed =
            cfg!(target_os = "macos") && previous.auto_camera != light.auto_camera;
        let effective_enabled = if camera_policy_applies(light) {
            if camera_mode_changed {
                self.lighting.camera_active
            } else {
                self.light_enabled_for(&runtime_key)
            }
        } else {
            light.enabled
        };
        let manual_override_rollback =
            self.light_mode_override_rollback(&runtime_key, camera_mode_changed);
        if camera_mode_changed && let Some(entry) = self.devices.runtime.get_mut(&runtime_key) {
            entry.light.manual_override = None;
        }
        let mut effective = light;
        effective.enabled = effective_enabled;
        let commands = capabilities.map_or_else(Vec::new, |capabilities| {
            openlogi_core::hid::commands_for_light_settings(effective, capabilities)
        });
        // If this request supersedes another optimistic write, both must roll
        // back to the last accepted value—not to the superseded pending value.
        let (rollback_settings, previous_volatile) =
            self.light_write_rollback(&runtime_key, previous);
        self.devices
            .runtime
            .entry(runtime_key.clone())
            .or_default()
            .light
            .volatile_settings = Some(light);
        if !commands.is_empty() {
            let can_apply = online && route.is_some();
            let superseded = if can_apply {
                self.supersede_light_command(&runtime_key)
            } else {
                Vec::new()
            };
            let request_id = self.begin_light_command(&runtime_key, can_apply);
            if can_apply && let Some(route) = route {
                if let Some(pending) = self
                    .devices
                    .runtime
                    .get_mut(&runtime_key)
                    .and_then(|entry| entry.light.pending.as_mut())
                {
                    pending.settings = Some(light);
                    pending.persistent_key.clone_from(&key);
                    pending.rollback_settings = rollback_settings;
                    pending.previous_volatile = previous_volatile;
                    pending.manual_override_rollback = manual_override_rollback;
                    pending.superseded = superseded;
                }
                for command in commands {
                    self.queue_light_command(&runtime_key, request_id, route.clone(), command);
                }
                return;
            }
        }
        if let Some(key) = key {
            if let Some(entry) = self.devices.runtime.get_mut(&runtime_key) {
                entry.light.volatile_settings = None;
            }
            self.config.edit(|config| config.set_light(&key, light));
            self.persist_and_reload("light");
        } else {
            self.devices
                .runtime
                .entry(runtime_key)
                .or_default()
                .light
                .volatile_settings = Some(light);
        }
    }

    /// Apply a transient manual power choice while camera automation remains
    /// enabled. The persisted `enabled` field is updated as the manual fallback,
    /// but the runtime override lasts only until the next camera transition.
    pub fn commit_manual_light_power(&mut self, enabled: bool) {
        let Some((runtime_key, key, route, online)) = self.current_record().map(|record| {
            (
                record.device_key(),
                record.persistent_config_key().map(str::to_string),
                record.route.clone(),
                record.online,
            )
        }) else {
            debug!("no active device — manual light power ignored");
            return;
        };
        let mut light = self.light_for(&runtime_key);
        if !camera_policy_applies(light) {
            light.enabled = enabled;
            self.commit_light(light);
            return;
        }

        let (rollback_settings, previous_volatile) = self.light_write_rollback(&runtime_key, light);
        // Unlike `commit_light`, a manual power toggle always changes the
        // override, so the fallback branch always applies — pass `true` to
        // unconditionally take it rather than gate it on a camera-mode flip.
        let manual_override_rollback = self.light_mode_override_rollback(&runtime_key, true);
        light.enabled = enabled;
        {
            let entry = self.devices.runtime.entry(runtime_key.clone()).or_default();
            entry.light.manual_override = Some(enabled);
            entry.light.volatile_settings = Some(light);
        }

        let can_apply = online && route.is_some();
        let superseded = if can_apply {
            self.supersede_light_command(&runtime_key)
        } else {
            Vec::new()
        };
        let request_id = self.begin_light_command(&runtime_key, can_apply);
        if can_apply && let Some(route) = route {
            if let Some(pending) = self
                .devices
                .runtime
                .get_mut(&runtime_key)
                .and_then(|entry| entry.light.pending.as_mut())
            {
                pending.pending = 1;
                pending.settings = Some(light);
                pending.persistent_key.clone_from(&key);
                pending.rollback_settings = rollback_settings;
                pending.previous_volatile = previous_volatile;
                pending.manual_override_rollback = manual_override_rollback;
                pending.superseded = superseded;
            }
            if !self.send_ipc(crate::services::ipc::Command::SetLightManualPower(
                route,
                enabled,
                runtime_key.to_string(),
                request_id,
            )) {
                self.apply_light_command_result(
                    runtime_key.to_string(),
                    request_id,
                    LightCommand::Power(enabled),
                    Err(WriteError::AgentUnavailable),
                );
            }
            return;
        }

        if let Some(key) = key {
            if let Some(entry) = self.devices.runtime.get_mut(&runtime_key) {
                entry.light.volatile_settings = None;
            }
            self.config.edit(|config| config.set_light(&key, light));
            self.persist_and_reload("manual light power");
        }
    }
}

fn apply_light_command(settings: &mut LightSettings, command: LightCommand) {
    match command {
        LightCommand::Power(enabled) => settings.enabled = enabled,
        LightCommand::BrightnessPercent(brightness_percent) => {
            settings.brightness_percent = brightness_percent;
        }
        LightCommand::TemperatureKelvin(temperature_kelvin) => {
            settings.temperature_kelvin = Some(temperature_kelvin);
        }
        LightCommand::BrightnessNative(_) => {}
    }
}

fn restore_manual_override(
    runtime: &mut BTreeMap<DeviceKey, DeviceRuntimeState>,
    key: &DeviceKey,
    rollback: Option<ManualOverrideRollback>,
) {
    if let Some(rollback) = rollback {
        runtime
            .entry(key.clone())
            .or_default()
            .light
            .manual_override = rollback.previous;
    }
}
