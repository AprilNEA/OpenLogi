//! macOS `LaunchAgent` reconciliation for launch-at-login.
//!
//! When `Config::app_settings.launch_at_login` is `true`, a plist at
//! `~/Library/LaunchAgents/org.openlogi.openlogi.plist` is kept in sync
//! with the currently running executable so the next user-login session
//! relaunches OpenLogi automatically. Setting the flag to `false`
//! removes the plist on the next startup.
//!
//! Linux and Windows are stubs: they accept the same API but do nothing.
//! XDG autostart on Linux and the Windows `Run` registry key are future
//! work tracked by the broader "P2.2 follow-up" item in PLAN.md.

use tracing::debug;

#[cfg(target_os = "macos")]
use std::io;
#[cfg(target_os = "macos")]
use std::path::PathBuf;
#[cfg(target_os = "macos")]
use tracing::{info, warn};

/// Stable launch-agent identifier — matches the bundle id in
/// `crates/openlogi-gui/Cargo.toml [package.metadata.bundle]`.
#[cfg(target_os = "macos")]
const LABEL: &str = "org.openlogi.openlogi";

/// Reconcile the on-disk `LaunchAgent` plist with `enabled`. Idempotent:
/// no-op when the file already matches the desired state.
///
/// Failures are logged at `warn` instead of bubbling up — startup
/// shouldn't abort because the user's `LaunchAgents` directory is
/// read-only.
pub fn reconcile(enabled: bool) {
    #[cfg(target_os = "macos")]
    {
        if let Err(e) = reconcile_macos(enabled) {
            warn!(error = %e, enabled, "LaunchAgent reconcile failed");
        }
    }
    #[cfg(not(target_os = "macos"))]
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
    // launchd accepts both XML and binary plists; XML is human-readable
    // and small enough that the cost is negligible.
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
        <string>{exe}</string>\n  \
        </array>\n  \
        <key>RunAtLoad</key>\n  \
        <true/>\n  \
        <key>KeepAlive</key>\n  \
        <false/>\n\
        </dict>\n\
        </plist>\n",
    )
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
    }
}
