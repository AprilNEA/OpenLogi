//! Opt-in update check, backed by the [`gpui_updater`] crate.
//!
//! A single shared [`Updater`] entity is installed at GPUI startup via
//! [`install`] and published as a [`SharedUpdater`] global. When
//! [`AppSettings::check_for_updates`] is enabled, exactly one check runs on
//! launch; the result is surfaced in the About window. No download, no polling.
//!
//! The manual "Check for Updates" button in About works regardless of the
//! setting — it is always user-initiated — and reuses this same shared entity,
//! so a launch-time result is already visible when the window opens.

use gpui::{App, AppContext as _, Entity, Global};
use gpui_updater::{EngineConfig, GitHubSource, Updater, Version};
use openlogi_core::config::AppSettings;

const OWNER: &str = "AprilNEA";
const REPO: &str = "OpenLogi";

/// App-global handle to the shared updater entity.
#[derive(Clone)]
pub struct SharedUpdater(pub Entity<Updater>);

impl Global for SharedUpdater {}

/// Build a fresh updater entity for this app's repo and running version. The
/// asset is matched by the running OS (`.dmg` on macOS) and verified against
/// the release's `SHA256SUMS`.
pub fn new_entity(cx: &mut App) -> Entity<Updater> {
    cx.new(|cx| {
        let source = GitHubSource::new(OWNER, REPO).with_checksums("SHA256SUMS");
        let version =
            Version::parse(env!("CARGO_PKG_VERSION")).unwrap_or_else(|_| Version::new(0, 0, 0));
        Updater::new(source, EngineConfig::new(version), cx)
    })
}

/// Publish the shared updater as a global and, when the user has opted in, run
/// exactly one check on launch. Call once from the GPUI `run` closure.
pub fn install(cx: &mut App, settings: &AppSettings) {
    let updater = new_entity(cx);
    if settings.check_for_updates {
        updater.update(cx, Updater::check);
    }
    cx.set_global(SharedUpdater(updater));
}

/// The shared updater entity, if [`install`] has run.
pub fn shared(cx: &App) -> Option<Entity<Updater>> {
    cx.try_global::<SharedUpdater>().map(|g| g.0.clone())
}
