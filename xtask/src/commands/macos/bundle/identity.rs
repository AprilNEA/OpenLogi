//! The macOS identity a bundle carries: `CFBundleIdentifier` plus the name
//! macOS lists it under.
//!
//! macOS keys TCC grants (Accessibility, Input Monitoring) to a bundle's code
//! identity, and `openlogi_core::paths` keys the config profile to that
//! identifier's suffix. A shipped bundle wearing the dev identity therefore
//! voids every existing permission grant *and* reads a different config
//! directory — which is what releases 0.6.24–0.6.26 did, because the identity
//! was a side effect of which command happened to produce the bundle.
//!
//! So it is never inferred: [`stamp`] writes the chosen [`Channel`]'s identity
//! over every component, and [`verify`] reads it back before anything signs,
//! packages or notarizes the result.

use std::path::{Path, PathBuf};

use anyhow::{Result, bail};
use clap::ValueEnum;
use strum::{Display, VariantArray};

use super::{read_plist_string, stamp_plist_strings};

/// The app's production bundle identifier, also advertised by the update
/// manifest so the running app and the manifest cannot disagree.
pub(crate) const APP_BUNDLE_ID: &str = "org.openlogi.openlogi";

/// The icon every component shares, as `CFBundleIconFile` spells it (the `.icns`
/// extension is optional there, so it is trimmed before comparing).
const ICON_STEM: &str = "AppIcon";

/// Which identity family a bundle carries.
///
/// `Display` renders the same spelling `--channel` accepts: clap renders the
/// flag's default through it and parses the result back.
#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum, Display)]
#[strum(serialize_all = "kebab-case")]
pub(crate) enum Channel {
    /// What ships. Users' permission grants and config directory are keyed to it.
    Production,
    /// Local builds. Both the identifier and the name are suffixed, so a local
    /// bundle can never claim a shipped grant and System Settings shows which
    /// of the two installed copies a row belongs to.
    Dev,
}

/// A bundle whose identity xtask owns: the app plus each nested login-item
/// helper it embeds.
///
/// `VariantArray` supplies `VARIANTS`, so every pass over the bundle covers a
/// newly added component without anyone remembering to extend a list.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Display, VariantArray)]
pub(crate) enum Component {
    /// `OpenLogi.app` itself.
    #[strum(serialize = "app")]
    App,
    /// The always-on agent: the process that owns the hook and holds the
    /// Accessibility grant.
    #[strum(serialize = "agent helper")]
    Agent,
    /// The Actions Ring renderer.
    #[strum(serialize = "overlay helper")]
    Overlay,
}

impl Component {
    /// This component's checked-in dev `Info.plist`, relative to the repo root.
    ///
    /// Test-only, because the consumer is a shell script: `.cargo/run-macos.sh`
    /// copies these verbatim and never calls [`stamp`], so the literals in them
    /// *are* the dev identity. `dev_plists_match_the_derived_identity` is what
    /// keeps them in step with the suffixing in [`Channel::identity`].
    #[cfg(test)]
    const fn dev_template_plist(self) -> &'static str {
        match self {
            Self::App => "crates/openlogi-desktop/bundle/desktop-dev/Info.plist",
            Self::Agent => "crates/openlogi-desktop/bundle/agent-dev/Info.plist",
            Self::Overlay => "crates/openlogi-desktop/bundle/overlay-dev/Info.plist",
        }
    }

    /// Where this component lives inside the app bundle; `None` is the app itself.
    pub(super) fn nested_bundle(self) -> Option<&'static str> {
        match self {
            Self::App => None,
            Self::Agent => Some("Contents/Library/LoginItems/OpenLogiAgent.app"),
            Self::Overlay => Some("Contents/Library/LoginItems/OpenLogiOverlay.app"),
        }
    }

    /// This component's bundle root inside `app`.
    pub(super) fn root(self, app: &Path) -> PathBuf {
        self.nested_bundle()
            .map_or_else(|| app.to_path_buf(), |nested| app.join(nested))
    }

    /// This component's `Info.plist`.
    pub(super) fn info_plist(self, app: &Path) -> PathBuf {
        self.root(app).join("Contents/Info.plist")
    }

    /// This component's copy of the shared app icon.
    pub(super) fn icon(self, app: &Path) -> PathBuf {
        self.root(app)
            .join(format!("Contents/Resources/{ICON_STEM}.icns"))
    }

    /// The shipped identity — the one macOS ties existing grants to.
    fn production(self) -> Identity {
        let (bundle_id, name) = match self {
            Self::App => (APP_BUNDLE_ID, "OpenLogi"),
            Self::Agent => ("org.openlogi.agent", "OpenLogi Agent"),
            Self::Overlay => ("org.openlogi.overlay", "OpenLogi Overlay"),
        };
        Identity {
            bundle_id: bundle_id.to_owned(),
            name: name.to_owned(),
        }
    }
}

/// What one component is called on one channel.
pub(crate) struct Identity {
    /// `CFBundleIdentifier` — what TCC and the config profile key off.
    pub(crate) bundle_id: String,
    /// `CFBundleName` / `CFBundleDisplayName` — what System Settings lists.
    pub(crate) name: String,
}

impl Channel {
    /// This channel's identity for `component`. The dev family is the shipped
    /// one suffixed on both halves, so the two families cannot collide.
    pub(crate) fn identity(self, component: Component) -> Identity {
        let production = component.production();
        match self {
            Self::Production => production,
            Self::Dev => Identity {
                bundle_id: format!("{}.dev", production.bundle_id),
                name: format!("{} Dev", production.name),
            },
        }
    }
}

/// The `Info.plist` keys that carry the identity.
pub(super) fn identity_entries(identity: &Identity) -> [(&str, &str); 3] {
    [
        ("CFBundleIdentifier", identity.bundle_id.as_str()),
        ("CFBundleName", identity.name.as_str()),
        ("CFBundleDisplayName", identity.name.as_str()),
    ]
}

/// Write `channel`'s identity over every component of the bundle at `app`.
///
/// Runs before codesigning, which seals the `Info.plist` it stamps.
pub(crate) fn stamp(app: &Path, channel: Channel) -> Result<()> {
    println!("==> bundle identity ({channel})");
    for &component in Component::VARIANTS {
        let identity = channel.identity(component);
        stamp_plist_strings(&component.info_plist(app), &identity_entries(&identity))?;
        println!(
            "    {component}: {} ({})",
            identity.bundle_id, identity.name
        );
    }
    Ok(())
}

/// Read every component's identity back, failing unless it is `channel`'s.
///
/// This is the gate a distribution artifact passes before it is signed or
/// packaged, so a bundle built for local use can never be shipped by mistake.
pub(crate) fn verify(app: &Path, channel: Channel) -> Result<()> {
    for &component in Component::VARIANTS {
        let expected = channel.identity(component);
        let plist = component.info_plist(app);
        for (key, want) in identity_entries(&expected) {
            let found = read_plist_string(&plist, key)?;
            if found.as_deref() != Some(want) {
                bail!(
                    "{component}: {key} is {found:?}, expected {want:?} on the {channel} channel ({})",
                    plist.display()
                );
            }
        }
    }
    Ok(())
}

/// Fail unless every component ships the shared app icon *and* declares it, so
/// no surface that lists OpenLogi's processes — System Settings' privacy panes,
/// Login Items — shows a blank icon for one of them.
pub(crate) fn verify_icons(app: &Path) -> Result<()> {
    for &component in Component::VARIANTS {
        let icon = component.icon(app);
        if !icon.is_file() {
            bail!(
                "{component}: missing the shared app icon at {}",
                icon.display()
            );
        }
        let plist = component.info_plist(app);
        let declared = read_plist_string(&plist, "CFBundleIconFile")?;
        if declared
            .as_deref()
            .map(|file| file.trim_end_matches(".icns"))
            != Some(ICON_STEM)
        {
            bail!(
                "{component}: CFBundleIconFile is {declared:?}, expected {ICON_STEM:?} ({})",
                plist.display()
            );
        }
    }
    Ok(())
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "unwrap is idiomatic in tests")]
mod tests {
    use super::*;

    /// A bundle skeleton with an empty `Info.plist` per component.
    fn bundle() -> tempfile::TempDir {
        let app = tempfile::tempdir().unwrap();
        for &component in Component::VARIANTS {
            let plist = component.info_plist(app.path());
            fs_err::create_dir_all(plist.parent().unwrap()).unwrap();
            plist::Value::Dictionary(plist::Dictionary::new())
                .to_file_xml(plist)
                .unwrap();
        }
        app
    }

    /// `--channel`'s default is rendered through `Display` and then parsed back
    /// by clap's value parser, so a name only one of the two knows would break
    /// `macos bundle` the moment the flag is omitted.
    #[test]
    fn each_channel_renders_as_the_flag_value_it_parses_from() {
        for channel in [Channel::Production, Channel::Dev] {
            assert_eq!(
                Channel::from_str(&channel.to_string(), false).ok(),
                Some(channel),
                "{channel} does not round-trip through the value parser"
            );
        }
    }

    #[test]
    fn a_dev_bundle_can_never_collide_with_a_shipped_one() {
        let shipped: Vec<Identity> = Component::VARIANTS
            .iter()
            .map(|&component| Channel::Production.identity(component))
            .collect();

        for &component in Component::VARIANTS {
            let dev = Channel::Dev.identity(component);
            assert!(
                shipped.iter().all(|other| other.bundle_id != dev.bundle_id),
                "dev {component} id {} collides with a shipped identity",
                dev.bundle_id
            );
            assert!(
                shipped.iter().all(|other| other.name != dev.name),
                "dev {component} name {} collides with a shipped identity",
                dev.name
            );
        }
    }

    #[test]
    fn shipped_identities_are_distinct_per_component() {
        let ids: Vec<String> = Component::VARIANTS
            .iter()
            .map(|&component| Channel::Production.identity(component).bundle_id)
            .collect();
        for (index, id) in ids.iter().enumerate() {
            assert!(
                !ids[index + 1..].contains(id),
                "{id} is claimed by two components"
            );
        }
    }

    /// The checked-in dev plists are the only identity the dev bundle gets:
    /// `.cargo/run-macos.sh` copies them verbatim and never calls [`stamp`], so
    /// a literal here that drifts from the `.dev` suffixing silently gives the
    /// dev build a different identity than the one packaging derives — and TCC
    /// keys grants off exactly that.
    #[test]
    fn dev_plists_match_the_derived_identity() {
        let root = crate::support::fs::repo_root().unwrap();
        for &component in Component::VARIANTS {
            let plist = root.join(component.dev_template_plist());
            let identity = Channel::Dev.identity(component);
            for (key, want) in identity_entries(&identity) {
                let found = read_plist_string(&plist, key).unwrap();
                assert_eq!(
                    found.as_deref(),
                    Some(want),
                    "{} carries a stale {key}",
                    component.dev_template_plist()
                );
            }
        }
    }

    #[test]
    fn stamping_a_channel_makes_it_verify() {
        for channel in [Channel::Production, Channel::Dev] {
            let app = bundle();

            stamp(app.path(), channel).unwrap();

            verify(app.path(), channel).unwrap();
        }
    }

    #[test]
    fn a_dev_bundle_fails_production_verification() {
        let app = bundle();
        stamp(app.path(), Channel::Dev).unwrap();

        let error = verify(app.path(), Channel::Production)
            .unwrap_err()
            .to_string();

        assert!(
            error.contains("org.openlogi.openlogi.dev") && error.contains("production"),
            "the error must name the dev identity it found and the channel it wanted, got: {error}"
        );
    }

    #[test]
    fn a_shipped_bundle_fails_dev_verification() {
        let app = bundle();
        stamp(app.path(), Channel::Production).unwrap();

        let error = verify(app.path(), Channel::Dev).unwrap_err().to_string();

        assert!(error.contains("dev"), "got: {error}");
    }

    #[test]
    fn verify_rejects_a_bundle_with_no_identity_at_all() {
        let app = bundle();

        assert!(verify(app.path(), Channel::Production).is_err());
    }

    #[test]
    fn missing_icons_are_reported_per_component() {
        let app = bundle();
        stamp(app.path(), Channel::Production).unwrap();

        let error = verify_icons(app.path()).unwrap_err().to_string();

        assert!(
            error.contains("missing the shared app icon"),
            "got: {error}"
        );
    }
}
