//! Windows: keep `HKCU\…\Run\OpenLogiAgent` pointed at the running agent
//! executable so the next login relaunches it, or remove the value when
//! disabled.
//!
//! Unlike the macOS service there is no crash-respawn — a Run-key entry only
//! fires once at login. A future SCM/Task Scheduler backend could add restart
//! semantics; the login-launch path is enough for the headless agent today.

use tracing::{debug, warn};

/// HKCU autostart subkey + value name for the agent.
const RUN_SUBKEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
const RUN_VALUE: &str = "OpenLogiAgent";

pub(super) fn reconcile(enabled: bool) {
    if let Err(e) = apply(enabled) {
        warn!(error = %e, enabled, "agent autostart reconcile failed");
    }
}

fn apply(enabled: bool) -> std::io::Result<()> {
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
