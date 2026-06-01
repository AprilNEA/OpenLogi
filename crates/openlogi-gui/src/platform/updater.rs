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
use gpui_updater::{
    EngineConfig, Error as UpdateError, Release, StaticManifestSource, UpdateSource, Updater,
    Version,
};
use openlogi_core::config::AppSettings;

const MANIFEST_URL: &str = match option_env!("OPENLOGI_UPDATE_MANIFEST_URL") {
    Some(url) => url,
    None => "https://updates.openlogi.org/channels/stable/latest.json",
};
const MINISIGN_PUBLIC_KEY: Option<&str> = option_env!("OPENLOGI_UPDATE_MINISIGN_PUBLIC_KEY");

/// App-global handle to the shared updater entity.
#[derive(Clone)]
pub struct SharedUpdater(pub Entity<Updater>);

impl Global for SharedUpdater {}

/// Build a fresh updater entity for this app's static update manifest and
/// running version. The asset is matched by platform metadata and verified
/// against the manifest's SHA-256.
pub fn new_entity(cx: &mut App) -> Entity<Updater> {
    cx.new(|cx| {
        let public_key = minisign_public_key();
        let source = StaticManifestSource::new(MANIFEST_URL)
            .os(std::env::consts::OS)
            .arch(release_arch())
            .format(release_format());
        let source = RequiredSignedSource {
            inner: source,
            public_key_configured: public_key.is_some(),
        };
        let version =
            Version::parse(env!("CARGO_PKG_VERSION")).unwrap_or_else(|_| Version::new(0, 0, 0));
        let mut config = EngineConfig::new(version);
        if let Some(key) = public_key {
            config = config.minisign_public_key(key);
        }
        Updater::new(source, config, cx)
    })
}

struct RequiredSignedSource<S> {
    inner: S,
    public_key_configured: bool,
}

impl<S: UpdateSource> UpdateSource for RequiredSignedSource<S> {
    fn fetch_latest(&self) -> gpui_updater::Result<Release> {
        if !self.public_key_configured {
            return Err(UpdateError::Signature(
                "update signing public key is not configured".to_string(),
            ));
        }
        let release = self.inner.fetch_latest()?;
        if release
            .signature
            .as_ref()
            .is_none_or(|s| s.trim().is_empty())
            && release
                .signature_url
                .as_ref()
                .is_none_or(|s| s.trim().is_empty())
        {
            return Err(UpdateError::Signature(
                "release manifest does not include a minisign signature".to_string(),
            ));
        }
        Ok(release)
    }
}

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
    if settings.check_for_updates {
        updater.update(cx, Updater::check);
    }
    cx.set_global(SharedUpdater(updater));
}

/// The shared updater entity, if [`install`] has run.
pub fn shared(cx: &App) -> Option<Entity<Updater>> {
    cx.try_global::<SharedUpdater>().map(|g| g.0.clone())
}

#[cfg(test)]
mod tests {
    use gpui_updater::{Asset, Release};

    use super::*;

    #[derive(Clone)]
    struct FakeSource {
        release: Release,
    }

    impl UpdateSource for FakeSource {
        fn fetch_latest(&self) -> gpui_updater::Result<Release> {
            Ok(self.release.clone())
        }
    }

    fn release(signature_url: Option<&str>) -> Release {
        Release {
            version: Version::new(99, 0, 0),
            notes: None,
            asset: Asset {
                name: "OpenLogi.dmg".to_string(),
                url: "https://updates.example/OpenLogi.dmg".to_string(),
                size: 1,
            },
            signature: None,
            signature_url: signature_url.map(str::to_string),
            sha256: Some("00".to_string()),
        }
    }

    #[test]
    fn signed_source_requires_configured_public_key() {
        let source = RequiredSignedSource {
            inner: FakeSource {
                release: release(Some("https://updates.example/OpenLogi.dmg.minisig")),
            },
            public_key_configured: false,
        };

        assert!(matches!(
            source.fetch_latest(),
            Err(UpdateError::Signature(_))
        ));
    }

    #[test]
    fn signed_source_rejects_unsigned_manifest_assets() {
        let source = RequiredSignedSource {
            inner: FakeSource {
                release: release(None),
            },
            public_key_configured: true,
        };

        assert!(matches!(
            source.fetch_latest(),
            Err(UpdateError::Signature(_))
        ));
    }

    #[test]
    fn signed_source_accepts_signature_url() {
        let source = RequiredSignedSource {
            inner: FakeSource {
                release: release(Some("https://updates.example/OpenLogi.dmg.minisig")),
            },
            public_key_configured: true,
        };

        assert!(source.fetch_latest().is_ok());
    }
}
