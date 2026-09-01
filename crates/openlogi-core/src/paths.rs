//! Per-OS application directories, following the XDG Base Directory spec on
//! **every** platform — including macOS, so configuration lives at the
//! familiar `~/.config/openlogi/` rather than macOS's
//! `~/Library/Application Support/`.
//!
//! | kind   | env override        | default                       |
//! |--------|---------------------|-------------------------------|
//! | config | `$XDG_CONFIG_HOME`  | `~/.config/openlogi`          |
//! | data   | `$XDG_DATA_HOME`    | `~/.local/share/openlogi`     |
//! | state  | `$XDG_STATE_HOME`   | `~/.local/state/openlogi`     |
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

//! Local packaged macOS builds stamped with dev-channel identifiers use the
//! same layout under an `openlogi-dev` app directory.

use std::path::PathBuf;
use std::sync::OnceLock;

use etcetera::{BaseStrategy, base_strategy::Xdg};
use thiserror::Error;

/// Production subdirectory created under each XDG base directory.
///
/// Public because the dev tooling has to name the same directories from
/// outside a running app — `xtask macos dev-bundle` remembers the developer's
/// codesigning certificate under [`DEV_APP_DIR`], where `cargo clean` cannot
/// reach it.
pub const APP_DIR: &str = "openlogi";
/// Local macOS dev builds use a separate profile so development agents
/// cannot take over the installed app's socket, lock, config, or asset cache.
pub const DEV_APP_DIR: &str = "openlogi-dev";

/// Failure resolving the per-user base directories.
#[derive(Debug, Error)]
pub enum PathsError {
    /// No home directory could be determined for the current user, so none
    /// of the XDG bases resolve.
    #[error("could not resolve a home directory for the current user")]
    HomeNotFound,
}

fn xdg() -> Result<Xdg, PathsError> {
    Xdg::new().map_err(|_| PathsError::HomeNotFound)
}

fn app_dir() -> &'static str {
    if is_dev_profile() {
        DEV_APP_DIR
    } else {
        APP_DIR
    }
}

/// Whether this process runs under the dev profile: forced by
/// `OPENLOGI_PROFILE=dev`/`prod`, or (macOS) detected from the bundle the
/// executable lives in carrying a dev identifier. Decides the
/// [`APP_DIR`]/[`DEV_APP_DIR`] split for every directory below, and which
/// launchd service label the GUI manages. Memoized — the answer cannot change
/// within a process lifetime.
#[must_use]
pub fn is_dev_profile() -> bool {
    static IS_DEV_PROFILE: OnceLock<bool> = OnceLock::new();
    *IS_DEV_PROFILE.get_or_init(detect_dev_profile)
}

fn detect_dev_profile() -> bool {
    match std::env::var("OPENLOGI_PROFILE") {
        Ok(value) if value == "dev" => return true,
        Ok(value) if matches!(value.as_str(), "prod" | "production") => return false,
        _ => {}
    }

    #[cfg(target_os = "macos")]
    {
        if let Some(identifier) = current_bundle_identifier() {
            return crate::brand::is_dev_id(&identifier);
        }
    }

    false
}

#[cfg(target_os = "macos")]
fn current_bundle_identifier() -> Option<String> {
    let exe = std::env::current_exe().ok()?;
    for ancestor in exe.ancestors() {
        if !ancestor
            .extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("app"))
        {
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

/// The current user's home directory.
///
/// The plain home, not an XDG base — for callers placing files under
/// OS-native locations (e.g. macOS `~/Library/LaunchAgents`).
pub fn home_dir() -> Result<PathBuf, PathsError> {
    Ok(xdg()?.home_dir().to_path_buf())
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
/// Local macOS dev builds use `openlogi-dev` instead.
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
/// Local macOS dev builds use `openlogi-dev` instead.
pub fn data_dir() -> Result<PathBuf, PathsError> {
    Ok(xdg()?.data_dir().join(app_dir()))
}

/// Directory for logs and other rebuildable process state — the agent's
/// rotated log files live here.
///
/// `$XDG_STATE_HOME/openlogi`, default `~/.local/state/openlogi`.
/// Local macOS dev builds use `openlogi-dev` instead.
pub fn state_dir() -> Result<PathBuf, PathsError> {
    let xdg = xdg()?;
    Ok(xdg
        .state_dir()
        .map_or_else(|| xdg.data_dir().join(app_dir()), |dir| dir.join(app_dir())))
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

#[cfg(test)]
#[cfg(unix)]
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
