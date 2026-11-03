//! Unix-domain socket endpoint: the profile-aware path from
//! `openlogi_core::paths`, under the caller's own config directory.

use std::io;
use std::path::PathBuf;

use interprocess::local_socket::tokio::prelude::*;
use interprocess::local_socket::tokio::{Listener, Stream};
use interprocess::local_socket::{GenericFilePath, ListenerOptions, Name};

pub(super) fn endpoint_name() -> io::Result<Name<'static>> {
    socket_path()?.to_fs_name::<GenericFilePath>()
}

/// `try_overwrite(true)` unlinks a stale socket left by a *non-clean* exit
/// (SIGKILL / panic=abort / power loss, where the listener's `Drop` never ran)
/// before binding — otherwise `bind` fails with `AddrInUse` on the leftover
/// file, since the OS does not remove a socket inode on process death, and the
/// agent's IPC would stay dead across every relaunch. `main` holds the
/// single-instance lock, so no *live* agent owns the socket.
pub(super) fn bind() -> io::Result<Listener> {
    let path = socket_path()?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    ListenerOptions::new()
        .name(path.to_fs_name::<GenericFilePath>()?)
        .try_overwrite(true)
        .create_tokio()
}

pub(super) async fn connect() -> io::Result<Stream> {
    Stream::connect(endpoint_name()?).await
}

fn socket_path() -> io::Result<PathBuf> {
    openlogi_core::paths::agent_socket_path().map_err(|e| io::Error::other(e.to_string()))
}
