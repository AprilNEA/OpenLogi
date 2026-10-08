//! The foreground application, in the form per-app profiles match against.
//!
//! Lives in the platform-free core crate because three layers need the same
//! shape and none of them can see the others: `openlogi-hook` reads it from the
//! window server, `openlogi-agent-core` holds it, and `openlogi-ipc` puts it on
//! the wire. That last one makes this a wire type — see
//! `crates/openlogi-ipc/AGENTS.md`.

use std::collections::BTreeMap;
use std::path::Path;

use serde::{Deserialize, Serialize};

/// One application, named the way a per-app profile names it.
///
/// [`Self::id`] is the whole of the matching contract; [`Self::display_name`]
/// exists only so a UI never has to show a reverse-DNS string to a human.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ForegroundApp {
    /// The exact string a per-app profile key is compared against: a macOS
    /// bundle identifier, an X11 `WM_CLASS` class or a Wayland xdg `app_id` on
    /// Linux, or the lower-cased executable path on Windows.
    ///
    /// Those namespaces do not map onto one another by any simple string rule,
    /// which is why a profile authored under one of them will not match under
    /// another. See
    /// [`Config::effective_bindings`](crate::config::Config::effective_bindings)
    /// for the matcher this feeds.
    pub id: String,
    /// Human-readable name for the UI. Equal to [`Self::id`] on the platforms
    /// that report no name of their own.
    pub display_name: String,
}

impl ForegroundApp {
    /// An application the platform identified but did not name — the display
    /// name falls back to the identifier, which on those platforms (X11's
    /// `WM_CLASS`, a Wayland `app_id`) is already close to readable.
    #[must_use]
    pub fn unnamed(id: String) -> Self {
        Self {
            display_name: id.clone(),
            id,
        }
    }
}

/// Resolve the most specific per-app entry for a foreground identifier — the
/// one matcher every per-app map uses, so a selector cannot mean one thing for
/// button overrides and another for Actions Ring layouts.
///
/// The exact key wins. On Windows the identifier is a lower-cased executable
/// path, so `exe:<filename>` is the stable fallback for Store and self-updating
/// applications whose install directory changes between versions; both path
/// separators are recognized so hand-authored Windows config stays inspectable
/// on every platform. An identifier with no path separator is a macOS bundle
/// identifier or a Linux application class, never an executable, so a name
/// that merely ends in `.exe` is not reinterpreted as one.
#[must_use]
pub fn overlay_for<'a, T>(overlays: &'a BTreeMap<String, T>, app: &str) -> Option<&'a T> {
    overlays.get(app).or_else(|| {
        let (_, executable_name) = app.rsplit_once(['\\', '/'])?;
        if !Path::new(executable_name)
            .extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("exe"))
        {
            return None;
        }
        overlays.get(&format!("exe:{}", executable_name.to_ascii_lowercase()))
    })
}

/// Read the identity of an installed macOS app without launching it.
/// Non-bundles, unreadable metadata, and missing identifiers are unresolved.
#[cfg(all(feature = "fs", target_os = "macos"))]
#[must_use]
pub fn bundle_identifier(path: &Path) -> Option<String> {
    use std::io::Read as _;

    const MAX_METADATA_BYTES: u64 = 1024 * 1024;
    if !path.is_absolute()
        || !path
            .extension()
            .is_some_and(|extension| extension.eq_ignore_ascii_case("app"))
    {
        return None;
    }
    let info = path.join("Contents/Info.plist");
    let file = crate::file_input::FileInput::Source.open(&info).ok()?;
    let metadata = file.metadata().ok()?;
    if metadata.len() > MAX_METADATA_BYTES {
        return None;
    }
    let mut bytes = Vec::new();
    file.take(MAX_METADATA_BYTES + 1)
        .read_to_end(&mut bytes)
        .ok()?;
    if bytes.len() > usize::try_from(MAX_METADATA_BYTES).ok()? {
        return None;
    }
    let value = plist::Value::from_reader(std::io::Cursor::new(bytes)).ok()?;
    let identifier = value
        .as_dictionary()?
        .get("CFBundleIdentifier")?
        .as_string()?;
    if identifier.is_empty() || identifier.contains(['/', '\\']) {
        return None;
    }
    Some(identifier.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn overlays(keys: &[&str]) -> BTreeMap<String, &'static str> {
        keys.iter()
            .map(|key| ((*key).to_string(), "overlay"))
            .collect()
    }

    #[test]
    fn the_exact_key_wins_over_the_executable_fallback() {
        let mut map = BTreeMap::new();
        map.insert(
            r"c:\program files\windowsapps\sharex_16.0_x64\sharex.exe".to_string(),
            "exact",
        );
        map.insert("exe:sharex.exe".to_string(), "fallback");
        assert_eq!(
            overlay_for(
                &map,
                r"c:\program files\windowsapps\sharex_16.0_x64\sharex.exe"
            ),
            Some(&"exact")
        );
    }

    #[test]
    fn a_versioned_path_falls_back_to_the_executable_name() {
        let map = overlays(&["exe:sharex.exe"]);
        // The install directory carries the version, so only the basename is
        // stable across updates.
        for path in [
            r"c:\program files\windowsapps\sharex_16.0_x64\sharex.exe",
            r"c:\program files\windowsapps\sharex_17.1_x64\sharex.exe",
            r"C:\Program Files\ShareX\ShareX.EXE",
            "/c/program files/sharex/sharex.exe",
        ] {
            assert_eq!(overlay_for(&map, path), Some(&"overlay"), "{path}");
        }
    }

    #[test]
    fn identifiers_that_name_no_executable_never_fall_back() {
        let map = overlays(&["exe:code.exe", "exe:com.example.exe", "exe:.exe"]);
        // macOS bundle ids and Linux classes are not paths, even when they end
        // in `.exe`; a path with no executable name has nothing stable to match.
        for app in [
            "com.microsoft.VSCode",
            "com.example.exe",
            "Firefox",
            "code.exe",
            r"c:\program files\microsoft vs code\code.exe.bak",
            r"c:\program files\microsoft vs code\",
            "",
        ] {
            assert_eq!(overlay_for(&map, app), None, "{app}");
        }
    }
}

#[cfg(test)]
#[cfg(all(feature = "fs", target_os = "macos"))]
mod bundle_tests;
