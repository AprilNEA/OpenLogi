//! Brand constants shared across the workspace: the project's public URLs,
//! the `openlogi://` deep-link command vocabulary, the three processes' bundle
//! identifiers, and the app bundle's layout — where the helpers live and what
//! they are called on disk.
//!
//! All of it lives here, in the platform-free core crate, so the agent (which
//! *emits* tray deep links and finds the overlay), the GUI (which *parses* the
//! deep links and finds the agent), and the packaging tooling share a single
//! source of truth: the command names can't drift across the process boundary,
//! a helper is looked up at runtime by the name packaging gave it, and a rename
//! touches one file.

use std::borrow::Cow;
use std::path::Path;

use percent_encoding::{AsciiSet, NON_ALPHANUMERIC, percent_decode_str, utf8_percent_encode};

use crate::device_order::PhysicalDeviceKey;

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

/// The app's display name: the outer bundle's `CFBundleName` and
/// `CFBundleDisplayName`, and the root the helpers' names are formed from.
pub const APP_NAME: &str = "OpenLogi";

/// The GUI's executable, as cargo builds it and as the macOS bundle and the
/// Linux packages ship it. The helpers' executables are [`Helper::executable`].
pub const GUI_EXECUTABLE: &str = "openlogi-desktop";

/// The CLI's executable — the command users type, so it never changes. The
/// Windows installer ships the GUI under the same name in another case
/// (`OpenLogi.exe`), which the case-insensitive match below covers.
pub const CLI_EXECUTABLE: &str = "openlogi";

/// `name`'s dev-channel counterpart, the way [`dev_id`] is for identifiers:
/// what System Settings shows for a local build's bundle, and the directory a
/// dev helper lives in.
#[must_use]
pub fn dev_name(name: &str) -> String {
    format!("{name} Dev")
}

/// Where the app bundle nests its login-item helpers, relative to its root.
pub const LOGIN_ITEMS_DIR: &str = "Contents/Library/LoginItems";

/// Where the app bundle carries the agent's launchd service plist, relative
/// to its root.
pub const LAUNCH_AGENTS_DIR: &str = "Contents/Library/LaunchAgents";

/// The nested login-item helpers the app bundle embeds.
///
/// Each helper's directory is named exactly like its display name, because
/// macOS privacy panes fall back to a bundle's filename whenever its metadata
/// is stale — a spelling that differs from the display name, or a dev helper
/// named like the shipped one, renders as a row nobody can trust. Packaging
/// writes these names and both the app and the agent look the helpers up by
/// them at runtime, so they are defined once here.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Helper {
    /// The always-on agent: the process that owns the hook and holds the
    /// Accessibility grant.
    Agent,
    /// The Actions Ring renderer.
    Overlay,
}

impl Helper {
    /// The helper's bundle identifier.
    #[must_use]
    pub const fn bundle_id(self) -> &'static str {
        match self {
            Self::Agent => AGENT_ID,
            Self::Overlay => OVERLAY_ID,
        }
    }

    /// The shipped display name — and so the shipped bundle directory's name.
    #[must_use]
    pub const fn display_name(self) -> &'static str {
        match self {
            Self::Agent => "OpenLogi Agent",
            Self::Overlay => "OpenLogi Overlay",
        }
    }

    /// The helper's executable: how cargo names the binary, and how the
    /// process shows up in a process list.
    #[must_use]
    pub const fn executable(self) -> &'static str {
        match self {
            Self::Agent => "openlogi-agent",
            Self::Overlay => "openlogi-overlay",
        }
    }

    /// The shipped helper's bundle, relative to the app bundle's root.
    #[must_use]
    pub fn bundle_dir(self) -> String {
        format!("{LOGIN_ITEMS_DIR}/{}.app", self.display_name())
    }

    /// A dev build's helper bundle, relative to the app bundle's root.
    #[must_use]
    pub fn dev_bundle_dir(self) -> String {
        format!("{LOGIN_ITEMS_DIR}/{}.app", dev_name(self.display_name()))
    }

    /// The helper's executable inside `bundle_dir`, one of [`Self::bundle_dir`]
    /// or [`Self::dev_bundle_dir`].
    #[must_use]
    pub fn executable_in(self, bundle_dir: &str) -> String {
        format!("{bundle_dir}/Contents/MacOS/{}", self.executable())
    }

    /// The bundle name this helper shipped under before it took its display
    /// name. History, so spelled out rather than derived: a future rename must
    /// not quietly stop old installs from being found.
    const fn legacy_bundle_name(self) -> &'static str {
        match self {
            Self::Agent => "OpenLogiAgent",
            Self::Overlay => "OpenLogiOverlay",
        }
    }

    /// Every path, relative to the app bundle's root, at which this helper's
    /// executable has ever shipped, newest layout first: the dev-suffixed
    /// name, the shipped name, and the pre-rename name for bundles built
    /// before the helpers took their display names.
    #[must_use]
    pub fn executable_candidates(self) -> [String; 3] {
        let legacy = format!("{LOGIN_ITEMS_DIR}/{}.app", self.legacy_bundle_name());
        [
            self.executable_in(&self.dev_bundle_dir()),
            self.executable_in(&self.bundle_dir()),
            self.executable_in(&legacy),
        ]
    }
}

/// The `.app` root of a packaged helper binary — `…/Foo.app/Contents/MacOS/foo`
/// gives `…/Foo.app` — and `None` for a bare binary such as a cargo build
/// output.
#[must_use]
pub fn helper_bundle_root(executable: &Path) -> Option<&Path> {
    let bundle = executable.ancestors().nth(3)?;
    (bundle.extension()? == "app").then_some(bundle)
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
/// builds included; and the Windows executable path, matched on its file name
/// — the installed names from `packaging/windows/OpenLogi.wxs` (which carries
/// its own literal copy, since it can't reference Rust — keep the two in sync)
/// plus the cargo artifact name a dev build's GUI runs under.
#[must_use]
pub fn is_openlogi_foreground_id(id: &str) -> bool {
    const WINDOWS_SUFFIX: &str = ".exe";
    let executables = [
        CLI_EXECUTABLE,
        GUI_EXECUTABLE,
        Helper::Agent.executable(),
        Helper::Overlay.executable(),
    ];

    let base = strip_dev_suffix(id);
    [APP_ID, AGENT_ID, OVERLAY_ID]
        .iter()
        .any(|own| base.eq_ignore_ascii_case(own))
        || id
            .rsplit(['\\', '/'])
            .next()
            .filter(|file| ends_with_ignore_ascii_case(file, WINDOWS_SUFFIX))
            .and_then(|file| file.get(..file.len() - WINDOWS_SUFFIX.len()))
            .is_some_and(|stem| executables.iter().any(|exe| stem.eq_ignore_ascii_case(exe)))
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
/// an `openlogi://<name>` URL, or `openlogi://<name>/<argument>` for a command
/// that names its target. macOS delivers it to the running GUI via an Apple
/// Event; the GUI parses it back into this enum and dispatches.
///
/// The agent builds URLs with [`DeeplinkCommand::to_url`]; the GUI reads them
/// with [`DeeplinkCommand::parse_url`]. Both go through this one type, so the
/// command names and the argument encoding cannot drift apart.
#[derive(Clone, Debug, Eq, PartialEq)]
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
    /// Open one device's detail page. A GUI launched by the link opens it once
    /// the agent's device list arrives.
    OpenDevice(DeviceLink),
    /// Open the Add Device window.
    AddDevice,
    /// Open the per-application profile the selected device runs this
    /// application under — a [`ForegroundApp::id`], never empty.
    ///
    /// [`ForegroundApp::id`]: crate::app::ForegroundApp::id
    OpenAppProfile(String),
    /// Open the Flow window.
    OpenFlow,
}

/// A device an `openlogi://` link may open: a [`PhysicalDeviceKey`] that names
/// the device itself. A receiver's pairing-slot key is refused, because it
/// passes to whichever device is paired into that slot next, and a link holding
/// one could open the replacement.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DeviceLink(PhysicalDeviceKey);

impl DeviceLink {
    /// `key` as a link target, or `None` for a pairing-slot key.
    #[must_use]
    pub fn new(key: PhysicalDeviceKey) -> Option<Self> {
        (!key.names_pairing_slot()).then_some(Self(key))
    }

    /// The configuration key the link names.
    #[must_use]
    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }
}

/// What a command argument escapes in its path segment: everything outside
/// RFC 3986's unreserved set except `:`, so a `/`, `?`, `#` or `%` in a serial
/// number or an executable path cannot end the segment early.
const ARGUMENT: &AsciiSet = &NON_ALPHANUMERIC
    .remove(b'-')
    .remove(b'.')
    .remove(b'_')
    .remove(b'~')
    .remove(b':');

impl DeeplinkCommand {
    /// The URL scheme OpenLogi registers with LaunchServices.
    pub const SCHEME: &str = "openlogi";

    /// The command's wire name, which is its URL's host component, and the
    /// argument that follows as the URL's one path segment.
    fn wire(&self) -> (&'static str, Option<&str>) {
        match self {
            Self::Show => ("show", None),
            Self::OpenSettings => ("open-settings", None),
            Self::OpenAbout => ("open-about", None),
            Self::CheckForUpdates => ("check-for-updates", None),
            Self::Quit => ("quit", None),
            Self::OpenDevice(device) => ("open-device", Some(device.as_str())),
            Self::AddDevice => ("add-device", None),
            Self::OpenAppProfile(app) => ("open-app-profile", Some(app)),
            Self::OpenFlow => ("open-flow", None),
        }
    }

    /// The inverse of [`Self::wire`]. A command without an argument ignores
    /// any it is given.
    fn from_wire(name: &str, argument: Option<String>) -> Option<Self> {
        Some(match (name, argument) {
            ("show", _) => Self::Show,
            ("open-settings", _) => Self::OpenSettings,
            ("open-about", _) => Self::OpenAbout,
            ("check-for-updates", _) => Self::CheckForUpdates,
            ("quit", _) => Self::Quit,
            ("open-device", Some(key)) => {
                Self::OpenDevice(DeviceLink::new(PhysicalDeviceKey::parse(&key)?)?)
            }
            ("add-device", _) => Self::AddDevice,
            ("open-app-profile", Some(app)) => Self::OpenAppProfile(app),
            ("open-flow", _) => Self::OpenFlow,
            _ => return None,
        })
    }

    /// Build the `openlogi://…` URL for this command.
    #[must_use]
    pub fn to_url(&self) -> String {
        match self.wire() {
            (name, None) => format!("{}://{name}", Self::SCHEME),
            (name, Some(argument)) => format!(
                "{}://{name}/{}",
                Self::SCHEME,
                utf8_percent_encode(argument, ARGUMENT)
            ),
        }
    }

    /// Parse a full `openlogi://…` URL. The command lives in the URL's host
    /// component and its argument, if it takes one, in the single path segment
    /// after it. A query or fragment is ignored, and so is the path of a
    /// command that takes no argument (`openlogi://show/`,
    /// `openlogi://show?x=1`). Returns `None` for a foreign scheme, an unknown
    /// command, or a missing, empty, multi-segment or invalid argument.
    #[must_use]
    pub fn parse_url(url: &str) -> Option<Self> {
        let rest = url.strip_prefix(Self::SCHEME)?.strip_prefix("://")?;
        let rest = rest.split(['?', '#']).next().unwrap_or(rest);
        let (name, path) = rest.split_once('/').unwrap_or((rest, ""));
        let segment = path.strip_suffix('/').unwrap_or(path);
        let argument = (!segment.is_empty() && !segment.contains('/'))
            .then(|| percent_decode_str(segment).decode_utf8().ok())
            .flatten()
            .map(Cow::into_owned);
        Self::from_wire(name, argument)
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::{
        AGENT_ID, APP_ID, DeeplinkCommand, DeviceLink, Helper, OVERLAY_ID, dev_id,
        helper_bundle_root, is_dev_id, is_openlogi_foreground_id,
    };
    use crate::device_order::PhysicalDeviceKey;

    fn device(key: &str) -> DeeplinkCommand {
        let key = PhysicalDeviceKey::parse(key).expect("a physical key");
        DeeplinkCommand::OpenDevice(DeviceLink::new(key).expect("names a device"))
    }

    fn app(id: &str) -> DeeplinkCommand {
        DeeplinkCommand::OpenAppProfile(id.to_owned())
    }

    #[test]
    fn url_round_trips() {
        let commands = [
            DeeplinkCommand::Show,
            DeeplinkCommand::OpenSettings,
            DeeplinkCommand::OpenAbout,
            DeeplinkCommand::CheckForUpdates,
            DeeplinkCommand::Quit,
            DeeplinkCommand::AddDevice,
            DeeplinkCommand::OpenFlow,
            device("unit:4f4c4401"),
            // A serial carries whatever the firmware reported, including the
            // characters that would end a path segment or a URL.
            device("serial:ab/c d?e#f%20g"),
            device("direct:046d:b034:unit:4f4c4403"),
            device("raw:046d:c900:ff43:0202:serial:litra 01"),
            app("com.apple.Safari"),
            app(r"c:\program files\adobe\photoshop 2026\photoshop.exe"),
            app("/Applications/Zed.app/Contents/MacOS/zed"),
            app("org.example.Ünïcødé"),
        ];
        for command in commands {
            let url = command.to_url();
            assert_eq!(DeeplinkCommand::parse_url(&url), Some(command), "{url}");
        }
    }

    #[test]
    fn a_targeted_command_carries_its_argument_as_one_escaped_path_segment() {
        assert_eq!(
            device("unit:4f4c4401").to_url(),
            "openlogi://open-device/unit:4f4c4401"
        );
        assert_eq!(
            app(r"c:\apps\my app.exe").to_url(),
            "openlogi://open-app-profile/c:%5Capps%5Cmy%20app.exe"
        );
        assert_eq!(DeeplinkCommand::AddDevice.to_url(), "openlogi://add-device");
        assert_eq!(DeeplinkCommand::OpenFlow.to_url(), "openlogi://open-flow");
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
        assert_eq!(
            DeeplinkCommand::parse_url("openlogi://open-device/unit:4f4c4401/?from=tray#x"),
            Some(device("unit:4f4c4401"))
        );
    }

    #[test]
    fn parse_url_rejects_foreign_scheme_and_unknown_command() {
        assert_eq!(DeeplinkCommand::parse_url("https://example.com/show"), None);
        assert_eq!(DeeplinkCommand::parse_url("openlogi://bogus"), None);
        assert_eq!(DeeplinkCommand::parse_url("openlogi://"), None);
    }

    #[test]
    fn a_link_never_names_a_pairing_slot() {
        // Whoever is paired into slot 1 next would answer to this key.
        let slot = PhysicalDeviceKey::parse("receiver:ol-bolt-uid-0001:slot:1")
            .expect("a slot key is a physical key");
        assert_eq!(DeviceLink::new(slot), None);
        assert_eq!(
            DeeplinkCommand::parse_url("openlogi://open-device/receiver:ol-bolt-uid-0001:slot:1"),
            None
        );
    }

    #[test]
    fn a_targeted_command_without_a_valid_argument_is_rejected() {
        for url in [
            "openlogi://open-device",
            "openlogi://open-device/",
            // Not a key that names one physical device.
            "openlogi://open-device/wpid:b034",
            "openlogi://open-device/unit:00000000",
            // An unescaped `/` splits the argument; a guess at either half
            // could open the wrong device or application.
            "openlogi://open-device/serial:ab/cd",
            "openlogi://open-app-profile",
            "openlogi://open-app-profile/?app=com.apple.Safari",
            "openlogi://open-app-profile//Applications/Zed.app",
            // Not UTF-8 once unescaped.
            "openlogi://open-app-profile/%FF",
        ] {
            assert_eq!(DeeplinkCommand::parse_url(url), None, "{url}");
        }
    }

    #[test]
    fn dev_ids_round_trip() {
        for id in [APP_ID, AGENT_ID, OVERLAY_ID] {
            assert!(is_dev_id(&dev_id(id)), "{id} suffixed must read as dev");
            assert!(!is_dev_id(id), "{id} is production");
        }
    }

    #[test]
    fn helper_bundles_are_named_after_their_display_names() {
        assert_eq!(
            Helper::Agent.bundle_dir(),
            "Contents/Library/LoginItems/OpenLogi Agent.app"
        );
        assert_eq!(
            Helper::Overlay.dev_bundle_dir(),
            "Contents/Library/LoginItems/OpenLogi Overlay Dev.app"
        );
        // Bundles built before the helpers took their display names are
        // still found, after the current layouts.
        assert_eq!(
            Helper::Agent.executable_candidates()[2],
            "Contents/Library/LoginItems/OpenLogiAgent.app/Contents/MacOS/openlogi-agent"
        );
    }

    #[test]
    fn a_helper_bundle_root_is_three_levels_above_its_executable() {
        let packaged = Path::new(
            "/Applications/OpenLogi.app/Contents/Library/LoginItems/OpenLogi Agent.app/Contents/MacOS/openlogi-agent",
        );
        assert_eq!(
            helper_bundle_root(packaged),
            Some(Path::new(
                "/Applications/OpenLogi.app/Contents/Library/LoginItems/OpenLogi Agent.app"
            ))
        );
        let dev = Path::new(
            "/Users/me/OpenLogi/target/dev/OpenLogi.app/Contents/Library/LoginItems/OpenLogi Agent Dev.app/Contents/MacOS/openlogi-agent",
        );
        assert_eq!(
            helper_bundle_root(dev),
            Some(Path::new(
                "/Users/me/OpenLogi/target/dev/OpenLogi.app/Contents/Library/LoginItems/OpenLogi Agent Dev.app"
            ))
        );
        assert_eq!(
            helper_bundle_root(Path::new("target/debug/openlogi-agent")),
            None
        );
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
