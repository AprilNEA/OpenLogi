//! Scripted status transitions. No descriptor or component performs device I/O.

use openlogi_agent_core::peripherals::sources::Sources;
use openlogi_core::peripheral::{
    ApplicationStatus, CapabilityId, ConnectionStatus, DescriptorId, DriverId, DriverSource,
    EndpointId, ModelId, OperationStatus, PeripheralConfig, PeripheralError, PeripheralSnapshot,
    SessionId, VerificationStatus,
};

use super::{Config, MockClock, State, built_in_profile};

pub(super) fn scenario(name: &str) -> Result<State, String> {
    let mut state = State::new(
        built_in_profile()?,
        MockClock::Test(std::time::Duration::ZERO),
    )
    .map_err(|e| e.to_string())?;
    state.peripheral_only = true;
    let sources = Sources::new().map_err(|e| e.to_string())?;
    let registration = sources
        .catalog
        .entries()
        .find(|entry| entry.descriptor.model().as_ref() == "dji.mic3.rx")
        .ok_or("compiled DJI descriptor is missing")?;
    let mut record = registration
        .record(
            SessionId {
                endpoint: EndpointId("mock-mic".into()),
                generation: 1,
            },
            Vec::new(),
        )
        .map_err(|e| e.to_string())?;
    let application = match name {
        "mic-only" => ApplicationStatus::Disabled,
        "mic-offline" => {
            record.connection = ConnectionStatus::Offline;
            ApplicationStatus::Failed(PeripheralError::Offline)
        }
        "restore-conflict" => ApplicationStatus::Failed(PeripheralError::ExternalModification),
        "plugin-fault" => {
            record.name = "Plugin test button".into();
            record.model = ModelId::try_new("example.counter-button").map_err(|e| e.to_string())?;
            record.driver.driver =
                DriverId::try_new("dev.example.counter-button").map_err(|e| e.to_string())?;
            record.driver.descriptor =
                DescriptorId::try_new("dev.example.counter-button.test-device")
                    .map_err(|e| e.to_string())?;
            record.driver.source = DriverSource::Plugin(record.driver.driver.to_string());
            record.capabilities.clear();
            let error = PeripheralError::ResourceLimit("scripted plugin fuel exhaustion".into());
            record.driver_error = Some(error.clone());
            ApplicationStatus::Failed(error)
        }
        _ => {
            return Err(format!(
                "unknown scenario {name}; choose mic-only, mic-offline, restore-conflict, or plugin-fault"
            ));
        }
    };
    record.operations.push(OperationStatus {
        capability: CapabilityId::try_new("input-remap/main").map_err(|e| e.to_string())?,
        revision: 0,
        application,
        verification: VerificationStatus::NotObserved,
    });
    state.peripherals.devices.push(record);
    Ok(state)
}

pub(super) fn load_config() -> Result<Config, PeripheralError> {
    Config::load_or_default().map_err(|error| PeripheralError::ConfigWriteFailed(error.to_string()))
}

pub(super) fn reconcile(snapshot: &mut PeripheralSnapshot, config: &Config) {
    for record in &mut snapshot.devices {
        let rule = config
            .peripheral_rule(record)
            .map(Option::<&PeripheralConfig>::cloned);
        for status in &mut record.operations {
            if status.application
                == ApplicationStatus::Failed(PeripheralError::ExternalModification)
            {
                continue;
            }
            status.application = if record.connection != ConnectionStatus::Online {
                ApplicationStatus::Failed(PeripheralError::Offline)
            } else if let Some(error) = &record.driver_error {
                ApplicationStatus::Failed(error.clone())
            } else {
                match &rule {
                    Err(error) => ApplicationStatus::Failed(error.clone()),
                    Ok(Some(rule))
                        if rule.enabled
                            && rule.capabilities.get(&status.capability).is_some_and(
                                |settings| settings.version == 1 && !settings.bindings.is_empty(),
                            ) =>
                    {
                        ApplicationStatus::Applied
                    }
                    _ if status.application == ApplicationStatus::Applied => {
                        ApplicationStatus::Restored
                    }
                    _ => ApplicationStatus::Disabled,
                }
            };
            status.verification = if status.application == ApplicationStatus::Applied {
                VerificationStatus::WaitingForPress
            } else {
                VerificationStatus::NotObserved
            };
            status.revision += 1;
        }
    }
}

pub(super) fn retry(
    snapshot: &mut PeripheralSnapshot,
    config: &Config,
    session: &SessionId,
) -> Result<(), PeripheralError> {
    let record = snapshot
        .devices
        .iter_mut()
        .find(|r| &r.session == session)
        .ok_or(PeripheralError::StaleSession)?;
    if record.connection != ConnectionStatus::Online {
        return Err(PeripheralError::Offline);
    }
    record.driver_error = None;
    reconcile(snapshot, config);
    Ok(())
}

pub(super) fn resolve(
    snapshot: &mut PeripheralSnapshot,
    config: &Config,
    id: &str,
) -> Result<(), PeripheralError> {
    let mut found = false;
    for record in &mut snapshot.devices {
        if config.peripheral_rule(record)?.is_some_and(|r| r.id == id) {
            found = true;
            for status in &mut record.operations {
                status.application = ApplicationStatus::Pending;
            }
        }
    }
    if !found {
        return Err(PeripheralError::InvalidSettings(
            "unknown peripheral rule".into(),
        ));
    }
    reconcile(snapshot, config);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use openlogi_core::binding::Action;
    use openlogi_core::peripheral::Capability;

    #[test]
    fn scripted_mapping_distinguishes_saved_applied_conflicted_and_offline() {
        let mut state = scenario("mic-only").unwrap();
        let record = state.peripherals.devices[0].clone();
        let capability = record.capabilities[0].id.clone();
        let Capability::InputRemap(input) = &record.capabilities[0].capability else {
            panic!("input capability")
        };
        let mut config = Config::ephemeral();
        config
            .set_peripheral_binding(
                &record,
                &capability,
                input.controls[0].id.clone(),
                Some(Action::CustomShortcut("F18".parse().unwrap())),
            )
            .unwrap();
        config.set_peripheral_enabled(&record, true).unwrap();
        reconcile(&mut state.peripherals, &config);
        let status = &state.peripherals.devices[0].operations[0];
        assert_eq!(status.application, ApplicationStatus::Applied);
        assert_eq!(status.verification, VerificationStatus::WaitingForPress);
        config.set_peripheral_enabled(&record, false).unwrap();
        reconcile(&mut state.peripherals, &config);
        assert_eq!(
            state.peripherals.devices[0].operations[0].application,
            ApplicationStatus::Restored
        );
        let mut conflict = scenario("restore-conflict").unwrap();
        reconcile(&mut conflict.peripherals, &config);
        assert_eq!(
            conflict.peripherals.devices[0].operations[0].application,
            ApplicationStatus::Failed(PeripheralError::ExternalModification)
        );
        resolve(
            &mut conflict.peripherals,
            &config,
            &config.peripherals[0].id,
        )
        .unwrap();
        assert_eq!(
            conflict.peripherals.devices[0].operations[0].application,
            ApplicationStatus::Disabled
        );
        let mut offline = scenario("mic-offline").unwrap();
        assert_eq!(
            retry(&mut offline.peripherals, &config, &record.session),
            Err(PeripheralError::Offline)
        );
        let mut plugin = scenario("plugin-fault").unwrap();
        assert!(plugin.peripherals.devices[0].driver_error.is_some());
        retry(
            &mut plugin.peripherals,
            &Config::ephemeral(),
            &record.session,
        )
        .unwrap();
        assert!(plugin.peripherals.devices[0].driver_error.is_none());
    }
}
