//! The foreground application, in the form per-app profiles match against.
//!
//! Lives in the platform-free core crate because three layers need the same
//! shape and none of them can see the others: `openlogi-hook` reads it from the
//! window server, `openlogi-agent-core` holds it, and `openlogi-ipc` puts it on
//! the wire. That last one makes this a wire type — see
//! `crates/openlogi-ipc/AGENTS.md`.
//!
//! [`AppAliases`] is the `[apps]` table that names one application across
//! platforms; [`overlay_for`] is the matcher that consults it.

use std::collections::BTreeMap;
use std::path::Path;

use serde::{Deserialize, Serialize};

mod aliases;

pub(crate) use self::aliases::AppPlatform;
pub use self::aliases::{AppAlias, AppAliasError, AppAliasName, AppAliasNameError, AppAliases};

/// One application, named the way a per-app profile names it.
///
/// [`Self::id`] is the whole of the matching contract; [`Self::display_name`]
/// exists only so a UI never has to show a reverse-DNS string to a human.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ForegroundApp {
    /// The exact application identifier a per-app profile key is compared against:
    ///
    /// - macOS: the bundle identifier, or the executable path when no bundle
    ///   identifier is available. Paths retain their original spelling and case.
    /// - Linux: the X11 `WM_CLASS` class or Wayland xdg `app_id`.
    /// - Windows: the lower-cased executable path.
    ///
    /// Those namespaces do not map onto one another by any simple string rule,
    /// which is why a profile keyed by one of them will not match under
    /// another; an [`AppAlias`] lists one application's identifier on each
    /// platform for a profile that must. See
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
///
/// Only then does an alias apply: the `app:<alias>` entry of the alias in
/// `aliases` that lists the identifier — or, failing that, its
/// `exe:<filename>` — for the platform OpenLogi runs on. A key in the
/// platform's own namespace names this installation more precisely than an
/// alias shared across platforms, so it wins.
#[must_use]
pub fn overlay_for<'a, T>(
    overlays: &'a BTreeMap<String, T>,
    aliases: &AppAliases,
    app: &str,
) -> Option<&'a T> {
    entry_for(|key| overlays.get(key), aliases, AppPlatform::CURRENT, app)
}

/// The key of the entry [`overlay_for`] resolves `app` to, for an editor that
/// must show which profile an application already runs under.
#[must_use]
pub fn profile_key_for<'a, T>(
    overlays: &'a BTreeMap<String, T>,
    aliases: &AppAliases,
    app: &str,
) -> Option<&'a str> {
    entry_for(
        |key| overlays.get_key_value(key).map(|(key, _)| key.as_str()),
        aliases,
        AppPlatform::CURRENT,
        app,
    )
}

/// Whether a profile keyed `key` applies to the foreground application `app`
/// when no other profile does: `key` is `app` itself, its `exe:<filename>`, or
/// the `app:<alias>` of the alias that lists either. Matched as
/// [`overlay_for`] matches, for a caller that holds one key rather than a map.
#[must_use]
pub fn profile_applies_to(key: &str, aliases: &AppAliases, app: &str) -> bool {
    entry_for(
        |selector| (selector == key).then_some(()),
        aliases,
        AppPlatform::CURRENT,
        app,
    )
    .is_some()
}

/// The first entry `lookup` finds, trying the selectors for `app` from the
/// most specific: `app`, its `exe:<filename>`, then the `app:<alias>` of the
/// alias that lists `app`, then of the alias that lists its `exe:<filename>`.
fn entry_for<V>(
    lookup: impl Fn(&str) -> Option<V>,
    aliases: &AppAliases,
    platform: AppPlatform,
    app: &str,
) -> Option<V> {
    let executable = executable_selector(app);
    let selectors = || std::iter::once(app).chain(executable.as_deref());
    selectors().find_map(&lookup).or_else(|| {
        selectors().find_map(|selector| {
            let alias = aliases.claiming(platform, selector)?;
            lookup(alias.profile_key().as_str())
        })
    })
}

/// The `exe:<filename>` selector of a Windows executable path, or `None` for
/// an identifier that names no executable.
fn executable_selector(app: &str) -> Option<String> {
    let (_, executable_name) = app.rsplit_once(['\\', '/'])?;
    Path::new(executable_name)
        .extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("exe"))
        .then(|| format!("exe:{}", executable_name.to_ascii_lowercase()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn overlays(keys: &[&str]) -> BTreeMap<String, &'static str> {
        keys.iter()
            .map(|key| ((*key).to_string(), "overlay"))
            .collect()
    }

    fn keyed(entries: &[(&str, &'static str)]) -> BTreeMap<String, &'static str> {
        entries
            .iter()
            .map(|(key, value)| ((*key).to_string(), *value))
            .collect()
    }

    fn no_aliases() -> AppAliases {
        AppAliases::default()
    }

    fn aliases(source: &str) -> AppAliases {
        #[derive(Deserialize)]
        struct Document {
            apps: AppAliases,
        }
        toml::from_str::<Document>(source)
            .expect("test aliases must parse")
            .apps
    }

    fn resolved(
        map: &BTreeMap<String, &'static str>,
        table: &AppAliases,
        platform: AppPlatform,
        app: &str,
    ) -> Option<&'static str> {
        entry_for(|key| map.get(key).copied(), table, platform, app)
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
                &no_aliases(),
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
            assert_eq!(
                overlay_for(&map, &no_aliases(), path),
                Some(&"overlay"),
                "{path}"
            );
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
            assert_eq!(overlay_for(&map, &no_aliases(), app), None, "{app}");
        }
    }

    #[test]
    fn an_alias_applies_where_it_lists_the_identifier() {
        let table = aliases(
            r#"
            [apps.chrome]
            macos = "com.google.Chrome"
            linux = ["google-chrome", "Google-chrome"]
            windows = "exe:chrome.exe"
            "#,
        );
        let map = keyed(&[("app:chrome", "chrome")]);
        for (platform, app) in [
            (AppPlatform::MacOs, "com.google.Chrome"),
            (AppPlatform::Linux, "Google-chrome"),
            (
                AppPlatform::Windows,
                r"c:\program files\google\chrome\application\chrome.exe",
            ),
        ] {
            assert_eq!(
                resolved(&map, &table, platform, app),
                Some("chrome"),
                "{platform:?} {app}"
            );
        }
    }

    #[test]
    fn an_alias_matches_only_the_identifiers_of_the_running_platform() {
        // The Linux class is listed, but a Mac never reports one: a match
        // across namespaces would make the platform fields meaningless.
        let table = aliases("[apps.editor]\nlinux = \"editor\"\n");
        let map = keyed(&[("app:editor", "editor")]);
        assert_eq!(resolved(&map, &table, AppPlatform::MacOs, "editor"), None);
        assert_eq!(resolved(&map, &table, AppPlatform::Windows, "editor"), None);
        assert_eq!(
            resolved(&map, &table, AppPlatform::Linux, "editor"),
            Some("editor")
        );
    }

    #[test]
    fn a_platform_key_wins_over_an_alias() {
        let table = aliases("[apps.sharex]\nwindows = 'c:\\program files\\sharex\\sharex.exe'\n");
        let path = r"c:\program files\sharex\sharex.exe";
        // Most specific first: the exact key, the executable fallback, then
        // the alias. Dropping each winner in turn exposes the next one.
        let ladder = [
            (path, "exact"),
            ("exe:sharex.exe", "fallback"),
            ("app:sharex", "alias"),
        ];
        for start in 0..ladder.len() {
            let map = keyed(&ladder[start..]);
            assert_eq!(
                resolved(&map, &table, AppPlatform::Windows, path),
                Some(ladder[start].1),
                "with {:?} present",
                &ladder[start..]
            );
        }
    }

    #[test]
    fn a_profile_key_applies_to_the_applications_the_matcher_sends_to_it() {
        // Listed under every platform field, so this holds on each CI host.
        let table = aliases(concat!(
            "[apps.tool]\nmacos = \"exe:tool.exe\"\nlinux = \"exe:tool.exe\"\n",
            "windows = \"exe:tool.exe\"\n",
            "[apps.editor]\nmacos = \"editor-id\"\nlinux = \"editor-id\"\n",
            "windows = \"editor-id\"\n",
        ));
        let path = r"c:\tools\tool.exe";
        for (key, app, applies) in [
            (path, path, true),
            ("exe:tool.exe", path, true),
            ("app:tool", path, true),
            ("app:editor", "editor-id", true),
            ("app:editor", path, false),
            ("app:tool", "editor-id", false),
            ("exe:other.exe", path, false),
            ("editor", "editor-id", false),
        ] {
            assert_eq!(
                profile_applies_to(key, &table, app),
                applies,
                "{key} for {app}"
            );
        }
    }

    #[test]
    fn an_alias_claims_only_identifiers_equal_to_one_it_lists() {
        let table =
            aliases("[apps.editor]\nmacos = \"com.example.Editor\"\nwindows = \"exe:tool.exe\"\n");
        let map = overlays(&["app:editor"]);
        for (platform, app) in [
            (AppPlatform::MacOs, "com.example.EditorBeta"),
            (AppPlatform::MacOs, "com.example.Edito"),
            (AppPlatform::MacOs, "com.example.editor"),
            (AppPlatform::Windows, r"c:\tools\tool.exe.old"),
            (AppPlatform::Windows, r"c:\tools\mytool.exe"),
        ] {
            assert_eq!(resolved(&map, &table, platform, app), None, "{app}");
        }
        assert_eq!(
            resolved(&map, &table, AppPlatform::MacOs, "com.example.Editor"),
            Some("overlay")
        );
    }

    #[test]
    fn an_alias_listing_the_exact_path_wins_over_one_listing_the_executable() {
        let table = aliases(
            "[apps.portable]\nwindows = 'd:\\portable\\code.exe'\n[apps.code]\nwindows = \"exe:code.exe\"\n",
        );
        let map = keyed(&[("app:portable", "portable"), ("app:code", "code")]);
        assert_eq!(
            resolved(&map, &table, AppPlatform::Windows, r"d:\portable\code.exe"),
            Some("portable")
        );
        assert_eq!(
            resolved(
                &map,
                &table,
                AppPlatform::Windows,
                r"c:\program files\microsoft vs code\code.exe"
            ),
            Some("code")
        );
    }

    #[test]
    fn an_alias_without_a_profile_selects_nothing() {
        let table = aliases("[apps.chrome]\nmacos = \"com.google.Chrome\"\n");
        let map = keyed(&[
            ("app:firefox", "firefox"),
            ("org.mozilla.firefox", "native"),
        ]);
        assert_eq!(
            resolved(&map, &table, AppPlatform::MacOs, "com.google.Chrome"),
            None
        );
    }

    #[test]
    fn the_resolved_key_is_the_one_the_overlay_came_from() {
        let table = aliases(&format!(
            "[apps.here]\n{} = \"com.example.here\"\n",
            AppPlatform::CURRENT.field()
        ));
        let map = keyed(&[("app:here", "alias"), ("com.example.Native", "native")]);
        assert_eq!(
            profile_key_for(&map, &table, "com.example.here"),
            Some("app:here")
        );
        assert_eq!(
            overlay_for(&map, &table, "com.example.here"),
            Some(&"alias")
        );
        assert_eq!(
            profile_key_for(&map, &table, "com.example.Native"),
            Some("com.example.Native")
        );
        assert_eq!(profile_key_for(&map, &table, "com.example.Elsewhere"), None);
    }
}
