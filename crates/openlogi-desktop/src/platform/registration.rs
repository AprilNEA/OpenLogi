//! Registration of the background agent's launchd service (macOS).
//!
//! The GUI owns this: `SMAppService` resolves plists against the *calling*
//! app's bundle, so only this process can register the service. There is one
//! service, registered once, and deliberately not coupled to the
//! `launch_at_login` preference — the plist always carries the login trigger
//! and supervision, while the preference is a config value the *agent* reads
//! (the sunk-switch model), so the toggle never touches `SMAppService`. A
//! service the user switched off under Login Items
//! ([`ServiceStatus::RequiresApproval`]) is never re-registered; the
//! settings window surfaces it instead. `macos` owns every `SMAppService`
//! call; `unsupported` reports [`ServiceStatus::Unsupported`] elsewhere.

#[cfg(target_os = "macos")]
mod macos;
#[cfg(not(target_os = "macos"))]
mod unsupported;

#[cfg(target_os = "macos")]
use macos as platform;
#[cfg(not(target_os = "macos"))]
use unsupported as platform;

#[cfg(target_os = "macos")]
pub use macos::agent_service_label;

/// Where the agent service stands with launchd, mirroring
/// `SMAppServiceStatus` plus a "not this platform" arm.
///
/// Off macOS only [`Self::Unsupported`] is constructed, but cross-platform
/// consumers name the other variants. Not `expect`: the dead-code lint fires
/// only on the non-macOS lanes.
#[cfg_attr(
    not(target_os = "macos"),
    expect(clippy::allow_attributes, reason = "see above")
)]
#[cfg_attr(
    not(target_os = "macos"),
    allow(dead_code, reason = "only Unsupported is constructed off macOS")
)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServiceStatus {
    /// Registered and eligible to run — `launchctl kickstart` can start it.
    Enabled,
    /// Switched off in System Settings › Login Items — the master switch:
    /// blocks login start, kickstart, and crash respawn alike.
    RequiresApproval,
    /// Not registered (never, or unregistered since).
    NotRegistered,
    /// The framework could not find the service at all — typically a bundle
    /// without the embedded plist (a bare dev binary).
    NotFound,
    /// Not macOS.
    #[cfg(not(target_os = "macos"))]
    Unsupported,
}

/// Current registration status of the agent service.
#[must_use]
pub fn status() -> ServiceStatus {
    platform::status()
}

/// Converge toward "the agent service is registered". Safe on a background
/// thread, independent of `launch_at_login` (module doc). Registering starts
/// the agent immediately (`SuccessfulExit` implies `RunAtLoad`);
/// re-registering restarts a running one — how an updated executable
/// replaces the old process under supervision.
///
/// # Errors
///
/// The framework's error description — unsigned bundle, missing embedded
/// plist (bare dev binary), or launchd refusing. Never fails off macOS.
pub fn ensure_registered() -> Result<(), String> {
    platform::ensure_registered()
}

/// Open System Settings on the Login Items pane — where the user re-enables a
/// service whose status is [`ServiceStatus::RequiresApproval`].
pub fn open_login_items_settings() {
    platform::open_login_items_settings();
}
