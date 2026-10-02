//! Explicit peripheral configuration and package commands through the existing agent.

use std::path::PathBuf;

use anyhow::{Context as _, Result, anyhow};
use clap::Subcommand;
use openlogi_core::{
    binding::{Action, KeyCombo},
    config::ConfigFile,
    peripheral::{
        CapabilityId, ControlId, DescriptorId, DriverId, PeripheralRecord, PeripheralSnapshot,
        PluginCommand,
    },
};

use crate::agent;

#[derive(Debug, Subcommand)]
pub enum PeripheralCmd {
    /// Print complete capability, status, package digest, and grant diagnostics as JSON.
    List,
    /// Save a shortcut for one advertised control. The rule remains disabled until enabled.
    Bind {
        /// Connection-local endpoint from `peripheral list`.
        endpoint: String,
        capability: String,
        control: String,
        key: KeyCombo,
    },
    /// Enable or disable a saved rule for the selected device.
    Enabled {
        endpoint: String,
        #[arg(action = clap::ArgAction::Set)]
        enabled: bool,
    },
    /// Retry a faulted current attachment.
    Retry { endpoint: String },
    /// Resolve an external mapping conflict before capturing a new baseline.
    Resolve { rule: String },
    /// Reload configuration and local descriptor files.
    Reload,
    /// Manage exact local plugin content. Installation alone cannot access devices.
    #[command(subcommand)]
    Plugin(PluginCmd),
}

#[derive(Debug, Subcommand)]
pub enum PluginCmd {
    /// Validate and install a local package directory without enabling it.
    Install { path: PathBuf },
    /// Grant the reviewed digest access to the explicitly selected descriptors.
    Enable {
        digest: String,
        #[arg(long = "descriptor", required = true)]
        descriptors: Vec<String>,
    },
    /// Revoke execution and restore owned native mappings.
    Disable { driver: String },
    /// Remove exact installed content; preserve settings and pending recovery.
    Remove { digest: String },
    /// Restore the previous selected package and settings through a guarded save.
    Rollback { driver: String },
}

impl PeripheralCmd {
    pub async fn run(self) -> Result<()> {
        let client = agent::connect()
            .await
            .context("start OpenLogi before managing peripherals")?;
        match self {
            Self::List => println!(
                "{}",
                serde_json::to_string_pretty(&agent::snapshot(&client).await?.peripherals)?
            ),
            Self::Plugin(command) => {
                let snapshot = agent::snapshot(&client).await?;
                agent::plugin(&client, command.prepare(&snapshot.peripherals)?).await?;
            }
            Self::Reload => agent::reload(&client).await?,
            Self::Resolve { rule } => agent::resolve_peripheral(&client, rule).await?,
            Self::Retry { endpoint } => {
                let snapshot = agent::snapshot(&client).await?;
                agent::retry_peripheral(
                    &client,
                    select(&snapshot.peripherals, &endpoint)?.session.clone(),
                )
                .await?;
            }
            command @ (Self::Bind { .. } | Self::Enabled { .. }) => {
                let snapshot = agent::snapshot(&client).await?;
                let (mut config, mut file) = ConfigFile::load_or_default()?;
                match command {
                    Self::Bind {
                        endpoint,
                        capability,
                        control,
                        key,
                    } => config.set_peripheral_binding(
                        select(&snapshot.peripherals, &endpoint)?,
                        &CapabilityId::try_new(capability)?,
                        ControlId::try_new(control)?,
                        Some(Action::CustomShortcut(key)),
                    )?,
                    Self::Enabled { endpoint, enabled } => config.set_peripheral_enabled(
                        select(&snapshot.peripherals, &endpoint)?,
                        enabled,
                    )?,
                    _ => unreachable!("only config edits reach this branch"),
                }
                file.save(&config)?;
                agent::reload(&client).await?;
                println!(
                    "Configuration saved. Read peripheral status to verify device application."
                );
            }
        }
        Ok(())
    }
}

fn select<'a>(snapshot: &'a PeripheralSnapshot, endpoint: &str) -> Result<&'a PeripheralRecord> {
    snapshot
        .devices
        .iter()
        .find(|record| record.session.endpoint.0 == endpoint)
        .ok_or_else(|| anyhow!("device endpoint is unavailable; run peripheral list again"))
}

impl PluginCmd {
    fn prepare(self, snapshot: &PeripheralSnapshot) -> Result<PluginCommand> {
        Ok(match self {
            PluginCmd::Install { path } => PluginCommand::Install {
                path: path
                    .canonicalize()?
                    .to_str()
                    .ok_or_else(|| anyhow!("package path must be UTF-8"))?
                    .into(),
            },
            PluginCmd::Enable {
                digest,
                descriptors,
            } => {
                let package = snapshot
                    .plugins
                    .iter()
                    .find(|p| p.digest == digest)
                    .ok_or_else(|| anyhow!("package is not installed"))?;
                let descriptors = descriptors
                    .into_iter()
                    .map(|id| {
                        let id = DescriptorId::try_new(id)?;
                        let details = package
                            .descriptors
                            .get(&id)
                            .ok_or_else(|| anyhow!("descriptor {id} is unavailable"))?;
                        Ok((id, details.fingerprint.clone()))
                    })
                    .collect::<Result<_>>()?;
                PluginCommand::Enable {
                    digest,
                    descriptors,
                }
            }
            PluginCmd::Disable { driver } => PluginCommand::Disable {
                driver: DriverId::try_new(driver)?,
            },
            PluginCmd::Remove { digest } => PluginCommand::Remove { digest },
            PluginCmd::Rollback { driver } => PluginCommand::Rollback {
                driver: DriverId::try_new(driver)?,
            },
        })
    }
}
