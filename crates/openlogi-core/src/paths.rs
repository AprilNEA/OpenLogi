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
//!
//! Local packaged macOS builds stamped with `.dev` bundle identifiers use the
//! same layout under an `openlogi-dev` app directory.

use std::path::PathBuf;
use std::sync::OnceLock;

use etcetera::{BaseStrategy, base_strategy::Xdg};
use thiserror::Error;

/// Production subdirectory created under each XDG base directory.
const APP_DIR: &str = "openlogi";
/// Local macOS `.dev` bundles use a separate profile so development agents
/// cannot take over the installed app's socket, lock, config, or asset cache.
const DEV_APP_DIR: &str = "openlogi-dev";

#[derive(Debug, Error)]
pub enum PathsError {
    #[error("could not resolve a home directory for the current user")]
    HomeNotFound,
}

fn xdg() -> Result<Xdg, PathsError> {
    Xdg::new().map_err(|_| PathsError::HomeNotFound)
}

fn app_dir() -> &'static str {
    static IS_DEV_PROFILE: OnceLock<bool> = OnceLock::new();
    if *IS_DEV_PROFILE.get_or_init(is_dev_profile) {
        DEV_APP_DIR
    } else {
        APP_DIR
    }
}

fn is_dev_profile() -> bool {
    match std::env::var("OPENLOGI_PROFILE") {
        Ok(value) if value == "dev" => return true,
        Ok(value) if matches!(value.as_str(), "prod" | "production") => return false,
        _ => {}
    }

    #[cfg(target_os = "macos")]
    {
        if let Some(identifier) = current_bundle_identifier() {
            return identifier.ends_with(".dev");
        }
    }

    false
}

#[cfg(target_os = "macos")]
fn current_bundle_identifier() -> Option<String> {
    let exe = std::env::current_exe().ok()?;
    for ancestor in exe.ancestors() {
        if ancestor.extension().and_then(|ext| ext.to_str()) != Some("app") {
            continue;
        }

        let info = ancestor.join("Contents/Info.plist");
        let Ok(plist) = plist::Value::from_file(info) else {
            continue;
        };
        let Some(identifier) = plist
            .as_dictionary()
            .and_then(|dictionary| dictionary.get("CFBundleIdentifier"))
            .and_then(plist::Value::as_string)
        else {
            continue;
        };
        return Some(identifier.to_owned());
    }

    None
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
/// Local macOS `.dev` bundles use `openlogi-dev` instead.
pub fn config_dir() -> Result<PathBuf, PathsError> {
    Ok(xdg_config_home()?.join(app_dir()))
}

/// Full path to the user config file.
pub fn config_path() -> Result<PathBuf, PathsError> {
    Ok(config_dir()?.join("config.toml"))
}

/// Directory for downloaded application data; the device-render asset cache
/// lives under `data_dir()/assets`.
///
/// `$XDG_DATA_HOME/openlogi`, default `~/.local/share/openlogi`.
/// Local macOS `.dev` bundles use `openlogi-dev` instead.
pub fn data_dir() -> Result<PathBuf, PathsError> {
    Ok(xdg()?.data_dir().join(app_dir()))
}

/// Directory for runtime sockets — the background agent's IPC endpoint.
pub fn runtime_dir() -> Result<PathBuf, PathsError> {
    let xdg = xdg()?;
    Ok(xdg.runtime_dir().map_or_else(
        || xdg.config_dir().join(app_dir()),
        |dir| dir.join(app_dir()),
    ))
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
