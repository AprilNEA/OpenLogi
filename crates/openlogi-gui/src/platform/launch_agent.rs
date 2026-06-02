//! Launch-at-login reconciliation.
//!
//! macOS uses a `LaunchAgent` plist at
//! `~/Library/LaunchAgents/org.openlogi.openlogi.plist`. Windows uses the
//! current user's `Run` registry key. Linux remains a stub until an XDG
//! autostart backend lands.

#[cfg(target_os = "macos")]
use std::io;
#[cfg(target_os = "macos")]
use std::path::PathBuf;
use tracing::debug;
#[cfg(target_os = "macos")]
use tracing::info;
#[cfg(any(target_os = "macos", target_os = "windows"))]
use tracing::warn;

/// Stable launch-agent identifier; matches the bundle id in
/// `crates/openlogi-gui/Cargo.toml [package.metadata.bundle]`.
#[cfg(target_os = "macos")]
const LABEL: &str = "org.openlogi.openlogi";

/// Stable Windows startup value name.
#[cfg(target_os = "windows")]
const RUN_VALUE: &str = "OpenLogi";
#[cfg(target_os = "windows")]
const RUN_KEY: &str = r"HKCU\Software\Microsoft\Windows\CurrentVersion\Run";
#[cfg(target_os = "windows")]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// Reconcile launch-at-login with `enabled`. Idempotent and best-effort:
/// failures are logged at `warn` instead of aborting startup.
pub fn reconcile(enabled: bool) {
    #[cfg(target_os = "macos")]
    {
        if let Err(e) = reconcile_macos(enabled) {
            warn!(error = %e, enabled, "LaunchAgent reconcile failed");
        }
    }
    #[cfg(target_os = "windows")]
    {
        if let Err(e) = reconcile_windows(enabled) {
            warn!(error = %e, enabled, "Windows startup registry reconcile failed");
        }
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        if enabled {
            debug!("launch_at_login set but no autostart backend on this platform");
        }
        let _ = enabled;
    }
}

#[cfg(target_os = "macos")]
fn reconcile_macos(enabled: bool) -> io::Result<()> {
    let path = plist_path()?;
    let exe = std::env::current_exe()?;
    let desired = enabled.then(|| render_plist(&exe.to_string_lossy()));

    let current = std::fs::read_to_string(&path).ok();
    match (desired.as_deref(), current.as_deref()) {
        (Some(want), Some(have)) if want == have => {
            debug!(path = %path.display(), "LaunchAgent already current");
        }
        (Some(want), _) => {
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::write(&path, want)?;
            info!(path = %path.display(), "LaunchAgent installed");
        }
        (None, Some(_)) => {
            std::fs::remove_file(&path)?;
            info!(path = %path.display(), "LaunchAgent removed");
        }
        (None, None) => {
            debug!("LaunchAgent already absent");
        }
    }
    Ok(())
}

#[cfg(target_os = "macos")]
fn plist_path() -> io::Result<PathBuf> {
    let home = std::env::var_os("HOME")
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "$HOME not set"))?;
    Ok(PathBuf::from(home)
        .join("Library")
        .join("LaunchAgents")
        .join(format!("{LABEL}.plist")))
}

#[cfg(target_os = "macos")]
fn render_plist(exe: &str) -> String {
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
        <!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \
        \"http://www.apple.com/DTD/PropertyList-1.0.dtd\">\n\
        <plist version=\"1.0\">\n\
        <dict>\n  \
        <key>Label</key>\n  \
        <string>{LABEL}</string>\n  \
        <key>ProgramArguments</key>\n  \
        <array>\n    \
        <string>{exe}</string>\n    \
        <string>--minimized</string>\n  \
        </array>\n  \
        <key>RunAtLoad</key>\n  \
        <true/>\n  \
        <key>KeepAlive</key>\n  \
        <false/>\n\
        </dict>\n\
        </plist>\n",
    )
}

#[cfg(target_os = "windows")]
fn reconcile_windows(enabled: bool) -> std::io::Result<()> {
    use std::os::windows::process::CommandExt;
    use std::process::Command;

    let mut command = Command::new("reg.exe");
    command.creation_flags(CREATE_NO_WINDOW);
    if enabled {
        let exe = std::env::current_exe()?;
        let command_line = format!("\"{}\"", exe.display());
        command.args([
            "add",
            RUN_KEY,
            "/v",
            RUN_VALUE,
            "/t",
            "REG_SZ",
            "/d",
            &command_line,
            "/f",
        ]);
    } else {
        command.args(["delete", RUN_KEY, "/v", RUN_VALUE, "/f"]);
    }

    let status = command.status()?;
    if !status.success() && enabled {
        return Err(std::io::Error::other(format!(
            "reg.exe exited with status {status}"
        )));
    }
    if !status.success() {
        debug!("Windows startup registry value was already absent");
    }
    Ok(())
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use super::*;

    #[test]
    fn rendered_plist_contains_expected_keys() {
        let body = render_plist("/Applications/OpenLogi.app/Contents/MacOS/openlogi-gui");
        assert!(body.contains(LABEL));
        assert!(body.contains("/Applications/OpenLogi.app/Contents/MacOS/openlogi-gui"));
        assert!(body.contains("RunAtLoad"));
        assert!(body.contains("--minimized"));
    }
}
