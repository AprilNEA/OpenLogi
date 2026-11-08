//! Optimistic standalone-light state and IPC result handling.

use std::collections::BTreeMap;
use std::iter;
use std::num::NonZeroUsize;

use openlogi_core::config::LightSettings;
use openlogi_core::device::LightState;
use openlogi_core::hid::{DeviceRoute, LightCommand, WriteError};
use openlogi_ipc::{ChosenLight, ObservedLight};
use tracing::debug;

use super::device_key::DeviceKey;
use super::device_session::DeviceSession;
use super::events::StateEvents;
use super::{AppState, StateEvent};

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
pub(super) struct LightSession {
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
    /// The latest optimistic write, which is also the status the panel shows.
    write: LightWrite,
}

/// Where the latest optimistic write to one light stands. The panel's
/// [`LightCommandStatus`] is a projection of it, so a status cannot disagree
/// with the write in flight.
#[derive(Debug, Default)]
enum LightWrite {
    /// Nothing to show: no write yet, or the latest one succeeded, which the
    /// controls themselves already show.
    #[default]
    Idle,
    /// The agent still owes answers; shown as Pending.
    Writing(LightWriteRequest),
    /// The latest write was rejected, shown until the next one.
    Failed(String),
    /// The latest change found the light offline, so nothing was sent.
    Offline,
}

/// A write in flight. Every value owes at least one answer:
/// [`LightWrite::begin`] starts it owing one per command sent, and the answer
/// that leaves nothing owed settles it in the same transition.
#[derive(Debug)]
struct LightWriteRequest {
    /// The latest request's commands.
    latest: LightBatch,
    /// The requests it superseded, oldest first. Their answers keep the write
    /// alive: a sibling may still be queued after the first failure, and its
    /// success must reach the reconciled GUI/config state instead of being
    /// discarded as stale.
    superseded: Vec<LightBatch>,
    setup: LightWriteSetup,
}

/// Everything a write carries beyond its answers. The caller gathers it
/// *before* [`AppState::begin_light_write`], so no write exists with a
/// placeholder rollback that an early return could persist to config.toml.
#[derive(Debug)]
struct LightWriteSetup {
    settings: LightSettings,
    persistent_key: Option<String>,
    rollback_settings: LightSettings,
    previous_volatile: Option<LightSettings>,
    manual_override_rollback: Option<ManualOverrideRollback>,
}

/// One request's commands as the agent answers them. The IPC client answers
/// every command it is handed exactly once — with the agent's verdict, or as
/// unavailable when no agent could be reached — so `unanswered` reaches zero
/// unless the client itself is gone ([`LightWrite::abandon`]).
#[derive(Debug)]
struct LightBatch {
    request_id: u64,
    /// Commands sent that the agent has not answered.
    unanswered: usize,
    /// Commands the light accepted, in answer order.
    applied: Vec<LightCommand>,
    /// The first rejection.
    failure: Option<String>,
}

/// A write whose every answer is in, handed back for its effects.
#[derive(Debug)]
struct SettledLightWrite {
    /// The latest request's first rejection, else the oldest superseded one's.
    failure: Option<String>,
    /// Every command the light accepted, oldest request first.
    applied: Vec<LightCommand>,
    setup: LightWriteSetup,
}

/// What one answer did to a light's write.
#[derive(Debug)]
enum LightAnswer {
    /// It belongs to no request still owed an answer.
    Stale,
    /// It was counted; the write still owes others.
    Counted,
    /// It was the last one owed: the write is settled.
    Settled(SettledLightWrite),
}

#[derive(Debug, Clone, Copy)]
struct ManualOverrideRollback {
    previous: Option<bool>,
}

impl LightWrite {
    /// The status the panel shows for this write.
    fn status(&self) -> Option<LightCommandStatus> {
        match self {
            Self::Idle => None,
            Self::Writing(_) => Some(LightCommandStatus::Pending),
            Self::Failed(error) => Some(LightCommandStatus::Failed(error.clone())),
            Self::Offline => Some(LightCommandStatus::Offline),
        }
    }

    /// The write in flight, if any.
    const fn writing(&self) -> Option<&LightWriteRequest> {
        match self {
            Self::Writing(request) => Some(request),
            Self::Idle | Self::Failed(_) | Self::Offline => None,
        }
    }

    /// Start request `request_id`, owed one answer per command `sent`. A write
    /// still in flight is superseded: its answers keep counting toward this one.
    #[must_use]
    fn begin(self, request_id: u64, sent: NonZeroUsize, setup: LightWriteSetup) -> Self {
        let superseded = match self {
            Self::Writing(previous) => previous.into_batches(),
            Self::Idle | Self::Failed(_) | Self::Offline => Vec::new(),
        };
        Self::Writing(LightWriteRequest {
            latest: LightBatch {
                request_id,
                unanswered: sent.get(),
                applied: Vec::new(),
                failure: None,
            },
            superseded,
            setup,
        })
    }

    /// Count one answer for `request_id`, settling the write when it was the
    /// last one owed.
    #[must_use]
    fn answer(
        self,
        request_id: u64,
        command: LightCommand,
        result: Result<(), WriteError>,
    ) -> (Self, LightAnswer) {
        let Self::Writing(mut request) = self else {
            return (self, LightAnswer::Stale);
        };
        if !request.record(request_id, command, result) {
            return (Self::Writing(request), LightAnswer::Stale);
        }
        if request.owes_answers() {
            return (Self::Writing(request), LightAnswer::Counted);
        }
        let settled = request.settle();
        (settled.outcome(), LightAnswer::Settled(settled))
    }

    /// Settle a write whose answers can no longer arrive, as if each one still
    /// owed had come back unavailable — the answer the IPC client gives a
    /// command it cannot send.
    #[must_use]
    fn abandon(self) -> (Self, Option<SettledLightWrite>) {
        let Self::Writing(mut request) = self else {
            return (self, None);
        };
        for batch in iter::once(&mut request.latest).chain(&mut request.superseded) {
            batch.abandon();
        }
        let settled = request.settle();
        (settled.outcome(), Some(settled))
    }
}

impl LightWriteRequest {
    /// Count one answer against the request it belongs to; `false` when it
    /// belongs to none still owed one.
    fn record(
        &mut self,
        request_id: u64,
        command: LightCommand,
        result: Result<(), WriteError>,
    ) -> bool {
        iter::once(&mut self.latest)
            .chain(&mut self.superseded)
            .find(|batch| batch.request_id == request_id)
            .is_some_and(|batch| batch.record(command, result))
    }

    fn owes_answers(&self) -> bool {
        iter::once(&self.latest)
            .chain(&self.superseded)
            .any(|batch| batch.unanswered > 0)
    }

    /// This write's batches, oldest first, for the write that supersedes it.
    fn into_batches(self) -> Vec<LightBatch> {
        let mut batches = self.superseded;
        batches.push(self.latest);
        batches
    }

    fn settle(self) -> SettledLightWrite {
        let Self {
            latest,
            superseded,
            setup,
        } = self;
        let applied = superseded
            .iter()
            .chain(iter::once(&latest))
            .flat_map(|batch| batch.applied.iter().copied())
            .collect();
        let failure = latest
            .failure
            .or_else(|| superseded.into_iter().find_map(|batch| batch.failure));
        SettledLightWrite {
            failure,
            applied,
            setup,
        }
    }
}

impl LightBatch {
    /// Count one answer; `false` when every command has been answered.
    fn record(&mut self, command: LightCommand, result: Result<(), WriteError>) -> bool {
        let Some(unanswered) = self.unanswered.checked_sub(1) else {
            return false;
        };
        self.unanswered = unanswered;
        match result {
            Ok(()) => self.applied.push(command),
            Err(error) => {
                self.failure.get_or_insert_with(|| error.to_string());
            }
        }
        true
    }

    fn abandon(&mut self) {
        if self.unanswered > 0 {
            self.unanswered = 0;
            self.failure
                .get_or_insert_with(|| WriteError::AgentUnavailable.to_string());
        }
    }
}

impl SettledLightWrite {
    /// The write state it leaves behind.
    fn outcome(&self) -> LightWrite {
        self.failure
            .clone()
            .map_or(LightWrite::Idle, LightWrite::Failed)
    }
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
                .sessions
                .get(key)
                .and_then(|entry| entry.light.manual_override)
                .unwrap_or(self.lights.camera_active)
        } else {
            light.enabled
        }
    }

    /// Update the runtime camera state used by camera-linked light rendering.
    /// A real transition clears every transient manual override, and is the
    /// only case reported.
    pub fn set_camera_active(&mut self, active: bool) -> StateEvents {
        let changed = self.lights.camera_active != active;
        if changed {
            for entry in self.devices.sessions.values_mut() {
                entry.light.manual_override = None;
            }
        }
        self.lights.camera_active = active;
        changed.then_some(StateEvent::CameraChanged).into()
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
        self.devices.sessions.get(&key)?.light.write.status()
    }

    /// Allocate the next request id and start a write of `sent` commands
    /// under it, superseding any write to `key` still in flight.
    fn begin_light_write(
        &mut self,
        key: &DeviceKey,
        sent: NonZeroUsize,
        setup: LightWriteSetup,
    ) -> u64 {
        self.lights.next_request_id = self.lights.next_request_id.wrapping_add(1);
        let request_id = self.lights.next_request_id;
        let light = &mut self.devices.sessions.entry(key.clone()).or_default().light;
        light.write = std::mem::take(&mut light.write).begin(request_id, sent, setup);
        request_id
    }

    /// A change to `key` found it offline, so nothing was sent. A write still
    /// in flight is dropped with it: its late answers are stale and cannot
    /// overwrite the newer change, which is saved directly.
    fn light_found_offline(&mut self, key: &DeviceKey) {
        self.devices
            .sessions
            .entry(key.clone())
            .or_default()
            .light
            .write = LightWrite::Offline;
    }

    /// Hand one command of request `request_id` to the IPC client, answering
    /// it as unavailable when the client is gone.
    fn send_light_command(
        &mut self,
        key: &DeviceKey,
        request_id: u64,
        route: DeviceRoute,
        command: LightCommand,
    ) {
        if !self.send_ipc(crate::services::ipc::SetLight {
            route,
            command,
            key: key.clone(),
            request_id,
        }) {
            self.answer_light_command(key, request_id, command, Err(WriteError::AgentUnavailable));
        }
    }

    /// Consume an asynchronous light-write result from the IPC client.
    /// Results from an older request are ignored so a slow failed write cannot
    /// overwrite the status of a newer slider release.
    pub fn apply_light_command_result(
        &mut self,
        key: DeviceKey,
        request_id: u64,
        command: LightCommand,
        result: Result<(), WriteError>,
    ) -> StateEvents {
        if self.answer_light_command(&key, request_id, command, result) {
            StateEvent::LightingChanged(key).into()
        } else {
            StateEvents::none()
        }
    }

    /// The IPC client is gone, so no light write still in flight will ever be
    /// answered. Settle each one, rolling back what the light did not
    /// confirm, so its status leaves Pending and adoption resumes.
    pub fn abandon_light_writes(&mut self) -> StateEvents {
        let settled = self
            .devices
            .sessions
            .iter_mut()
            .filter_map(|(key, entry)| {
                let (write, settled) = std::mem::take(&mut entry.light.write).abandon();
                entry.light.write = write;
                settled.map(|settled| (key.clone(), settled))
            })
            .collect::<Vec<_>>();
        let mut events = StateEvents::none();
        for (key, settled) in settled {
            self.finish_light_write(&key, settled);
            events = events.and(StateEvent::LightingChanged(key));
        }
        events
    }

    /// Record one write result against the request it belongs to, reporting
    /// whether it belonged to a live one.
    fn answer_light_command(
        &mut self,
        key: &DeviceKey,
        request_id: u64,
        command: LightCommand,
        result: Result<(), WriteError>,
    ) -> bool {
        let Some(entry) = self.devices.sessions.get_mut(key) else {
            return false;
        };
        let (write, answer) =
            std::mem::take(&mut entry.light.write).answer(request_id, command, result);
        entry.light.write = write;
        match answer {
            LightAnswer::Stale => false,
            LightAnswer::Counted => true,
            LightAnswer::Settled(settled) => {
                self.finish_light_write(key, settled);
                true
            }
        }
    }

    /// Reconcile one settled light write with runtime and persisted state.
    /// Its status is already the write's outcome.
    fn finish_light_write(&mut self, key: &DeviceKey, settled: SettledLightWrite) {
        let SettledLightWrite {
            failure,
            applied,
            setup,
        } = settled;
        if failure.is_none() {
            // Successful writes are reflected by the controls themselves; the
            // write is Idle, so no success banner stays in the panel.
            if let Some(persistent_key) = setup.persistent_key {
                self.config
                    .edit(|config| config.set_light(&persistent_key, setup.settings));
                if let Some(entry) = self.devices.sessions.get_mut(key) {
                    entry.light.volatile_settings = None;
                }
                self.persist_and_reload("light");
            }
            return;
        }
        if applied.is_empty() {
            self.devices
                .sessions
                .entry(key.clone())
                .or_default()
                .light
                .volatile_settings = setup.previous_volatile;
            restore_manual_override(
                &mut self.devices.sessions,
                key,
                setup.manual_override_rollback,
            );
            return;
        }
        let mut accepted = setup.rollback_settings;
        for &command in &applied {
            apply_light_command(&mut accepted, command);
        }
        if let Some(persistent_key) = setup.persistent_key {
            if let Some(entry) = self.devices.sessions.get_mut(key) {
                entry.light.volatile_settings = None;
            }
            self.config
                .edit(|config| config.set_light(&persistent_key, accepted));
            self.persist_and_reload("partial light");
        } else {
            self.devices
                .sessions
                .entry(key.clone())
                .or_default()
                .light
                .volatile_settings = Some(accepted);
        }
        if !applied
            .iter()
            .any(|command| matches!(command, LightCommand::Power(_)))
        {
            restore_manual_override(
                &mut self.devices.sessions,
                key,
                setup.manual_override_rollback,
            );
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
        self.saved_light_for(key).unwrap_or_default()
    }

    /// The standalone-light settings kept for `key` — this session's or the
    /// saved ones — if any.
    fn saved_light_for(&self, key: &DeviceKey) -> Option<LightSettings> {
        self.devices
            .sessions
            .get(key)
            .and_then(|entry| entry.light.volatile_settings)
            .or_else(|| self.config.light(key.as_str()))
    }

    /// Keep each change made on a light itself as its setting, as Options+
    /// does: the values the agent reports as changed on the light replace
    /// the kept ones. A light with a GUI write in flight is left until the
    /// write settles. The agent takes the controls a write sets before it
    /// writes, so a later report offers only the changes still the light's.
    pub(super) fn adopt_changes_on_lights(&mut self, lights: &[ObservedLight]) -> StateEvents {
        let mut events = StateEvents::none();
        let mut saved = false;
        for light in lights {
            if light.changed_on_light.is_empty() {
                continue;
            }
            let Some((runtime_key, persistent_key)) = self
                .devices
                .records
                .iter()
                .find(|record| record.addressable_route() == Some(&light.route))
                .map(|record| {
                    (
                        record.device_key(),
                        record.persistent_config_key().map(str::to_string),
                    )
                })
            else {
                continue;
            };
            if self
                .devices
                .sessions
                .get(&runtime_key)
                .is_some_and(|entry| entry.light.write.writing().is_some())
            {
                continue;
            }
            let kept = self.saved_light_for(&runtime_key);
            let adopted = adopted_light(kept, light);
            if kept == Some(adopted) {
                continue;
            }
            if let Some(key) = persistent_key {
                if let Some(entry) = self.devices.sessions.get_mut(&runtime_key) {
                    entry.light.volatile_settings = None;
                }
                self.config.edit(|config| config.set_light(&key, adopted));
                saved = true;
            } else {
                self.devices
                    .sessions
                    .entry(runtime_key.clone())
                    .or_default()
                    .light
                    .volatile_settings = Some(adopted);
            }
            events = events.and(StateEvent::LightingChanged(runtime_key));
        }
        // The saved reload takes these values as copies of what the lights
        // reported, so a newer change on a light outlives it.
        if saved {
            self.persist_and_reload("light");
        }
        events
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
            .sessions
            .get(key)
            .and_then(|entry| entry.light.write.writing())
            .map_or_else(
                || {
                    (
                        fallback,
                        self.devices
                            .sessions
                            .get(key)
                            .and_then(|entry| entry.light.volatile_settings),
                    )
                },
                |request| {
                    (
                        request.setup.rollback_settings,
                        request.setup.previous_volatile,
                    )
                },
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
            .sessions
            .get(key)
            .and_then(|entry| entry.light.write.writing())
            .and_then(|request| request.setup.manual_override_rollback)
            .or_else(|| {
                camera_mode_changed.then(|| ManualOverrideRollback {
                    previous: self
                        .devices
                        .sessions
                        .get(key)
                        .and_then(|entry| entry.light.manual_override),
                })
            })
    }

    /// Persist and apply standalone-light settings through the agent-owned
    /// raw-HID path. Online persistent changes are committed only after every
    /// advertised device command succeeds; failures roll optimistic state back.
    pub fn commit_light(&mut self, light: LightSettings) -> StateEvents {
        let events = self.for_current_device(StateEvent::LightingChanged);
        let Some((runtime_key, key, route, capabilities)) = self.current_record().map(|record| {
            (
                record.device_key(),
                record.persistent_config_key().map(str::to_string),
                record.addressable_route().cloned(),
                record.light_capabilities,
            )
        }) else {
            debug!("no active device — light change ignored");
            return events;
        };
        let previous = self.light_for(&runtime_key);
        let camera_mode_changed =
            cfg!(target_os = "macos") && previous.auto_camera != light.auto_camera;
        let effective_enabled = if camera_policy_applies(light) {
            if camera_mode_changed {
                self.lights.camera_active
            } else {
                self.light_enabled_for(&runtime_key)
            }
        } else {
            light.enabled
        };
        let manual_override_rollback =
            self.light_mode_override_rollback(&runtime_key, camera_mode_changed);
        if camera_mode_changed && let Some(entry) = self.devices.sessions.get_mut(&runtime_key) {
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
            .sessions
            .entry(runtime_key.clone())
            .or_default()
            .light
            .volatile_settings = Some(light);
        if let Some(sent) = NonZeroUsize::new(commands.len()) {
            if let Some(route) = route {
                let request_id = self.begin_light_write(
                    &runtime_key,
                    sent,
                    LightWriteSetup {
                        settings: light,
                        persistent_key: key.clone(),
                        rollback_settings,
                        previous_volatile,
                        manual_override_rollback,
                    },
                );
                for command in commands {
                    self.send_light_command(&runtime_key, request_id, route.clone(), command);
                }
                return events;
            }
            self.light_found_offline(&runtime_key);
        }
        if let Some(key) = key {
            if let Some(entry) = self.devices.sessions.get_mut(&runtime_key) {
                entry.light.volatile_settings = None;
            }
            let chosen = chosen_light(&key, previous, light);
            self.config.edit(|config| config.set_light(&key, light));
            self.persist_and_reload_choosing("light", chosen);
        } else {
            self.devices
                .sessions
                .entry(runtime_key)
                .or_default()
                .light
                .volatile_settings = Some(light);
        }
        events
    }

    /// Apply a transient manual power choice while camera automation remains
    /// enabled. The persisted `enabled` field is updated as the manual fallback,
    /// but the runtime override lasts only until the next camera transition.
    pub fn commit_manual_light_power(&mut self, enabled: bool) -> StateEvents {
        let events = self.for_current_device(StateEvent::LightingChanged);
        let Some((runtime_key, key, route)) = self.current_record().map(|record| {
            (
                record.device_key(),
                record.persistent_config_key().map(str::to_string),
                record.addressable_route().cloned(),
            )
        }) else {
            debug!("no active device — manual light power ignored");
            return events;
        };
        let mut light = self.light_for(&runtime_key);
        if !camera_policy_applies(light) {
            light.enabled = enabled;
            return self.commit_light(light);
        }

        let (rollback_settings, previous_volatile) = self.light_write_rollback(&runtime_key, light);
        // Unlike `commit_light`, a manual power toggle always changes the
        // override, so the fallback branch always applies — pass `true` to
        // unconditionally take it rather than gate it on a camera-mode flip.
        let manual_override_rollback = self.light_mode_override_rollback(&runtime_key, true);
        light.enabled = enabled;
        {
            let entry = self
                .devices
                .sessions
                .entry(runtime_key.clone())
                .or_default();
            entry.light.manual_override = Some(enabled);
            entry.light.volatile_settings = Some(light);
        }

        if let Some(route) = route {
            let request_id = self.begin_light_write(
                &runtime_key,
                NonZeroUsize::MIN,
                LightWriteSetup {
                    settings: light,
                    persistent_key: key.clone(),
                    rollback_settings,
                    previous_volatile,
                    manual_override_rollback,
                },
            );
            if !self.send_ipc(crate::services::ipc::SetLightManualPower {
                route,
                enabled,
                key: runtime_key.clone(),
                request_id,
            }) {
                self.answer_light_command(
                    &runtime_key,
                    request_id,
                    LightCommand::Power(enabled),
                    Err(WriteError::AgentUnavailable),
                );
            }
            return events;
        }
        self.light_found_offline(&runtime_key);

        if let Some(key) = key {
            if let Some(entry) = self.devices.sessions.get_mut(&runtime_key) {
                entry.light.volatile_settings = None;
            }
            let chosen = ChosenLight {
                device: key.clone(),
                power: true,
                ..ChosenLight::default()
            };
            self.config.edit(|config| config.set_light(&key, light));
            self.persist_and_reload_choosing("manual light power", vec![chosen]);
        }
        events
    }
}

/// `kept` with the values changed on the light in their place. A light with
/// no kept settings starts from what it reads back, so the controls the
/// change left alone keep the light's own values too.
fn adopted_light(kept: Option<LightSettings>, light: &ObservedLight) -> LightSettings {
    let mut adopted = kept.unwrap_or_else(|| read_back_light(&light.state));
    adopted.apply_changes_on_light(light.changed_on_light);
    adopted
}

/// The settings that reproduce what a light reads back.
fn read_back_light(state: &LightState) -> LightSettings {
    let defaults = LightSettings::default();
    LightSettings {
        enabled: state.on,
        brightness_percent: state
            .brightness
            .zip(state.capabilities.brightness)
            .and_then(|(native, range)| range.percent_for_native(native))
            .unwrap_or(defaults.brightness_percent),
        temperature_kelvin: state.temperature_kelvin.or(defaults.temperature_kelvin),
        ..defaults
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
    sessions: &mut BTreeMap<DeviceKey, DeviceSession>,
    key: &DeviceKey,
    rollback: Option<ManualOverrideRollback>,
) {
    if let Some(rollback) = rollback {
        sessions
            .entry(key.clone())
            .or_default()
            .light
            .manual_override = rollback.previous;
    }
}

/// The controls of the light saved under `key` that a save made without a
/// write to the light changes, from its settings `before` and `after`.
fn chosen_light(key: &str, before: LightSettings, after: LightSettings) -> Vec<ChosenLight> {
    let chosen = ChosenLight {
        device: key.to_string(),
        power: before.enabled != after.enabled,
        brightness: before.brightness_percent != after.brightness_percent,
        temperature: before.temperature_kelvin != after.temperature_kelvin,
    };
    if chosen.power || chosen.brightness || chosen.temperature {
        vec![chosen]
    } else {
        Vec::new()
    }
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroUsize;

    use openlogi_core::config::LightSettings;
    use openlogi_core::hid::{LightCommand, WriteError};

    use super::{LightAnswer, LightCommandStatus, LightWrite, LightWriteSetup, SettledLightWrite};

    const POWER: LightCommand = LightCommand::Power(true);
    const BRIGHTNESS: LightCommand = LightCommand::BrightnessPercent(40);
    const TEMPERATURE: LightCommand = LightCommand::TemperatureKelvin(4000);

    fn sent(commands: usize) -> NonZeroUsize {
        NonZeroUsize::new(commands).expect("a write sends at least one command")
    }

    fn setup() -> LightWriteSetup {
        LightWriteSetup {
            settings: LightSettings::new(true, 40, Some(4000)),
            persistent_key: None,
            rollback_settings: LightSettings::default(),
            previous_volatile: None,
            manual_override_rollback: None,
        }
    }

    fn ambiguous() -> Result<(), WriteError> {
        Err(WriteError::AmbiguousRawDevice)
    }

    fn unavailable() -> Result<(), WriteError> {
        Err(WriteError::AgentUnavailable)
    }

    fn failed(error: &WriteError) -> LightCommandStatus {
        LightCommandStatus::Failed(error.to_string())
    }

    /// Answer `command` of `request_id`, which must leave the write pending.
    fn counted(
        write: LightWrite,
        request_id: u64,
        command: LightCommand,
        result: Result<(), WriteError>,
    ) -> LightWrite {
        let (write, answer) = write.answer(request_id, command, result);
        assert!(matches!(answer, LightAnswer::Counted), "got {answer:?}");
        assert_eq!(write.status(), Some(LightCommandStatus::Pending));
        write
    }

    /// Answer `command` of `request_id`, which must be the last answer owed.
    fn last(
        write: LightWrite,
        request_id: u64,
        command: LightCommand,
        result: Result<(), WriteError>,
    ) -> (LightWrite, SettledLightWrite) {
        let (write, answer) = write.answer(request_id, command, result);
        let LightAnswer::Settled(settled) = answer else {
            panic!("expected the last answer to settle the write, got {answer:?}");
        };
        (write, settled)
    }

    #[test]
    fn a_write_is_pending_until_its_last_answer_and_then_shows_nothing() {
        let write = LightWrite::Failed("an earlier write".into()).begin(1, sent(2), setup());
        assert_eq!(write.status(), Some(LightCommandStatus::Pending));

        let write = counted(write, 1, POWER, Ok(()));
        let (write, settled) = last(write, 1, BRIGHTNESS, Ok(()));

        assert_eq!(write.status(), None);
        assert_eq!(settled.failure, None);
        assert_eq!(settled.applied, [POWER, BRIGHTNESS]);
    }

    #[test]
    fn a_rejection_waits_for_its_siblings_and_shows_the_first_error() {
        let write = LightWrite::Offline.begin(1, sent(3), setup());
        let write = counted(write, 1, POWER, ambiguous());
        let write = counted(write, 1, BRIGHTNESS, Ok(()));
        let (write, settled) = last(write, 1, TEMPERATURE, unavailable());

        assert_eq!(
            write.status(),
            Some(failed(&WriteError::AmbiguousRawDevice))
        );
        assert_eq!(
            settled.failure,
            Some(WriteError::AmbiguousRawDevice.to_string())
        );
        assert_eq!(settled.applied, [BRIGHTNESS]);
    }

    #[test]
    fn a_superseded_request_holds_its_successor_open_and_counts_first() {
        let write = LightWrite::Idle.begin(1, sent(2), setup());
        let write = counted(write, 1, POWER, ambiguous());
        let write = write.begin(2, sent(2), setup());
        // Every answer of the latest request is in, but request 1 still owes one.
        let write = counted(write, 2, TEMPERATURE, Ok(()));
        let write = counted(write, 2, POWER, unavailable());
        let (write, settled) = last(write, 1, BRIGHTNESS, Ok(()));

        // The latest request's rejection wins over the older, earlier one.
        assert_eq!(write.status(), Some(failed(&WriteError::AgentUnavailable)));
        assert_eq!(
            settled.failure,
            Some(WriteError::AgentUnavailable.to_string())
        );
        // Oldest request first, not in the order the answers arrived.
        assert_eq!(settled.applied, [BRIGHTNESS, TEMPERATURE]);
    }

    #[test]
    fn an_answer_no_request_still_owes_changes_nothing() {
        let (write, answer) = LightWrite::Failed("shown".into()).answer(1, POWER, Ok(()));
        assert!(matches!(answer, LightAnswer::Stale), "got {answer:?}");
        assert_eq!(
            write.status(),
            Some(LightCommandStatus::Failed("shown".into()))
        );

        let write = LightWrite::Idle
            .begin(1, sent(1), setup())
            .begin(2, sent(1), setup());
        let (write, answer) = write.answer(7, POWER, Ok(()));
        assert!(matches!(answer, LightAnswer::Stale), "got {answer:?}");
        let write = counted(write, 1, POWER, Ok(()));
        // Request 1 is fully answered; a second answer under its id must not
        // count against request 2.
        let (write, answer) = write.answer(1, BRIGHTNESS, Ok(()));
        assert!(matches!(answer, LightAnswer::Stale), "got {answer:?}");
        assert_eq!(write.status(), Some(LightCommandStatus::Pending));

        let (write, settled) = last(write, 2, TEMPERATURE, Ok(()));
        assert_eq!(write.status(), None);
        assert_eq!(settled.applied, [POWER, TEMPERATURE]);
    }

    #[test]
    fn an_abandoned_write_settles_what_never_came_back_as_unavailable() {
        let write = counted(
            LightWrite::Idle.begin(1, sent(2), setup()),
            1,
            POWER,
            Ok(()),
        );
        let (write, settled) = write.abandon();
        let settled = settled.expect("a write in flight settles");
        assert_eq!(write.status(), Some(failed(&WriteError::AgentUnavailable)));
        assert_eq!(settled.applied, [POWER]);

        // Only the superseded request still owes an answer.
        let write = LightWrite::Idle
            .begin(1, sent(1), setup())
            .begin(2, sent(1), setup());
        let write = counted(write, 2, BRIGHTNESS, Ok(()));
        let (write, settled) = write.abandon();
        assert_eq!(write.status(), Some(failed(&WriteError::AgentUnavailable)));
        assert_eq!(settled.expect("settled").applied, [BRIGHTNESS]);

        // A rejection already in is the one shown.
        let write = counted(
            LightWrite::Idle.begin(1, sent(2), setup()),
            1,
            POWER,
            ambiguous(),
        );
        let (write, _) = write.abandon();
        assert_eq!(
            write.status(),
            Some(failed(&WriteError::AmbiguousRawDevice))
        );

        for write in [
            LightWrite::Idle,
            LightWrite::Failed("shown".into()),
            LightWrite::Offline,
        ] {
            let status = write.status();
            let (write, settled) = write.abandon();
            assert!(settled.is_none());
            assert_eq!(write.status(), status);
        }
    }
}
