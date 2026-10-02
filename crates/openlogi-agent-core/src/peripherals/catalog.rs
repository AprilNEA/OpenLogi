//! Deterministic selection and atomic endpoint claims, before any driver I/O.

use std::collections::{BTreeMap, BTreeSet};

use openlogi_core::peripheral::{
    ConfigScope, ControlId, DescriptorId, DriverId, DriverSelection, DriverSource, Endpoint,
    EndpointId, PeripheralError, SessionId,
};
use openlogi_core::{
    device::DeviceKind,
    peripheral::{
        Capability, CapabilityEvidence, CapabilityId, CapabilityRecord, ConnectionStatus,
        InputControl, InputRemapCapability, PeripheralRecord, ScopeKind, TargetKind,
    },
};
use openlogi_plugin::descriptor::{Descriptor, Platform};

/// One accepted descriptor and its provenance.
#[derive(Clone, Debug, PartialEq)]
pub struct Registration {
    /// Validated descriptor whose driver parameters have been checked at activation.
    pub descriptor: Descriptor,
    /// Selection provenance, including an exact package digest for code plugins.
    pub selection: DriverSelection,
}

impl Registration {
    /// Publish declared capabilities before attachment, without opening a device.
    pub fn record(
        &self,
        session: SessionId,
        endpoints: Vec<Endpoint>,
    ) -> Result<PeripheralRecord, PeripheralError> {
        let native = self.selection.driver.as_ref()
            == openlogi_device_registry::native_remap::NATIVE_REMAP_DRIVER_ID;
        let mut capabilities = Vec::new();
        if native {
            let parameters = self
                .descriptor
                .native_parameters()
                .map_err(|error| PeripheralError::InvalidSettings(error.to_string()))?;
            capabilities.push(CapabilityRecord {
                id: CapabilityId::try_new("input-remap/main")
                    .map_err(|error| PeripheralError::InvalidSettings(error.to_string()))?,
                version: 1,
                capability: Capability::InputRemap(InputRemapCapability {
                    controls: parameters.controls.iter().map(InputControl::from).collect(),
                    targets: TargetKind::KeyboardKey,
                    per_app: false,
                }),
                scopes: vec![ScopeKind::Model],
                unavailable: None,
                evidence: CapabilityEvidence::Declared,
                values: BTreeMap::new(),
            });
        }
        Ok(PeripheralRecord {
            model: self.descriptor.model().clone(),
            physical: None,
            session,
            endpoints,
            name: self.descriptor.name().into(),
            kind: DeviceKind::Unknown,
            driver: self.selection.clone(),
            driver_error: None,
            connection: ConnectionStatus::Online,
            capabilities,
            scopes: if native {
                vec![ScopeKind::Model]
            } else {
                // Physical identity is unavailable until the protocol owner probes the device.
                let preferred = match self.descriptor.identity().default_scope {
                    ScopeKind::Physical => ScopeKind::Session,
                    preferred => preferred,
                };
                std::iter::once(preferred)
                    .chain(
                        [ScopeKind::Model, ScopeKind::Session]
                            .into_iter()
                            .filter(|scope| *scope != preferred),
                    )
                    .collect()
            },
            operations: Vec::new(),
        })
    }
}

/// A selected descriptor and the exact endpoint roles it can access.
pub struct Selected<'a> {
    /// The unique winning registration.
    pub registration: &'a Registration,
    /// Endpoints selected for all required roles.
    pub roles: BTreeMap<ControlId, &'a Endpoint>,
}

/// Current attachment facts used to resolve saved driver selections.
#[derive(Clone, Copy)]
pub struct SelectionContext<'a> {
    /// Complete current attachment identity.
    pub session: &'a SessionId,
    /// Last record, when a physical or model scope has already been established.
    pub current: Option<&'a PeripheralRecord>,
    /// Compiled transport owner protected unless the user explicitly replaces it.
    pub builtin_owner: Option<&'a DriverId>,
}

/// An immutable generation of accepted sources, replaced one source at a time.
#[derive(Clone, Default)]
pub struct Catalog {
    entries: BTreeMap<DescriptorId, Registration>,
    generation: u64,
}

impl Catalog {
    /// Apply session, physical, and model precedence once for every host adapter.
    pub fn select_with_config<'a>(
        &'a self,
        group: &'a [Endpoint],
        config: &openlogi_core::config::Config,
        context: SelectionContext<'_>,
    ) -> Result<Option<Selected<'a>>, PeripheralError> {
        let models: Vec<_> =
            context
                .current
                .map(|record| record.model.clone())
                .into_iter()
                .chain(
                    self.entries()
                        .filter(|entry| {
                            entry.descriptor.selectors().iter().any(|selector| {
                                group.iter().any(|endpoint| selector.matches(endpoint))
                            })
                        })
                        .map(|entry| entry.descriptor.model().clone()),
                )
                .collect();
        let rule = config.peripheral_selection(
            context.session,
            context.current.and_then(|record| record.physical.as_ref()),
            &models,
        )?;
        let platform = Platform::current()
            .ok_or_else(|| PeripheralError::Unsupported("host platform".into()))?;
        let selected = self.select(
            group,
            platform,
            rule.map(|rule| &rule.descriptor),
            context.builtin_owner,
        )?;
        if let (Some(rule), Some(selected)) = (rule, &selected)
            && let ConfigScope::Model(model) = &rule.scope
            && model != selected.registration.descriptor.model()
        {
            return Err(PeripheralError::InvalidSettings(format!(
                "rule {} must use the selected descriptor's model {}",
                rule.id,
                selected.registration.descriptor.model()
            )));
        }
        Ok(selected)
    }

    /// Current accepted generation. Rejected updates do not advance it.
    #[must_use]
    pub const fn generation(&self) -> u64 {
        self.generation
    }

    /// Accepted registrations in stable ID order.
    pub fn entries(&self) -> impl Iterator<Item = &Registration> {
        self.entries.values()
    }

    /// Atomically replace one validated source without disturbing independent sources.
    pub fn replace_source(
        &mut self,
        source: &DriverSource,
        entries: Vec<Registration>,
    ) -> Result<(), PeripheralError> {
        let mut ids = BTreeSet::new();
        for entry in &entries {
            if &entry.selection.source != source
                || entry.selection.descriptor != *entry.descriptor.id()
                || entry.selection.driver != entry.descriptor.driver().id
                || !ids.insert(entry.descriptor.id())
                || self
                    .entries
                    .get(entry.descriptor.id())
                    .is_some_and(|old| &old.selection.source != source)
            {
                return Err(PeripheralError::DriverConflict(format!(
                    "duplicate or inconsistent descriptor {}",
                    entry.descriptor.id()
                )));
            }
        }
        if self
            .entries
            .values()
            .filter(|entry| &entry.selection.source == source)
            .count()
            == entries.len()
            && entries
                .iter()
                .all(|entry| self.entries.get(entry.descriptor.id()) == Some(entry))
        {
            return Ok(());
        }
        self.entries
            .retain(|_, old| &old.selection.source != source);
        for entry in entries {
            self.entries.insert(entry.descriptor.id().clone(), entry);
        }
        self.generation += 1;
        Ok(())
    }

    /// Resolve one verified endpoint group before probing or writing.
    ///
    /// `builtin_owner` protects a pre-existing protocol owner such as a HID++
    /// receiver. Only an explicit descriptor selection can replace that owner.
    pub fn select<'a>(
        &'a self,
        group: &'a [Endpoint],
        platform: Platform,
        selection: Option<&DescriptorId>,
        builtin_owner: Option<&DriverId>,
    ) -> Result<Option<Selected<'a>>, PeripheralError> {
        let mut candidates = Vec::new();
        for registration in self.entries.values() {
            if !registration.descriptor.platforms().contains(&platform) {
                continue;
            }
            if registration.descriptor.selectors().iter().all(|required| {
                group.iter().any(|endpoint| {
                    registration.descriptor.selectors().iter().any(|selector| {
                        selector.role == required.role && selector.matches(endpoint)
                    })
                })
            }) {
                candidates.push(registration);
            }
        }
        if let Some(selected) = selection {
            let registration = candidates
                .into_iter()
                .find(|candidate| candidate.descriptor.id() == selected)
                .ok_or_else(|| PeripheralError::DriverUnavailable(selected.to_string()))?;
            return resolve_selected(registration, group).map(Some);
        }
        if let Some(owner) = builtin_owner {
            candidates.retain(|candidate| {
                &candidate.selection.driver == owner
                    && candidate.selection.source == DriverSource::Builtin
            });
        } else if candidates
            .iter()
            .any(|candidate| candidate.selection.source == DriverSource::Builtin)
        {
            candidates.retain(|candidate| candidate.selection.source == DriverSource::Builtin);
        }
        if candidates.len() <= 1 {
            return candidates
                .pop()
                .map(|registration| resolve_selected(registration, group))
                .transpose();
        }
        let winner = candidates.iter().position(|candidate| {
            candidates.iter().all(|other| {
                candidate.descriptor.id() == other.descriptor.id()
                    || strictly_refines(&candidate.descriptor, &other.descriptor)
            })
        });
        match winner {
            Some(index) => resolve_selected(candidates.remove(index), group).map(Some),
            None => Err(PeripheralError::DriverConflict(
                candidates
                    .iter()
                    .map(|candidate| candidate.descriptor.id().to_string())
                    .collect::<Vec<_>>()
                    .join(", "),
            )),
        }
    }
}

fn resolve_selected<'a>(
    registration: &'a Registration,
    group: &'a [Endpoint],
) -> Result<Selected<'a>, PeripheralError> {
    let roles = resolve_roles(registration.descriptor.selectors(), group)?.ok_or_else(|| {
        PeripheralError::DriverUnavailable(registration.descriptor.id().to_string())
    })?;
    Ok(Selected {
        registration,
        roles,
    })
}

fn resolve_roles<'a>(
    selectors: &[openlogi_plugin::selector::Selector],
    group: &'a [Endpoint],
) -> Result<Option<BTreeMap<ControlId, &'a Endpoint>>, PeripheralError> {
    let roles: BTreeSet<_> = selectors.iter().map(|selector| &selector.role).collect();
    let mut resolved = BTreeMap::new();
    for role in roles {
        let mut matches = group.iter().filter(|endpoint| {
            selectors
                .iter()
                .any(|selector| &selector.role == role && selector.matches(endpoint))
        });
        let Some(endpoint) = matches.next() else {
            return Ok(None);
        };
        if matches.next().is_some() {
            return Err(PeripheralError::DriverConflict(format!(
                "endpoint role {role} is ambiguous"
            )));
        }
        resolved.insert(role.clone(), endpoint);
    }
    Ok(Some(resolved))
}

fn subset(left: &Descriptor, right: &Descriptor) -> bool {
    let right_roles: BTreeSet<_> = right
        .selectors()
        .iter()
        .map(|selector| &selector.role)
        .collect();
    let left_roles: BTreeSet<_> = left
        .selectors()
        .iter()
        .map(|selector| &selector.role)
        .collect();
    right_roles.is_subset(&left_roles)
        && left.selectors().iter().all(|selector| {
            !right_roles.contains(&selector.role)
                || right
                    .selectors()
                    .iter()
                    .any(|other| selector.is_subset_of(other))
        })
}

fn strictly_refines(left: &Descriptor, right: &Descriptor) -> bool {
    subset(left, right) && !subset(right, left)
}

/// Transport ownership. A stale detach cannot release a successor's claim.
#[derive(Default)]
pub struct Claims(BTreeMap<EndpointId, SessionId>);

impl Claims {
    /// Whether a different live session still owns one of these endpoint groups.
    #[must_use]
    pub fn occupied(&self, session: &SessionId, endpoints: &[EndpointId]) -> bool {
        endpoints
            .iter()
            .any(|endpoint| self.0.get(endpoint).is_some_and(|owner| owner != session))
    }

    /// Claim all endpoint groups or none. No hardware access precedes this decision.
    pub fn acquire(
        &mut self,
        session: &SessionId,
        endpoints: &[EndpointId],
    ) -> Result<(), PeripheralError> {
        if self.occupied(session, endpoints) {
            return Err(PeripheralError::DriverConflict(
                "endpoint already belongs to another session".into(),
            ));
        }
        for endpoint in endpoints {
            self.0.insert(endpoint.clone(), session.clone());
        }
        Ok(())
    }

    /// Release only claims still owned by the complete publication identity.
    pub fn release(&mut self, session: &SessionId) {
        self.0.retain(|_, owner| owner != session);
    }
}

#[cfg(test)]
mod tests;
