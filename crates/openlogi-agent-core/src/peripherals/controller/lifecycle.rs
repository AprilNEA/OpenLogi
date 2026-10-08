use super::{
    Config, ConnectionStatus, Controller, DriverState, PeripheralError, PluginCommand, wire,
};

impl Controller {
    pub(super) async fn package_command(
        &mut self,
        command: PluginCommand,
    ) -> Result<Option<Config>, PeripheralError> {
        // Package validation and compilation must not starve live device queues.
        self.sources.runtime()?;
        let mut sources = self.sources.clone();
        let packages = self.packages.clone();
        let paths = self.paths.clone();
        let preparation = async move { packages.prepare(command, &mut sources, &paths).await };
        tokio::pin!(preparation);
        let mut prepared = loop {
            tokio::select! {
                result = &mut preparation => break result?,
                (id, result) = self.next_event() => { self.event(&id, result).await; self.publish(); }
            }
        };
        if let Some(driver) = &prepared.driver {
            for id in self
                .entries
                .iter()
                .filter(|(_, e)| &e.record.driver.driver == driver)
                .map(|(id, _)| id.clone())
                .collect::<Vec<_>>()
            {
                let Some(mut entry) = self.entries.remove(&id) else {
                    continue;
                };
                self.stop_entry(&mut entry, wire::DetachReason::Replaced)
                    .await;
                self.entries.insert(id, entry);
            }
            self.reconcile_mappings().await;
        }
        if let Err(error) = self.packages.commit(&mut prepared, &self.paths) {
            let (current, _) =
                openlogi_core::config::ConfigFile::load_from_path(&self.paths.config)
                    .map_err(super::super::packages::config_error)?;
            self.handle.reload(&current);
            self.reload().await;
            return Err(error);
        }
        if prepared.driver.is_none() {
            self.reload().await;
            return Ok(None);
        }
        self.handle.reload(&prepared.next);
        self.reload().await;
        let failure = prepared.driver.as_ref().and_then(|driver| {
            let selection = prepared.next.plugins.get(driver).filter(|s| s.enabled)?;
            if self
                .sources
                .plugins
                .get(driver)
                .is_none_or(|p| p.package.digest() != selection.digest)
            {
                return Some(PeripheralError::DriverUnavailable(format!(
                    "{driver}: package activation was rejected"
                )));
            }
            self.entries.values().find_map(|e| match &e.state {
                DriverState::Fault(error)
                    if &e.record.driver.driver == driver
                        && e.record.connection == ConnectionStatus::Online =>
                {
                    Some(error.clone())
                }
                _ => None,
            })
        });
        if let Some(failure) = failure {
            let rollback = self.packages.rollback_failed_activation(&mut prepared);
            let (actual, _) = openlogi_core::config::ConfigFile::load_from_path(&self.paths.config)
                .map_err(super::super::packages::config_error)?;
            self.handle.reload(&actual);
            self.reload().await;
            return Err(rollback.err().unwrap_or(failure));
        }
        Ok(Some(prepared.next))
    }
}
