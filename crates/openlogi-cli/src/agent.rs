//! The running agent as the CLI sees it.
//!
//! Two decisions every subcommand that talks to the agent must make the same
//! way: what kind of client the CLI is — one a dormant agent serves without
//! arming its input stack — and how long any one call may take once
//! connected. The handshake's own timeout is `openlogi_ipc::client`'s.

use std::future::Future;
use std::time::Duration;

use anyhow::{Result, anyhow};
use openlogi_ipc::client::{self, ConnectError};
use openlogi_ipc::{AgentClient, AgentSnapshot, ClientKind};
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
