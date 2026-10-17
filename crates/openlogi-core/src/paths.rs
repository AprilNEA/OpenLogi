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
const APP_DIR: &str = "openlogi";
/// Local macOS dev builds use a separate profile so development agents
/// cannot take over the installed app's socket, lock, config, or asset cache.
const DEV_APP_DIR: &str = "openlogi-dev";
/// The user's configuration file, under [`config_dir`].
pub const CONFIG_FILE: &str = "config.toml";

/// Which of the two side-by-side installs a process belongs to.
///
/// Decides the per-profile directory under every XDG base, so a local dev
/// build never touches the shipped app's socket, lock, config, or asset cache.
/// Tooling that has to name a profile's files from outside a process of that
/// profile — `xtask macos dev-bundle` waiting for the dev agent's socket, or
/// remembering the developer's codesigning certificate — asks for that
/// profile's paths by name instead of rebuilding them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Profile {
    /// The shipped app.
    Production,
    /// A local packaged macOS build stamped with dev-channel identifiers.
    Dev,
}

impl Profile {
    /// The profile this process runs under: forced by [`crate::env::PROFILE`],
    /// or detected from the bundle the executable lives in. Memoized — the
    /// answer cannot change within a process lifetime.
    #[must_use]
    pub fn current() -> Self {
        static CURRENT: OnceLock<Profile> = OnceLock::new();
        *CURRENT.get_or_init(Self::detect)
    }

    /// The launchd service label this profile's bundle declares — what its
    /// embedded LaunchAgent plist carries, `SMAppService` registers, and
    /// `launchctl` addresses. The dev variant is suffixed, so a dev
    /// registration can never collide with the shipped one. Frozen once
    /// shipped; see [`crate::brand::AGENT_SERVICE_LABEL`].
    #[must_use]
    pub fn agent_service_label(self) -> String {
        match self {
            Self::Production => crate::brand::AGENT_SERVICE_LABEL.to_owned(),
            Self::Dev => crate::brand::dev_id(crate::brand::AGENT_SERVICE_LABEL),
        }
    }

    fn detect() -> Self {
        match std::env::var(crate::env::PROFILE) {
            Ok(value) if value == Self::Dev.env_value() => return Self::Dev,
            // `production` is the long-hand spelling older docs used.
            Ok(value) if value == Self::Production.env_value() || value == "production" => {
                return Self::Production;
            }
            _ => {}
        }

        #[cfg(target_os = "macos")]
        {
            if let Some(identifier) = current_bundle_identifier()
                && crate::brand::is_dev_id(&identifier)
            {
                return Self::Dev;
            }
        }

        Self::Production
    }

    /// The value of [`crate::env::PROFILE`] that forces this profile.
    #[must_use]
    pub const fn env_value(self) -> &'static str {
        match self {
            Self::Production => "prod",
            Self::Dev => "dev",
        }
    }

    /// The per-profile directory name under each XDG base.
    const fn app_dir(self) -> &'static str {
        match self {
            Self::Production => APP_DIR,
            Self::Dev => DEV_APP_DIR,
        }
    }
}

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
    Profile::current().app_dir()
}

/// Whether this process runs under the dev profile; see [`Profile::current`].
#[must_use]
pub fn is_dev_profile() -> bool {
    Profile::current() == Profile::Dev
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
/// Useful when reading files that belong to another app's namespace under the
/// same base. This tier is the user's own: generated files belong under
/// [`xdg_data_home`] instead, which systemd and friends rank below it.
pub fn xdg_config_home() -> Result<PathBuf, PathsError> {
    Ok(xdg()?.config_dir())
}

/// The raw XDG data home directory (without the `openlogi` subdirectory).
///
/// Honours an absolute `$XDG_DATA_HOME`; falls back to `~/.local/share`.
/// The counterpart to [`xdg_config_home`] for files that belong to another
/// app's namespace under the same base — generated systemd user units at
/// `$XDG_DATA_HOME/systemd/user/`, which is the tier systemd reserves for
/// units installed on the user's behalf rather than authored by them.
pub fn xdg_data_home() -> Result<PathBuf, PathsError> {
    Ok(xdg()?.data_dir())
}

/// Directory holding the user's `config.toml`.
///
/// `$XDG_CONFIG_HOME/openlogi`, default `~/.config/openlogi`.
/// Local macOS dev builds use `openlogi-dev` instead.
pub fn config_dir() -> Result<PathBuf, PathsError> {
    config_dir_for(Profile::current())
}

/// [`config_dir`] for a named profile, for tooling outside that profile.
pub fn config_dir_for(profile: Profile) -> Result<PathBuf, PathsError> {
    Ok(xdg_config_home()?.join(profile.app_dir()))
}

/// Full path to the user config file.
pub fn config_path() -> Result<PathBuf, PathsError> {
    Ok(config_dir()?.join(CONFIG_FILE))
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
    runtime_dir_for(Profile::current())
}

/// [`runtime_dir`] for a named profile, for tooling outside that profile.
pub fn runtime_dir_for(profile: Profile) -> Result<PathBuf, PathsError> {
    let xdg = xdg()?;
    Ok(xdg.runtime_dir().map_or_else(
        || xdg.config_dir().join(profile.app_dir()),
        |dir| dir.join(profile.app_dir()),
    ))
}

/// Path to the background agent's Unix-domain IPC socket: the GUI connects here
/// to reach the agent that owns device I/O.
pub fn agent_socket_path() -> Result<PathBuf, PathsError> {
    agent_socket_path_for(Profile::current())
}

/// [`agent_socket_path`] for a named profile: where tooling waits for a dev
/// agent it just started, without being a dev-profile process itself.
pub fn agent_socket_path_for(profile: Profile) -> Result<PathBuf, PathsError> {
    Ok(runtime_dir_for(profile)?.join("agent.sock"))
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

    #[test]
    fn a_named_profile_resolves_its_own_directories() {
        assert!(
            config_dir_for(Profile::Dev)
                .expect("dev config dir")
                .ends_with("openlogi-dev")
        );
        assert!(
            agent_socket_path_for(Profile::Dev)
                .expect("dev socket")
                .ends_with("openlogi-dev/agent.sock")
        );
        assert!(
            agent_socket_path_for(Profile::Production)
                .expect("production socket")
                .ends_with("openlogi/agent.sock")
        );
    }
}
