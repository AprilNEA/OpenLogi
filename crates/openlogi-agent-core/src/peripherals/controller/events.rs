use super::{
    ApplicationStatus, BTreeMap, Controller, DriverState, EndpointId, Entry, Envelope, Event,
    PeripheralError, Poll, failure_state, set_status, wire,
};

impl Controller {
    pub(super) async fn next_event(&mut self) -> (EndpointId, Result<Envelope, PeripheralError>) {
        next_event(&mut self.entries, &mut self.event_cursor).await
    }

    pub(super) async fn event(
        &mut self,
        id: &EndpointId,
        event: Result<Envelope, PeripheralError>,
    ) {
        let Some(mut entry) = self.entries.remove(id) else {
            return;
        };
        let result = async {
            let envelope = event?;
            if envelope.session != entry.record.session {
                return Err(PeripheralError::StaleSession);
            }
            let DriverState::Plugin(plugin) = &mut entry.state else {
                return Err(PeripheralError::StaleSession);
            };
            plugin.broker.check()?;
            match envelope.event {
                Event::Guest(event) => {
                    let update = plugin.guest.event(event).await?;
                    self.update(&mut entry, update)
                }
                Event::Deadline(request) if plugin.pending.contains_key(&request) => {
                    Err(PeripheralError::WriteFailed(
                        "plugin command did not complete within 5 seconds".into(),
                    ))
                }
                Event::Deadline(_) => Ok(()),
            }
        }
        .await;
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
        self.entries.insert(id.clone(), entry);
        self.reconcile_mappings().await;
    }

    pub(super) async fn stop_entry(&mut self, entry: &mut Entry, reason: wire::DetachReason) {
        let state = std::mem::replace(&mut entry.state, DriverState::Idle);
        let owner = entry.rule.as_ref().map_or_else(
            || entry.record.driver.descriptor.to_string(),
            |rule| rule.id.clone(),
        );
        let mut effects: BTreeMap<_, _> = self
            .mappings
            .owned_effects(&owner)
            .into_iter()
            .map(|key| (key, owner.clone()))
            .collect();
        effects.extend(
            self.plugin_effects
                .iter()
                .filter(|(_, effect)| effect.session == entry.record.session)
                .map(|((key, _), effect)| (*key, effect.owner.clone())),
        );
        let input_pending = matches!(state, DriverState::Plugin(_));
        let io = if let DriverState::Plugin(mut plugin) = state {
            plugin.input.cancel();
            let io = plugin.broker.stop().await;
            if let Err(error) = plugin.guest.detach(reason).await {
                tracing::warn!(%error, "plugin detach failed after host cancellation");
            }
            Some(io)
        } else {
            None
        };
        self.plugin_effects
            .retain(|_, effect| effect.session != entry.record.session);
        let release_transport = matches!(
            reason,
            wire::DetachReason::Disconnected
                | wire::DetachReason::Replaced
                | wire::DetachReason::Shutdown
        );
        if !input_pending
            && effects.is_empty()
            && !self.retiring.contains_key(&entry.record.session)
        {
            self.claims.release(&entry.record.session);
            if release_transport {
                self.ownership.release(&entry.record.session);
            }
        } else {
            let pending = self
                .retiring
                .entry(entry.record.session.clone())
                .or_insert_with(|| super::Retirement {
                    effects: BTreeMap::new(),
                    release_transport,
                    input_pending,
                    io: None,
                });
            pending.effects.extend(effects);
            pending.release_transport |= release_transport;
            pending.input_pending |= input_pending;
            if io.is_some() {
                pending.io = io;
            }
        }
    }

    pub(super) async fn stop_all(&mut self, reason: wire::DetachReason) {
        for id in self.entries.keys().cloned().collect::<Vec<_>>() {
            let Some(mut entry) = self.entries.remove(&id) else {
                continue;
            };
            let fault = match &entry.state {
                DriverState::Fault(error) => Some(DriverState::Fault(error.clone())),
                DriverState::SelectionError(error) => {
                    Some(DriverState::SelectionError(error.clone()))
                }
                _ => None,
            };
            self.stop_entry(&mut entry, reason).await;
            if let Some(state) = fault {
                entry.state = state;
            }
            self.entries.insert(id, entry);
        }
    }
}

pub(super) async fn next_event(
    entries: &mut BTreeMap<EndpointId, Entry>,
    cursor: &mut usize,
) -> (EndpointId, Result<Envelope, PeripheralError>) {
    std::future::poll_fn(|cx| {
        let keys: Vec<_> = entries.keys().cloned().collect();
        for offset in 0..keys.len() {
            let index = (*cursor + offset) % keys.len();
            let id = &keys[index];
            if let Some(Entry {
                state: DriverState::Plugin(plugin),
                ..
            }) = entries.get_mut(id)
                && let Poll::Ready(event) = plugin.broker.poll(cx)
            {
                *cursor = index + 1;
                return Poll::Ready((id.clone(), event));
            }
        }
        Poll::Pending
    })
    .await
}
