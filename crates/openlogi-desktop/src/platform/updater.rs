//! Opt-in update checks: one shared [`Updater`], run by Sparkle where the
//! bundle embeds it and by the built-in engine everywhere else.
//!
//! [`install`] builds the entity at GPUI startup and publishes it as a
//! [`SharedUpdater`] global. Settings → Updates, the app menu and the consent
//! prompt all drive that one entity and read the same [`UpdateStatus`] from
//! it, whichever backend runs it:
//!
//! - **Sparkle** ([`gpui_updater_sparkle`]) when the running bundle embeds
//!   `Sparkle.framework` — the macOS release bundle `xtask macos package`
//!   builds. It reads its per-architecture appcast and EdDSA key from
//!   `Info.plist`, installs on quit, and can replace an `/Applications` copy
//!   the user cannot write to.
//! - **The built-in engine** otherwise — Windows, Linux, an unbundled
//!   `cargo run`, a bundle without Sparkle, or one whose Sparkle fails to
//!   load — on the minisign-verified static `latest.json` manifest.
//!
//! Neither backend polls. With [`AppSettings::check_for_updates`] on, exactly
//! one check runs per launch and it opens no window; the result shows in
//! Settings → Updates. With [`AppSettings::auto_install_updates`] also on, a
//! found update downloads and stages in the background and applies on the
//! next restart. Sparkle's own schedule stays off: the bundle ships
//! `SUEnableAutomaticChecks = NO`, and [`install`] switches it back off should
//! the user defaults ever say otherwise.
//!
//! A check the user asks for ("Check for Updates" in Settings → Updates or the
//! app menu) works regardless of the settings. Under Sparkle it opens
//! Sparkle's update window, which carries the release notes and the install
//! button.
//!
//! [`AppSettings::auto_install_updates`] is owned by the config; Sparkle's
//! `automaticallyDownloadsUpdates` mirrors it. Sparkle's update window has a
//! checkbox for the same preference, so a change made there is adopted back
//! into the config instead of being overwritten at the next launch.

use gpui::{App, AppContext as _, Context, Entity, Global, Subscription};
use gpui_updater::{
    EngineConfig, StaticManifestSource, UpdateStatus, Updater, Verification, Version,
};
use gpui_updater_sparkle::{SparkleDriver, SparkleOptions, StagedUpdatePolicy};
use openlogi_core::config::AppSettings;

use crate::state::AppState;

const MANIFEST_URL: &str = match option_env!("OPENLOGI_UPDATE_MANIFEST_URL") {
    Some(url) => url,
    None => "https://updates.openlogi.org/channels/stable/latest.json",
};

/// Host of the compiled update manifest URL, without credentials or path data.
///
/// The release bundle's Sparkle feed (`SUFeedURL`) lives on the same host:
/// `xtask macos package` derives both from `OPENLOGI_UPDATE_BASE_URL`.
pub(crate) fn manifest_host() -> Result<String, url::ParseError> {
    manifest_host_from_url(MANIFEST_URL)
}

fn manifest_host_from_url(manifest_url: &str) -> Result<String, url::ParseError> {
    let url = url::Url::parse(manifest_url)?;
    url.host_str()
        .map(str::to_owned)
        .ok_or(url::ParseError::EmptyHost)
}

/// Base64 minisign public key, embedded at build time by the release workflow.
/// Absent in local/dev builds, which then fail closed (see [`new_entity`]).
const MINISIGN_PUBLIC_KEY: Option<&str> = option_env!("OPENLOGI_UPDATE_MINISIGN_PUBLIC_KEY");

/// App-global handle to the shared updater entity.
#[derive(Clone)]
pub struct SharedUpdater(pub Entity<Updater>);

impl Global for SharedUpdater {}

/// Holds the shared updater's observer alive for the app's lifetime. Dropping
/// the [`Subscription`] would stop both the engine's background download of a
/// found update and the adoption of Sparkle's auto-install checkbox, so it
/// lives in a global rather than a local.
struct UpdaterObserver(
    #[expect(dead_code, reason = "held to keep the observer alive")] Subscription,
);

impl Global for UpdaterObserver {}

/// Build an updater entity on the built-in engine, for this app's static
/// update manifest and running version. The shared entity [`install`]
/// publishes prefers Sparkle; this one is for a view opened without it.
///
/// The asset is matched by platform metadata and, under
/// [`Verification::Strict`], verified against both the manifest's SHA-256 and
/// a minisign signature made with [`MINISIGN_PUBLIC_KEY`]. Release builds
/// embed that key and update normally. A build without it (local/dev) fails
/// closed: `check` returns an error rather than installing an unverified
/// artifact.
pub fn new_entity(cx: &mut App) -> Entity<Updater> {
    cx.new(engine_updater)
}

fn engine_updater(cx: &mut Context<Updater>) -> Updater {
    let source = StaticManifestSource::new(MANIFEST_URL)
        .os(std::env::consts::OS)
        .arch(release_arch())
        .format(release_format());
    #[expect(
        clippy::expect_used,
        reason = "CARGO_PKG_VERSION is cargo-provided and always valid semver"
    )]
    let version = Version::parse(env!("CARGO_PKG_VERSION")).expect("valid embedded version");
    let mut config = EngineConfig::new(version).verification(Verification::Strict);
    if let Some(key) = minisign_public_key() {
        config = config.minisign_public_key(key);
    }
    Updater::new(source, config, cx)
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
        // Matches the `format` field `xtask release latest-json` emits for the
        // per-arch MSIs, which gpui-updater applies via its staged msiexec flow.
        "windows" => "msi",
        _ => "tar.gz",
    }
}

/// The Sparkle.framework the running bundle embeds, or `None` — and the engine
/// takes over — when it embeds none or Sparkle cannot run.
fn load_sparkle() -> Option<SparkleDriver> {
    // Settings → Updates offers "Restart to Update" itself, so Sparkle prompts
    // for nothing once an update is staged; it still installs on quit.
    let options = SparkleOptions::new().staged_updates(StagedUpdatePolicy::HostRelaunches);
    SparkleDriver::load(options).unwrap_or_else(|error| {
        tracing::warn!(%error, "Sparkle is embedded but cannot run; updating through the built-in engine");
        None
    })
}

/// Publish the shared updater as a global and, when the user has opted in, run
/// exactly one check on launch. Call once from the GPUI `run` closure, on the
/// main thread (Sparkle requires it).
pub fn install(cx: &mut App, settings: &AppSettings) {
    let updater = cx.new(|cx| match load_sparkle() {
        Some(sparkle) => Updater::with_driver(sparkle, cx),
        None => engine_updater(cx),
    });
    with_sparkle(&updater, cx, |sparkle| {
        // OpenLogi never polls — see the module docs.
        if sparkle.automatically_checks_for_updates() == Some(true) {
            sparkle.set_automatically_checks_for_updates(false);
        }
        mirror_auto_install(sparkle, settings.auto_install_updates);
    });

    // Reads the setting live, so toggling it at runtime — or a later manual
    // check — is honoured. Inert until the app state is up.
    let observer = cx.observe(&updater, |updater, cx| {
        let Some(auto_install) =
            AppState::try_read(cx).map(|state| state.app_settings().auto_install_updates)
        else {
            return;
        };
        if updater.read(cx).is_driver_backed() {
            adopt_sparkle_auto_install(&updater, auto_install, cx);
        } else if auto_install && matches!(updater.read(cx).status(), UpdateStatus::Available(_)) {
            // The engine found a newer version and the user opted into
            // automatic install: download and stage it (applied on the next
            // restart, never mid-session). Sparkle does this on its own.
            updater.update(cx, Updater::download_and_install);
        }
    });
    cx.set_global(UpdaterObserver(observer));

    if settings.check_for_updates {
        start_background_check(&updater, settings.auto_install_updates, cx);
    }
    cx.set_global(SharedUpdater(updater));
}

/// The shared updater entity, if [`install`] has run.
pub fn shared(cx: &App) -> Option<Entity<Updater>> {
    cx.try_global::<SharedUpdater>().map(|g| g.0.clone())
}

/// Run the check OpenLogi starts on its own — at launch, or right after the
/// user opts in — as opposed to one the user asks for. It opens no window, and
/// a found update downloads only when the user opted into automatic install.
pub(crate) fn check_in_background(cx: &mut App) {
    let Some(updater) = shared(cx) else {
        return;
    };
    let auto_install =
        AppState::try_read(cx).is_some_and(|state| state.app_settings().auto_install_updates);
    start_background_check(&updater, auto_install, cx);
}

fn start_background_check(updater: &Entity<Updater>, auto_install: bool, cx: &mut App) {
    let checked_by_sparkle = with_sparkle(updater, cx, |sparkle| {
        if auto_install {
            // Sparkle's unattended check: with automatic downloads mirrored
            // on, a found update downloads and stages without any window.
            sparkle.check_in_background();
        } else {
            // Ask the feed only; the answer lands in Settings → Updates.
            sparkle.check_for_update_information();
        }
    });
    if checked_by_sparkle.is_none() {
        // The engine's check is silent; the observer installed by `install`
        // downloads what it finds when the user opted in.
        updater.update(cx, Updater::check);
    }
}

/// Commit the user's automatic-install choice from Settings → Updates and
/// mirror it into Sparkle.
///
/// Sparkle is written first: were it still holding the old value when an
/// updater notification arrives, the observer would adopt that value back. The
/// value the config retains is mirrored once more after the commit, so a
/// refused save takes Sparkle back with it.
pub(crate) fn set_auto_install_updates(enabled: bool, cx: &mut App) {
    let updater = shared(cx);
    if let Some(updater) = &updater {
        with_sparkle(updater, cx, |sparkle| {
            mirror_auto_install(sparkle, enabled);
        });
    }
    AppState::apply(cx, |state| state.commit_auto_install_updates(enabled));
    if let Some(updater) = &updater {
        mirror_retained_auto_install(updater, cx);
    }
}

/// Run `f` on the Sparkle driver when Sparkle runs `updater`; `None` when the
/// engine does.
fn with_sparkle<R>(
    updater: &Entity<Updater>,
    cx: &mut App,
    f: impl FnOnce(&SparkleDriver) -> R,
) -> Option<R> {
    updater.update(cx, |updater, _| {
        updater.driver_mut::<SparkleDriver>().as_deref().map(f)
    })
}

/// Write `enabled` to Sparkle's automatic downloads unless it already holds
/// it — Sparkle persists every write in the user defaults.
fn mirror_auto_install(sparkle: &SparkleDriver, enabled: bool) {
    if sparkle.automatically_downloads_updates() != Some(enabled) {
        sparkle.set_automatically_downloads_updates(enabled);
    }
}

/// Mirror the automatic-install value the config retained into Sparkle. A save
/// the config refuses — `config.toml` changed on disk, or it cannot be
/// written — restores the previous value, and Sparkle must not keep
/// downloading on its own while the config says it does not.
fn mirror_retained_auto_install(updater: &Entity<Updater>, cx: &mut App) {
    let Some(retained) =
        AppState::try_read(cx).map(|state| state.app_settings().auto_install_updates)
    else {
        return;
    };
    with_sparkle(updater, cx, |sparkle| {
        mirror_auto_install(sparkle, retained);
    });
}

/// Adopt a change made with the "Automatically download and install updates"
/// checkbox in Sparkle's update window. OpenLogi writes Sparkle's value only
/// alongside the config ([`install`], [`set_auto_install_updates`]), so a
/// difference can only have come from that checkbox.
fn adopt_sparkle_auto_install(updater: &Entity<Updater>, configured: bool, cx: &mut App) {
    let chosen =
        with_sparkle(updater, cx, SparkleDriver::automatically_downloads_updates).flatten();
    if let Some(chosen) = chosen
        && chosen != configured
    {
        AppState::apply(cx, |state| state.commit_auto_install_updates(chosen));
        mirror_retained_auto_install(updater, cx);
    }
}

#[cfg(test)]
mod tests {
    use gpui::TestAppContext;
    use openlogi_core::config::Config;

    use super::*;
    use crate::services::assets::AssetResolver;
    use crate::state::{ConfigPersistence, Sources};

    #[test]
    fn manifest_host_excludes_credentials_paths_and_queries() {
        for (manifest_url, host) in [
            (
                "https://updates.openlogi.org/channels/stable/latest.json",
                "updates.openlogi.org",
            ),
            (
                "https://mirror.example.org/channels/stable/latest.json",
                "mirror.example.org",
            ),
            (
                "https://user:secret@mirror.example.org:8443/latest.json?token=private#fragment",
                "mirror.example.org",
            ),
            (
                "https://mirror.example.org?token=private#fragment",
                "mirror.example.org",
            ),
            ("https://[2001:db8::1]:8443/latest.json", "[2001:db8::1]"),
        ] {
            assert_eq!(manifest_host_from_url(manifest_url).unwrap(), host);
        }
    }

    #[test]
    fn manifest_host_rejects_invalid_or_hostless_urls() {
        assert_eq!(
            manifest_host_from_url("not a URL").unwrap_err(),
            url::ParseError::RelativeUrlWithoutBase
        );
        for manifest_url in [
            "https://",
            "file:///latest.json",
            "mailto:updates@example.org",
        ] {
            assert_eq!(
                manifest_host_from_url(manifest_url).unwrap_err(),
                url::ParseError::EmptyHost
            );
        }
    }

    /// A test binary is no `.app` bundle, so it exercises the path every
    /// non-release build takes: no Sparkle, the engine, and settings that
    /// still reach the config.
    #[gpui::test]
    fn without_an_embedded_sparkle_the_engine_runs_and_settings_still_commit(
        cx: &mut TestAppContext,
    ) {
        cx.update(|cx| {
            let (commands, _) = tokio::sync::mpsc::unbounded_channel();
            let state = cx.new(|_| {
                AppState::new(Sources::in_memory(
                    Config::ephemeral(),
                    &AssetResolver::new(),
                    commands,
                ))
            });
            AppState::set_global(state, cx);
            let settings = AppState::try_read(cx).unwrap().app_settings().clone();
            assert!(!settings.check_for_updates, "no check may run in this test");
            install(cx, &settings);

            let updater = shared(cx).unwrap();
            assert!(!updater.read(cx).is_driver_backed());
            assert!(with_sparkle(&updater, cx, |_| ()).is_none());

            for enabled in [true, false] {
                set_auto_install_updates(enabled, cx);
                assert_eq!(
                    AppState::try_read(cx)
                        .unwrap()
                        .app_settings()
                        .auto_install_updates,
                    enabled
                );
            }
        });
    }

    /// A save the config refuses keeps the configured value; that retained
    /// value is what `set_auto_install_updates` mirrors into Sparkle afterwards.
    #[gpui::test]
    fn a_refused_save_keeps_the_configured_auto_install(cx: &mut TestAppContext) {
        cx.update(|cx| {
            let (commands, _) = tokio::sync::mpsc::unbounded_channel();
            let resolver = AssetResolver::new();
            let state = cx.new(|_| {
                AppState::new(Sources {
                    persistence: ConfigPersistence::ReadOnly("read-only".into()),
                    ..Sources::in_memory(Config::ephemeral(), &resolver, commands)
                })
            });
            AppState::set_global(state, cx);
            let settings = AppState::try_read(cx).unwrap().app_settings().clone();
            assert!(!settings.check_for_updates, "no check may run in this test");
            install(cx, &settings);

            let configured = settings.auto_install_updates;
            set_auto_install_updates(!configured, cx);
            assert_eq!(
                AppState::try_read(cx)
                    .unwrap()
                    .app_settings()
                    .auto_install_updates,
                configured
            );
        });
    }
}
