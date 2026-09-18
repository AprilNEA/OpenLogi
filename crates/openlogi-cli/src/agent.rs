//! The running agent as the CLI sees it.
//!
//! Two decisions every subcommand that asks the agent for its device picture
//! must make the same way: what kind of client the CLI is — one a dormant
//! agent serves without arming its input stack — and how long a snapshot may
//! take once connected. The handshake's own deadline is
//! `openlogi_ipc::client`'s.

use std::time::Duration;

use anyhow::{Result, anyhow};
use openlogi_ipc::client::{self, ConnectError};
use openlogi_ipc::{AgentClient, AgentSnapshot, ClientKind};
use tarpc::context;

/// How long the agent may take to answer a snapshot once connected: an agent
/// mid-enumeration answers slower than the handshake, but not by more than
/// this.
const SNAPSHOT_TIMEOUT: Duration = Duration::from_secs(5);

/// Connect as the CLI. A dormant agent (launch-at-login off, started at login)
/// serves the query without arming its whole input stack.
pub(crate) async fn connect() -> Result<AgentClient, ConnectError> {
    client::connect_as(ClientKind::Cli).await
}

/// The agent's device picture, or why it did not arrive within
/// [`SNAPSHOT_TIMEOUT`].
pub(crate) async fn snapshot(client: &AgentClient) -> Result<AgentSnapshot> {
    tokio::time::timeout(SNAPSHOT_TIMEOUT, client.snapshot(context::current()))
        .await
        .map_err(|_| anyhow!("the running Agent timed out while providing its device snapshot"))?
        .map_err(|_| anyhow!("the running Agent disconnected while providing its device snapshot"))
}
