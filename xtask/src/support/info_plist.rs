//! Reading and stamping `Info.plist` keys.
//!
//! Every write here has to happen before signing: a signature seals the plists,
//! so a later rewrite invalidates it.

use std::path::Path;

use anyhow::{Context as _, Result};
use plist::Value;

/// Stamp `NSCameraUsageDescription` on the GUI app (cargo-bundle can't; matches
/// the dev plist) so camera requests prompt instead of killing the app, and
/// `NSBluetoothAlwaysUsageDescription` on the nested agent helper so the
/// agent's Bluetooth sheet has a usage string.
pub(crate) fn stamp_privacy_usage_descriptions(app: &Path) -> Result<()> {
    println!("==> privacy usage descriptions");
    stamp_plist_strings(
        &app.join("Contents/Info.plist"),
        &[(
            "NSCameraUsageDescription",
            "OpenLogi previews your Logitech webcam locally. Video never leaves your Mac.",
        )],
    )?;
    let login_items = app.join("Contents/Library/LoginItems");
    for helper in ["OpenLogi Agent.app", "OpenLogi Agent Dev.app"] {
        let plist = login_items.join(helper).join("Contents/Info.plist");
        if plist.is_file() {
            stamp_plist_strings(
                &plist,
                &[(
                    "NSBluetoothAlwaysUsageDescription",
                    "OpenLogi Agent uses Bluetooth so macOS can authorize this helper.",
                )],
            )?;
        }
    }
    Ok(())
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
    let plist = Value::from_file(info_plist)
        .with_context(|| format!("could not read {}", info_plist.display()))?;
    let dict = plist
        .as_dictionary()
        .with_context(|| format!("{} is not a plist dictionary", info_plist.display()))?;
    Ok(dict.get(key).and_then(Value::as_string).map(str::to_owned))
}

pub(crate) fn stamp_plist_strings(info_plist: &Path, entries: &[(&str, &str)]) -> Result<()> {
    let mut plist = Value::from_file(info_plist)
        .with_context(|| format!("could not read {}", info_plist.display()))?;
    let dict = plist
        .as_dictionary_mut()
        .with_context(|| format!("{} is not a plist dictionary", info_plist.display()))?;
    for (key, value) in entries {
        dict.insert((*key).into(), Value::String((*value).to_string()));
    }
    plist
        .to_file_xml(info_plist)
        .with_context(|| format!("could not write {}", info_plist.display()))
}
