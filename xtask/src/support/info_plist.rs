//! Reading and stamping `Info.plist` keys.
//!
//! Every write here has to happen before signing: a signature seals the plists,
//! so a later rewrite invalidates it.

use std::path::Path;

use anyhow::{Context as _, Result};
use plist::Value;

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

pub(crate) fn stamp_bundle_version(info_plist: &Path, version: &str) -> Result<()> {
    let mut plist = Value::from_file(info_plist)
        .with_context(|| format!("could not read {}", info_plist.display()))?;
    let dict = plist
        .as_dictionary_mut()
        .with_context(|| format!("{} is not a plist dictionary", info_plist.display()))?;
    for key in ["CFBundleShortVersionString", "CFBundleVersion"] {
        dict.insert(key.into(), Value::String(version.to_string()));
    }
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
