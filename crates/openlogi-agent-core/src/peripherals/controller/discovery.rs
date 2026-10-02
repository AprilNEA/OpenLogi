//! Endpoint selection and attachment reconciliation before driver I/O.

use super::{
    ApplicationStatus, BTreeMap, BTreeSet, CapabilityEvidence, Config, ConfigScope,
    ConnectionStatus, Controller, DiscoveredEndpoint, DriverId, DriverState, Endpoint, EndpointId,
    Entry, PeripheralError, Selection, SessionId, facts_for_roles, failure_state, invalid,
    set_status, wire,
};

impl Controller {
    pub(super) async fn reconcile(&mut self) {
        self.reconcile_cameras();
        let observation = self.handle.discovery.borrow().clone();
        let endpoints = match observation.result {
            Some(Ok(endpoints)) => endpoints,
            Some(Err(error)) => {
                self.stop_all(wire::DetachReason::Suspended).await;
                for entry in self.entries.values_mut() {
                    entry.record.connection = ConnectionStatus::Unavailable;
                    set_status(
                        &mut entry.record,
                        self.handle.desired.borrow().revision,
                        &ApplicationStatus::Failed(error.clone()),
                    );
                }
                self.publish();
                return;
            }
            None => {
                self.reconcile_mappings().await;
                self.publish();
                return;
            }
        };
        let mut groups: BTreeMap<EndpointId, Vec<DiscoveredEndpoint>> = BTreeMap::new();
        for endpoint in endpoints {
            let key = endpoint
                .endpoint
                .parent
                .as_ref()
                .unwrap_or(&endpoint.endpoint.id)
                .clone();
            groups.entry(key).or_default().push(endpoint);
        }
        let mut seen = BTreeSet::new();
        let config = self.config();
        for group in groups.values() {
            if let Some(primary) = self.reconcile_group(group, &config).await {
                seen.insert(primary);
            }
        }
        for id in self
            .entries
            .keys()
            .filter(|id| !seen.contains(*id))
            .cloned()
            .collect::<Vec<_>>()
        {
            let Some(mut entry) = self.entries.remove(&id) else {
                continue;
            };
            self.stop_entry(&mut entry, wire::DetachReason::Disconnected)
                .await;
            entry.record.connection = ConnectionStatus::Offline;
            for capability in &mut entry.record.capabilities {
                capability.evidence = CapabilityEvidence::LastKnown;
            }
            set_status(
                &mut entry.record,
                self.revision(),
                &ApplicationStatus::Failed(PeripheralError::Offline),
            );
            self.entries.insert(id, entry);
        }
        // Unverified endpoints are connection-local. Retain one offline model record,
        // while every currently connected identical unit keeps its own session.
        let online_models: BTreeSet<_> = self
            .entries
            .values()
            .filter(|e| e.record.connection == ConnectionStatus::Online)
            .map(|e| e.record.model.clone())
            .collect();
        let saved_models: BTreeSet<_> = config
            .peripherals
            .iter()
            .filter_map(|r| match &r.scope {
                ConfigScope::Model(model) => Some(model.clone()),
                _ => None,
            })
            .collect();
        let mut retained_models = BTreeSet::new();
        self.entries.retain(|_, e| {
            e.record.connection != ConnectionStatus::Offline
                || (saved_models.contains(&e.record.model)
                    && !online_models.contains(&e.record.model)
                    && retained_models.insert(e.record.model.clone()))
        });
        self.offline_rules(&config);
        self.reconcile_mappings().await;
        self.publish();
    }

    async fn reconcile_group(
        &mut self,
        group: &[DiscoveredEndpoint],
        config: &Config,
    ) -> Option<EndpointId> {
        let mut facts: Vec<_> = group.iter().map(|e| e.endpoint.clone()).collect();
        facts.sort_by(|a, b| a.id.cmp(&b.id));
        let primary = facts.first().map(|e| e.id.clone())?;
        let session = self
            .entries
            .get(&primary)
            .filter(|e| e.record.connection != ConnectionStatus::Offline)
            .map_or_else(
                || {
                    self.generation += 1;
                    SessionId {
                        endpoint: primary.clone(),
                        generation: self.generation,
                    }
                },
                |e| e.record.session.clone(),
            );
        let selected = self.select(&facts, &session, group, config);
        let Selection {
            registration,
            roles,
            mut handoff,
        } = match selected {
            Ok(Some(selected)) => selected,
            Ok(None) => return None,
            Err(error) => {
                self.selection_failed(&primary, error).await;
                return Some(primary);
            }
        };
        let mut entry = self.entries.remove(&primary);
        let reusable = entry.as_ref().is_some_and(|e| {
            e.registration == registration
                && e.record.session == session
                && e.record.endpoints == facts_for_roles(&roles)
                && e.handoff.as_ref().map(|h| &h.endpoints)
                    == handoff.as_ref().map(|h| &h.endpoints)
        });
        if !reusable {
            if let Some(old) = &mut entry {
                // Replacing a driver retains the attachment's configuration scope.
                // Cleanup drains old I/O and invalidates producer tokens before reuse.
                self.stop_entry(old, wire::DetachReason::Replaced).await;
            }
            entry = Some(Entry {
                record: match registration.record(session, facts_for_roles(&roles)) {
                    Ok(record) => record,
                    Err(error) => {
                        self.diagnostic(primary.0.clone(), error);
                        return None;
                    }
                },
                registration,
                roles,
                rule: None,
                state: DriverState::Idle,
                handoff: handoff.take(),
            });
        }
        let mut entry = entry?;
        // Receiver route publication can grow without changing the physical endpoint group.
        if reusable {
            entry.handoff = handoff;
        }
        entry.record.connection = ConnectionStatus::Online;
        entry.record.driver_error = None;
        let result = self.configure(&mut entry, config).await;
        if let Err(error) = result {
            self.stop_entry(&mut entry, wire::DetachReason::Fault).await;
            entry.record.driver_error = Some(error.clone());
            entry.state = failure_state(&error);
            set_status(
                &mut entry.record,
                self.revision(),
                &ApplicationStatus::Failed(error),
            );
        }
        self.entries.insert(primary.clone(), entry);
        Some(primary)
    }

    async fn selection_failed(&mut self, primary: &EndpointId, error: PeripheralError) {
        if let Some(mut entry) = self.entries.remove(primary) {
            if !matches!(&entry.state, DriverState::SelectionError(old) if old == &error) {
                self.stop_entry(&mut entry, wire::DetachReason::Suspended)
                    .await;
            }
            if let Some(target) = &entry.handoff {
                match self.ownership.reserve(
                    &entry.record.session,
                    target.nodes.clone(),
                    target.routes.clone(),
                ) {
                    Ok(()) => {
                        self.ownership.ready(&entry.record.session);
                    }
                    Err(failure) => self.diagnostic(primary.0.clone(), failure),
                }
            }
            entry.record.connection = ConnectionStatus::Online;
            entry.record.driver_error = Some(error.clone());
            entry.state = DriverState::SelectionError(error.clone());
            set_status(
                &mut entry.record,
                self.revision(),
                &ApplicationStatus::Failed(error),
            );
            self.entries.insert(primary.clone(), entry);
        } else {
            self.diagnostic(format!("endpoint {}", primary.0), error);
        }
    }

    fn select(
        &self,
        facts: &[Endpoint],
        session: &SessionId,
        group: &[DiscoveredEndpoint],
        config: &Config,
    ) -> Result<Option<Selection>, PeripheralError> {
        let old = self.entries.get(&session.endpoint);
        let owner = group
            .iter()
            .find_map(|e| e.builtin_owner)
            .map(DriverId::try_new)
            .transpose()
            .map_err(invalid)?;
        let selected = self.sources.catalog.select_with_config(
            facts,
            config,
            super::super::catalog::SelectionContext {
                session,
                current: old.map(|entry| &entry.record),
                builtin_owner: owner.as_ref(),
            },
        )?;
        selected
            .map(|selected| {
                let roles = selected
                    .roles
                    .iter()
                    .map(|(role, endpoint)| {
                        let discovered = group
                            .iter()
                            .find(|d| d.endpoint.id == endpoint.id)
                            .ok_or(PeripheralError::StaleSession)?;
                        Ok((role.clone(), discovered.clone()))
                    })
                    .collect::<Result<BTreeMap<_, _>, PeripheralError>>()?;
                Ok(Selection {
                    registration: selected.registration.clone(),
                    roles,
                    handoff: owner
                        .map(|_| {
                            let nodes: std::collections::HashSet<_> =
                                group.iter().map(|e| e.node().id).collect();
                            let mut routes =
                                self.registry.routes_for_nodes(&nodes).map_err(|error| {
                                    PeripheralError::DiscoveryUnavailable(error.to_string())
                                })?;
                            if let Some(previous) = old.and_then(|e| e.handoff.as_ref())
                                && previous.nodes == nodes
                            {
                                routes.extend(previous.routes.iter().cloned());
                            }
                            for endpoint in group {
                                let node = endpoint.node();
                                routes.push(openlogi_hid::DeviceRoute::Direct {
                                    vendor_id: node.vendor_id,
                                    product_id: node.product_id,
                                });
                                routes.push(openlogi_hid::DeviceRoute::from(
                                    &openlogi_core::device::RawDeviceAddress {
                                        vendor_id: node.vendor_id,
                                        product_id: node.product_id,
                                        usage_page: node.usage_page,
                                        usage_id: node.usage_id,
                                        identity: node.identity(),
                                    },
                                ));
                            }
                            let mut unique = Vec::new();
                            for route in routes {
                                if !unique.contains(&route) {
                                    unique.push(route);
                                }
                            }
                            Ok::<_, PeripheralError>(super::HandoffTarget {
                                endpoints: facts.to_vec(),
                                nodes,
                                routes: unique,
                            })
                        })
                        .transpose()?,
                })
            })
            .transpose()
    }

    fn offline_rules(&mut self, config: &Config) {
        for rule in &config.peripherals {
            let ConfigScope::Model(model) = &rule.scope else {
                continue;
            };
            if self.entries.values().any(|e| &e.record.model == model) {
                continue;
            }
            let Some(registration) = self
                .sources
                .catalog
                .entries()
                .find(|e| e.descriptor.id() == &rule.descriptor)
                .cloned()
            else {
                continue;
            };
            let id = EndpointId(format!("offline/{model}"));
            let mut record = match registration.record(
                SessionId {
                    endpoint: id.clone(),
                    generation: self.generation,
                },
                Vec::new(),
            ) {
                Ok(record) => record,
                Err(error) => {
                    self.diagnostic(id.0.clone(), error);
                    continue;
                }
            };
            record.connection = ConnectionStatus::Offline;
            set_status(
                &mut record,
                self.revision(),
                &ApplicationStatus::Failed(PeripheralError::Offline),
            );
            self.entries.insert(
                id,
                Entry {
                    record,
                    registration,
                    roles: BTreeMap::new(),
                    rule: Some(rule.clone()),
                    state: DriverState::Idle,
                    handoff: None,
                },
            );
        }
    }
}
