//! Cross-platform local-socket transport for the agent ↔ GUI tarpc IPC.
//!
//! `interprocess` exposes one API over a Unix-domain socket on Unix and a named
//! pipe on Windows, so the agent (server) and GUI (client) share a single code
//! path. The agent [`bind`]s a listener; the GUI [`connect`]s a stream; each
//! connection is wrapped in a tarpc `serde_transport` `Transport` with the same
//! length-delimited + bincode framing on both ends (see [`wrap`]).
//!
//! This replaces the previous Unix-domain-socket-only `tarpc::serde_transport::unix`
//! path, which did not compile on Windows.

use std::io;

use interprocess::local_socket::ListenerOptions;
use interprocess::local_socket::tokio::prelude::*;
use interprocess::local_socket::tokio::{Listener, Stream};
use serde::{Serialize, de::DeserializeOwned};
use tarpc::serde_transport::Transport;
use tarpc::tokio_serde::formats::Bincode;

/// Resolve the IPC endpoint name.
///
/// On Unix this is the filesystem Unix-domain socket at
/// [`agent_socket_path`](openlogi_core::paths::agent_socket_path) (preserving
/// the existing `~/.config/openlogi/agent.sock` location, so macOS/Linux see no
/// behavior change). On Windows it is a named pipe in the OS namespace
/// (`\\.\pipe\openlogi-agent.sock`).
///
/// # Errors
///
/// Fails if the home directory can't be resolved (Unix) or the platform rejects
/// the name.
pub fn endpoint_name() -> io::Result<interprocess::local_socket::Name<'static>> {
    #[cfg(unix)]
    {
        use interprocess::local_socket::{GenericFilePath, ToFsName};
        openlogi_core::paths::agent_socket_path()
            .map_err(|e| io::Error::other(e.to_string()))?
            .to_fs_name::<GenericFilePath>()
    }
    #[cfg(windows)]
    {
        use interprocess::local_socket::{GenericNamespaced, ToNsName};
        // A fixed per-machine pipe name. The default DACL grants the creating
        // user + administrators, which is acceptable for a single-user desktop;
        // per-user isolation on a shared machine is a future hardening point.
        "openlogi-agent.sock".to_ns_name::<GenericNamespaced>()
    }
}

/// Bind the agent's IPC listener.
///
/// `try_overwrite(true)` unlinks a stale Unix-domain socket left by a *non-clean*
/// exit (SIGKILL / panic=abort / power loss, where the listener's `Drop` never
/// ran) before binding — otherwise `bind` fails with `AddrInUse` on the leftover
/// file, since the OS does not remove a socket inode on process death, and the
/// agent's IPC would stay dead across every relaunch. `main` holds the
/// single-instance lock, so no *live* agent owns the socket. No-op for Windows
/// named pipes (no filesystem entry to reclaim).
///
/// # Errors
///
/// Fails if the endpoint name can't be resolved, the socket directory can't be
/// created (Unix), or the listener can't be created.
pub fn bind() -> io::Result<Listener> {
    // On Unix the socket is a filesystem path; ensure its directory exists.
    #[cfg(unix)]
    if let Some(parent) = openlogi_core::paths::agent_socket_path()
        .map_err(|e| io::Error::other(e.to_string()))?
        .parent()
    {
        std::fs::create_dir_all(parent)?;
    }
    ListenerOptions::new()
        .name(endpoint_name()?)
        .try_overwrite(true)
        .create_tokio()
}

/// Connect a client stream to the agent's IPC endpoint.
///
/// # Errors
///
/// Fails if the endpoint name can't be resolved or the agent isn't listening.
pub async fn connect() -> io::Result<Stream> {
    Stream::connect(endpoint_name()?).await
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
