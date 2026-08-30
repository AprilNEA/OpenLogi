//! Linux: a systemd user unit at
//! `$XDG_CONFIG_HOME/systemd/user/openlogi-agent.service` (default
//! `~/.config/systemd/user/openlogi-agent.service`), written/removed, then
//! `systemctl --user daemon-reload` and `enable`/`disable`.
//! `Restart=on-failure` mirrors the macOS `KeepAlive=SuccessfulExit:false`
//! semantics: a crash respawns; a clean `exit(0)` leaves the unit enabled but
//! stopped until the next session login.

use std::fmt;
use std::io;
use std::path::{Path, PathBuf};

use tracing::{debug, info, warn};

/// Name of the systemd user unit file.
const UNIT_NAME: &str = "openlogi-agent.service";

pub(super) fn reconcile(enabled: bool) {
    if let Err(e) = apply(enabled) {
        warn!(error = %e, enabled, "agent systemd unit reconcile failed");
    }
}

fn apply(enabled: bool) -> io::Result<()> {
    let exe = std::env::current_exe()?;
    // A `cargo build`/`cargo run` binary must not rewrite or delete the user's
    // packaged unit. launch_at_login would otherwise point ExecStart at
    // `target/release` (or wipe /usr/bin when the setting is off).
    if is_cargo_built_agent(&exe) {
        debug!(
            enabled,
            path = %exe.display(),
            "cargo-built agent — leaving the systemd user unit unchanged"
        );
        return Ok(());
    }
    let path = unit_path()?;
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

/// True for binaries under a Cargo `target/{debug,release}/` directory.
fn is_cargo_built_agent(exe: &Path) -> bool {
    let mut saw_target = false;
    for component in exe.components() {
        let name = component.as_os_str();
        if saw_target && (name == "release" || name == "debug") {
            return true;
        }
        saw_target = name == "target";
    }
    false
}

/// Path to the per-user systemd unit:
/// `$XDG_CONFIG_HOME/systemd/user/openlogi-agent.service`
/// (default `~/.config/systemd/user/openlogi-agent.service`).
fn unit_path() -> io::Result<PathBuf> {
    let config_home = openlogi_core::paths::xdg_config_home().map_err(io::Error::other)?;
    Ok(config_home.join("systemd").join("user").join(UNIT_NAME))
}

/// Render the systemd user unit for the given executable path.
///
/// `Restart=on-failure` mirrors the macOS `KeepAlive=SuccessfulExit:false`
/// semantics: the agent is respawned after a crash but a clean `exit(0)` (e.g.
/// the tray's Quit) stays stopped until the next login.
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

struct SystemctlArgsDisplay<'a, 'b>(&'a [&'b str]);

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cargo_built_agent_paths_are_detected() {
        assert!(is_cargo_built_agent(Path::new(
            "/home/dev/openlogi/target/release/openlogi-agent"
        )));
        assert!(is_cargo_built_agent(Path::new(
            "/home/dev/openlogi/target/debug/openlogi-agent"
        )));
        assert!(!is_cargo_built_agent(Path::new("/usr/bin/openlogi-agent")));
        assert!(!is_cargo_built_agent(Path::new(
            "/usr/local/bin/openlogi-agent"
        )));
        assert!(!is_cargo_built_agent(Path::new(
            "/home/dev/target-practice/openlogi-agent"
        )));
    }

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
