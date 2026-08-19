//! Connecting a client to the agent, handshake included.
//!
//! Every client — the settings app and the overlay helper alike — has to
//! connect, wrap the stream, spawn the tarpc client, and read the agent's
//! protocol version before issuing any real RPC. Only the *policy* on a
//! mismatch differs between them (the app tells the user which side is stale;
//! the overlay yields its role), so the mechanics live here and the version
//! comes back for the caller to judge.

use tarpc::client::{self, RpcError};
use tarpc::context;

use crate::{AgentClient, transport};

/// Why a client could not be established.
///
/// No caller distinguishes these today — both treat any failure as "no usable
/// agent" — but the two are genuinely different conditions, and collapsing
/// them in the type would throw away the reason before anyone can log it.
#[derive(Debug, thiserror::Error)]
pub enum ConnectError {
    /// The agent's socket could not be reached: it is not running, not
    /// listening yet, or the endpoint name could not be resolved.
    #[error("could not reach the agent's IPC endpoint: {0}")]
    Endpoint(#[from] std::io::Error),
    /// The socket accepted the connection but the agent never answered the
    /// handshake — a hung or dying agent rather than an absent one.
    #[error("the agent did not answer the protocol handshake: {0}")]
    Handshake(#[from] RpcError),
}

/// A live client and the protocol version the agent reported.
///
/// The version is deliberately *not* checked here. Compare it against
/// [`PROTOCOL_VERSION`](crate::PROTOCOL_VERSION) and act on the direction:
/// an older agent is waiting to be replaced, a newer one means this process
/// is the stale side.
pub struct Connection {
    /// The spawned tarpc client, ready for calls.
    pub client: AgentClient,
    /// What the agent answered to `protocol_version` — method 0, wire-stable
    /// across every version, and therefore the only call worth making before
    /// the two sides are known to agree.
    pub version: u32,
}

/// Connect to the agent and complete the protocol handshake.
///
/// # Errors
///
/// Returns [`ConnectError::Endpoint`] if the socket cannot be reached, or
/// [`ConnectError::Handshake`] if the agent does not answer the version call.
pub async fn connect() -> Result<Connection, ConnectError> {
    let stream = transport::connect().await?;
    let client = AgentClient::new(client::Config::default(), transport::wrap(stream)).spawn();
    let version = client.protocol_version(context::current()).await?;
    Ok(Connection { client, version })
}
