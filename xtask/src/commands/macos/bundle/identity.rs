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
use openlogi_core::brand;
use strum::{Display, VariantArray};

use super::{read_plist_string, stamp_plist_strings};

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
    /// Where this component lives inside the app bundle; `None` is the app itself.
    ///
    /// The dev family spells "Dev" in the *directory* name as well as in
    /// `CFBundleDisplayName`, because macOS privacy panes fall back to a
    /// bundle's filename whenever its metadata is stale — a dev helper in a
    /// directory named like the shipped one renders as a second row nobody can
    /// tell from the real thing. The shipped spellings are frozen: the GUI's
    /// `agent_binary_path` and the agent's `overlay_binary_path` look for both
    /// families by name at runtime.
    pub(crate) fn nested_bundle(self, channel: Channel) -> Option<&'static str> {
        match (self, channel) {
            (Self::App, _) => None,
            (Self::Agent, Channel::Production) => {
                Some("Contents/Library/LoginItems/OpenLogiAgent.app")
            }
            (Self::Agent, Channel::Dev) => {
                Some("Contents/Library/LoginItems/OpenLogi Agent Dev.app")
            }
            (Self::Overlay, Channel::Production) => {
                Some("Contents/Library/LoginItems/OpenLogiOverlay.app")
            }
            (Self::Overlay, Channel::Dev) => {
                Some("Contents/Library/LoginItems/OpenLogi Overlay Dev.app")
            }
        }
    }

    /// This component's bundle root inside `app`.
    pub(crate) fn root(self, app: &Path, channel: Channel) -> PathBuf {
        self.nested_bundle(channel)
            .map_or_else(|| app.to_path_buf(), |nested| app.join(nested))
    }

    /// This component's `Info.plist`.
    pub(crate) fn info_plist(self, app: &Path, channel: Channel) -> PathBuf {
        self.root(app, channel).join("Contents/Info.plist")
    }

    /// This component's copy of the shared app icon.
    pub(crate) fn icon(self, app: &Path, channel: Channel) -> PathBuf {
        self.root(app, channel)
            .join(format!("Contents/Resources/{ICON_STEM}.icns"))
    }

    /// The shipped identity — the one macOS ties existing grants to.
    fn production(self) -> Identity {
        let (bundle_id, name) = match self {
            Self::App => (brand::APP_ID, "OpenLogi"),
            Self::Agent => (brand::AGENT_ID, "OpenLogi Agent"),
            Self::Overlay => (brand::OVERLAY_ID, "OpenLogi Overlay"),
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
                bundle_id: brand::dev_id(&production.bundle_id),
                name: format!("{} Dev", production.name),
            },
        }
    }
}

/// The `Info.plist` keys that carry the identity.
pub(crate) fn identity_entries(identity: &Identity) -> [(&str, &str); 3] {
    [
        ("CFBundleIdentifier", identity.bundle_id.as_str()),
        ("CFBundleName", identity.name.as_str()),
        ("CFBundleDisplayName", identity.name.as_str()),
    ]
}

/// Write `channel`'s identity over each of `components` in the bundle at `app`.
///
/// Runs before codesigning, which seals the `Info.plist` it stamps. Callers
/// pass [`Component::VARIANTS`] unless they deliberately assembled a partial
/// bundle — `xtask macos dev-bundle` does when the developer asked it not to
/// embed the helpers.
pub(crate) fn stamp(app: &Path, channel: Channel, components: &[Component]) -> Result<()> {
    println!("==> bundle identity ({channel})");
    for &component in components {
        let identity = channel.identity(component);
        stamp_plist_strings(
            &component.info_plist(app, channel),
            &identity_entries(&identity),
        )?;
        println!(
            "    {component}: {} ({})",
            identity.bundle_id, identity.name
        );
    }
    Ok(())
}

/// Read each of `components`' identity back, failing unless it is `channel`'s.
///
/// This is the gate a distribution artifact passes before it is signed or
/// packaged, so a bundle built for local use can never be shipped by mistake.
pub(crate) fn verify(app: &Path, channel: Channel, components: &[Component]) -> Result<()> {
    for &component in components {
        let expected = channel.identity(component);
        let plist = component.info_plist(app, channel);
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
pub(crate) fn verify_icons(app: &Path, channel: Channel, components: &[Component]) -> Result<()> {
    for &component in components {
        let icon = component.icon(app, channel);
        if !icon.is_file() {
            bail!(
                "{component}: missing the shared app icon at {}",
                icon.display()
            );
        }
        let plist = component.info_plist(app, channel);
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
mod tests;
