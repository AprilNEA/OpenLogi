//! Reading and stamping `Info.plist` keys.
//!
//! Every write here has to happen before signing: a signature seals the plists,
//! so a later rewrite invalidates it.

use std::path::Path;

use anyhow::{Context as _, Result, ensure};
use plist::{Dictionary, Value};

use crate::support::release_tag::BuildVersion;
use crate::support::update_feed::MACOS_MINIMUM_OS_VERSION;

/// The scheme `openlogi://` deep links open the app with.
const URL_SCHEME: &str = "openlogi";
/// The URL type's `CFBundleURLName`: the bundle name, as cargo-bundle wrote it
/// and the dev bundle's `Info.plist` declares it, so both register the type
/// identically.
const URL_TYPE_NAME: &str = "OpenLogi";

/// Stamp `NSCameraUsageDescription` (cargo-bundle can't; matches the dev plist) so camera requests prompt instead of killing the app.
pub(crate) fn stamp_privacy_usage_descriptions(app: &Path) -> Result<()> {
    println!("==> privacy usage descriptions");
    stamp_plist_strings(
        &app.join("Contents/Info.plist"),
        &[(
            "NSCameraUsageDescription",
            "OpenLogi previews your Logitech webcam locally. Video never leaves your Mac.",
        )],
    )
}

/// Stamp the Launch Services keys `Cargo.toml` cannot carry for every
/// cargo-bundle: the minimum macOS version and the `openlogi://` URL type,
/// each read back. cargo-bundle 0.11 reads them only as flat `osx_*` keys and
/// silently ignores the `[package.metadata.bundle.osx]` table that 0.12
/// requires, rejecting the flat keys, so `Cargo.toml` declares neither.
pub(crate) fn stamp_launch_services_keys(app: &Path) -> Result<()> {
    println!("==> Launch Services keys (macOS {MACOS_MINIMUM_OS_VERSION}+, {URL_SCHEME}://)");
    let info_plist = app.join("Contents/Info.plist");
    let entries = launch_services_entries();
    stamp_plist_values(&info_plist, &entries)?;
    for (key, expected) in &entries {
        let actual = read_plist_value(&info_plist, key)?;
        ensure!(
            actual.as_ref() == Some(expected),
            "{} has {key} = {actual:?}, expected {expected:?}",
            info_plist.display()
        );
    }
    Ok(())
}

/// The same entries cargo-bundle generated from `osx_minimum_system_version`
/// and `osx_url_schemes`.
fn launch_services_entries() -> [(&'static str, Value); 2] {
    let mut url_type = Dictionary::new();
    url_type.insert("CFBundleURLName".into(), URL_TYPE_NAME.into());
    url_type.insert("CFBundleTypeRole".into(), "Viewer".into());
    url_type.insert(
        "CFBundleURLSchemes".into(),
        Value::Array(vec![URL_SCHEME.into()]),
    );
    [
        (
            "LSMinimumSystemVersion",
            Value::String(MACOS_MINIMUM_OS_VERSION.to_owned()),
        ),
        (
            "CFBundleURLTypes",
            Value::Array(vec![Value::Dictionary(url_type)]),
        ),
    ]
}

/// The two versions every bundle's `Info.plist` carries.
#[derive(Clone, Debug)]
pub(crate) struct BundleVersions {
    /// `CFBundleShortVersionString`: the release version people read, which
    /// is the crate version.
    pub(crate) short: String,
    /// `CFBundleVersion`: the numeric version Sparkle orders releases by.
    pub(crate) build: String,
}

impl BundleVersions {
    /// This build's versions. `build` is the release's [`BuildVersion`];
    /// without one, `CFBundleVersion` is the crate version too, which is a
    /// stable release's build version.
    pub(crate) fn of_build(build: Option<&BuildVersion>) -> Self {
        let short = env!("CARGO_PKG_VERSION").to_owned();
        let build = build.map_or_else(|| short.clone(), BuildVersion::to_string);
        Self { short, build }
    }
}

/// Stamp `versions` into `info_plist`, replacing cargo-bundle's build
/// timestamp in `CFBundleVersion`.
pub(crate) fn stamp_bundle_version(info_plist: &Path, versions: &BundleVersions) -> Result<()> {
    let mut plist = Value::from_file(info_plist)
        .with_context(|| format!("could not read {}", info_plist.display()))?;
    let dict = plist
        .as_dictionary_mut()
        .with_context(|| format!("{} is not a plist dictionary", info_plist.display()))?;
    dict.insert(
        "CFBundleShortVersionString".into(),
        Value::String(versions.short.clone()),
    );
    dict.insert(
        "CFBundleVersion".into(),
        Value::String(versions.build.clone()),
    );
    plist
        .to_file_xml(info_plist)
        .with_context(|| format!("could not write {}", info_plist.display()))
}

/// Read one string value from an `Info.plist`; `None` when the key is absent.
pub(crate) fn read_plist_string(info_plist: &Path, key: &str) -> Result<Option<String>> {
    Ok(read_plist_value(info_plist, key)?
        .as_ref()
        .and_then(Value::as_string)
        .map(str::to_owned))
}

/// Read one value of any type from an `Info.plist`; `None` when the key is
/// absent.
pub(crate) fn read_plist_value(info_plist: &Path, key: &str) -> Result<Option<Value>> {
    let plist = Value::from_file(info_plist)
        .with_context(|| format!("could not read {}", info_plist.display()))?;
    let dict = plist
        .as_dictionary()
        .with_context(|| format!("{} is not a plist dictionary", info_plist.display()))?;
    Ok(dict.get(key).cloned())
}

pub(crate) fn stamp_plist_strings(info_plist: &Path, entries: &[(&str, &str)]) -> Result<()> {
    let entries: Vec<_> = entries
        .iter()
        .map(|(key, value)| (*key, Value::String((*value).to_string())))
        .collect();
    stamp_plist_values(info_plist, &entries)
}

/// Stamp values of any type — strings, booleans — into an `Info.plist`.
pub(crate) fn stamp_plist_values(info_plist: &Path, entries: &[(&str, Value)]) -> Result<()> {
    let mut plist = Value::from_file(info_plist)
        .with_context(|| format!("could not read {}", info_plist.display()))?;
    let dict = plist
        .as_dictionary_mut()
        .with_context(|| format!("{} is not a plist dictionary", info_plist.display()))?;
    for (key, value) in entries {
        dict.insert((*key).into(), value.clone());
    }
    plist
        .to_file_xml(info_plist)
        .with_context(|| format!("could not write {}", info_plist.display()))
}

#[cfg(test)]
mod tests;
