//! `openlogi keypad`: show an image on a keypad LCD key, through the running
//! agent.
//!
//! The image is the agent's runtime state, not configuration: it stands in
//! for the key's binding icon until it is cleared or the agent restarts, and
//! pressing the key still runs its binding.

use std::fs;
use std::io::{self, Write};
use std::path::PathBuf;

use anyhow::{Context as _, Result, anyhow};
use clap::{Args, Subcommand};
use openlogi_core::binding::ButtonId;
use openlogi_hid::key_display_for;
use openlogi_ipc::client::ConnectError;
use openlogi_ipc::{AgentClient, check_key_image_size};
use tarpc::context;

use crate::agent::{self, CallFailure};

#[derive(Debug, Args)]
#[command(
    after_help = "Requires a running, compatible OpenLogi agent in the same OPENLOGI_PROFILE. Without --device, selects the one online device with LCD keys. The image lives in the agent's memory only: an agent restart clears it."
)]
pub struct KeypadArgs {
    /// Select one online device by a case-insensitive name substring.
    #[arg(long, value_name = "NAME", global = true)]
    pub device: Option<String>,

    #[command(subcommand)]
    pub action: KeypadAction,
}

#[derive(Debug, Subcommand)]
pub enum KeypadAction {
    /// Show a PNG or JPEG on one LCD key in place of its binding's icon.
    ///
    /// The agent crops it to a centred square and scales it to the key.
    SetImage {
        /// The grid key by its config name: `gridMiddle`, or
        /// `page2:gridTopLeft` for a key on a later page.
        #[arg(long, value_name = "KEY", value_parser = grid_key)]
        key: ButtonId,
        /// The PNG or JPEG file.
        #[arg(value_name = "FILE")]
        file: PathBuf,
    },
    /// Show the key's binding icon again.
    ClearImage {
        /// The grid key by its config name: `gridMiddle`, or
        /// `page2:gridTopLeft` for a key on a later page.
        #[arg(long, value_name = "KEY", value_parser = grid_key)]
        key: ButtonId,
    },
}

/// Parse a keypad grid key's config name, refusing every other button.
fn grid_key(raw: &str) -> Result<ButtonId, String> {
    raw.parse::<ButtonId>()
        .ok()
        .filter(|button| button.grid_slot().is_some())
        .ok_or_else(|| {
            format!(
                "`{raw}` is not a keypad grid key; name one like gridMiddle or page2:gridTopLeft"
            )
        })
}

pub async fn run(args: KeypadArgs) -> Result<()> {
    let connection = agent::connect().await;
    execute(args, connection, &mut io::stdout()).await
}

/// The connection result and output sink let tests drive this without a
/// socket, a running agent, or hardware.
async fn execute(
    args: KeypadArgs,
    connection: Result<AgentClient, ConnectError>,
    out: &mut impl Write,
) -> Result<()> {
    // A file that cannot be sent fails here, before the agent is asked.
    let (key, image) = match args.action {
        KeypadAction::SetImage { key, file } => {
            let unreadable = || format!("could not read {}", file.display());
            let len = fs::metadata(&file).with_context(unreadable)?.len();
            check_key_image_size(usize::try_from(len).unwrap_or(usize::MAX))
                .with_context(|| format!("{} cannot be sent", file.display()))?;
            (key, Some(fs::read(&file).with_context(unreadable)?))
        }
        KeypadAction::ClearImage { key } => (key, None),
    };
    let client = connection.context(
        "keypad requires a running, compatible OpenLogi agent; start the matching OpenLogi build with the same OPENLOGI_PROFILE",
    )?;
    let snapshot = agent::snapshot(&client).await?;
    let (route, name, _) = agent::select_device(
        &snapshot,
        args.device.as_deref(),
        "keypad",
        "a device with LCD keys",
        |_, route| route.is_some_and(|route| key_display_for(route).is_some()),
    )?;
    let key_name = key.config_name();

    if let Some(image) = image {
        agent::call(client.set_key_image(context::current(), route.clone(), key, image))
            .await
            .map_err(call_error)?
            .with_context(|| {
                format!("could not set the image on {key_name} of {name} ({route})")
            })?;
        writeln!(
            out,
            "{name} ({route}): {key_name} shows the image until it is cleared or the agent restarts"
        )?;
    } else {
        agent::call(client.clear_key_image(context::current(), route.clone(), key))
            .await
            .map_err(call_error)?
            .with_context(|| {
                format!("could not clear the image on {key_name} of {name} ({route})")
            })?;
        writeln!(out, "{name} ({route}): {key_name} shows its binding again")?;
    }
    Ok(())
}

fn call_error(failure: CallFailure) -> anyhow::Error {
    match failure {
        CallFailure::TimedOut => anyhow!("agent keypad call timed out"),
        CallFailure::Disconnected => anyhow!("agent disconnected during keypad call"),
    }
}

#[cfg(test)]
mod tests;
