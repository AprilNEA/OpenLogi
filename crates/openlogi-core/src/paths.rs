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
//! `%USERPROFILE%\.config\openlogi` etc. — best-effort until a real Windows
//! port lands.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use thiserror::Error;

/// Subdirectory created under each XDG base directory.
const APP_DIR: &str = "openlogi";

#[derive(Debug, Error)]
pub enum PathsError {
    #[error("could not resolve a home directory for the current user")]
    HomeNotFound,
}

/// The user's home directory: `$HOME`, falling back to `%USERPROFILE%`.
fn home() -> Result<PathBuf, PathsError> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .filter(|h| !h.is_empty())
        .map(PathBuf::from)
        .ok_or(PathsError::HomeNotFound)
}

/// Resolve an XDG base directory plus the [`APP_DIR`] subdir.
///
/// Honours `env_value` only when it is an absolute path — per the spec a
/// relative `$XDG_*_HOME` is invalid and must be ignored — otherwise falls
/// back to `$HOME/<fallback>`. Split from the `std::env` read so the
/// branching can be unit-tested without mutating process-global env vars.
fn xdg_base(env_value: Option<OsString>, fallback: &[&str]) -> Result<PathBuf, PathsError> {
    match env_value {
        Some(v) if Path::new(&v).is_absolute() => Ok(PathBuf::from(v).join(APP_DIR)),
        _ => {
            let mut dir = home()?;
            dir.extend(fallback);
            dir.push(APP_DIR);
            Ok(dir)
        }
    }
}

/// Directory holding the user's `config.toml`.
///
/// `$XDG_CONFIG_HOME/openlogi`, default `~/.config/openlogi`.
pub fn config_dir() -> Result<PathBuf, PathsError> {
    xdg_base(std::env::var_os("XDG_CONFIG_HOME"), &[".config"])
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
    xdg_base(std::env::var_os("XDG_DATA_HOME"), &[".local", "share"])
}

/// Resolve the runtime directory holding the agent's IPC socket. Honours an
/// absolute `$XDG_RUNTIME_DIR` (Linux); otherwise falls back to [`config_dir`]
/// — macOS has no `$XDG_RUNTIME_DIR`, and the config dir is already
/// user-private. Split from the env read so the branch is unit-testable.
fn runtime_base(env_value: Option<OsString>) -> Result<PathBuf, PathsError> {
    match env_value {
        Some(v) if Path::new(&v).is_absolute() => Ok(PathBuf::from(v).join(APP_DIR)),
        _ => config_dir(),
    }
}

/// Directory for runtime sockets — the background agent's IPC endpoint.
pub fn runtime_dir() -> Result<PathBuf, PathsError> {
    runtime_base(std::env::var_os("XDG_RUNTIME_DIR"))
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
    fn absolute_xdg_override_is_used_verbatim() {
        let dir = xdg_base(Some("/tmp/xdg-config".into()), &[".config"])
            .expect("absolute override needs no home dir");
        assert_eq!(dir, PathBuf::from("/tmp/xdg-config/openlogi"));
    }

    #[test]
    fn relative_xdg_value_is_ignored_per_spec() {
        // A relative $XDG_*_HOME is invalid, so this must fall back to
        // $HOME/.config/openlogi rather than honour the relative value.
        let dir = xdg_base(Some("relative/dir".into()), &[".config"]).expect("home dir resolves");
        assert!(dir.ends_with("openlogi"));
        assert!(!dir.to_string_lossy().contains("relative"));
    }

    #[test]
    fn absolute_runtime_dir_is_used_verbatim() {
        let dir = runtime_base(Some("/run/user/501".into())).expect("absolute override");
        assert_eq!(dir, PathBuf::from("/run/user/501/openlogi"));
    }

    #[test]
    fn relative_runtime_dir_falls_back_to_config() {
        // A relative $XDG_RUNTIME_DIR is invalid, so this falls back to the
        // config dir (also ending in openlogi) rather than honouring it.
        let dir = runtime_base(Some("relative/run".into())).expect("config dir resolves");
        assert!(dir.ends_with("openlogi"));
        assert!(!dir.to_string_lossy().contains("relative"));
    }
}
