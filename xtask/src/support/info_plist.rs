//! Reading and stamping `Info.plist` keys.
//!
//! Every write here has to happen before signing: a signature seals the plists,
//! so a later rewrite invalidates it.

use std::path::Path;

use anyhow::{Context as _, Result, bail};
use openlogi_core::locale::SUPPORTED;
use plist::Value;
use toml::Table;

/// Where the shared locale catalogs live, relative to the repository root.
pub(crate) const LOCALES_DIR: &str = "crates/openlogi-ui/locales";

/// `Info.plist` keys whose values come from the shared locale catalogs, as
/// `(plist key, catalog table, catalog key)`.
///
/// macOS shows the base `Info.plist` value wherever no `InfoPlist.strings`
/// overrides it, and a camera request from a bundle without
/// `NSCameraUsageDescription` kills the app instead of prompting, so every key
/// here must have a non-empty English value.
const LOCALIZED_KEYS: &[(&str, &str, &str)] = &[(
    "NSCameraUsageDescription",
    "permissions",
    "camera_usage_description",
)];

/// Localize the app's `Info.plist` from the shared locale catalogs.
///
/// Stamps each key's English value into `Contents/Info.plist`, declares every
/// shipped locale in `CFBundleLocalizations`, and writes
/// `Contents/Resources/<localization>.lproj/InfoPlist.strings` for each locale
/// whose value differs from English. A declared locale without a strings file
/// falls back to the English value, so a catalog still carrying the English
/// fill-in needs no file.
///
/// Declaring the locales is also what lets AppKit localize the menu items it
/// inserts on its own (Start Dictation, Emoji & Symbols): without it they
/// follow the development region and stay English in every language.
pub(crate) fn localize_app(app: &Path, locales_dir: &Path) -> Result<()> {
    println!("==> Info.plist localization");
    let english = localized_values(&read_catalog(locales_dir, "en")?, "en")?;
    if let Some((key, _)) = english.iter().find(|(_, value)| value.trim().is_empty()) {
        bail!("en.toml leaves {key} empty; the app would ship without it");
    }
    remove_strings(&app.join("Contents/Resources"))?;

    let mut localizations = Vec::with_capacity(SUPPORTED.len());
    for &(code, _) in SUPPORTED {
        let localization = apple_localization(code);
        localizations.push(Value::String(localization.to_owned()));
        if code == "en" {
            continue;
        }
        let values = localized_values(&read_catalog(locales_dir, code)?, code)?;
        let mut translated = Vec::new();
        for ((key, value), (_, english_value)) in values.into_iter().zip(&english) {
            if value != *english_value {
                translated.push((key, value));
            }
        }
        if !translated.is_empty() {
            let strings = format!("Contents/Resources/{localization}.lproj/InfoPlist.strings");
            write_strings(&app.join(strings), translated)?;
        }
    }

    let info_plist = app.join("Contents/Info.plist");
    let mut plist = Value::from_file(&info_plist)
        .with_context(|| format!("could not read {}", info_plist.display()))?;
    let dict = plist
        .as_dictionary_mut()
        .with_context(|| format!("{} is not a plist dictionary", info_plist.display()))?;
    for (key, value) in english {
        dict.insert(key.into(), Value::String(value));
    }
    dict.insert("CFBundleLocalizations".into(), Value::Array(localizations));
    plist
        .to_file_xml(&info_plist)
        .with_context(|| format!("could not write {}", info_plist.display()))
}

/// The `.lproj` name Apple uses for an OpenLogi locale code.
///
/// Apple names Chinese localizations by script, except Hong Kong: a
/// `zh-Hant-TW` preference negotiates `zh-Hant`, and `zh-Hant-HK` negotiates
/// `zh-HK`. Every other shipped code is already the name Apple uses.
fn apple_localization(code: &str) -> &str {
    match code {
        "zh-CN" => "zh-Hans",
        "zh-TW" => "zh-Hant",
        other => other,
    }
}

fn read_catalog(locales_dir: &Path, code: &str) -> Result<Table> {
    let path = locales_dir.join(format!("{code}.toml"));
    let text = fs_err::read_to_string(&path)?;
    toml::from_str(&text).with_context(|| format!("could not parse {}", path.display()))
}

/// Each [`LOCALIZED_KEYS`] entry's value in `catalog`, in that order.
fn localized_values(catalog: &Table, code: &str) -> Result<Vec<(&'static str, String)>> {
    LOCALIZED_KEYS
        .iter()
        .map(|&(plist_key, table, key)| {
            catalog
                .get(table)
                .and_then(|entries| entries.get(key))
                .and_then(toml::Value::as_str)
                .map(|value| (plist_key, value.to_owned()))
                .with_context(|| format!("{code}.toml has no string {table}.{key}"))
        })
        .collect()
}

/// Remove every `InfoPlist.strings` a previous run left in `resources`.
///
/// The dev bundle is rebuilt in place, so a locale that has since fallen back to
/// the English fill-in would otherwise keep its stale translation. An `.lproj`
/// left empty goes too; one holding anything else is not ours to remove.
fn remove_strings(resources: &Path) -> Result<()> {
    for entry in fs_err::read_dir(resources)? {
        let dir = entry?.path();
        if dir.extension().is_none_or(|extension| extension != "lproj") {
            continue;
        }
        let strings = dir.join("InfoPlist.strings");
        if strings.exists() {
            fs_err::remove_file(&strings)?;
        }
        if fs_err::read_dir(&dir)?.next().is_none() {
            fs_err::remove_dir(&dir)?;
        }
    }
    Ok(())
}

/// Write an `InfoPlist.strings` holding `entries`, as an XML property list.
fn write_strings(path: &Path, entries: Vec<(&str, String)>) -> Result<()> {
    if let Some(dir) = path.parent() {
        fs_err::create_dir_all(dir)?;
    }
    let dict: plist::Dictionary = entries
        .into_iter()
        .map(|(key, value)| (key.to_owned(), Value::String(value)))
        .collect();
    Value::Dictionary(dict)
        .to_file_xml(path)
        .with_context(|| format!("could not write {}", path.display()))
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

#[cfg(test)]
mod tests;
