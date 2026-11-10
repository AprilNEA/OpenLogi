//! Cross-platform local-socket transport for the agent ↔ GUI tarpc IPC.
//!
//! `interprocess` exposes one API over a Unix-domain socket on Unix and a named
//! pipe on Windows, so the agent (server) and GUI (client) share a single code
//! path. The agent [`bind`]s a listener; the GUI [`connect`]s a stream; each
//! connection is wrapped in a tarpc `serde_transport` `Transport` with the same
//! length-delimited + bincode framing on both ends (see [`wrap`]).
//!
//! Where the endpoint lives and who may reach it is the platform's business:
//! `transport/unix.rs` and `transport/windows.rs` each implement
//! `endpoint_name`, `bind`, and `connect`, and this facade owns the framing.

use std::io;

use interprocess::local_socket::Name;
use interprocess::local_socket::tokio::{Listener, Stream};
use serde::{Serialize, de::DeserializeOwned};
use tarpc::serde_transport::Transport;
use tarpc::tokio_serde::formats::Bincode;

#[cfg(unix)]
mod unix;
#[cfg(windows)]
mod windows;

#[cfg(unix)]
use unix as platform;
#[cfg(windows)]
use windows as platform;

/// Resolve the IPC endpoint name.
///
/// On Unix this is the filesystem Unix-domain socket at
/// [`agent_socket_path`](openlogi_core::paths::agent_socket_path). Production
/// builds keep `~/.config/openlogi/agent.sock`; local macOS `-dev` bundles use
/// the sibling `openlogi-dev` profile so development agents cannot occupy the
/// installed app's endpoint. On Windows it is the named pipe
/// `\\.\pipe\openlogi-agent-<SID>` (`openlogi-dev-agent-<SID>` for the dev
/// profile) — one per user and profile, owned by and ACLed to that user and
/// `SYSTEM` alone (`transport/windows.rs` explains the holes that closes).
///
/// # Errors
///
/// Fails if the home directory can't be resolved (Unix), the caller's SID
/// can't be read (Windows), or the platform rejects the name.
pub fn endpoint_name() -> io::Result<Name<'static>> {
    platform::endpoint_name()
}

/// Bind the agent's IPC listener.
///
/// A failure is the agent's to treat as fatal, not to serve around. On Unix
/// nothing live can own the socket but this process (`main` holds the
/// single-instance lock, and a stale file is reclaimed first), so a failure
/// means the config directory itself is unusable. On Windows the pipe name
/// carries the user and the profile, so it is taken only when another
/// process — another principal, on a shared machine — holds it, and an agent
/// that ran on without its endpoint would leave every client connecting to
/// whoever does.
///
/// # Errors
///
/// Fails if the endpoint name can't be resolved, the socket directory can't be
/// created (Unix), or the listener can't be created — including a Windows pipe
/// name another process already holds.
pub fn bind() -> io::Result<Listener> {
    platform::bind()
}

/// Connect a client stream to the agent's IPC endpoint.
///
/// # Errors
///
/// Fails if the endpoint name can't be resolved, the agent isn't listening,
/// or (Windows) the pipe is owned by another principal.
pub async fn connect() -> io::Result<Stream> {
    platform::connect().await
}

/// Wrap a connected local-socket [`Stream`] in a tarpc transport with the
/// length-delimited + bincode framing both sides expect. The `Item`/`SinkItem`
/// types are inferred by the caller (`BaseChannel` on the server, `*Client::new`
/// on the client).
#[must_use]
pub fn wrap<Item, SinkItem>(
    stream: Stream,
) -> Transport<Stream, Item, SinkItem, Bincode<Item, SinkItem>>
where
    Item: DeserializeOwned,
    SinkItem: Serialize,
{
    Transport::from((stream, Bincode::default()))
}
