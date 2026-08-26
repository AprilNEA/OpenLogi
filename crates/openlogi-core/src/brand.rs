//! Brand constants shared across the workspace: the project's public URLs and
//! the `openlogi://` deep-link command vocabulary.
//!
//! Both live here, in the platform-free core crate, so the agent (which *emits*
//! tray deep links and renders help links) and the GUI (which *parses* the deep
//! links and renders the same help links) share a single source of truth — the
//! command names can't drift across the process boundary, and a repo move
//! touches one file instead of three.

/// The OpenLogi GitHub repository.
pub const REPO_URL: &str = "https://github.com/AprilNEA/OpenLogi";
/// The README, used as the in-app "Help" link.
pub const HELP_URL: &str = "https://github.com/AprilNEA/OpenLogi#readme";
/// The "latest release" page.
pub const RELEASES_URL: &str = "https://github.com/AprilNEA/OpenLogi/releases/latest";

/// The application identifier: the Wayland xdg-toplevel `app_id` (and X11
/// `WM_CLASS`) the GUI advertises, the root of the macOS bundle-id family
/// (`org.openlogi.agent`, `org.openlogi.openlogi-dev`), and the value the Linux
/// `.desktop` file pins as `StartupWMClass`. Defined once here so the window the
/// compositor sees, the launcher that groups it, and the frontmost backend that
/// self-identifies OpenLogi can never disagree. The `.desktop` file carries its
/// own literal copy (it can't reference Rust) — keep the two in sync.
pub const APP_ID: &str = "org.openlogi.openlogi";

/// The always-on agent's bundle identifier — the process that owns the hook and
/// holds the Accessibility grant, shipped as a nested login item.
pub const AGENT_ID: &str = "org.openlogi.agent";

/// The Actions Ring overlay's bundle identifier, the second nested login item.
pub const OVERLAY_ID: &str = "org.openlogi.overlay";

/// The agent's macOS launchd service label: the `Label` in the app bundle's
/// embedded `Contents/Library/LaunchAgents/<label>.plist`, what `SMAppService`
/// registers, and the name `launchctl` addresses (`gui/<uid>/<label>`). Dev
/// bundles use [`dev_id`] of this, so a dev registration can never collide
/// with the shipped one.
///
/// A launchd label is a *namespace key*, not a TCC identity — deliberately not
/// [`AGENT_ID`], although the two look related: `org.openlogi.agent` is the
/// frozen label of the legacy hand-written `~/Library/LaunchAgents` plist
/// (see `openlogi-agent/src/autostart/macos.rs`), and reusing a legacy label
/// would make the migration's "is this job ours or the old file's?" question
/// unanswerable. Once shipped, this value is frozen the same way: renaming it
/// orphans the registration users already approved in Login Items.
pub const AGENT_SERVICE_LABEL: &str = "org.openlogi.agent.service";

/// What a dev build appends to every identifier above, so a local build can
/// never claim a shipped TCC grant and System Settings shows which of the two
/// installed copies a row belongs to.
const DEV_SUFFIX: &str = "-dev";

/// `id`'s dev-channel counterpart.
///
/// Packaging (`cargo xtask macos`) stamps the result into every `Info.plist`;
/// the agent matches running GUI processes against it. Defined here so the
/// identity a dev bundle carries and the identity anything looks for cannot
/// diverge.
#[must_use]
pub fn dev_id(id: &str) -> String {
    format!("{id}{DEV_SUFFIX}")
}

/// Whether `id` names a dev build — the inverse of [`dev_id`].
///
/// The profile split keys off this: a dev bundle gets its own config directory
/// and IPC socket, so a false negative points a dev build at the user's real
/// config. That asymmetry is why the legacy `.dev` spelling is still accepted —
/// a local bundle built before the rename must not silently claim production
/// state just because nobody rebuilt it.
#[must_use]
pub fn is_dev_id(id: &str) -> bool {
    strip_dev_suffix(id) != id
}

/// Whether `id` — a foreground-application identifier, as
/// [`ForegroundApp::id`](crate::app::ForegroundApp::id) defines one — names one
/// of OpenLogi's own three processes.
///
/// The frontmost-app reader sees the GUI whenever its window is in front, so
/// without this OpenLogi would offer itself as a target for a per-app profile.
/// Both identifier shapes are recognised: the bundle-id family above (macOS
/// bundle ids, and the `WM_CLASS` / `app_id` the GUI advertises on Linux), dev
/// builds included; and the Windows executable path, matched on its file name.
/// `packaging/windows/OpenLogi.wxs` carries its own literal copy of those names
/// (it can't reference Rust) — keep the two in sync.
#[must_use]
pub fn is_openlogi_foreground_id(id: &str) -> bool {
    /// Installed names from `OpenLogi.wxs`, plus the cargo artifact name a dev
    /// build runs under.
    const EXECUTABLES: [&str; 4] = [
        "openlogi.exe",
        "openlogi-agent.exe",
        "openlogi-overlay.exe",
        "openlogi-desktop.exe",
    ];

    let base = strip_dev_suffix(id);
    [APP_ID, AGENT_ID, OVERLAY_ID]
        .iter()
        .any(|own| base.eq_ignore_ascii_case(own))
        || id
            .rsplit(['\\', '/'])
            .next()
            .is_some_and(|file| EXECUTABLES.iter().any(|exe| file.eq_ignore_ascii_case(exe)))
}

/// The dev suffix before it was hyphenated. Recognised, never produced.
const LEGACY_DEV_SUFFIX: &str = ".dev";

/// `id` with a recognised dev suffix removed, or `id` unchanged.
fn strip_dev_suffix(id: &str) -> &str {
    [DEV_SUFFIX, LEGACY_DEV_SUFFIX]
        .iter()
        .find(|suffix| ends_with_ignore_ascii_case(id, suffix))
        .and_then(|suffix| id.get(..id.len() - suffix.len()))
        .unwrap_or(id)
}

fn ends_with_ignore_ascii_case(haystack: &str, suffix: &str) -> bool {
    haystack.len() > suffix.len()
        && haystack
            .get(haystack.len() - suffix.len()..)
            .is_some_and(|tail| tail.eq_ignore_ascii_case(suffix))
}

/// The release page for a specific version tag (e.g. the running build).
#[must_use]
pub fn release_tag_url(version: &str) -> String {
    format!("{REPO_URL}/releases/tag/v{version}")
}

/// A GUI action the agent's tray (or any external caller) requests by opening
/// an `openlogi://<name>` URL. macOS delivers it to the running GUI via an
/// Apple Event; the GUI parses it back into this enum and dispatches.
///
/// The agent builds URLs with [`DeeplinkCommand::to_url`]; the GUI reads them
/// with [`DeeplinkCommand::parse_url`]. The command names are defined once, in
/// [`DeeplinkCommand::as_name`], so the two sides cannot disagree.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DeeplinkCommand {
    /// Show / foreground the main window.
    Show,
    /// Open the Settings window.
    OpenSettings,
    /// Open Settings on the About page.
    OpenAbout,
    /// Run a manual update check and open Settings on the Updates page, where
    /// its status is rendered.
    CheckForUpdates,
    /// Quit the GUI.
    Quit,
}

impl DeeplinkCommand {
    /// The URL scheme OpenLogi registers with LaunchServices.
    pub const SCHEME: &str = "openlogi";

    /// The wire name for this command — the host component of its URL.
    #[must_use]
    pub const fn as_name(self) -> &'static str {
        match self {
            Self::Show => "show",
            Self::OpenSettings => "open-settings",
            Self::OpenAbout => "open-about",
            Self::CheckForUpdates => "check-for-updates",
            Self::Quit => "quit",
        }
    }

    /// Build the `openlogi://<name>` URL for this command.
    #[must_use]
    pub fn to_url(self) -> String {
        format!("{}://{}", Self::SCHEME, self.as_name())
    }

    /// Parse a command from its wire name (the part after `openlogi://`).
    #[must_use]
    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "show" => Some(Self::Show),
            "open-settings" => Some(Self::OpenSettings),
            "open-about" => Some(Self::OpenAbout),
            "check-for-updates" => Some(Self::CheckForUpdates),
            "quit" => Some(Self::Quit),
            _ => None,
        }
    }

    /// Parse a full `openlogi://…` URL. The command lives in the URL's host
    /// component, so any trailing path or query (`openlogi://show/`,
    /// `openlogi://show?x=1`) is ignored. Returns `None` for a foreign scheme
    /// or an unknown command.
    #[must_use]
    pub fn parse_url(url: &str) -> Option<Self> {
        let rest = url.strip_prefix(Self::SCHEME)?.strip_prefix("://")?;
        let name = rest.split(['/', '?']).next().unwrap_or(rest);
        Self::from_name(name)
    }
}

#[cfg(test)]
mod tests {
    use super::{
        AGENT_ID, APP_ID, DeeplinkCommand, OVERLAY_ID, dev_id, is_dev_id, is_openlogi_foreground_id,
    };

    const ALL: [DeeplinkCommand; 5] = [
        DeeplinkCommand::Show,
        DeeplinkCommand::OpenSettings,
        DeeplinkCommand::OpenAbout,
        DeeplinkCommand::CheckForUpdates,
        DeeplinkCommand::Quit,
    ];

    #[test]
    fn url_round_trips() {
        for cmd in ALL {
            assert_eq!(DeeplinkCommand::parse_url(&cmd.to_url()), Some(cmd));
        }
    }

    #[test]
    fn parse_url_ignores_trailing_path_and_query() {
        assert_eq!(
            DeeplinkCommand::parse_url("openlogi://show/"),
            Some(DeeplinkCommand::Show)
        );
        assert_eq!(
            DeeplinkCommand::parse_url("openlogi://open-settings?from=tray"),
            Some(DeeplinkCommand::OpenSettings)
        );
    }

    #[test]
    fn parse_url_rejects_foreign_scheme_and_unknown_command() {
        assert_eq!(DeeplinkCommand::parse_url("https://example.com/show"), None);
        assert_eq!(DeeplinkCommand::parse_url("openlogi://bogus"), None);
        assert_eq!(DeeplinkCommand::parse_url("openlogi://"), None);
    }

    #[test]
    fn dev_ids_round_trip() {
        for id in [APP_ID, AGENT_ID, OVERLAY_ID] {
            assert!(is_dev_id(&dev_id(id)), "{id} suffixed must read as dev");
            assert!(!is_dev_id(id), "{id} is production");
        }
    }

    #[test]
    fn the_legacy_dotted_suffix_still_reads_as_dev() {
        // A stale `target/dev` bundle from before the rename must not fall
        // through to the production config directory and IPC socket.
        assert!(is_dev_id("org.openlogi.agent.dev"));
        assert!(is_dev_id("org.openlogi.openlogi.dev"));
    }

    #[test]
    fn a_bare_suffix_is_not_a_dev_id() {
        assert!(!is_dev_id("-dev"));
        assert!(!is_dev_id(".dev"));
        assert!(!is_dev_id(""));
    }

    #[test]
    fn matching_ignores_case_but_not_position() {
        assert!(is_dev_id("org.openlogi.agent-DEV"));
        assert!(!is_dev_id("org.openlogi.dev-agent"));
    }

    #[test]
    fn our_own_processes_are_recognised_in_both_identifier_shapes() {
        for id in [APP_ID, AGENT_ID, OVERLAY_ID] {
            assert!(is_openlogi_foreground_id(id), "{id}");
            assert!(is_openlogi_foreground_id(&dev_id(id)), "dev {id}");
        }
        // Windows reports a lower-cased executable path; a dev build runs the
        // cargo artifact out of `target/`.
        assert!(is_openlogi_foreground_id(
            r"c:\program files\openlogi\openlogi.exe"
        ));
        assert!(is_openlogi_foreground_id(
            r"c:\program files\openlogi\openlogi-agent.exe"
        ));
        assert!(is_openlogi_foreground_id(
            r"c:\src\openlogi\target\debug\openlogi-desktop.exe"
        ));
    }

    #[test]
    fn a_foreign_app_that_merely_starts_the_same_way_is_not_ours() {
        assert!(!is_openlogi_foreground_id("org.openlogi.openlogi.helper"));
        assert!(!is_openlogi_foreground_id(r"c:\apps\openlogic.exe"));
        assert!(!is_openlogi_foreground_id("com.apple.Safari"));
        assert!(!is_openlogi_foreground_id(""));
    }
}
