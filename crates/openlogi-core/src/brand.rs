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
/// (`org.openlogi.agent`, `org.openlogi.openlogi.dev`), and the value the Linux
/// `.desktop` file pins as `StartupWMClass`. Defined once here so the window the
/// compositor sees, the launcher that groups it, and the frontmost backend that
/// self-identifies OpenLogi can never disagree. The `.desktop` file carries its
/// own literal copy (it can't reference Rust) — keep the two in sync.
pub const APP_ID: &str = "org.openlogi.openlogi";

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
    use super::DeeplinkCommand;

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
}
