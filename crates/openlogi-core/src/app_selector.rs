//! Resolving a foreground-application identifier against per-app config keys.
//!
//! Per-app overlays are keyed by whatever the platform reports as the frontmost
//! application: a bundle identifier on macOS, a `WM_CLASS` or xdg app id on
//! Linux, and a lower-cased executable path on Windows. A Windows path is not
//! stable — Store and self-updating applications live under a versioned
//! directory that changes from under the config — so `exe:<filename>.exe` is
//! accepted as a fallback selector for the same overlay maps.
//!
//! Every map keyed by that identifier resolves through [`overlay_for`], so a
//! selector means the same thing wherever it is written: button overlays and
//! Actions Ring layouts cannot disagree about which application is in front.

use std::collections::BTreeMap;
use std::path::Path;

/// Resolve the most specific overlay for a foreground identifier.
///
/// An exact key always wins, so a per-path overlay still beats the
/// executable-name fallback when a config carries both.
pub(crate) fn overlay_for<'a, T>(overlays: &'a BTreeMap<String, T>, app: &str) -> Option<&'a T> {
    if let Some(exact) = overlays.get(app) {
        return Some(exact);
    }

    overlays.get(&executable_selector(app)?)
}

/// The `exe:<filename>` selector a Windows-style identifier falls back to, or
/// `None` for an identifier that does not name an executable — macOS bundle ids
/// and Linux application classes must never acquire one by accident.
fn executable_selector(app: &str) -> Option<String> {
    // `rsplit` always yields, so this is the trailing path component, or the
    // whole identifier when it carries no separator. Both separators are
    // recognized so a Windows config stays inspectable on any platform.
    let executable_name = app.rsplit(['\\', '/']).next().unwrap_or(app);
    if executable_name.is_empty()
        || !Path::new(executable_name)
            .extension()
            .is_some_and(|extension| extension.eq_ignore_ascii_case("exe"))
    {
        return None;
    }
    Some(format!("exe:{}", executable_name.to_ascii_lowercase()))
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
    fn exact_key_wins_over_the_executable_fallback() {
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
            "/c/program files/sharex/sharex.exe",
            "sharex.exe",
        ] {
            assert_eq!(overlay_for(&map, path), Some(&"overlay"), "{path}");
        }
    }

    #[test]
    fn identifiers_that_name_no_executable_never_fall_back() {
        let map = overlays(&["exe:code.exe", "exe:.exe"]);
        // macOS bundle ids and Linux classes must not be reinterpreted as paths,
        // and a path with no executable name has nothing stable to match on.
        for app in [
            "com.microsoft.VSCode",
            "Firefox",
            "org.mozilla.firefox",
            r"c:\program files\microsoft vs code\code.exe.bak",
            r"c:\program files\microsoft vs code\",
            "",
        ] {
            assert_eq!(overlay_for(&map, app), None, "{app}");
        }
    }

    #[test]
    fn a_bundle_id_ending_in_exe_still_resolves_by_its_own_key() {
        // Contrived, but it must not be swallowed by the fallback: the exact
        // key is what the platform reported.
        let mut map = BTreeMap::new();
        map.insert("com.example.exe".to_string(), "exact");
        assert_eq!(overlay_for(&map, "com.example.exe"), Some(&"exact"));
    }
}
