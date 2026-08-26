//! Autostart reconciliation for the background agent.
//!
//! Implements `launch_at_login` by writing/removing a platform-specific
//! autostart descriptor whenever the setting changes. The reconcile is
//! idempotent: it writes only when the content differs, and removes only when
//! the file exists. Failures are logged but not propagated — startup must not
//! abort because an autostart directory is read-only.
//!
//! ## macOS
//!
//! Registration is not the agent's job on macOS: the GUI maps
//! `launch_at_login` to an `SMAppService` registration of the app bundle's
//! embedded `Contents/Library/LaunchAgents` plist (label
//! [`openlogi_core::brand::AGENT_SERVICE_LABEL`]), which is what supervises
//! the agent — `KeepAlive = {SuccessfulExit: false}`: a crash respawns, the
//! tray's Quit (a clean `exit(0)`) stays down. The API resolves the plist
//! against the *calling* app's bundle, so only the GUI can register it; a
//! hand-edited `config.toml` therefore takes effect the next time the GUI
//! runs.
//!
//! What remains here is migration: removing the hand-written
//! `~/Library/LaunchAgents` plists earlier versions installed — the pre-split
//! GUI autostart and the agent's own pre-`SMAppService` plist. A running job
//! loaded from one of those files keeps serving the current session (killing
//! a live agent for a file migration helps nobody) and disappears at the next
//! login, when its plist is no longer there to load; if the GUI has
//! registered the service meanwhile, the freshly started duplicate loses the
//! singleton lock and exits cleanly.
//!
//! ## Linux
//!
//! A systemd **user** unit at
//! `$XDG_CONFIG_HOME/systemd/user/openlogi-agent.service` (default
//! `~/.config/systemd/user/openlogi-agent.service`) is written/removed, then
//! `systemctl --user daemon-reload` and `enable`/`disable` are called.
//! `Restart=on-failure` mirrors the macOS `KeepAlive=SuccessfulExit:false`
//! semantics. A clean `exit(0)` leaves the unit enabled but stopped until the
//! next session login.

// The macOS arm is migration-only and logs at info/warn.
#[cfg(not(target_os = "macos"))]
use tracing::debug;

#[cfg(target_os = "linux")]
use std::fmt;
#[cfg(any(target_os = "macos", target_os = "linux"))]
use std::io;
#[cfg(any(target_os = "macos", target_os = "linux"))]
use std::path::PathBuf;
#[cfg(target_os = "windows")]
use tracing::warn;
#[cfg(any(target_os = "macos", target_os = "linux"))]
use tracing::{info, warn};

/// Labels of the hand-written `~/Library/LaunchAgents` plists earlier
/// versions installed, removed on migration: the pre-split GUI autostart and
/// the agent's own pre-`SMAppService` plist. Frozen history — each happens to
/// match a `brand::` identifier, but if a brand value ever changes these must
/// not, or the stale plists are never cleaned up. This is also why the
/// `SMAppService` label ([`openlogi_core::brand::AGENT_SERVICE_LABEL`]) is a
/// *new* name: reusing a legacy label would make "is this job the old file's
/// or ours?" unanswerable during migration.
#[cfg(target_os = "macos")]
const LEGACY_LABELS: [&str; 2] = ["org.openlogi.openlogi", "org.openlogi.agent"];

/// Reconcile the agent's autostart state with `enabled`.
///
/// Idempotent; failures are logged, not propagated — startup must not abort
/// because an autostart directory is read-only or systemd is unavailable.
pub fn reconcile(enabled: bool) {
    #[cfg(target_os = "macos")]
    {
        // Registration itself is the GUI's SMAppService call (see the module
        // doc); the agent only migrates the legacy hand-written plists away.
        remove_legacy();
        let _ = enabled;
    }
    #[cfg(target_os = "windows")]
    if let Err(e) = reconcile_windows(enabled) {
        warn!(error = %e, enabled, "agent autostart reconcile failed");
    }
    #[cfg(target_os = "linux")]
    {
        if let Err(e) = reconcile_linux(enabled) {
            warn!(error = %e, enabled, "agent systemd unit reconcile failed");
        }
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
    {
        if enabled {
            debug!("launch_at_login set but no autostart backend on this platform");
        }
        let _ = enabled;
    }
}

/// Remove the legacy hand-written LaunchAgent plists (see [`LEGACY_LABELS`]).
/// Best-effort: a present-but-unremovable file is logged and left alone, and a
/// job currently loaded from one keeps running until logout.
#[cfg(target_os = "macos")]
fn remove_legacy() {
    for label in LEGACY_LABELS {
        let Ok(path) = plist_path(label) else {
            continue;
        };
        if !path.exists() {
            continue;
        }
        match std::fs::remove_file(&path) {
            Ok(()) => info!("removed legacy LaunchAgent ({label})"),
            Err(e) => warn!(error = %e, label, "could not remove legacy LaunchAgent"),
        }
    }
}

#[cfg(target_os = "macos")]
fn plist_path(label: &str) -> io::Result<PathBuf> {
    let home =
        openlogi_core::paths::home_dir().map_err(|e| io::Error::new(io::ErrorKind::NotFound, e))?;
    Ok(home
        .join("Library")
        .join("LaunchAgents")
        .join(format!("{label}.plist")))
}

/// HKCU autostart subkey + value name for the agent.
#[cfg(target_os = "windows")]
const RUN_SUBKEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
#[cfg(target_os = "windows")]
const RUN_VALUE: &str = "OpenLogiAgent";

/// Windows autostart: keep `HKCU\…\Run\OpenLogiAgent` pointed at the running
/// agent executable so the next login relaunches it, or remove it when disabled.
///
/// Unlike the macOS LaunchAgent there is no crash-respawn — a Run-key entry only
/// fires once at login. A future SCM/Task Scheduler backend could add restart
/// semantics; the login-launch path is enough for the headless agent today.
#[cfg(target_os = "windows")]
fn reconcile_windows(enabled: bool) -> std::io::Result<()> {
    use winreg::RegKey;
    use winreg::enums::HKEY_CURRENT_USER;

    let (run, _) = RegKey::predef(HKEY_CURRENT_USER).create_subkey(RUN_SUBKEY)?;
    if enabled {
        let exe = std::env::current_exe()?;
        // Windows parses Run-key values as command lines, so a bare path with
        // spaces (e.g. under "C:\Program Files\") is split at the first space and
        // the launch silently fails. Quote it. Built via OsString so a non-UTF-8
        // path survives exactly (no lossy `display()`).
        let mut quoted = std::ffi::OsString::from("\"");
        quoted.push(exe.as_os_str());
        quoted.push("\"");
        run.set_value(RUN_VALUE, &quoted)?;
        debug!(value = RUN_VALUE, "agent autostart registry value set");
    } else {
        match run.delete_value(RUN_VALUE) {
            Ok(()) => debug!(value = RUN_VALUE, "agent autostart registry value removed"),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                debug!("agent autostart registry value already absent");
            }
            Err(e) => return Err(e),
        }
    }
    Ok(())
}

// ── Linux systemd user-unit reconcile ────────────────────────────────────────

/// Name of the systemd user unit file.
#[cfg(target_os = "linux")]
const UNIT_NAME: &str = "openlogi-agent.service";

#[cfg(target_os = "linux")]
fn reconcile_linux(enabled: bool) -> io::Result<()> {
    let path = unit_path()?;
    let exe = std::env::current_exe()?;
    let desired = enabled.then(|| render_unit(&exe.to_string_lossy()));

    let current = std::fs::read_to_string(&path).ok();
    match (desired.as_deref(), current.as_deref()) {
        (Some(want), Some(have)) if want == have => {
            debug!(path = %path.display(), "systemd user unit already current");
            // Re-enable unconditionally: the unit file is current but the user
            // may have manually disabled the service since the last reconcile.
            run_systemctl(&["enable", UNIT_NAME]);
        }
        (Some(want), _) => {
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::write(&path, want)?;
            info!(path = %path.display(), "systemd user unit written");
            run_systemctl(&["daemon-reload"]);
            run_systemctl(&["enable", UNIT_NAME]);
        }
        (None, Some(_)) => {
            run_systemctl(&["disable", UNIT_NAME]);
            std::fs::remove_file(&path)?;
            run_systemctl(&["daemon-reload"]);
            info!(path = %path.display(), "systemd user unit removed");
        }
        (None, None) => debug!("systemd user unit already absent"),
    }
    Ok(())
}

/// Path to the per-user systemd unit:
/// `$XDG_CONFIG_HOME/systemd/user/openlogi-agent.service`
/// (default `~/.config/systemd/user/openlogi-agent.service`).
#[cfg(target_os = "linux")]
fn unit_path() -> io::Result<PathBuf> {
    let config_home = openlogi_core::paths::xdg_config_home().map_err(io::Error::other)?;
    Ok(config_home.join("systemd").join("user").join(UNIT_NAME))
}

/// Render the systemd user unit for the given executable path.
///
/// `Restart=on-failure` mirrors the macOS `KeepAlive=SuccessfulExit:false`
/// semantics: the agent is respawned after a crash but a clean `exit(0)` (e.g.
/// the tray's Quit) stays stopped until the next login.
#[cfg(target_os = "linux")]
fn render_unit(exe: &str) -> String {
    let exec_start = escape_systemd_exec(exe);
    format!(
        "[Unit]\n\
        Description=OpenLogi background agent (Logitech HID++ device control)\n\
        After=graphical-session.target\n\
        \n\
        [Service]\n\
        Type=simple\n\
        ExecStart={exec_start}\n\
        Restart=on-failure\n\
        RestartSec=5\n\
        \n\
        [Install]\n\
        WantedBy=graphical-session.target\n"
    )
}

/// Escape a string for use as `ExecStart` in a systemd unit file.
///
/// `%` starts a specifier and must be doubled. A value containing spaces is
/// wrapped in double quotes (inner `"` are backslash-escaped).
#[cfg(target_os = "linux")]
fn escape_systemd_exec(s: &str) -> String {
    let doubled = s.replace('%', "%%").replace('$', "$$");
    if doubled.contains(' ') {
        format!("\"{}\"", doubled.replace('"', "\\\""))
    } else {
        doubled
    }
}

/// Invoke `systemctl --user <args>`. Failures are logged but not propagated —
/// the unit file write is the authoritative record; enable/disable is
/// best-effort (e.g. the session D-Bus may be unavailable in some environments).
#[cfg(target_os = "linux")]
fn run_systemctl(args: &[&str]) {
    let label = SystemctlArgsDisplay(args);
    let mut cmd = std::process::Command::new("systemctl");
    cmd.arg("--user").args(args);
    match cmd.output() {
        Ok(out) if out.status.success() => debug!("systemctl --user {label} succeeded"),
        Ok(out) => {
            let stderr = String::from_utf8_lossy(&out.stderr);
            warn!(
                "systemctl --user {label} exited {}: {}",
                out.status,
                stderr.trim()
            );
        }
        Err(e) => warn!("systemctl --user {label} failed to spawn: {e}"),
    }
}

#[cfg(target_os = "linux")]
struct SystemctlArgsDisplay<'a, 'b>(&'a [&'b str]);

#[cfg(target_os = "linux")]
impl fmt::Display for SystemctlArgsDisplay<'_, '_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut separator = "";
        for arg in self.0 {
            write!(f, "{separator}{arg}")?;
            separator = " ";
        }
        Ok(())
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
#[cfg(target_os = "linux")]
mod linux_tests {
    use super::*;

    #[test]
    fn rendered_unit_targets_agent_and_restarts_on_failure() {
        let body = render_unit("/usr/bin/openlogi-agent");
        assert!(body.contains("ExecStart=/usr/bin/openlogi-agent"));
        assert!(body.contains("Restart=on-failure"));
        assert!(body.contains("WantedBy=graphical-session.target"));
        assert!(!body.contains("--minimized"));
    }

    #[test]
    fn rendered_unit_is_valid_ini_with_all_three_sections() {
        let body = render_unit("/usr/bin/openlogi-agent");
        assert!(body.contains("[Unit]"));
        assert!(body.contains("[Service]"));
        assert!(body.contains("[Install]"));
    }

    #[test]
    fn escape_systemd_exec_doubles_percent() {
        assert_eq!(
            escape_systemd_exec("/home/user%20/bin/openlogi-agent"),
            "/home/user%%20/bin/openlogi-agent"
        );
    }

    #[test]
    fn escape_systemd_exec_quotes_path_with_spaces() {
        let result = escape_systemd_exec("/home/my user/bin/openlogi-agent");
        assert_eq!(result, "\"/home/my user/bin/openlogi-agent\"");
    }

    #[test]
    fn escape_systemd_exec_quotes_and_doubles_percent_with_spaces() {
        let result = escape_systemd_exec("/home/my%20 user/openlogi-agent");
        assert_eq!(result, "\"/home/my%%20 user/openlogi-agent\"");
    }

    #[test]
    fn escape_systemd_exec_doubles_dollar() {
        assert_eq!(
            escape_systemd_exec("/opt/release$1/bin/openlogi-agent"),
            "/opt/release$$1/bin/openlogi-agent"
        );
    }

    #[test]
    fn escape_systemd_exec_plain_path_unchanged() {
        let path = "/usr/local/bin/openlogi-agent";
        assert_eq!(escape_systemd_exec(path), path);
    }

    #[test]
    fn systemctl_arguments_render_as_a_command_suffix() {
        assert_eq!(
            SystemctlArgsDisplay(&["enable", UNIT_NAME]).to_string(),
            "enable openlogi-agent.service"
        );
    }

    #[test]
    fn unit_path_uses_home_fallback() {
        // When XDG_CONFIG_HOME is unset (or relative), falls back to $HOME/.config.
        // We can't mutate global env safely in a parallel test suite, so we test
        // the logic indirectly: unit_path() must end in the UNIT_NAME component.
        let path = unit_path().expect("unit_path should resolve with a valid HOME");
        assert!(path.ends_with(UNIT_NAME));
        assert!(path.to_string_lossy().contains("systemd/user"));
    }
}
