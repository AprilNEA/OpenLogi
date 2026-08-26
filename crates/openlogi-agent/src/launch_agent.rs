//! Autostart reconciliation for the background agent.
//!
//! Implements `launch_at_login` by keeping a platform-specific autostart
//! descriptor in sync whenever the setting changes. One module per OS —
//! [`reconcile`] dispatches to the platform's file, which owns its mechanism,
//! its logging, and its tests:
//!
//! - **macOS** ([`macos`]): migration only. The GUI owns registration via
//!   `SMAppService` (the API resolves the service plist against the *calling*
//!   app's bundle, so only the GUI can call it); the agent just removes the
//!   hand-written legacy `~/Library/LaunchAgents` plists. A hand-edited
//!   `config.toml` therefore takes effect the next time the GUI runs.
//! - **Linux** ([`linux`]): a systemd **user** unit, written/removed and
//!   `systemctl --user` enabled/disabled. `Restart=on-failure` mirrors the
//!   macOS service's `KeepAlive = {SuccessfulExit: false}` semantics.
//! - **Windows** ([`windows`]): an `HKCU\…\Run` registry value — login launch
//!   only, no crash respawn.
//!
//! Every arm is idempotent — it writes only when the content differs and
//! removes only what exists — and failures are logged, never propagated:
//! startup must not abort because an autostart directory is read-only or
//! systemd is unavailable.

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "windows")]
mod windows;

/// Reconcile the agent's autostart state with `enabled`.
pub fn reconcile(enabled: bool) {
    #[cfg(target_os = "macos")]
    macos::reconcile(enabled);
    #[cfg(target_os = "linux")]
    linux::reconcile(enabled);
    #[cfg(target_os = "windows")]
    windows::reconcile(enabled);
    #[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
    {
        if enabled {
            tracing::debug!("launch_at_login set but no autostart backend on this platform");
        }
    }
}
