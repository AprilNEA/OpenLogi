//! Scope precedence and contract validation for peripheral settings.

use std::collections::{BTreeMap, BTreeSet};

use crate::peripheral::{
    Capability, CapabilityId, CapabilitySettings, ConfigScope, ControlId, ModelId,
    PeripheralConfig, PeripheralError, PeripheralRecord, PhysicalDeviceId, ScopeKind, SessionId,
    SettingAccess, SettingValue,
};

use super::Config;

impl Config {
    /// Commit one control binding after validating the selected driver's target contract.
    pub fn set_peripheral_binding(
        &mut self,
        record: &PeripheralRecord,
        capability: &CapabilityId,
        control: ControlId,
        action: Option<crate::binding::Action>,
    ) -> Result<(), PeripheralError> {
        self.editable_capability(record, capability)?;
        self.edit_peripheral(record, |rule| {
            let settings = rule
                .capabilities
                .entry(capability.clone())
                .or_insert_with(|| CapabilitySettings {
                    version: 1,
                    bindings: BTreeMap::new(),
                    values: BTreeMap::new(),
                });
            match action {
                Some(action) => {
                    settings.bindings.insert(control, action);
                }
                None => {
                    settings.bindings.remove(&control);
                }
            }
        })
    }

    /// Preserve the desired settings while enabling or conditionally restoring their effects.
    pub fn set_peripheral_enabled(
        &mut self,
        record: &PeripheralRecord,
        enabled: bool,
    ) -> Result<(), PeripheralError> {
        self.edit_peripheral(record, |rule| rule.enabled = enabled)
    }

    /// Change one declared scalar setting without dropping unknown capability versions.
    pub fn set_peripheral_value(
        &mut self,
        record: &PeripheralRecord,
        capability: &CapabilityId,
        key: String,
        value: SettingValue,
    ) -> Result<(), PeripheralError> {
        self.editable_capability(record, capability)?;
        self.edit_peripheral(record, |rule| {
            rule.capabilities
                .entry(capability.clone())
                .or_insert_with(|| CapabilitySettings {
                    version: 1,
                    bindings: BTreeMap::new(),
                    values: BTreeMap::new(),
                })
                .values
                .insert(key, value);
        })
    }

    fn edit_peripheral(
        &mut self,
        record: &PeripheralRecord,
        edit: impl FnOnce(&mut PeripheralConfig),
    ) -> Result<(), PeripheralError> {
        let mut rule = if let Some(rule) = self.selected_peripheral_rule(record)? {
            rule.clone()
        } else {
            let scope = match record.scopes.first() {
                Some(ScopeKind::Model) => ConfigScope::Model(record.model.clone()),
                Some(ScopeKind::Physical) => ConfigScope::Physical(
                    record
                        .physical
                        .as_ref()
                        .ok_or_else(|| invalid("physical scope requires verified identity"))?
                        .key
                        .clone(),
                ),
                Some(ScopeKind::Session) => ConfigScope::Session(record.session.clone()),
                None => return Err(invalid("driver has no configurable scope")),
            };
            let mut number = 1;
            while self
                .peripherals
                .iter()
                .any(|r| r.id == format!("peripheral-{number}"))
            {
                number += 1;
            }
            PeripheralConfig {
                id: format!("peripheral-{number}"),
                enabled: false,
                scope,
                descriptor: record.driver.descriptor.clone(),
                capabilities: BTreeMap::new(),
            }
        };
        edit(&mut rule);
        rule.validate_for(record)?;
        if let Some(old) = self.peripherals.iter_mut().find(|old| old.id == rule.id) {
            *old = rule;
        } else {
            self.peripherals.push(rule);
        }
        Ok(())
    }

    fn editable_capability(
        &self,
        record: &PeripheralRecord,
        id: &CapabilityId,
    ) -> Result<(), PeripheralError> {
        let capability = record
            .capabilities
            .iter()
            .find(|c| &c.id == id)
            .ok_or_else(|| invalid("unknown capability"))?;
        if capability.version != 1
            || capability.unavailable.is_some()
            || self
                .selected_peripheral_rule(record)?
                .and_then(|r| r.capabilities.get(id))
                .is_some_and(|settings| settings.version != capability.version)
        {
            return Err(PeripheralError::Unsupported(format!(
                "{id}: capability settings are unavailable"
            )));
        }
        Ok(())
    }

    /// Validate persistence invariants without requiring an installed driver.
    pub fn validate_peripherals(&self) -> Result<(), PeripheralError> {
        if self.peripherals.len() > 256 || self.plugins.len() > 128 {
            return Err(invalid("too many peripheral rules or plugin selections"));
        }
        let mut ids = BTreeSet::new();
        for rule in &self.peripherals {
            ControlId::try_new(rule.id.clone()).map_err(|e| invalid(&e.to_string()))?;
            if !ids.insert(&rule.id) || rule.capabilities.len() > 128 {
                return Err(invalid("duplicate rule ID or too many capabilities"));
            }
            for settings in rule.capabilities.values() {
                if settings.version == 0
                    || settings.bindings.len() > 128
                    || settings.values.len() > 128
                {
                    return Err(invalid(
                        "invalid settings version or too many controls/fields",
                    ));
                }
            }
        }
        for selection in self.plugins.values() {
            if selection.settings_schema == 0
                || selection.digest.len() != 64
                || !selection
                    .digest
                    .bytes()
                    .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
            {
                return Err(invalid(
                    "plugin selections require a schema and an exact lowercase SHA-256 digest",
                ));
            }
        }
        Ok(())
    }

    /// Resolve one rule using explicit session, physical, then model precedence.
    /// Disabled rules participate, so disabling a physical override cannot enable a model default.
    pub fn peripheral_rule(
        &self,
        record: &PeripheralRecord,
    ) -> Result<Option<&PeripheralConfig>, PeripheralError> {
        let rule = self.selected_peripheral_rule(record)?;
        if let Some(rule) = rule {
            rule.validate_for(record)?;
        }
        Ok(rule)
    }

    fn selected_peripheral_rule(
        &self,
        record: &PeripheralRecord,
    ) -> Result<Option<&PeripheralConfig>, PeripheralError> {
        let rule = self.peripheral_selection(
            &record.session,
            record.physical.as_ref(),
            std::slice::from_ref(&record.model),
        )?;
        if let Some(rule) = rule
            && rule.descriptor != record.driver.descriptor
        {
            return Err(PeripheralError::DriverUnavailable(
                rule.descriptor.to_string(),
            ));
        }
        Ok(rule)
    }

    /// Resolve saved scope precedence before a driver is selected or configured.
    /// `models` contains the models established by the catalog or the current record.
    pub fn peripheral_selection(
        &self,
        session: &SessionId,
        physical: Option<&PhysicalDeviceId>,
        models: &[ModelId],
    ) -> Result<Option<&PeripheralConfig>, PeripheralError> {
        let mut candidates = self
            .peripherals
            .iter()
            .filter_map(|rule| {
                let priority = match &rule.scope {
                    ConfigScope::Session(target) if target == session => 3,
                    ConfigScope::Physical(key) if physical.is_some_and(|id| &id.key == key) => 2,
                    ConfigScope::Model(model) if models.contains(model) => 1,
                    _ => return None,
                };
                Some((priority, rule))
            })
            .collect::<Vec<_>>();
        candidates.sort_by_key(|(priority, _)| std::cmp::Reverse(*priority));
        let Some((priority, rule)) = candidates.first() else {
            return Ok(None);
        };
        if candidates
            .get(1)
            .is_some_and(|(other, _)| other == priority)
        {
            return Err(PeripheralError::DriverConflict(
                "multiple rules target the same configuration scope".into(),
            ));
        }
        Ok(Some(rule))
    }
}

impl PeripheralConfig {
    /// Reject commands the selected capability cannot execute. Future versions remain inert.
    pub fn validate_for(&self, record: &PeripheralRecord) -> Result<(), PeripheralError> {
        if !record.scopes.contains(&self.scope.kind()) {
            return Err(invalid("driver cannot enforce this configuration scope"));
        }
        for capability in &record.capabilities {
            let Some(settings) = self.capabilities.get(&capability.id) else {
                continue;
            };
            if capability.version != 1
                || capability.version != settings.version
                || capability.unavailable.is_some()
            {
                continue;
            }
            if !capability.scopes.contains(&self.scope.kind()) {
                return Err(invalid("capability cannot enforce this scope"));
            }
            match &capability.capability {
                Capability::InputRemap(input) => {
                    if !settings.values.is_empty() {
                        return Err(invalid("input bindings cannot contain extension values"));
                    }
                    for (id, action) in &settings.bindings {
                        if !input.controls.iter().any(|control| &control.id == id) {
                            return Err(invalid("binding names an undeclared control"));
                        }
                        input.targets.validate(action)?;
                    }
                }
                Capability::Extension(fields) => {
                    if !settings.bindings.is_empty() {
                        return Err(invalid("extension fields cannot contain input bindings"));
                    }
                    for (key, value) in &settings.values {
                        let field = fields
                            .get(key)
                            .ok_or_else(|| invalid("unknown extension field"))?;
                        if field.access == SettingAccess::ReadOnly {
                            return Err(invalid("extension field is read-only"));
                        }
                        field.validate(value)?;
                    }
                }
                _ => {}
            }
        }
        Ok(())
    }
}

fn invalid(message: &str) -> PeripheralError {
    PeripheralError::InvalidSettings(message.into())
}

#[cfg(test)]
mod tests;
