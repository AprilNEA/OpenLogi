//! `openlogi modules`: the key modules the running agent found, for
//! debugging a module folder.

use std::io::{self, Write};

use anyhow::{Context as _, Result, anyhow};
use clap::{Args, Subcommand};
use openlogi_ipc::client::ConnectError;
use openlogi_ipc::{AgentClient, ModuleState, ModuleStatus};
use tarpc::context;

use crate::agent::{self, CallFailure};

#[derive(Debug, Args)]
#[command(
    after_help = "Requires a running, compatible OpenLogi agent in the same OPENLOGI_PROFILE. Modules live in the modules folder beside config.toml; see docs/MODULES.md."
)]
pub struct ModulesArgs {
    #[command(subcommand)]
    pub action: ModulesAction,
}

#[derive(Debug, Subcommand)]
pub enum ModulesAction {
    /// List every folder in the modules folder and where its module stands.
    List,
}

pub async fn run(args: ModulesArgs) -> Result<()> {
    let connection = agent::connect().await;
    execute(args, connection, &mut io::stdout()).await
}

/// The connection result and output sink let tests drive this without a
/// socket or a running agent.
async fn execute(
    args: ModulesArgs,
    connection: Result<AgentClient, ConnectError>,
    out: &mut impl Write,
) -> Result<()> {
    let ModulesAction::List = args.action;
    let client = connection.context(
        "modules requires a running, compatible OpenLogi agent; start the matching OpenLogi build with the same OPENLOGI_PROFILE",
    )?;
    let modules = agent::call(client.list_modules(context::current()))
        .await
        .map_err(|failure| match failure {
            CallFailure::TimedOut => anyhow!("agent modules call timed out"),
            CallFailure::Disconnected => anyhow!("agent disconnected during modules call"),
        })?;
    write_list(&modules, out)
}

fn write_list(modules: &[ModuleStatus], out: &mut impl Write) -> Result<()> {
    if modules.is_empty() {
        writeln!(out, "No modules installed.")?;
        return Ok(());
    }
    for (index, status) in modules.iter().enumerate() {
        if index > 0 {
            writeln!(out)?;
        }
        match &status.module {
            Some(module) => {
                let version = module
                    .version
                    .as_deref()
                    .map_or_else(String::new, |version| format!(" {version}"));
                writeln!(out, "{}{version} ({})", module.name, module.id)?;
                writeln!(out, "  folder:    {}", status.folder)?;
                if let Some(description) = &module.description {
                    writeln!(out, "  about:     {description}")?;
                }
                writeln!(out, "  platforms: {}", module.platforms.join(", "))?;
            }
            None => writeln!(out, "{} (no valid module)", status.folder)?,
        }
        writeln!(
            out,
            "  state:     {}{}",
            state_name(status.state),
            if status.module.is_some() && !status.enabled && status.state != ModuleState::Disabled {
                ", turned off"
            } else {
                ""
            }
        )?;
        if let Some(error) = &status.error {
            writeln!(out, "  error:     {error}")?;
        }
    }
    Ok(())
}

fn state_name(state: ModuleState) -> &'static str {
    match state {
        ModuleState::Invalid => "invalid",
        ModuleState::Unsupported => "not for this platform",
        ModuleState::Disabled => "turned off",
        ModuleState::Starting => "starting",
        ModuleState::Running => "running",
        ModuleState::Restarting => "restarting",
        ModuleState::Failed => "failed",
    }
}

#[cfg(test)]
mod tests;
