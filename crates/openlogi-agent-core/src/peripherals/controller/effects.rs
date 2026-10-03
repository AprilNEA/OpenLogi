use super::{
    ApplicationStatus, BTreeMap, BTreeSet, Capability, CatalogDiagnostic, Controller,
    DesiredEffect, DriverState, EffectKey, Entry, HidUsage, HostRequest, InputSource, MappingScope,
    MappingUsage, PeripheralError, Permission, invalid, native_key, set_capability_status,
};

impl Controller {
    pub(super) fn native_requests(
        &mut self,
        entry: &mut Entry,
        requests: Vec<HostRequest>,
    ) -> Result<(), PeripheralError> {
        for request in requests {
            let HostRequest::NativeRemap {
                request,
                control,
                key,
            } = request
            else {
                continue;
            };
            let plugin = self
                .sources
                .plugins
                .get(&entry.record.driver.driver)
                .ok_or(PeripheralError::StaleSession)?;
            let (role, source) = plugin
                .manifest
                .permissions
                .iter()
                .find_map(|p| match p {
                    Permission::NativeRemap { endpoint, controls } => controls
                        .iter()
                        .find(|c| c.id == control)
                        .map(|c| (endpoint, c.source.usage())),
                    Permission::Hid { .. } => None,
                })
                .ok_or_else(|| {
                    PeripheralError::PermissionDenied("undeclared native control".into())
                })?;
            let endpoint = entry.roles.get(role).ok_or(PeripheralError::StaleSession)?;
            let capability = entry.record.capabilities.iter().find(|c| matches!(
                &c.capability, Capability::InputRemap(input) if input.controls.iter().any(|c| c.id == control)
            )).ok_or_else(|| invalid("native effect has no declared capability"))?.id.clone();
            let effect = EffectKey {
                scope: MappingScope {
                    vendor_id: endpoint.endpoint.vendor_id,
                    product_id: endpoint.endpoint.product_id,
                },
                source: source.into(),
            };
            let owner = entry.rule.as_ref().map_or_else(
                || format!("plugin-{}", entry.record.driver.descriptor),
                |r| r.id.clone(),
            );
            let target = key.map(|usage| MappingUsage::from(HidUsage { page: 7, usage }));
            let DriverState::Plugin(session) = &mut entry.state else {
                return Err(PeripheralError::StaleSession);
            };
            session.broker.check()?;
            let intent = self
                .plugin_effects
                .entry((effect, entry.record.session.clone()))
                .or_insert_with(|| DesiredEffect {
                    owner: owner.clone(),
                    target,
                    session: entry.record.session.clone(),
                    capability,
                    requests: Vec::new(),
                });
            if intent.owner != owner || intent.target != target {
                for old in intent.requests.drain(..) {
                    session
                        .broker
                        .completed(old, Err(PeripheralError::StaleSession));
                }
                intent.owner = owner;
                intent.target = target;
            }
            intent.requests.push(request);
        }
        // All sessions contribute intents before the shared journal performs I/O.
        Ok(())
    }

    pub(super) async fn reconcile_mappings(&mut self) {
        self.drain_retired_inputs().await;
        let desired = self.desired_mappings();
        let revision = self.revision();
        let keys: BTreeSet<_> = self
            .mappings
            .effects()
            .chain(desired.keys().copied())
            .collect();
        let observed_revision = self.handle.discovery.borrow().revision;
        let mut transferred = BTreeMap::new();
        for key in keys {
            let intents = desired.get(&key).map_or(&[][..], Vec::as_slice);
            let handle = self.handle.clone();
            let gate = self.gate.clone();
            let result = match agreed_intent(intents) {
                Err(error) => Err(error),
                Ok((owner, target)) => {
                    self.mappings
                        .reconcile(key, owner, target, || {
                            gate.ensure_allowed()
                                .map_err(|_| PeripheralError::Suspended)?;
                            if handle.desired.borrow().revision != revision
                                || handle.discovery.borrow().revision != observed_revision
                            {
                                return Err(PeripheralError::StaleSession);
                            }
                            Ok(())
                        })
                        .await
                }
            };
            if handle.desired.borrow().revision != revision
                || handle.discovery.borrow().revision != observed_revision
            {
                break;
            }
            if matches!(result, Ok(ApplicationStatus::Applied))
                && let Ok((owner, Some(_))) = agreed_intent(intents)
            {
                transferred.insert(key, owner.to_owned());
            }
            for intent in intents {
                if let Some(entry) = self
                    .entries
                    .get_mut(&intent.session.endpoint)
                    .filter(|e| e.record.session == intent.session)
                {
                    set_capability_status(
                        &mut entry.record,
                        &intent.capability,
                        revision,
                        result.clone().unwrap_or_else(ApplicationStatus::Failed),
                    );
                    if let DriverState::Plugin(plugin) = &mut entry.state {
                        for request in &intent.requests {
                            plugin
                                .broker
                                .completed(*request, result.clone().map(|_| ()));
                        }
                    }
                }
                if let Some(effect) = self.plugin_effects.get_mut(&(key, intent.session.clone())) {
                    effect.requests.clear();
                }
            }
            if intents.is_empty()
                && let Err(error) = result
            {
                self.diagnostic(format!("mapping recovery {key:?}"), error);
            }
        }
        let effects: BTreeSet<_> = self.mappings.effects().collect();
        let mut released = Vec::new();
        for (session, pending) in &mut self.retiring {
            if pending.settled(&effects, &transferred) {
                released.push(session.clone());
            }
        }
        for session in released {
            let retired = self.retiring.remove(&session);
            self.claims.release(&session);
            if retired.is_some_and(|pending| pending.release_transport) {
                self.ownership.release(&session);
            }
            self.ownership.reconcile();
        }
    }

    async fn drain_retired_inputs(&mut self) {
        if !self.retiring.values().any(|pending| pending.input_pending) {
            return;
        }
        match self
            .dispatcher
            .drain_buttons(crate::runtime::ButtonDrain::Queued)
            .await
        {
            Ok(()) => {
                for pending in self.retiring.values_mut() {
                    pending.input_pending = false;
                }
                self.sources
                    .diagnostics
                    .retain(|d| d.source != "input cleanup");
            }
            Err(error) => self.diagnostic(
                "input cleanup".into(),
                PeripheralError::PluginFault(format!(
                    "waiting for input handlers to finish: {error}"
                )),
            ),
        }
    }

    fn desired_mappings(&mut self) -> BTreeMap<EffectKey, Vec<DesiredEffect>> {
        let mut desired: BTreeMap<EffectKey, Vec<DesiredEffect>> = BTreeMap::new();
        for ((key, _), effect) in &self.plugin_effects {
            desired.entry(*key).or_default().push(effect.clone());
        }
        for entry in self.entries.values_mut() {
            if !matches!(entry.state, DriverState::Native) {
                continue;
            }
            let rule = entry.rule.as_ref().filter(|r| r.enabled);
            for capability in &entry.record.capabilities {
                let Capability::InputRemap(input) = &capability.capability else {
                    continue;
                };
                for control in &input.controls {
                    let InputSource::HidUsage(source) = control.source else {
                        continue;
                    };
                    let Some(endpoint) = entry.record.endpoints.first() else {
                        continue;
                    };
                    let effect = EffectKey {
                        scope: MappingScope {
                            vendor_id: endpoint.vendor_id,
                            product_id: endpoint.product_id,
                        },
                        source: source.into(),
                    };
                    let action = rule
                        .and_then(|r| r.capabilities.get(&capability.id))
                        .filter(|s| {
                            s.version == capability.version && capability.unavailable.is_none()
                        })
                        .and_then(|s| s.bindings.get(&control.id));
                    // Config::peripheral_rule already validates each native target.
                    let target = match action.map(native_key).transpose() {
                        Ok(key) => key.map(MappingUsage::from),
                        Err(error) => {
                            self.sources.diagnostics.push(CatalogDiagnostic {
                                source: entry.record.driver.descriptor.to_string(),
                                retained: false,
                                error,
                            });
                            continue;
                        }
                    };
                    desired.entry(effect).or_default().push(DesiredEffect {
                        owner: entry.rule.as_ref().map_or_else(
                            || entry.record.driver.descriptor.to_string(),
                            |r| r.id.clone(),
                        ),
                        target,
                        session: entry.record.session.clone(),
                        capability: capability.id.clone(),
                        requests: Vec::new(),
                    });
                }
            }
        }
        desired
    }

    pub(super) async fn restore_all(&mut self) {
        for key in self.mappings.effects().collect::<Vec<_>>() {
            if let Err(error) = self
                .mappings
                .reconcile(key, "recovery", None, || {
                    self.gate
                        .ensure_allowed()
                        .map_err(|_| PeripheralError::Suspended)
                })
                .await
            {
                self.diagnostic(format!("mapping recovery {key:?}"), error);
            }
        }
        self.publish();
    }
}

impl super::Retirement {
    fn settled(
        &mut self,
        outstanding: &BTreeSet<EffectKey>,
        transferred: &BTreeMap<EffectKey, String>,
    ) -> bool {
        self.effects
            .retain(|key, owner| outstanding.contains(key) && transferred.get(key) != Some(owner));
        self.effects.is_empty()
            && !self.input_pending
            && self.io.as_ref().is_none_or(|io| io.strong_count() == 0)
    }
}

fn agreed_intent(
    intents: &[DesiredEffect],
) -> Result<(&str, Option<MappingUsage>), PeripheralError> {
    let Some(first) = intents.first() else {
        return Ok(("recovery", None));
    };
    if intents
        .iter()
        .any(|other| other.owner != first.owner || other.target != first.target)
    {
        return Err(PeripheralError::DriverConflict(
            "native source has competing desired rules".into(),
        ));
    }
    Ok((&first.owner, first.target))
}

#[cfg(test)]
mod tests {
    use super::*;
    use openlogi_core::peripheral::{CapabilityId, EndpointId, SessionId};

    #[test]
    fn retirement_waits_for_recovery_or_a_successful_same_owner_transfer() {
        let key = EffectKey {
            scope: MappingScope {
                vendor_id: 65535,
                product_id: 1,
            },
            source: HidUsage {
                page: 12,
                usage: 233,
            }
            .into(),
        };
        let mut pending = super::super::Retirement {
            effects: BTreeMap::from([(key, "rule-one".into())]),
            release_transport: true,
            input_pending: false,
            io: None,
        };
        let outstanding = BTreeSet::from([key]);
        assert!(
            !pending.settled(&outstanding, &BTreeMap::new()),
            "offline or conflicted effects keep their claim"
        );
        assert!(
            !pending.settled(&outstanding, &BTreeMap::from([(key, "rule-two".into())])),
            "another rule cannot assume the baseline"
        );
        assert!(
            pending.settled(&outstanding, &BTreeMap::from([(key, "rule-one".into())])),
            "an active same-owner session retains the shared effect"
        );
        pending.effects.insert(key, "rule-one".into());
        assert!(
            pending.settled(&BTreeSet::new(), &BTreeMap::new()),
            "completed restoration releases the claim"
        );
        pending.input_pending = true;
        assert!(
            !pending.settled(&BTreeSet::new(), &BTreeMap::new()),
            "input handlers must finish before a replacement acquires the device"
        );
    }

    #[test]
    fn native_and_plugin_intents_must_agree_before_mapping_io() {
        let intent = DesiredEffect {
            owner: "shared-rule".into(),
            target: Some(
                HidUsage {
                    page: 7,
                    usage: 0x6d,
                }
                .into(),
            ),
            session: SessionId {
                endpoint: EndpointId("first".into()),
                generation: 1,
            },
            capability: CapabilityId::try_new("input-remap/main").unwrap(),
            requests: vec![1],
        };
        let mut other = intent.clone();
        other.session.endpoint = EndpointId("second".into());
        assert_eq!(
            agreed_intent(&[intent.clone(), other.clone()]).unwrap(),
            ("shared-rule", intent.target)
        );
        other.target = None;
        for intents in [
            vec![intent.clone(), other.clone()],
            vec![other, intent.clone()],
        ] {
            assert!(matches!(
                agreed_intent(&intents),
                Err(PeripheralError::DriverConflict(_))
            ));
        }
        let mut other = intent.clone();
        other.owner = "another-rule".into();
        assert!(matches!(
            agreed_intent(&[intent, other]),
            Err(PeripheralError::DriverConflict(_))
        ));
    }
}
