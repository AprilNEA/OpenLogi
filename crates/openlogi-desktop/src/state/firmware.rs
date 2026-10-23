//! Firmware status reads and agent-owned update observation. No values persist.

use std::collections::BTreeMap;

use openlogi_core::hid::{
    DeviceRoute, FirmwareBatteryCharge, FirmwareStatus, FirmwareUpdate, FirmwareUpdateRefusal,
    FirmwareVersion, WriteError,
};

use super::{AgentLink, AppState, DeviceKey, Load, StateEvent, StateEvents};
use crate::services::ipc::{ReadFirmware, StartFirmwareUpdate};

/// Why the agent did not acknowledge the update request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum FirmwareStartFailure {
    Unavailable,
    Refused(FirmwareUpdateRefusal),
}

struct Reading {
    route: DeviceRoute,
    flight: u64,
    load: Load<FirmwareStatus>,
}

struct Start {
    key: DeviceKey,
    route: DeviceRoute,
    flight: u64,
    prior_job: Option<u64>,
    result: Result<(), FirmwareStartFailure>,
}

struct ObservedFirmware {
    update: FirmwareUpdate,
    key: Option<DeviceKey>,
}

#[derive(Default)]
/// Firmware reads, request admission and observed job identity for this GUI.
pub(super) struct FirmwareSession {
    reads: BTreeMap<DeviceKey, Reading>,
    next_flight: u64,
    start: Option<Start>,
    observed: Option<ObservedFirmware>,
    agent_identity: Option<openlogi_ipc::Identity>,
}

impl FirmwareSession {
    fn flight(&mut self) -> u64 {
        self.next_flight = self.next_flight.wrapping_add(1);
        self.next_flight
    }

    /// Fence the previous reading and its in-flight response.
    pub(super) fn invalidate(&mut self, key: &DeviceKey) {
        self.reads.remove(key);
    }

    /// Drop readings when their physical device leaves the merged catalog.
    pub(super) fn retain_present(&mut self, present: impl Fn(&str) -> bool) {
        self.reads.retain(|key, _| present(key.as_str()));
    }

    /// Whether the latest job belongs to this device and route.
    pub(super) fn references_route(&self, route: &DeviceRoute, key: &DeviceKey) -> bool {
        self.observed
            .as_ref()
            .is_some_and(|job| job.update.route == *route && job.key.as_ref() == Some(key))
    }

    /// Whether an active job requires retaining this device during restart.
    pub(super) fn protects_route(&self, route: &DeviceRoute, key: &DeviceKey) -> bool {
        self.observed.as_ref().is_some_and(|job| {
            job.update.route == *route
                && job.key.as_ref() == Some(key)
                && !job.update.phase.is_finished()
        })
    }
}

impl AppState {
    /// Charge eligibility for the device shown by a firmware confirmation.
    pub(crate) fn firmware_battery_charge(&self, key: &DeviceKey) -> FirmwareBatteryCharge {
        if !self.is_current_device(key) {
            return FirmwareBatteryCharge::Unknown;
        }
        FirmwareBatteryCharge::from_percentage(
            self.current_record()
                .filter(|record| record.online)
                .and_then(|record| record.battery.as_ref()?.percentage),
        )
    }

    /// Read the selected device once per connection; rendering only consumes this cache.
    pub(super) fn load_current_firmware(&mut self) {
        if self.firmware_busy() || !matches!(self.agent_link(), AgentLink::Ready(_)) {
            return;
        }
        let Some((key, route)) = self
            .current_record()
            .filter(|record| record.online)
            .and_then(|record| Some((record.device_key(), record.route.clone()?)))
        else {
            return;
        };
        if matches!(route, DeviceRoute::RawHid { .. }) {
            return;
        }
        if self
            .firmware
            .reads
            .get(&key)
            .is_some_and(|read| read.route == route)
        {
            return;
        }
        let flight = self.firmware.flight();
        self.firmware.reads.insert(
            key.clone(),
            Reading {
                route: route.clone(),
                flight,
                load: Load::Loading,
            },
        );
        if !self.send_ipc(ReadFirmware {
            key: key.clone(),
            route,
            flight,
        }) && let Some(read) = self.firmware.reads.get_mut(&key)
        {
            read.load = Load::Failed(WriteError::AgentUnavailable.to_string());
        }
    }

    /// The selected device's cached running version and update offer.
    pub(crate) fn current_firmware_load(&self) -> Load<FirmwareStatus> {
        self.current_record()
            .and_then(|record| {
                self.firmware
                    .reads
                    .get(&record.device_key())
                    .filter(|read| record.route.as_ref() == Some(&read.route))
            })
            .map(|read| read.load.clone())
            .unwrap_or_default()
    }

    /// Whether an observed job or a local admission is still active.
    pub(crate) fn firmware_busy(&self) -> bool {
        self.firmware
            .observed
            .as_ref()
            .is_some_and(|job| !job.update.phase.is_finished())
            || self
                .firmware
                .start
                .as_ref()
                .is_some_and(|start| start.result.is_ok())
    }

    /// Whether the selected device is awaiting a new job snapshot.
    pub(crate) fn current_firmware_starting(&self) -> bool {
        self.firmware
            .start
            .as_ref()
            .is_some_and(|start| self.is_current_device(&start.key) && start.result.is_ok())
    }

    /// The selected physical device's latest observed update.
    pub(crate) fn current_firmware_update(&self) -> Option<&FirmwareUpdate> {
        self.firmware
            .observed
            .as_ref()
            .filter(|job| {
                job.key
                    .as_ref()
                    .is_some_and(|key| self.is_current_device(key))
            })
            .map(|job| &job.update)
    }

    /// The selected device's last request-admission failure.
    pub(crate) fn current_firmware_start_failure(&self) -> Option<&FirmwareStartFailure> {
        let record = self.current_record()?;
        self.firmware
            .start
            .as_ref()
            .filter(|start| {
                self.is_current_device(&start.key) && record.route.as_ref() == Some(&start.route)
            })?
            .result
            .as_ref()
            .err()
    }

    /// Retry the selected device's status when no update owns the transport.
    pub(crate) fn retry_firmware_read(&mut self, key: &DeviceKey) -> StateEvents {
        if !self.is_current_device(key) || self.firmware_busy() {
            return StateEvents::none();
        }
        self.firmware.invalidate(key);
        self.load_current_firmware();
        StateEvent::FirmwareChanged.into()
    }

    /// Accept a response only for the retained route and read flight.
    pub(crate) fn apply_firmware_read(
        &mut self,
        key: &DeviceKey,
        route: &DeviceRoute,
        flight: u64,
        result: Result<FirmwareStatus, WriteError>,
    ) -> StateEvents {
        let Some(read) = self
            .firmware
            .reads
            .get_mut(key)
            .filter(|read| read.route == *route && read.flight == flight)
        else {
            return StateEvents::none();
        };
        read.load = match result {
            Ok(status) => Load::Ready(status),
            Err(error @ WriteError::FeatureUnsupported { .. }) => {
                Load::Unsupported(error.to_string())
            }
            Err(error) => Load::Failed(error.to_string()),
        };
        StateEvent::FirmwareChanged.into()
    }

    /// Confirmation is scoped to the device, route and release the dialog showed.
    pub(crate) fn start_firmware_update(
        &mut self,
        key: &DeviceKey,
        target: FirmwareVersion,
    ) -> StateEvents {
        if self.firmware_busy()
            || !self.is_current_device(key)
            || !matches!(self.agent_link(), AgentLink::Ready(_))
            || !self.firmware_battery_charge(key).is_sufficient()
        {
            return StateEvents::none();
        }
        let Load::Ready(status) = self.current_firmware_load() else {
            return StateEvents::none();
        };
        if status.installation_target() != Some(target) {
            return StateEvents::none();
        }
        let Some(route) = self
            .current_record()
            .filter(|record| record.online)
            .and_then(|record| record.route.clone())
        else {
            return StateEvents::none();
        };
        let flight = self.firmware.flight();
        let prior_job = self.firmware.observed.as_ref().map(|job| job.update.id);
        self.firmware.start = Some(Start {
            key: key.clone(),
            route: route.clone(),
            flight,
            prior_job,
            result: Ok(()),
        });
        if !self.send_ipc(StartFirmwareUpdate {
            key: key.clone(),
            route,
            flight,
        }) && let Some(start) = &mut self.firmware.start
        {
            start.result = Err(FirmwareStartFailure::Unavailable);
        }
        StateEvent::FirmwareChanged.into()
    }

    /// Apply admission acknowledgement without replacing newer observed progress.
    pub(crate) fn apply_firmware_started(
        &mut self,
        key: &DeviceKey,
        route: &DeviceRoute,
        flight: u64,
        result: Result<(), FirmwareStartFailure>,
    ) -> StateEvents {
        let Some(start) =
            self.firmware.start.as_mut().filter(|start| {
                start.key == *key && start.route == *route && start.flight == flight
            })
        else {
            return StateEvents::none();
        };
        let refresh = matches!(
            &result,
            Err(FirmwareStartFailure::Refused(
                FirmwareUpdateRefusal::NoUpdate | FirmwareUpdateRefusal::DeviceNotFound
            ))
        );
        start.result = result;
        if refresh {
            self.firmware.invalidate(key);
            self.load_current_firmware();
        }
        // Acceptance remains pending until the new job's snapshot arrives.
        StateEvent::FirmwareChanged.into()
    }

    /// Adopt progress and invalidate cached versions when a job ends.
    pub(super) fn observe_firmware(&mut self, update: Option<FirmwareUpdate>) -> StateEvents {
        if self.firmware.observed.as_ref().map(|job| &job.update) == update.as_ref() {
            return StateEvents::none();
        }
        // A route does not identify a physical unit. Only retain an existing
        // binding or the key captured by this GUI's own pending admission.
        let key = update.as_ref().and_then(|update| {
            self.firmware
                .observed
                .as_ref()
                .filter(|previous| {
                    previous.update.id == update.id && previous.update.route == update.route
                })
                .and_then(|previous| previous.key.clone())
                .or_else(|| {
                    self.firmware
                        .start
                        .as_ref()
                        .filter(|start| {
                            start.route == update.route
                                && start.prior_job != Some(update.id)
                                // A lost reply leaves admission uncertain;
                                // an explicit refusal proves this is not our job.
                                && !matches!(&start.result, Err(FirmwareStartFailure::Refused(_)))
                        })
                        .map(|start| start.key.clone())
                })
        });
        if let Some(job) = &update {
            if self
                .firmware
                .start
                .as_ref()
                .is_some_and(|start| start.route == job.route && start.prior_job != Some(job.id))
            {
                self.firmware.start = None;
            }
            if job.phase.is_finished() {
                self.firmware
                    .reads
                    .retain(|_, read| read.route != job.route);
            }
        }
        self.firmware.observed = update.map(|update| ObservedFirmware { update, key });
        StateEvent::FirmwareChanged.into()
    }

    /// Every connection fences old read flights. Pending admissions and bound
    /// jobs survive reconnects to the same agent; a new agent resets both.
    pub(crate) fn firmware_reconnected(&mut self, identity: openlogi_ipc::Identity) -> StateEvents {
        self.firmware.reads.clear();
        if self.firmware.agent_identity != Some(identity) {
            self.firmware.start = None;
            self.firmware.observed = None;
        }
        self.firmware.agent_identity = Some(identity);
        StateEvent::FirmwareChanged.into()
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::services::assets::AssetResolver;
    use crate::services::ipc::Command;
    use crate::services::ipc::tests::agent_identity as identity;
    use crate::state::{Sources, tests::direct_inventory};
    use openlogi_core::config::Config;
    use openlogi_core::hid::{FirmwareOffer, FirmwareUpdatePhase};
    use openlogi_ipc::{AgentStatus, InventoryHealth, PROTOCOL_VERSION};

    fn observe(state: &mut AppState, update: Option<FirmwareUpdate>) -> StateEvents {
        state.observe_firmware(update)
    }

    fn ready_status() -> AgentStatus {
        AgentStatus {
            accessibility_granted: true,
            hook_installed: true,
            launch_at_login: false,
            inventory: InventoryHealth::Ready,
            protocol_version: PROTOCOL_VERSION,
            agent_version: "test".into(),
            input_monitoring_granted: true,
            hid_open_failures: false,
            secure_input: openlogi_ipc::SecureInput::Off,
            virtual_keyboard: openlogi_ipc::VirtualKeyboardStatus::Unsupported,
        }
    }

    fn state() -> (AppState, tokio::sync::mpsc::UnboundedReceiver<Command>) {
        let resolver = AssetResolver::new();
        let mut inventory = [direct_inventory([0xa3, 0x93, 0xca, 0xe0])];
        inventory[0].paired[0].battery = Some(openlogi_core::device::BatteryInfo {
            percentage: Some(75),
            level: openlogi_core::device::BatteryLevel::Good,
            status: openlogi_core::device::BatteryStatus::Discharging,
        });
        let (commands, mut received) = tokio::sync::mpsc::unbounded_channel();
        let mut state = AppState::new(Sources {
            inventories: &inventory,
            ..Sources::in_memory(Config::ephemeral(), &resolver, commands)
        });
        let _ = state.set_agent_link(AgentLink::Ready(ready_status()));
        let _ = state.firmware_reconnected(identity(1));
        while received.try_recv().is_ok() {}
        (state, received)
    }

    fn status() -> FirmwareStatus {
        FirmwareStatus {
            running: FirmwareVersion {
                number: 27,
                revision: 1,
                build: 16,
            },
            offer: FirmwareOffer::Available(target()),
        }
    }

    fn target() -> FirmwareVersion {
        FirmwareVersion {
            number: 27,
            revision: 3,
            build: 19,
        }
    }

    fn read(
        state: &mut AppState,
        received: &mut tokio::sync::mpsc::UnboundedReceiver<Command>,
    ) -> ReadFirmware {
        state.load_current_firmware();
        loop {
            if let Command::ReadFirmware(read) =
                received.try_recv().expect("a firmware read was scheduled")
            {
                return read;
            }
        }
    }

    fn ready(
        state: &mut AppState,
        received: &mut tokio::sync::mpsc::UnboundedReceiver<Command>,
    ) -> ReadFirmware {
        let read = read(state, received);
        let _ = state.apply_firmware_read(&read.key, &read.route, read.flight, Ok(status()));
        read
    }

    /// A connected device with a completed firmware status read.
    pub(crate) fn ready_state() -> (AppState, tokio::sync::mpsc::UnboundedReceiver<Command>) {
        let (mut state, mut received) = state();
        let _ = ready(&mut state, &mut received);
        (state, received)
    }

    /// A connected device running the exact catalog release.
    pub(crate) fn ready_current_state() -> (AppState, tokio::sync::mpsc::UnboundedReceiver<Command>)
    {
        let (mut state, mut received) = state();
        let read = read(&mut state, &mut received);
        let _ = state.apply_firmware_read(
            &read.key,
            &read.route,
            read.flight,
            Ok(FirmwareStatus {
                running: target(),
                offer: FirmwareOffer::UpToDate,
            }),
        );
        (state, received)
    }

    #[test]
    fn cold_start_and_new_agent_do_not_bind_history_to_a_replacement_on_the_same_route() {
        for phase in [
            FirmwareUpdatePhase::Preparing,
            FirmwareUpdatePhase::Succeeded,
        ] {
            let (mut original, mut received) = state();
            let read = ready(&mut original, &mut received);
            let job = FirmwareUpdate {
                source: None,
                events: Vec::new(),
                id: 10,
                route: read.route.clone(),
                target: target(),
                phase,
            };
            let _ = original.start_firmware_update(&read.key, target());
            let _ = observe(&mut original, Some(job.clone()));
            let replacement = direct_inventory([1, 2, 3, 4]);
            let (mut cold, _) = state();
            for app in [&mut cold, &mut original] {
                let _ = app.refresh_inventories(
                    std::slice::from_ref(&replacement),
                    &[],
                    &AssetResolver::new(),
                    &[],
                );
                let index = app
                    .devices
                    .records
                    .iter()
                    .position(|record| record.online && record.route.as_ref() == Some(&read.route))
                    .unwrap();
                let _ = app.select_device(index);
                assert_ne!(app.current_record().unwrap().device_key(), read.key);
                let _ = app.firmware_reconnected(identity(2));
                let _ = observe(app, Some(job.clone()));
                assert!(
                    app.current_firmware_update().is_none(),
                    "an identity-less historical job must stay unbound"
                );
                assert_eq!(app.firmware_busy(), !job.phase.is_finished());
            }
        }
    }

    #[test]
    fn same_agent_reconnect_before_first_snapshot_preserves_progress_and_history() {
        for (before_reconnect, after_reconnect) in [
            (None, None),
            (Some(Ok(())), None),
            (Some(Err(FirmwareStartFailure::Unavailable)), None),
            (None, Some(Err(FirmwareStartFailure::Unavailable))),
        ] {
            let (mut state, mut received) = state();
            let read = ready(&mut state, &mut received);
            let _ = state.start_firmware_update(&read.key, target());
            let Command::StartFirmwareUpdate(start) = received.try_recv().unwrap() else {
                panic!("start command");
            };
            if let Some(answer) = before_reconnect {
                let _ =
                    state.apply_firmware_started(&start.key, &start.route, start.flight, answer);
            }
            let _ = state.firmware_reconnected(identity(1));
            if let Some(answer) = after_reconnect {
                let _ =
                    state.apply_firmware_started(&start.key, &start.route, start.flight, answer);
            }
            for phase in [
                FirmwareUpdatePhase::Preparing,
                FirmwareUpdatePhase::Transferring { percent: 10 },
                FirmwareUpdatePhase::Succeeded,
            ] {
                let job = FirmwareUpdate {
                    source: None,
                    id: 42,
                    route: read.route.clone(),
                    target: target(),
                    events: vec![openlogi_core::hid::FirmwareUpdateEvent {
                        timestamp_millis: 42_000,
                        phase: phase.clone(),
                    }],
                    phase,
                };
                let _ = observe(&mut state, Some(job.clone()));
                assert_eq!(state.current_firmware_update(), Some(&job));
                assert!(state.current_firmware_start_failure().is_none());
                assert!(!state.current_firmware_starting());
            }
        }
    }

    #[test]
    fn a_new_agent_cannot_inherit_a_pending_start_before_its_first_snapshot() {
        let (mut state, mut received) = state();
        let read = ready(&mut state, &mut received);
        let _ = state.start_firmware_update(&read.key, target());
        let Command::StartFirmwareUpdate(start) = received.try_recv().unwrap() else {
            panic!("start command");
        };
        let _ = state.firmware_reconnected(identity(2));
        assert!(
            state
                .apply_firmware_started(&start.key, &start.route, start.flight, Ok(()))
                .is_empty()
        );
        let _ = observe(
            &mut state,
            Some(FirmwareUpdate {
                source: None,
                events: vec![],
                id: 42,
                route: read.route,
                target: target(),
                phase: FirmwareUpdatePhase::Preparing,
            }),
        );
        assert!(state.current_firmware_update().is_none());
    }

    #[test]
    fn reconnect_does_not_bind_a_definitively_refused_start() {
        let (mut state, mut received) = state();
        let read = ready(&mut state, &mut received);
        let _ = state.start_firmware_update(&read.key, target());
        let Command::StartFirmwareUpdate(start) = received.try_recv().unwrap() else {
            panic!("start command");
        };
        let _ = state.apply_firmware_started(
            &start.key,
            &start.route,
            start.flight,
            Err(FirmwareStartFailure::Refused(FirmwareUpdateRefusal::Busy)),
        );
        let _ = state.firmware_reconnected(identity(1));
        let _ = observe(
            &mut state,
            Some(FirmwareUpdate {
                source: None,
                events: vec![],
                id: 42,
                route: read.route,
                target: target(),
                phase: FirmwareUpdatePhase::Preparing,
            }),
        );
        assert!(state.current_firmware_update().is_none());
    }

    #[test]
    fn current_firmware_reinstall_admission_requires_the_exact_displayed_target() {
        let (mut state, mut received) = ready_current_state();
        let key = state.current_record().unwrap().device_key();
        assert!(
            state
                .start_firmware_update(&key, status().running)
                .is_empty()
        );
        assert!(
            received.try_recv().is_err(),
            "an unrelated target cannot be admitted"
        );
        assert!(!state.start_firmware_update(&key, target()).is_empty());
        assert!(matches!(
            received.try_recv().unwrap(),
            Command::StartFirmwareUpdate(_)
        ));
        assert!(
            state.start_firmware_update(&key, target()).is_empty(),
            "no duplicate reinstall submission"
        );
        assert!(received.try_recv().is_err());
    }

    #[test]
    fn stale_reads_cannot_replace_a_new_connection_s_reading() {
        let (mut state, mut received) = state();
        let old = read(&mut state, &mut received);
        let _ = state.firmware_reconnected(identity(1));
        let new = read(&mut state, &mut received);
        assert_ne!(old.flight, new.flight);
        assert!(
            state
                .apply_firmware_read(&old.key, &old.route, old.flight, Ok(status()))
                .is_empty()
        );
        assert_eq!(state.current_firmware_load(), Load::Loading);
        let _ = state.apply_firmware_read(&new.key, &new.route, new.flight, Ok(status()));
        assert_eq!(state.current_firmware_load(), Load::Ready(status()));
    }

    #[test]
    fn a_read_for_an_unselected_device_is_cached_until_that_device_is_forgotten() {
        let (mut state, mut received) = state();
        let first = direct_inventory([0xa3, 0x93, 0xca, 0xe0]);
        let mut second = direct_inventory([1, 2, 3, 4]);
        second.receiver.product_id = 0xb024;
        let _ = state.refresh_inventories(&[first, second], &[], &AssetResolver::new(), &[]);
        let read = read(&mut state, &mut received);
        let other = state
            .devices
            .records
            .iter()
            .position(|record| record.config_key != read.key.as_str())
            .unwrap();
        let _ = state.select_device(other);
        assert!(
            !state
                .apply_firmware_read(&read.key, &read.route, read.flight, Ok(status()))
                .is_empty()
        );
        let original = state
            .devices
            .records
            .iter()
            .position(|record| record.config_key == read.key.as_str())
            .unwrap();
        let _ = state.select_device(original);
        assert_eq!(state.current_firmware_load(), Load::Ready(status()));
        while received.try_recv().is_ok() {}
        let _ = state.refresh_inventories(&[], &[], &AssetResolver::new(), &[]);
        assert!(!state.current_record().unwrap().online);
        let record_key = state.current_record().unwrap().record_key();
        let _ = state.forget_device(&record_key);
        assert!(
            state
                .apply_firmware_read(&read.key, &read.route, read.flight, Ok(status()))
                .is_empty()
        );
    }

    #[test]
    fn observation_before_start_reply_wins_and_completion_refreshes_the_version() {
        let (mut state, mut received) = state();
        let read = ready(&mut state, &mut received);
        assert!(!state.start_firmware_update(&read.key, target()).is_empty());
        let Command::StartFirmwareUpdate(start) = received.try_recv().unwrap() else {
            panic!("start command");
        };
        assert!(
            state.start_firmware_update(&read.key, target()).is_empty(),
            "no duplicate submission"
        );
        let job = FirmwareUpdate {
            source: None,
            events: Vec::new(),
            id: 42,
            route: read.route.clone(),
            target: target(),
            phase: FirmwareUpdatePhase::Restarting,
        };
        let _ = observe(&mut state, Some(job.clone()));
        assert!(
            state
                .apply_firmware_started(
                    &start.key,
                    &start.route,
                    start.flight,
                    Err(FirmwareStartFailure::Unavailable)
                )
                .is_empty(),
            "late request failure cannot replace observed progress"
        );
        assert!(state.firmware_busy());
        state.load_current_firmware();
        assert!(
            received.try_recv().is_err(),
            "no reads wait behind the active flash"
        );
        let _ = observe(
            &mut state,
            Some(FirmwareUpdate {
                phase: FirmwareUpdatePhase::Succeeded,
                ..job
            }),
        );
        assert!(!state.firmware_busy());
        assert_eq!(state.current_firmware_load(), Load::Unknown);
        let refresh = self::read(&mut state, &mut received);
        let updated = FirmwareStatus {
            running: target(),
            offer: FirmwareOffer::UpToDate,
        };
        let _ = state.apply_firmware_read(
            &refresh.key,
            &refresh.route,
            refresh.flight,
            Ok(updated.clone()),
        );
        assert_eq!(state.current_firmware_load(), Load::Ready(updated));
    }

    #[test]
    fn an_old_terminal_cannot_finish_a_new_admission_and_start_failure_is_retryable() {
        let (mut state, mut received) = state();
        let read = ready(&mut state, &mut received);
        let old = FirmwareUpdate {
            source: None,
            events: Vec::new(),
            id: 5,
            route: read.route.clone(),
            target: target(),
            phase: FirmwareUpdatePhase::Failed(
                openlogi_core::hid::FirmwareUpdateFailure::Unsupported,
            ),
        };
        let _ = observe(&mut state, Some(old.clone()));
        let _ = ready(&mut state, &mut received);
        let _ = state.start_firmware_update(&read.key, target());
        let Command::StartFirmwareUpdate(start) = received.try_recv().unwrap() else {
            panic!("start command");
        };
        let _ = observe(&mut state, Some(old));
        assert!(state.current_firmware_starting());
        let _ = state.apply_firmware_started(
            &start.key,
            &start.route,
            start.flight,
            Err(FirmwareStartFailure::Refused(FirmwareUpdateRefusal::Busy)),
        );
        assert!(!state.current_firmware_starting());
        assert_eq!(
            state.current_firmware_start_failure(),
            Some(&FirmwareStartFailure::Refused(FirmwareUpdateRefusal::Busy))
        );
        assert!(!state.start_firmware_update(&read.key, target()).is_empty());
    }

    #[test]
    fn a_completed_job_stays_with_its_device_when_another_device_reuses_the_route() {
        let (mut state, mut received) = state();
        let read = ready(&mut state, &mut received);
        let _ = state.start_firmware_update(&read.key, target());
        let completed = FirmwareUpdate {
            source: None,
            events: Vec::new(),
            id: 7,
            route: read.route.clone(),
            target: target(),
            phase: FirmwareUpdatePhase::Succeeded,
        };
        let _ = observe(&mut state, Some(completed.clone()));
        let mut replacement = direct_inventory([1, 2, 3, 4]);
        replacement.paired[0].battery = state.current_record().unwrap().battery.clone();
        let _ = state.refresh_inventories(&[replacement], &[], &AssetResolver::new(), &[]);
        let replacement_index = state
            .devices
            .records
            .iter()
            .position(|record| record.online && record.route.as_ref() == Some(&read.route))
            .unwrap();
        let _ = state.select_device(replacement_index);
        assert!(
            state.current_firmware_update().is_none(),
            "route reuse cannot inherit another device's result"
        );
        let _ = state.firmware_reconnected(identity(1));
        let _ = observe(&mut state, Some(completed));
        assert!(
            state.current_firmware_update().is_none(),
            "same-agent reconnect keeps the old physical binding"
        );
        let replacement = ready(&mut state, &mut received);
        assert!(
            !state
                .start_firmware_update(&replacement.key, target())
                .is_empty()
        );
        let _ = observe(
            &mut state,
            Some(FirmwareUpdate {
                source: None,
                events: Vec::new(),
                id: 8,
                route: read.route,
                target: target(),
                phase: FirmwareUpdatePhase::Preparing,
            }),
        );
        assert_eq!(
            state.current_firmware_update().unwrap().id,
            8,
            "a new job can bind the new device"
        );
    }

    #[test]
    fn a_new_agent_run_resets_the_job_namespace_and_old_read_flights() {
        let (mut state, mut received) = state();
        let read = ready(&mut state, &mut received);
        let _ = state.start_firmware_update(&read.key, target());
        let _ = observe(
            &mut state,
            Some(FirmwareUpdate {
                source: None,
                events: Vec::new(),
                id: 1,
                route: read.route.clone(),
                target: target(),
                phase: FirmwareUpdatePhase::Preparing,
            }),
        );
        assert!(state.current_firmware_update().is_some());
        let _ = state.firmware_reconnected(identity(2));
        assert!(state.current_firmware_update().is_none());
        assert!(
            state
                .apply_firmware_read(&read.key, &read.route, read.flight, Ok(status()))
                .is_empty()
        );
        let _ = ready(&mut state, &mut received);
        let _ = state.start_firmware_update(&read.key, target());
        let _ = observe(
            &mut state,
            Some(FirmwareUpdate {
                source: None,
                events: Vec::new(),
                id: 1,
                route: read.route,
                target: target(),
                phase: FirmwareUpdatePhase::Preparing,
            }),
        );
        assert!(
            state.current_firmware_update().is_some(),
            "the successor can start again at job one"
        );
    }

    #[test]
    fn restart_keeps_the_device_after_normal_inventory_miss_grace() {
        let (mut state, mut received) = state();
        let read = ready(&mut state, &mut received);
        let _ = state.start_firmware_update(&read.key, target());
        let _ = observe(
            &mut state,
            Some(FirmwareUpdate {
                source: None,
                events: Vec::new(),
                id: 9,
                route: read.route.clone(),
                target: target(),
                phase: FirmwareUpdatePhase::Restarting,
            }),
        );
        let resolver = AssetResolver::new();
        for _ in 0..5 {
            let _ = state.refresh_inventories(&[], &[], &resolver, &[]);
        }
        assert_eq!(
            state.current_record().unwrap().route.as_ref(),
            Some(&read.route)
        );
        assert_eq!(
            state.current_firmware_update().unwrap().phase,
            FirmwareUpdatePhase::Restarting
        );
        let failed =
            FirmwareUpdatePhase::Failed(openlogi_core::hid::FirmwareUpdateFailure::NotActivated);
        let _ = observe(
            &mut state,
            Some(FirmwareUpdate {
                source: None,
                events: Vec::new(),
                id: 9,
                route: read.route.clone(),
                target: target(),
                phase: failed.clone(),
            }),
        );
        for _ in 0..5 {
            let _ = state.refresh_inventories(&[], &[], &resolver, &[]);
        }
        assert!(!state.current_record().unwrap().online);
        assert_eq!(state.current_firmware_update().unwrap().phase, failed);
    }
}
