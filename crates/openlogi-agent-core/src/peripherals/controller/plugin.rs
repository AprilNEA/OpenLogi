use super::{
    ApplicationStatus, Arc, BTreeMap, Broker, Capability, CapabilityId, Config, ControlId,
    Controller, DriverState, Entry, Guard, InputTransition, PeripheralError, PluginSession,
    SessionContext, TargetKind, extension_values, invalid, native_key, native_keys, runtime,
    set_capability_status, set_status, wire,
};

impl Controller {
    pub(super) async fn configure(
        &mut self,
        entry: &mut Entry,
        config: &Config,
    ) -> Result<(), PeripheralError> {
        let rule = config.peripheral_rule(&entry.record)?.cloned();
        let changed = entry.rule != rule;
        if changed {
            if let DriverState::Plugin(plugin) = &mut entry.state {
                plugin.input = self.dispatcher.peripheral_session(&entry.record.session);
            }
            if let DriverState::Fault(_) = &entry.state {
                entry.state = DriverState::Idle;
            }
        }
        entry.rule = rule;
        if matches!(entry.state, DriverState::SelectionError(_)) {
            entry.state = DriverState::Idle;
        }
        if !self.gate.allows_io() {
            set_status(
                &mut entry.record,
                self.revision(),
                &ApplicationStatus::Failed(PeripheralError::Suspended),
            );
            return Ok(());
        }
        let builtin =
            openlogi_core::peripheral::builtin::owns_inventory(&entry.record.driver.driver);
        let disabled = entry.rule.as_ref().is_some_and(|rule| !rule.enabled);
        if builtin && disabled && matches!(entry.state, DriverState::Builtin) {
            entry.state = DriverState::Idle;
        }
        let claims: Vec<_> = entry
            .record
            .endpoints
            .iter()
            .map(|endpoint| endpoint.parent.as_ref().unwrap_or(&endpoint.id).clone())
            .collect();
        if self.retiring.contains_key(&entry.record.session)
            || self.claims.occupied(&entry.record.session, &claims)
        {
            set_status(
                &mut entry.record,
                self.revision(),
                &ApplicationStatus::RestorePending,
            );
            return Ok(());
        }
        if (!builtin || disabled) && !self.handoff_ready(entry)? {
            return Ok(());
        }
        if entry.rule.as_ref().is_some_and(|r| !r.enabled)
            && entry.record.driver.driver.as_ref()
                != openlogi_device_registry::native_remap::NATIVE_REMAP_DRIVER_ID
        {
            self.stop_entry(entry, wire::DetachReason::Suspended).await;
            let status = if self.retiring.contains_key(&entry.record.session) {
                ApplicationStatus::RestorePending
            } else {
                ApplicationStatus::Disabled
            };
            set_status(&mut entry.record, self.revision(), &status);
            return Ok(());
        }
        if let DriverState::Fault(error) = &entry.state {
            entry.record.driver_error = Some(error.clone());
            set_status(
                &mut entry.record,
                self.revision(),
                &ApplicationStatus::Failed(error.clone()),
            );
            return Ok(());
        }
        if let DriverState::Plugin(plugin) = &mut entry.state {
            if changed {
                plugin.broker.configure(entry.rule.clone());
                return self.apply_plugin(entry).await;
            }
            return Ok(());
        }
        if !matches!(entry.state, DriverState::Idle) {
            return Ok(());
        }
        if builtin {
            self.claims.acquire(&entry.record.session, &claims)?;
            self.ownership.release(&entry.record.session);
            entry.state = DriverState::Builtin;
            return Ok(());
        }
        self.claims.acquire(&entry.record.session, &claims)?;
        if entry.registration.descriptor.driver().id.as_ref()
            == openlogi_device_registry::native_remap::NATIVE_REMAP_DRIVER_ID
        {
            entry.state = DriverState::Native;
            return Ok(());
        }
        self.attach_plugin(entry).await
    }

    fn handoff_ready(&self, entry: &mut Entry) -> Result<bool, PeripheralError> {
        let Some(handoff) = &entry.handoff else {
            return Ok(true);
        };
        self.ownership.reserve(
            &entry.record.session,
            handoff.nodes.clone(),
            handoff.routes.clone(),
        )?;
        if self.ownership.ready(&entry.record.session) {
            return Ok(true);
        }
        entry.record.driver_error = Some(PeripheralError::DriverUnavailable(
            "waiting for the built-in driver to restore firmware and release its transport".into(),
        ));
        set_status(
            &mut entry.record,
            self.revision(),
            &ApplicationStatus::RestorePending,
        );
        Ok(false)
    }

    async fn attach_plugin(&mut self, entry: &mut Entry) -> Result<(), PeripheralError> {
        let plugin = self
            .sources
            .plugins
            .get(&entry.record.driver.driver)
            .ok_or_else(|| {
                PeripheralError::DriverUnavailable(entry.record.driver.driver.to_string())
            })?;
        if entry.record.driver.digest.as_deref() != Some(plugin.package.digest()) {
            return Err(PeripheralError::DriverUnavailable(
                "catalog implementation digest changed".into(),
            ));
        }
        let parameters = plugin
            .manifest
            .validate_descriptor(&entry.registration.descriptor)
            .map_err(invalid)?;
        let settings = extension_values(entry.rule.as_ref())?;
        let settings =
            openlogi_plugin::manifest::validate_values(&plugin.manifest.settings, &settings, true)
                .map_err(invalid)?;
        let context = SessionContext {
            session: entry.record.session.clone(),
            model: entry.record.model.clone(),
            endpoints: entry
                .roles
                .iter()
                .map(|(role, e)| (role.clone(), e.endpoint.clone()))
                .collect(),
            permissions: plugin.manifest.permissions.clone(),
            settings,
            native_keys: native_keys(entry.rule.as_ref(), &plugin.manifest.permissions)?,
        };
        let mut guest = plugin
            .component
            .attach(Arc::clone(&plugin.manifest), context, parameters)
            .await?;
        entry.record.capabilities = guest.capabilities().to_vec();
        if let Some(rule) = &entry.rule {
            rule.validate_for(&entry.record)?;
        }
        let guard = Guard::new(
            self.handle.clone(),
            &entry.record,
            entry.rule.clone(),
            self.gate.clone(),
        );
        guard.check_desired()?;
        let mut broker = Broker::new(
            entry.roles.clone(),
            plugin.manifest.permissions.clone(),
            guard,
        );
        let requests = broker.submit(guest.pending())?;
        entry.state = DriverState::Plugin(Box::new(PluginSession {
            input: self.dispatcher.peripheral_session(&entry.record.session),
            guest,
            broker,
            pending: BTreeMap::new(),
        }));
        self.native_requests(entry, requests)?;
        self.apply_plugin(entry).await
    }

    pub(super) async fn apply_plugin(&mut self, entry: &mut Entry) -> Result<(), PeripheralError> {
        let revision = self.revision();
        set_status(&mut entry.record, revision, &ApplicationStatus::Disabled);
        let Some(rule) = entry.rule.clone() else {
            return Ok(());
        };
        for capability in entry.record.capabilities.clone() {
            let Some(settings) = rule.capabilities.get(&capability.id) else {
                continue;
            };
            if settings.version != capability.version || capability.unavailable.is_some() {
                continue;
            }
            let commands = match &capability.capability {
                Capability::Extension(_) if rule.enabled => vec![wire::Command::Settings(
                    runtime::to_wire_settings(&settings.values),
                )],
                Capability::InputRemap(input) if input.targets == TargetKind::KeyboardKey => input
                    .controls
                    .iter()
                    .map(|control| {
                        let key = rule
                            .enabled
                            .then(|| settings.bindings.get(&control.id))
                            .flatten()
                            .map(native_key)
                            .transpose()?
                            .map(|key| u16::from(key.code()));
                        Ok(wire::Command::Key(wire::KeyBinding {
                            control: control.id.to_string(),
                            key,
                        }))
                    })
                    .collect::<Result<Vec<_>, PeripheralError>>()?,
                Capability::InputRemap(_) if rule.enabled => {
                    set_capability_status(
                        &mut entry.record,
                        &capability.id,
                        revision,
                        ApplicationStatus::Applied,
                    );
                    Vec::new()
                }
                _ => Vec::new(),
            };
            for command in commands {
                let request = self.request;
                self.request += 1;
                let DriverState::Plugin(plugin) = &mut entry.state else {
                    return Err(PeripheralError::StaleSession);
                };
                plugin.broker.check()?;
                plugin
                    .pending
                    .insert(request, (capability.id.clone(), revision));
                plugin.broker.command_deadline(request);
                let update = plugin
                    .guest
                    .apply(wire::Request {
                        id: request,
                        revision,
                        capability: capability.id.to_string(),
                        command,
                    })
                    .await?;
                self.update(entry, update)?;
            }
        }
        Ok(())
    }

    pub(super) fn update(
        &mut self,
        entry: &mut Entry,
        update: runtime::Update,
    ) -> Result<(), PeripheralError> {
        if update.session != entry.record.session {
            return Err(PeripheralError::StaleSession);
        }
        let DriverState::Plugin(plugin) = &mut entry.state else {
            return Err(PeripheralError::StaleSession);
        };
        plugin.broker.check()?;
        for update in update.updates {
            match update {
                wire::Update::Input(event) => {
                    let capability = CapabilityId::try_new(event.capability).map_err(invalid)?;
                    let control = ControlId::try_new(event.control).map_err(invalid)?;
                    let contract = entry
                        .record
                        .capabilities
                        .iter()
                        .find(|c| c.id == capability && c.unavailable.is_none());
                    let binding = contract
                        .and_then(|c| {
                            entry
                                .rule
                                .as_ref()
                                .filter(|r| r.enabled)
                                .and_then(|r| r.capabilities.get(&capability))
                                .filter(|s| s.version == c.version)
                        })
                        .and_then(|s| s.bindings.get(&control));
                    if let Some(action) = binding {
                        let transition = match event.transition {
                            wire::Transition::Press => InputTransition::Press,
                            wire::Transition::Release => InputTransition::Release,
                            wire::Transition::Trigger => InputTransition::Trigger,
                        };
                        if !plugin.input.send(&capability, &control, transition, action) {
                            return Err(PeripheralError::ResourceLimit(
                                "input action queue".into(),
                            ));
                        }
                    }
                }
                wire::Update::Completed(completion) => {
                    let (capability, revision) = plugin
                        .pending
                        .remove(&completion.request)
                        .ok_or_else(|| invalid("unknown driver command completion"))?;
                    let status = match completion.outcome {
                        Ok(()) => ApplicationStatus::Applied,
                        Err(error) => {
                            ApplicationStatus::Failed(PeripheralError::WriteFailed(error.message))
                        }
                    };
                    if revision == self.revision() {
                        set_capability_status(&mut entry.record, &capability, revision, status);
                    }
                }
                wire::Update::Setting(setting) => {
                    let values = runtime::from_wire_settings(&[setting])?;
                    for capability in &mut entry.record.capabilities {
                        if let Capability::Extension(fields) = &capability.capability {
                            for (key, value) in &values {
                                if fields.contains_key(key) {
                                    capability.values.insert(key.clone(), value.clone());
                                }
                            }
                        }
                    }
                }
            }
        }
        let native = plugin.broker.submit(update.requests)?;
        self.native_requests(entry, native)
    }
}
