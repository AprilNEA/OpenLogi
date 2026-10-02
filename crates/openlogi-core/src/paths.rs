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
//!
//! **Windows only, production profile only:** a run from anywhere other than
//! the MSI's fixed per-user install directory (`%LOCALAPPDATA%\Programs\OpenLogi`,
//! see `packaging/windows/OpenLogi.wxs`'s `INSTALLFOLDER`) is a portable
//! `.zip` release, extracted and run from wherever the user put it — a USB
//! drive, another machine, anywhere. [`config_dir`]/[`data_dir`]/[`state_dir`]
//! then resolve under a `data` folder next to the executable instead of the
//! user profile, so moving that folder (executable included) carries the data
//! with it. This does not revisit decision #347: an MSI install's exe path is
//! fixed, so every existing installed user keeps resolving to the same
//! `%USERPROFILE%\.config\openlogi` they always have — only a portable run,
//! which had no stranded data to begin with, takes the new path.

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
    /// detected on macOS from the bundle the executable lives in, or —
    /// on every platform, since only macOS has a packaged dev-bundle
    /// mechanism — detected from the executable still sitting in a Cargo
    /// `target/debug`/`target/release` output directory. Memoized — the
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

        // Only macOS has a packaged dev-bundle mechanism (`xtask macos
        // dev-bundle`); without this fallback, an unpackaged dev build on
        // Windows or Linux silently lands on the production profile and
        // shares the installed app's config directory, IPC socket, and
        // single-instance lock.
        if running_from_cargo_target() {
            return Self::Dev;
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

/// The MSI's fixed per-user install directory — the layout only that
/// installer produces (`packaging/windows/OpenLogi.wxs`'s `INSTALLFOLDER`).
#[cfg(windows)]
fn windows_msi_install_dir(local_app_data: &std::path::Path) -> PathBuf {
    local_app_data.join("Programs").join("OpenLogi")
}

/// Whether `exe_dir` is a portable Windows run: the production profile,
/// launched from somewhere other than [`windows_msi_install_dir`]. `false`
/// for an MSI install (keeps decision #347's location), the dev profile
/// (already isolated by [`DEV_APP_DIR`]), and when `local_app_data` can't be
/// resolved — treating an unresolvable installed-location check as "not
/// installed" would move a real install's data on nothing more than a
/// missing environment variable. Windows paths are case-insensitive, so the
/// comparison is too. A pure function of its inputs so it's testable without
/// mutating the process environment or `current_exe()`.
#[cfg(windows)]
fn is_windows_portable(
    profile: Profile,
    exe_dir: &std::path::Path,
    local_app_data: Option<&std::path::Path>,
) -> bool {
    if profile != Profile::Production {
        return false;
    }
    let Some(local_app_data) = local_app_data else {
        return false;
    };
    let installed = windows_msi_install_dir(local_app_data);
    !installed
        .as_os_str()
        .eq_ignore_ascii_case(exe_dir.as_os_str())
}

/// The executable's own directory, when [`is_windows_portable`] holds for
/// the current process.
#[cfg(windows)]
fn windows_portable_root() -> Option<PathBuf> {
    let exe_dir = std::env::current_exe().ok()?.parent()?.to_path_buf();
    let local_app_data = std::env::var_os("LOCALAPPDATA").map(PathBuf::from);
    is_windows_portable(Profile::current(), &exe_dir, local_app_data.as_deref()).then_some(exe_dir)
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
/// `$XDG_CONFIG_HOME/openlogi`, default `~/.config/openlogi`. Local macOS dev
/// builds use `openlogi-dev` instead; a portable Windows run uses `data` next
/// to the executable instead — see `windows_portable_root` below.
pub fn config_dir() -> Result<PathBuf, PathsError> {
    #[cfg(windows)]
    if let Some(root) = windows_portable_root() {
        return Ok(root.join("data"));
    }
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
/// `$XDG_DATA_HOME/openlogi`, default `~/.local/share/openlogi`. Local macOS
/// dev builds use `openlogi-dev` instead; a portable Windows run uses `data`
/// next to the executable instead, the same folder [`config_dir`] uses — see
/// `windows_portable_root` below.
pub fn data_dir() -> Result<PathBuf, PathsError> {
    #[cfg(windows)]
    if let Some(root) = windows_portable_root() {
        return Ok(root.join("data"));
    }
    Ok(xdg()?.data_dir().join(app_dir()))
}

/// Directory for logs and other rebuildable process state — the agent's
/// rotated log files live here.
///
/// `$XDG_STATE_HOME/openlogi`, default `~/.local/state/openlogi`. Local
/// macOS dev builds use `openlogi-dev` instead; a portable Windows run uses
/// `data\state` next to the executable instead — see
/// `windows_portable_root` below.
pub fn state_dir() -> Result<PathBuf, PathsError> {
    #[cfg(windows)]
    if let Some(root) = windows_portable_root() {
        return Ok(root.join("data").join("state"));
    }
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
    use super::{Profile, agent_socket_path_for, config_dir_for, is_cargo_target_path};

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

#[cfg(windows)]
#[cfg(test)]
mod windows_portable_tests {
    use std::path::Path;

    use super::{Profile, is_windows_portable, windows_msi_install_dir};

    #[test]
    fn msi_install_dir_sits_under_local_app_data_programs() {
        assert_eq!(
            windows_msi_install_dir(Path::new(r"C:\Users\dev\AppData\Local")),
            Path::new(r"C:\Users\dev\AppData\Local\Programs\OpenLogi")
        );
    }

    #[test]
    fn an_msi_install_is_never_portable() {
        let local_app_data = Path::new(r"C:\Users\dev\AppData\Local");
        let installed = windows_msi_install_dir(local_app_data);
        assert!(!is_windows_portable(
            Profile::Production,
            &installed,
            Some(local_app_data)
        ));
    }

    #[test]
    fn an_msi_install_path_comparison_is_case_insensitive() {
        let local_app_data = Path::new(r"C:\Users\dev\AppData\Local");
        assert!(!is_windows_portable(
            Profile::Production,
            Path::new(r"c:\users\dev\appdata\local\programs\openlogi"),
            Some(local_app_data)
        ));
    }

    #[test]
    fn a_run_from_anywhere_else_is_portable() {
        let local_app_data = Path::new(r"C:\Users\dev\AppData\Local");
        for exe_dir in [
            Path::new(r"D:\OpenLogiPortable"),
            Path::new(r"C:\Users\dev\Desktop\OpenLogi"),
            Path::new(r"C:\Users\dev\AppData\Local\Programs\OpenLogiOld"),
        ] {
            assert!(
                is_windows_portable(Profile::Production, exe_dir, Some(local_app_data)),
                "{exe_dir:?} should be treated as portable"
            );
        }
    }

    #[test]
    fn the_dev_profile_is_never_portable_even_when_relocated() {
        assert!(!is_windows_portable(
            Profile::Dev,
            Path::new(r"D:\OpenLogiPortable"),
            Some(Path::new(r"C:\Users\dev\AppData\Local"))
        ));
    }

    #[test]
    fn an_unresolvable_local_app_data_falls_back_to_not_portable() {
        assert!(!is_windows_portable(
            Profile::Production,
            Path::new(r"D:\OpenLogiPortable"),
            None
        ));
    }
}
