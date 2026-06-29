//! Opt-in update check, backed by the [`gpui_updater`] crate.
//!
//! A single shared [`Updater`] entity is installed at GPUI startup via
//! [`install`] and published as a [`SharedUpdater`] global. When
//! [`AppSettings::check_for_updates`] is enabled, exactly one check runs on
//! launch; the result is surfaced in Settings → Updates. With
//! [`AppSettings::auto_install_updates`] also on, a found update downloads and
//! stages in the background (applied on next restart); otherwise no download,
//! no polling.
//!
//! The manual "Check for Updates" button in Settings → Updates works regardless
//! of the setting — it is always user-initiated — and reuses this same shared
//! entity, so a launch-time result is already visible when the window opens.

use gpui::{App, AppContext as _, Entity, Global, Subscription};
use gpui_updater::{
    EngineConfig, StaticManifestSource, UpdateStatus, Updater, Verification, Version,
};
use openlogi_core::config::AppSettings;

use crate::state::AppState;

const MANIFEST_URL: &str = match option_env!("OPENLOGI_UPDATE_MANIFEST_URL") {
    Some(url) => url,
    None => "https://updates.openlogi.org/channels/stable/latest.json",
};

/// Base64 minisign public key, embedded at build time by the release workflow.
/// Absent in local/dev builds, which then fail closed (see [`new_entity`]).
const MINISIGN_PUBLIC_KEY: Option<&str> = option_env!("OPENLOGI_UPDATE_MINISIGN_PUBLIC_KEY");

/// App-global handle to the shared updater entity.
#[derive(Clone)]
pub struct SharedUpdater(pub Entity<Updater>);

impl Global for SharedUpdater {}

/// Holds the auto-install observer alive for the app's lifetime. Dropping the
/// [`Subscription`] would stop the "download a found update in the background"
/// behaviour, so it lives in a global rather than a local.
struct AutoInstaller(#[expect(dead_code, reason = "held to keep the observer alive")] Subscription);

impl Global for AutoInstaller {}

/// Build a fresh updater entity for this app's static update manifest and
/// running version. The asset is matched by platform metadata and, under
/// [`Verification::Strict`], verified against both the manifest's SHA-256 and a
/// minisign signature made with [`MINISIGN_PUBLIC_KEY`].
///
/// Release builds embed that key and update normally. A build without it
/// (local/dev) fails closed: `check` returns an error rather than installing an
/// unverified artifact.
pub fn new_entity(cx: &mut App) -> Entity<Updater> {
    cx.new(|cx| {
        let source = StaticManifestSource::new(MANIFEST_URL)
            .os(std::env::consts::OS)
            .arch(release_arch())
            .format(release_format());
        let version =
            Version::parse(env!("CARGO_PKG_VERSION")).unwrap_or_else(|_| Version::new(0, 0, 0));
        let mut config = EngineConfig::new(version).verification(Verification::Strict);
        if let Some(key) = minisign_public_key() {
            config = config.minisign_public_key(key);
        }
        Updater::new(source, config, cx)
    })
}

/// The embedded minisign public key, trimmed, or `None` when the build did not
/// bake one in.
fn minisign_public_key() -> Option<&'static str> {
    MINISIGN_PUBLIC_KEY
        .map(str::trim)
        .filter(|key| !key.is_empty())
}

fn release_arch() -> &'static str {
    match std::env::consts::ARCH {
        "aarch64" => "arm64",
        arch => arch,
    }
}

fn release_format() -> &'static str {
    match std::env::consts::OS {
        "macos" => "dmg",
        "windows" => "exe",
        _ => "tar.gz",
    }
}

/// Publish the shared updater as a global and, when the user has opted in, run
/// exactly one check on launch. Call once from the GPUI `run` closure.
pub fn install(cx: &mut App, settings: &AppSettings) {
    let updater = new_entity(cx);

    // Watch for a check surfacing a newer version; when the user has opted into
    // automatic install, download and stage it (applied on next restart, never
    // mid-session). Reads the setting live so toggling it at runtime — or a
    // later manual check — is honoured. Installed unconditionally; it's inert
    // until both the flag is on and a check resolves to `Available`.
    let auto_install = cx.observe(&updater, |updater, cx| {
        let opted_in = cx
            .try_global::<AppState>()
            .is_some_and(|s| s.app_settings().auto_install_updates);
        if opted_in && matches!(updater.read(cx).status(), UpdateStatus::Available(_)) {
            updater.update(cx, Updater::download_and_install);
        }
    });
    cx.set_global(AutoInstaller(auto_install));

    if settings.check_for_updates {
        updater.update(cx, Updater::check);
    }
    cx.set_global(SharedUpdater(updater));
}

/// The shared updater entity, if [`install`] has run.
pub fn shared(cx: &App) -> Option<Entity<Updater>> {
    cx.try_global::<SharedUpdater>().map(|g| g.0.clone())
}
