//! The agent's provisioning profile: what lets a release claim the HID
//! virtual-device entitlement.
//!
//! The agent types shortcut keys through a virtual HID keyboard
//! (`openlogi_inject::VirtualKeyboard`), and creating one needs the restricted
//! `com.apple.developer.hid.virtual.device` entitlement. A restricted
//! entitlement is valid only with a provisioning profile that grants it:
//! AMFI kills a process that claims one without such a profile, before `main`
//! runs. A profile and the claim it backs must therefore ship together or not
//! at all, so this module owns both. Given a profile, it embeds the profile in
//! the agent and writes the entitlements the agent is signed with. Without
//! one, the agent is signed without entitlements and its keys stay on
//! `CGEventPost`.
//!
//! Every check a broken profile would fail at the user's first launch runs
//! here, before anything is built or signed: the platform, the device scope,
//! the App ID, the grant and the expiry. The one check that needs a signature,
//! that the profile includes the signing certificate, runs right after the
//! agent is signed.

use std::path::{Path, PathBuf};
use std::time::SystemTime;

use anyhow::{Context as _, Result, bail};
use plist::{Dictionary, Value};
use xshell::{Shell, cmd};

use super::identity::{Channel, Component};

/// The Developer ID provisioning profile `macos package` embeds in the agent.
pub(in crate::commands::macos) const AGENT_PROFILE_ENV: &str =
    "OPENLOGI_AGENT_PROVISIONING_PROFILE";

const APPLICATION_IDENTIFIER: &str = "com.apple.application-identifier";
const TEAM_IDENTIFIER: &str = "com.apple.developer.team-identifier";
const HID_VIRTUAL_DEVICE: &str = "com.apple.developer.hid.virtual.device";

/// Where macOS looks for a bundle's profile.
const EMBEDDED_PROFILE: &str = "Contents/embedded.provisionprofile";

/// A provisioning profile checked to grant the production agent the HID
/// virtual-device entitlement.
#[derive(Debug)]
pub(crate) struct AgentProfile {
    source: PathBuf,
    app_id: String,
    team: String,
    /// The DER certificates the profile allows to sign the agent.
    certificates: Vec<Vec<u8>>,
}

impl AgentProfile {
    /// Decode the profile at `path` and check it against the production
    /// agent's identity.
    pub(crate) fn load(path: &Path) -> Result<Self> {
        let sh = Shell::new()?;
        let output = cmd!(sh, "security cms -D -i {path}")
            .quiet()
            .output()
            .with_context(|| format!("could not decode {}", path.display()))?;
        let plist = Value::from_reader_xml(output.stdout.as_slice())
            .with_context(|| format!("{} does not hold a property list", path.display()))?;
        let bundle_id = Channel::Production.identity(Component::Agent).bundle_id;
        let profile = Self::from_plist(path, &plist, &bundle_id, SystemTime::now())?;
        println!(
            "==> agent provisioning profile: {} ({})",
            path.display(),
            profile.app_id
        );
        Ok(profile)
    }

    /// Check the decoded `profile` against the agent's `bundle_id` at `now`.
    fn from_plist(
        source: &Path,
        profile: &Value,
        bundle_id: &str,
        now: SystemTime,
    ) -> Result<Self> {
        let name = source.display();
        let Some(profile) = profile.as_dictionary() else {
            bail!("{name}: the profile is not a dictionary");
        };
        let macos = profile
            .get("Platform")
            .and_then(Value::as_array)
            .is_some_and(|platforms| platforms.iter().any(|p| p.as_string() == Some("OSX")));
        if !macos {
            bail!("{name}: not a macOS profile");
        }
        // A development profile lists the Macs it runs on. On any other Mac,
        // AMFI would kill the agent at launch.
        if profile
            .get("ProvisionsAllDevices")
            .and_then(Value::as_boolean)
            != Some(true)
        {
            bail!(
                "{name}: the profile is limited to registered devices; a release needs a Developer ID profile"
            );
        }
        let Some(team) = profile
            .get("TeamIdentifier")
            .and_then(Value::as_array)
            .and_then(|teams| teams.first())
            .and_then(Value::as_string)
        else {
            bail!("{name}: the profile names no team");
        };
        let Some(entitlements) = profile.get("Entitlements").and_then(Value::as_dictionary) else {
            bail!("{name}: the profile grants no entitlements");
        };
        // Exact match only: a wildcard App ID cannot carry the capability.
        let expected = format!("{team}.{bundle_id}");
        let app_id = entitlements
            .get(APPLICATION_IDENTIFIER)
            .and_then(Value::as_string);
        if app_id != Some(expected.as_str()) {
            let found = app_id.unwrap_or("no App ID");
            bail!("{name}: the profile is for {found}, but the agent is {expected}");
        }
        if entitlements.get(TEAM_IDENTIFIER).and_then(Value::as_string) != Some(team) {
            bail!("{name}: {TEAM_IDENTIFIER} does not match the profile's team {team:?}");
        }
        if entitlements
            .get(HID_VIRTUAL_DEVICE)
            .and_then(Value::as_boolean)
            != Some(true)
        {
            bail!(
                "{name}: the profile does not grant {HID_VIRTUAL_DEVICE}; enable the HID Virtual \
                 Device capability on App ID {bundle_id} and regenerate the profile"
            );
        }
        let Some(expires) = profile.get("ExpirationDate").and_then(Value::as_date) else {
            bail!("{name}: the profile has no expiration date");
        };
        if SystemTime::from(expires) <= now {
            bail!("{name}: the profile expired on {}", expires.to_xml_format());
        }
        let certificates: Vec<Vec<u8>> = profile
            .get("DeveloperCertificates")
            .and_then(Value::as_array)
            .map(|certs| {
                certs
                    .iter()
                    .filter_map(Value::as_data)
                    .map(<[u8]>::to_vec)
                    .collect()
            })
            .unwrap_or_default();
        if certificates.is_empty() {
            bail!("{name}: the profile allows no signing certificate");
        }
        Ok(Self {
            source: source.to_path_buf(),
            app_id: expected,
            team: team.to_owned(),
            certificates,
        })
    }

    /// The entitlements the agent is signed with: the virtual keyboard's grant
    /// plus the two identifiers AMFI matches it to the profile by. Nothing
    /// else the profile grants is claimed.
    fn entitlements(&self) -> Dictionary {
        let mut entitlements = Dictionary::new();
        entitlements.insert(
            APPLICATION_IDENTIFIER.to_owned(),
            Value::String(self.app_id.clone()),
        );
        entitlements.insert(TEAM_IDENTIFIER.to_owned(), Value::String(self.team.clone()));
        entitlements.insert(HID_VIRTUAL_DEVICE.to_owned(), Value::Boolean(true));
        entitlements
    }

    /// Embed the profile in the agent bundle at `agent` and write the
    /// entitlements file into `scratch`. Runs before the agent is signed,
    /// because the signature seals the embedded profile.
    pub(super) fn install(&self, agent: &Path, scratch: &Path) -> Result<PathBuf> {
        let embedded = agent.join(EMBEDDED_PROFILE);
        fs_err::copy(&self.source, &embedded)?;
        let path = scratch.join("agent.entitlements");
        Value::Dictionary(self.entitlements())
            .to_file_xml(&path)
            .with_context(|| format!("could not write {}", path.display()))?;
        Ok(path)
    }

    /// Read the signed agent at `agent` back: it must claim exactly
    /// [`Self::entitlements`] and be signed by a certificate the profile
    /// includes. `scratch` receives the extracted certificates.
    pub(super) fn verify_signed(&self, agent: &Path, scratch: &Path) -> Result<()> {
        let sh = Shell::new()?;
        let output = cmd!(sh, "codesign -d --entitlements - --xml {agent}")
            .quiet()
            .output()?;
        let claimed = Value::from_reader_xml(output.stdout.as_slice())
            .context("could not read the signed agent's entitlements")?;
        if claimed.as_dictionary() != Some(&self.entitlements()) {
            bail!(
                "the signed agent claims {claimed:?}, expected {:?}",
                self.entitlements()
            );
        }
        let prefix = scratch.join("agent-certificate");
        let extract = format!("--extract-certificates={}", prefix.display());
        cmd!(sh, "codesign -d {extract} {agent}").quiet().run()?;
        let leaf = scratch.join("agent-certificate0");
        if !leaf.is_file() {
            bail!(
                "the signed agent carries no certificate; a provisioning profile needs a Developer ID signature, not an ad-hoc one"
            );
        }
        let leaf = fs_err::read(leaf)?;
        if !self.certificates.contains(&leaf) {
            bail!(
                "the agent's signing certificate is not in {}; regenerate the profile with the \
                 certificate the release signs with",
                self.source.display()
            );
        }
        println!("    agent helper: embedded profile, {HID_VIRTUAL_DEVICE}");
        Ok(())
    }
}

#[cfg(test)]
mod tests;
