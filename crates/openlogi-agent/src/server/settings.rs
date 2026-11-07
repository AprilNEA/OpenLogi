//! Device settings the agent changes and saves itself, for the menu-bar
//! item, reloaded the way every saved edit is: through
//! [`AgentServer::reload`].

use std::sync::Arc;

use openlogi_agent_core::orchestrator::{ConfigReload, DeviceSettings};
use openlogi_ipc::ConfigReloadError;

use super::AgentServer;

impl ConfigReload for AgentServer {
    async fn reload_config_file(&self) -> Result<(), ConfigReloadError> {
        self.reload(None).await.map(|_| ())
    }
}

impl AgentServer {
    /// The device settings the menu-bar item changes: written to the device,
    /// saved in `config.toml` and reloaded, with the effect of the same
    /// change in the settings app.
    #[expect(
        dead_code,
        reason = "the menu-bar item's device controls are the first caller"
    )]
    pub fn device_settings(&self) -> DeviceSettings<Self> {
        DeviceSettings::new(
            Arc::clone(&self.orchestrator),
            self.shared.clone(),
            self.clone(),
        )
    }
}
