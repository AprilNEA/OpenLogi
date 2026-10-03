//! Reviewed package changes use the existing conflict-checked configuration writer.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use openlogi_core::{
    config::{Config, ConfigError, ConfigFile},
    peripheral::{
        DescriptorId, DriverId, PeripheralConfig, PeripheralError, PluginCommand, PluginSelection,
    },
};
use openlogi_plugin::{manifest::validate_values, package::Package};
use serde::{Deserialize, Serialize};

use super::{
    Paths,
    sources::{Grants, Sources},
    storage::StateFile,
};

#[derive(Clone, Default, Serialize, Deserialize)]
struct State {
    grants: Grants,
    rollback: BTreeMap<DriverId, Rollback>,
}

#[derive(Clone, Serialize, Deserialize)]
struct Rollback {
    selection: Option<PluginSelection>,
    previous: Vec<PeripheralConfig>,
    activated: Vec<PeripheralConfig>,
    activated_digest: String,
    descriptors: BTreeSet<DescriptorId>,
}

#[derive(Clone)]
pub(super) struct Packages {
    file: StateFile,
    state: State,
}

pub(super) struct Prepared {
    pub(super) driver: Option<DriverId>,
    pub(super) next: Config,
    previous: Config,
    file: ConfigFile,
    state: State,
    remove: Option<String>,
    previous_state: State,
}

impl Packages {
    pub(super) fn load(paths: &Paths) -> Result<Self, PeripheralError> {
        let mut file = StateFile::new(paths.state.join("plugins.json"));
        let state: State = file.load()?.unwrap_or_default();
        if state.grants.len() > 128 || state.grants.values().any(|entries| entries.len() > 256) {
            return Err(PeripheralError::JournalFailed(
                "plugin grant store exceeds limits".into(),
            ));
        }
        Ok(Self { file, state })
    }

    pub(super) fn grants(&self) -> &Grants {
        &self.state.grants
    }

    pub(super) fn can_rollback(&self, driver: &DriverId, digest: &str) -> bool {
        self.state
            .rollback
            .get(driver)
            .is_some_and(|rollback| rollback.activated_digest == digest)
    }

    pub(super) async fn prepare(
        &self,
        command: PluginCommand,
        sources: &mut Sources,
        paths: &Paths,
    ) -> Result<Prepared, PeripheralError> {
        let (config, file) = ConfigFile::load_from_path(&paths.config).map_err(config_error)?;
        let mut prepared = Prepared {
            driver: None,
            previous: config.clone(),
            next: config,
            file,
            state: self.state.clone(),
            previous_state: self.state.clone(),
            remove: None,
        };
        match command {
            PluginCommand::Install { path } => {
                let package = Package::read(std::path::Path::new(&path)).map_err(invalid)?;
                sources
                    .runtime()?
                    .compile(package.component().to_vec())
                    .await?;
                package.install(&paths.packages).map_err(invalid)?;
            }
            PluginCommand::Enable {
                digest,
                descriptors,
            } => {
                self.enable(&mut prepared, &digest, &descriptors, sources, paths)
                    .await?;
            }
            PluginCommand::Disable { driver } => {
                if let Some(selection) = prepared.next.plugins.get_mut(&driver) {
                    selection.enabled = false;
                    prepared.state.grants.remove(&selection.digest);
                }
                prepared.driver = Some(driver);
            }
            PluginCommand::Remove { digest } => {
                let package = Package::installed(&paths.packages, &digest).map_err(invalid)?;
                let driver = package.manifest().id.clone();
                if let Some(selection) = prepared.next.plugins.get_mut(&driver)
                    && selection.digest == digest
                {
                    selection.enabled = false;
                }
                prepared.state.grants.remove(&digest);
                prepared.driver = Some(driver);
                prepared.remove = Some(digest);
            }
            PluginCommand::Rollback { driver } => {
                let rollback = prepared
                    .state
                    .rollback
                    .get(&driver)
                    .ok_or_else(|| invalid("no previous package selection"))?
                    .clone();
                let current = prepared.next.plugins.get(&driver);
                let current_rules = selected_rules(&prepared.next, &rollback.descriptors);
                if current.is_none_or(|s| s.digest != rollback.activated_digest)
                    || current_rules != rollback.activated
                {
                    return Err(PeripheralError::ConfigConflict);
                }
                if let Some(selection) = &rollback.selection {
                    let package =
                        Package::installed(&paths.packages, &selection.digest).map_err(invalid)?;
                    if selection.enabled && !prepared.state.grants.contains_key(&selection.digest) {
                        return Err(PeripheralError::PermissionDenied(
                            "the previous content must be granted before rollback".into(),
                        ));
                    }
                    sources
                        .runtime()?
                        .compile(package.component().to_vec())
                        .await?;
                    prepared
                        .next
                        .plugins
                        .insert(driver.clone(), selection.clone());
                } else {
                    prepared.next.plugins.remove(&driver);
                }
                prepared
                    .next
                    .peripherals
                    .retain(|r| !rollback.descriptors.contains(&r.descriptor));
                prepared.next.peripherals.extend(rollback.previous);
                prepared.state.rollback.remove(&driver);
                prepared.driver = Some(driver);
            }
        }
        prepared.next.validate_peripherals()?;
        Ok(prepared)
    }

    async fn enable(
        &self,
        prepared: &mut Prepared,
        digest: &str,
        approved: &BTreeMap<DescriptorId, String>,
        sources: &mut Sources,
        paths: &Paths,
    ) -> Result<(), PeripheralError> {
        let package = Package::installed(&paths.packages, digest).map_err(invalid)?;
        let descriptors = sources.descriptors_for(&package)?;
        let available: BTreeSet<_> = descriptors.iter().map(|d| d.id().clone()).collect();
        let selected: BTreeSet<_> = approved.keys().cloned().collect();
        if selected.is_empty()
            || selected.len() != approved.len()
            || !selected.is_subset(&available)
        {
            return Err(invalid(
                "enable requires unique installed descriptor selections",
            ));
        }
        let fingerprints = descriptors
            .iter()
            .filter(|d| selected.contains(d.id()))
            .map(|d| Ok((d.id().clone(), d.fingerprint().map_err(invalid)?)))
            .collect::<Result<BTreeMap<_, _>, PeripheralError>>()?;
        if &fingerprints != approved {
            return Err(PeripheralError::PermissionDenied(
                "descriptor content changed since review; inspect and approve the current content"
                    .into(),
            ));
        }
        let manifest = Arc::new(package.manifest().clone());
        let component = sources
            .runtime()?
            .compile(package.component().to_vec())
            .await?;
        let old = prepared.next.plugins.get(&manifest.id).cloned();
        let mut affected = available;
        if let Some(old) = &old {
            let previous = Package::installed(&paths.packages, &old.digest).map_err(invalid)?;
            affected.extend(previous.descriptors().iter().map(|d| d.id().clone()));
            for before in previous.descriptors() {
                if let Some(after) = descriptors.iter().find(|d| d.id() == before.id())
                    && after.model() != before.model()
                {
                    return Err(invalid(
                        "an update cannot change a descriptor's model identity",
                    ));
                }
            }
        }
        let before = selected_rules(&prepared.next, &affected);
        for rule in &mut prepared.next.peripherals {
            if !affected.contains(&rule.descriptor) {
                continue;
            }
            if !descriptors.iter().any(|d| d.id() == &rule.descriptor) {
                return Err(invalid("candidate removed a configured descriptor"));
            }
            for (id, settings) in &mut rule.capabilities {
                if !id.as_ref().starts_with("extension/") {
                    continue;
                }
                let values = if let Some(old) = &old
                    && old.settings_schema != manifest.settings_schema
                {
                    component
                        .migrate(
                            Arc::clone(&manifest),
                            old.settings_schema,
                            settings.values.clone(),
                        )
                        .await?
                } else {
                    settings.values.clone()
                };
                settings.values =
                    validate_values(&manifest.settings, &values, true).map_err(invalid)?;
            }
        }
        if old.as_ref().is_some_and(|s| s.digest != digest) {
            prepared.state.rollback.insert(
                manifest.id.clone(),
                Rollback {
                    selection: old,
                    previous: before,
                    activated: selected_rules(&prepared.next, &affected),
                    activated_digest: digest.into(),
                    descriptors: affected,
                },
            );
        }
        prepared.state.grants.insert(digest.into(), fingerprints);
        prepared.next.plugins.insert(
            manifest.id.clone(),
            PluginSelection {
                enabled: true,
                digest: digest.into(),
                settings_schema: manifest.settings_schema,
            },
        );
        prepared.driver = Some(manifest.id.clone());
        Ok(())
    }

    pub(super) fn commit(
        &mut self,
        prepared: &mut Prepared,
        paths: &Paths,
    ) -> Result<(), PeripheralError> {
        if prepared.driver.is_none() {
            return Ok(());
        }
        // A grant alone cannot activate code; commit the selection only after the grant is durable.
        self.file.save(&prepared.state)?;
        self.state = prepared.state.clone();
        if let Err(error) = prepared.file.save(&prepared.next) {
            self.file.save(&prepared.previous_state)?;
            self.state = prepared.previous_state.clone();
            return Err(config_error(error));
        }
        if let Some(digest) = &prepared.remove {
            let package = Package::installed(&paths.packages, digest).map_err(invalid)?;
            package.remove(&paths.packages).map_err(invalid)?;
        }
        Ok(())
    }

    pub(super) fn rollback_failed_activation(
        &mut self,
        prepared: &mut Prepared,
    ) -> Result<Config, PeripheralError> {
        prepared
            .file
            .save(&prepared.previous)
            .map_err(config_error)?;
        self.file.save(&prepared.previous_state)?;
        self.state = prepared.previous_state.clone();
        Ok(prepared.previous.clone())
    }
}

fn selected_rules(config: &Config, descriptors: &BTreeSet<DescriptorId>) -> Vec<PeripheralConfig> {
    config
        .peripherals
        .iter()
        .filter(|r| descriptors.contains(&r.descriptor))
        .cloned()
        .collect()
}

fn invalid(error: impl std::fmt::Display) -> PeripheralError {
    PeripheralError::InvalidSettings(error.to_string())
}
pub(super) fn config_error(error: ConfigError) -> PeripheralError {
    match error {
        ConfigError::Conflict { .. } => PeripheralError::ConfigConflict,
        other => PeripheralError::ConfigWriteFailed(other.to_string()),
    }
}

#[cfg(test)]
mod tests;
