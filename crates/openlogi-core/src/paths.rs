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

//! An unpackaged dev build uses the same layout under an `openlogi-dev` app
//! directory instead: a local packaged macOS build stamped with a dev-channel
//! bundle identifier, or (on any platform, since only macOS has a packaged
//! dev-bundle mechanism) an executable still sitting in Cargo's own
//! `target/debug`/`target/release` output.

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
/// `OPENLOGI_PROFILE=dev`/`prod`, detected on macOS from the bundle the
/// executable lives in carrying a dev identifier, or — on every platform,
/// since only macOS has a packaged dev-bundle mechanism — detected from the
/// executable still sitting in a Cargo `target/debug`/`target/release`
/// output directory. Decides the [`APP_DIR`]/[`DEV_APP_DIR`] split for every
/// directory below, and which launchd service label the GUI manages.
/// Memoized — the answer cannot change within a process lifetime.
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

    running_from_cargo_target()
}

/// True when the running executable lives inside a Cargo build output
/// directory (`target/debug/…`, `target/release/…`, or a `--target
/// <triple>` cross-compile's `target/<triple>/debug/…` /
/// `target/<triple>/release/…`) — the layout every `cargo build`/`cargo run`
/// produces and no installer ever does. Only Windows and Linux have no
/// packaged dev-bundle mechanism (that is macOS-only, see `xtask macos
/// dev-bundle`), so without this fallback an unpackaged dev build on those
/// platforms silently lands on the production profile and shares the
/// installed app's config directory, IPC socket, and single-instance lock.
fn running_from_cargo_target() -> bool {
    std::env::current_exe().is_ok_and(|exe| is_cargo_target_path(&exe))
}

fn is_cargo_target_path(exe: &std::path::Path) -> bool {
    let components: Vec<_> = exe
        .components()
        .filter_map(|component| component.as_os_str().to_str())
        .collect();
    components
        .windows(2)
        .any(|pair| pair[0] == "target" && matches!(pair[1], "debug" | "release"))
        || components
            .windows(3)
            .any(|triple| triple[0] == "target" && matches!(triple[2], "debug" | "release"))
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

    // The test binary itself runs from `target/debug/deps/…`, so
    // `is_dev_profile()` correctly reports dev here (proven by
    // `cargo_target_debug_and_release_binaries_are_dev` below) and `app_dir()`
    // resolves to `DEV_APP_DIR`. Accept either app dir rather than asserting
    // the production one.
    fn ends_with_an_openlogi_app_dir(path: &std::path::Path) -> bool {
        path.ends_with(APP_DIR) || path.ends_with(DEV_APP_DIR)
    }

    #[test]
    fn config_dir_keeps_openlogi_under_xdg_config_home() {
        assert!(ends_with_an_openlogi_app_dir(
            &config_dir().expect("config dir")
        ));
    }

    #[test]
    fn data_dir_keeps_openlogi_under_xdg_data_home() {
        assert!(ends_with_an_openlogi_app_dir(
            &data_dir().expect("data dir")
        ));
    }

    #[test]
    fn runtime_dir_keeps_openlogi_suffix() {
        assert!(ends_with_an_openlogi_app_dir(
            &runtime_dir().expect("runtime dir")
        ));
    }
}

// `is_cargo_target_path` is pure path-string logic with no XDG/home
// dependency, and it exists specifically to cover Windows (the platform with
// no packaged dev-bundle mechanism) — unlike the `#[cfg(unix)]` module above,
// it must compile and run there too.
#[cfg(test)]
mod cargo_target_path_tests {
    use super::is_cargo_target_path;

    // `/`-separated paths only: `std::path::Path` parses separators for the
    // *compiling* target, so a literal `C:\...` string only splits into
    // components when this test itself is compiled for Windows — on the
    // Linux/macOS hosts that actually run `cargo test` (no CI test job
    // targets Windows, only cross-compiled clippy) it would stay one opaque
    // component and silently fail this test. `/` is accepted as a separator
    // on every target Rust supports, Windows included, so it exercises the
    // same `Path::components()` logic on all three hosts.
    #[test]
    fn cargo_target_debug_and_release_binaries_are_dev() {
        assert!(is_cargo_target_path(std::path::Path::new(
            "/home/dev/openlogi/target/debug/openlogi-desktop"
        )));
        assert!(is_cargo_target_path(std::path::Path::new(
            "/home/dev/openlogi/target/release/openlogi-desktop"
        )));
        assert!(is_cargo_target_path(std::path::Path::new(
            "C:/Users/dev/openlogi/target/debug/openlogi-desktop.exe"
        )));
    }

    // `cargo build --target <triple>` (used for cross-compiled clippy checks
    // in this repo, e.g. `x86_64-pc-windows-gnu`) nests an extra
    // target-triple directory between `target/` and `debug`/`release`.
    #[test]
    fn cross_compiled_target_triple_binaries_are_dev() {
        assert!(is_cargo_target_path(std::path::Path::new(
            "/home/dev/openlogi/target/x86_64-pc-windows-gnu/debug/openlogi-desktop.exe"
        )));
        assert!(is_cargo_target_path(std::path::Path::new(
            "/home/dev/openlogi/target/aarch64-unknown-linux-musl/release/openlogi-agent"
        )));
    }

    #[test]
    fn an_installed_binary_is_not_mistaken_for_a_cargo_target_build() {
        assert!(!is_cargo_target_path(std::path::Path::new(
            "/usr/bin/openlogi-desktop"
        )));
        assert!(!is_cargo_target_path(std::path::Path::new(
            "/opt/openlogi/openlogi-desktop"
        )));
        assert!(!is_cargo_target_path(std::path::Path::new(
            "C:/Program Files/OpenLogi/openlogi-desktop.exe"
        )));
        // A stray "target" *file/dir name* that isn't Cargo's own build
        // output (e.g. a user directory literally named "target") must not
        // be mistaken for one — only "target/debug" or "target/release".
        assert!(!is_cargo_target_path(std::path::Path::new(
            "/home/user/target/openlogi-desktop"
        )));
    }
}
