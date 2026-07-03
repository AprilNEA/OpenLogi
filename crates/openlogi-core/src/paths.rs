//! Per-OS application directories, following the XDG Base Directory spec on
//! **every** platform — including macOS, so configuration lives at the
//! familiar `~/.config/openlogi/` rather than macOS's
//! `~/Library/Application Support/`.
//!
//! | kind   | env override        | default                       |
//! |--------|---------------------|-------------------------------|
//! | config | `$XDG_CONFIG_HOME`  | `~/.config/openlogi`          |
//! | data   | `$XDG_DATA_HOME`    | `~/.local/share/openlogi`     |
//!
//! On Windows `$HOME` falls back to `%USERPROFILE%`, so paths resolve to
//! `%USERPROFILE%\.config\openlogi` etc.
//!
//! **Decision (#347):** the Windows location is final, not best-effort.
//! XDG-on-every-platform is this module's deliberate design — macOS also
//! skips its native `~/Library/Application Support` — and Windows follows
//! the same rule rather than `%APPDATA%`. Recorded before the agent first
//! shipped in Windows artifacts, because moving it afterwards would strand
//! every existing user's `config.toml` and the agent's first-run state.

use std::path::PathBuf;

use etcetera::{BaseStrategy, base_strategy::Xdg};
use thiserror::Error;

/// Subdirectory created under each XDG base directory.
const APP_DIR: &str = "openlogi";

#[derive(Debug, Error)]
pub enum PathsError {
    #[error("could not resolve a home directory for the current user")]
    HomeNotFound,
}

fn xdg() -> Result<Xdg, PathsError> {
    Xdg::new().map_err(|_| PathsError::HomeNotFound)
}

/// The raw XDG config home directory (without the `openlogi` subdirectory).
///
/// Honours an absolute `$XDG_CONFIG_HOME`; falls back to `~/.config`.
/// Useful when placing files that belong to other apps under the same base
/// (e.g. systemd user units at `$XDG_CONFIG_HOME/systemd/user/`).
pub fn xdg_config_home() -> Result<PathBuf, PathsError> {
    Ok(xdg()?.config_dir())
}

/// Directory holding the user's `config.toml`.
///
/// `$XDG_CONFIG_HOME/openlogi`, default `~/.config/openlogi`.
pub fn config_dir() -> Result<PathBuf, PathsError> {
    Ok(xdg_config_home()?.join(APP_DIR))
}

/// Full path to the user config file.
pub fn config_path() -> Result<PathBuf, PathsError> {
    Ok(config_dir()?.join("config.toml"))
}

/// Directory for downloaded application data; the device-render asset cache
/// lives under `data_dir()/assets`.
///
/// `$XDG_DATA_HOME/openlogi`, default `~/.local/share/openlogi`.
pub fn data_dir() -> Result<PathBuf, PathsError> {
    Ok(xdg()?.data_dir().join(APP_DIR))
}

/// Directory for runtime sockets — the background agent's IPC endpoint.
pub fn runtime_dir() -> Result<PathBuf, PathsError> {
    let xdg = xdg()?;
    Ok(xdg
        .runtime_dir()
        .map_or_else(|| xdg.config_dir().join(APP_DIR), |dir| dir.join(APP_DIR)))
}

/// Path to the background agent's Unix-domain IPC socket: the GUI connects here
/// to reach the agent that owns device I/O.
pub fn agent_socket_path() -> Result<PathBuf, PathsError> {
    Ok(runtime_dir()?.join("agent.sock"))
}

#[cfg(all(test, unix))]
#[allow(clippy::expect_used, reason = "expect/unwrap are idiomatic in tests")]
mod tests {
    use super::*;

    #[test]
    fn config_dir_keeps_openlogi_under_xdg_config_home() {
        assert!(config_dir().expect("config dir").ends_with("openlogi"));
    }

    #[test]
    fn data_dir_keeps_openlogi_under_xdg_data_home() {
        assert!(data_dir().expect("data dir").ends_with("openlogi"));
    }

    #[test]
    fn runtime_dir_keeps_openlogi_suffix() {
        assert!(runtime_dir().expect("runtime dir").ends_with("openlogi"));
    }
}
