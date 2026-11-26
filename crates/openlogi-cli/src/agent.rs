//! The running agent as the CLI sees it.
//!
//! Two decisions every subcommand that talks to the agent must make the same
//! way: what kind of client the CLI is — one a dormant agent serves without
//! arming its input stack — and how long any one call may take once
//! connected. The handshake's own timeout is `openlogi_ipc::client`'s.

use std::future::Future;
use std::time::Duration;

use anyhow::{Context as _, Result, anyhow, bail};
use openlogi_core::device::Capabilities;
use openlogi_core::hid::DeviceRoute;
use openlogi_ipc::client::{self, ConnectError};
use openlogi_ipc::{AgentClient, AgentSnapshot, ClientKind, InventoryHealth};
use tarpc::client::RpcError;
use tarpc::context;

/// How long the agent may take to answer one call once connected: an agent
/// mid-enumeration, or reading a setting off a device, answers slower than the
/// handshake, but not by more than this.
const CALL_TIMEOUT: Duration = Duration::from_secs(5);

/// Why a call produced no answer. The caller words it: a snapshot says which
/// of the two happened, a fixture read reports both as one refusal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CallFailure {
    /// No answer within [`CALL_TIMEOUT`].
    TimedOut,
    /// The connection dropped before the answer arrived.
    Disconnected,
}

/// Make one call to the agent, bounded by [`CALL_TIMEOUT`]. Every RPC the CLI
/// issues goes through here, so none of them can wait on a wedged agent
/// forever and none of them picks a budget of its own.
pub(crate) async fn call<T>(
    request: impl Future<Output = Result<T, RpcError>>,
) -> Result<T, CallFailure> {
    match tokio::time::timeout(CALL_TIMEOUT, request).await {
        Ok(Ok(answer)) => Ok(answer),
        Ok(Err(_)) => Err(CallFailure::Disconnected),
        Err(_) => Err(CallFailure::TimedOut),
    }
}

/// Connect as the CLI. A dormant agent (launch-at-login off, started at login)
/// serves the query without arming its whole input stack.
pub(crate) async fn connect() -> Result<AgentClient, ConnectError> {
    client::connect_as(ClientKind::Cli).await
}

/// The agent's device picture, or why it did not arrive.
pub(crate) async fn snapshot(client: &AgentClient) -> Result<AgentSnapshot> {
    call(client.snapshot(context::current()))
        .await
        .map_err(|failure| match failure {
            CallFailure::TimedOut => {
                anyhow!("the running Agent timed out while providing its device snapshot")
            }
            CallFailure::Disconnected => {
                anyhow!("the running Agent disconnected while providing its device snapshot")
            }
        })
}

/// The device feature a human-facing command selects a device for.
pub(crate) struct Feature {
    /// The feature as messages name it, such as `backlight`.
    pub(crate) noun: &'static str,
    /// Whether a device's measured capabilities offer the feature. Only such a
    /// device is selected without `--device`.
    pub(crate) measured: fn(&Capabilities) -> bool,
    /// What a device measured without it lacks, such as `HID++ Backlight2
    /// support`.
    pub(crate) missing: &'static str,
}

/// Select the one online HID++ device a human-facing command acts on: the
/// one whose name contains `query` (case-insensitively), or without a query
/// the one online device measured to offer `feature`. Returns its route and
/// display name.
///
/// Refuses an incomplete inventory, no match, several matches, a match with
/// no route or a route another device shares, and a device measured without
/// the feature. Human-readable selection differs from the JSON API's exact
/// opaque IDs. Only the agent's HID++ inventory is eligible, never standalone
/// raw-HID lights.
pub(crate) fn select_device(
    snapshot: &AgentSnapshot,
    query: Option<&str>,
    feature: &Feature,
) -> Result<(DeviceRoute, String)> {
    let noun = feature.noun;
    if snapshot.status.inventory != InventoryHealth::Ready {
        bail!("agent inventory is not ready; wait for OpenLogi to finish scanning and try again");
    }
    let devices: Vec<_> = snapshot
        .inventory
        .iter()
        .flat_map(|inventory| {
            inventory
                .paired
                .iter()
                .filter(|device| device.online)
                .map(|device| {
                    (
                        DeviceRoute::for_slot(inventory, device.slot),
                        device
                            .codename
                            .clone()
                            .unwrap_or_else(|| format!("Slot {}", device.slot)),
                        device.capabilities.as_ref(),
                    )
                })
        })
        .collect();
    let needle = query.map(str::to_lowercase);
    let matches: Vec<_> = devices
        .iter()
        .filter(|(_, name, caps)| {
            needle.as_ref().map_or_else(
                || caps.is_some_and(feature.measured),
                |needle| name.to_lowercase().contains(needle),
            )
        })
        .collect();
    let selected = if let [selected] = matches.as_slice() {
        *selected
    } else {
        let reason = if matches.is_empty() {
            format!("no matching online {noun} device")
        } else {
            format!("ambiguous {noun} device selection")
        };
        let list = devices
            .iter()
            .map(|(route, name, _)| {
                format!(
                    "  - {name} ({})",
                    route
                        .as_ref()
                        .map_or_else(|| "route unavailable".into(), ToString::to_string)
                )
            })
            .collect::<Vec<_>>()
            .join("\n");
        bail!(
            "{reason}; use --device <unique name substring>. Automatic selection requires measured {noun} support.\nOnline HID++ devices:\n{list}"
        );
    };
    let (route, name, caps) = selected;
    let route = route
        .as_ref()
        .context("selected device has no resolvable agent route")?;
    // Distinct names cannot disambiguate identical direct VID/PID routes.
    if devices
        .iter()
        .filter(|(candidate, _, _)| candidate.as_ref() == Some(route))
        .count()
        != 1
    {
        bail!("ambiguous agent route for {name} ({route}); no operation was attempted");
    }
    if caps.is_some_and(|caps| !(feature.measured)(caps)) {
        bail!("{name} does not advertise {}", feature.missing);
    }
    Ok((route.clone(), name.clone()))
}
