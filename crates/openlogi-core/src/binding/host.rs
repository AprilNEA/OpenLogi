//! Which host an action runs on, and which actions that host can perform.
//!
//! The catalog is one list for every platform, but `openlogi-inject` cannot
//! perform every entry everywhere: App Exposé has no Linux equivalent, the
//! Super-key overview actions only exist on GNOME, and `TypeText` has no Linux
//! or Windows backend yet. [`Action::is_available_on`] is the single table of
//! those gaps, read both by the pickers (to stop offering a binding that would
//! never fire) and by the Linux backend (to decide whether to fire it), so the
//! two cannot drift apart.

use super::action::Action;

/// The operating system, and on Linux the desktop, an action is dispatched on.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Host {
    /// macOS: every catalog action has a backend.
    MacOs,
    /// Windows.
    Windows,
    /// Linux, split by desktop because the window-manager actions are
    /// synthesised as that desktop's own default keybindings.
    Linux(LinuxDesktop),
    /// A target with no input backend at all (e.g. `wasm32`). Nothing is
    /// hidden there, since nothing is dispatched there either.
    Other,
}

/// The Linux desktop, as far as action dispatch cares.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum LinuxDesktop {
    /// GNOME Shell, including distro sessions such as `ubuntu:GNOME`.
    Gnome,
    /// Any other desktop, or none detected.
    Other,
}

impl LinuxDesktop {
    /// Classify a `$XDG_CURRENT_DESKTOP` value, which is a colon-separated
    /// list such as `ubuntu:GNOME` or `KDE`.
    #[must_use]
    pub fn from_current_desktop(value: &str) -> Self {
        if value.to_lowercase().contains("gnome") {
            Self::Gnome
        } else {
            Self::Other
        }
    }

    /// The desktop of the running session, read from `$XDG_CURRENT_DESKTOP`.
    #[must_use]
    pub fn current() -> Self {
        Self::from_current_desktop(&std::env::var("XDG_CURRENT_DESKTOP").unwrap_or_default())
    }
}

impl Host {
    /// The host this process runs on.
    #[must_use]
    pub fn current() -> Self {
        if cfg!(target_os = "macos") {
            Self::MacOs
        } else if cfg!(target_os = "windows") {
            Self::Windows
        } else if cfg!(target_os = "linux") {
            Self::Linux(LinuxDesktop::current())
        } else {
            Self::Other
        }
    }
}

impl Action {
    /// Whether `host` can perform this action, i.e. whether binding it does
    /// anything there.
    ///
    /// Payloads are ignored: a [`Action::Workflow`] is available even though a
    /// `TypeText` step inside it is skipped on hosts without a text backend.
    #[must_use]
    pub fn is_available_on(&self, host: Host) -> bool {
        match host {
            Host::MacOs | Host::Other => true,
            Host::Windows => !matches!(
                self,
                Action::TypeText(_) | Action::RunAppleScript(_) | Action::Sleep
            ),
            Host::Linux(desktop) => match self {
                Action::TypeText(_) | Action::RunAppleScript(_) | Action::AppExpose => false,
                // Bare Super, Super+D and Super+A are GNOME's defaults; other
                // desktops bind those keys to something else (KDE opens
                // KRunner on Super), so they are not sent there.
                Action::MissionControl | Action::ShowDesktop | Action::LaunchpadShow => {
                    desktop == LinuxDesktop::Gnome
                }
                _ => true,
            },
        }
    }

    /// The catalog key for this action's label on `host`.
    ///
    /// Where a host names the same action differently, the label follows the
    /// host: GNOME's bare Super key opens the Activities overview, not Mission
    /// Control, and Linux desktops switch workspaces rather than desktops.
    /// Everything else falls back to [`Action::translation_key`].
    #[must_use]
    pub fn translation_key_on(&self, host: Host) -> Option<&'static str> {
        match (self, host) {
            (Action::MissionControl, Host::Linux(LinuxDesktop::Gnome)) => {
                Some("actions.activities_overview")
            }
            (Action::LaunchpadShow, Host::Linux(LinuxDesktop::Gnome)) => {
                Some("actions.show_applications")
            }
            (Action::PreviousDesktop, Host::Linux(_)) => Some("actions.previous_workspace"),
            (Action::NextDesktop, Host::Linux(_)) => Some("actions.next_workspace"),
            _ => self.translation_key(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const GNOME: Host = Host::Linux(LinuxDesktop::Gnome);
    const OTHER_LINUX: Host = Host::Linux(LinuxDesktop::Other);
    const ALL_HOSTS: [Host; 5] = [Host::MacOs, Host::Windows, GNOME, OTHER_LINUX, Host::Other];

    #[test]
    fn current_desktop_is_parsed_case_insensitively_from_the_list() {
        for gnome in ["GNOME", "ubuntu:GNOME", "GNOME-Classic:GNOME", "gnome"] {
            assert_eq!(
                LinuxDesktop::from_current_desktop(gnome),
                LinuxDesktop::Gnome,
                "{gnome}"
            );
        }
        for other in ["KDE", "Hyprland", "niri", "X-Cinnamon", ""] {
            assert_eq!(
                LinuxDesktop::from_current_desktop(other),
                LinuxDesktop::Other,
                "{other}"
            );
        }
    }

    #[test]
    fn macos_performs_every_catalog_action() {
        for action in Action::catalog() {
            assert!(action.is_available_on(Host::MacOs), "{action:?}");
        }
    }

    #[test]
    fn app_expose_has_no_linux_backend() {
        assert!(!Action::AppExpose.is_available_on(GNOME));
        assert!(!Action::AppExpose.is_available_on(OTHER_LINUX));
    }

    #[test]
    fn super_key_actions_are_gnome_only() {
        for action in [
            Action::MissionControl,
            Action::ShowDesktop,
            Action::LaunchpadShow,
        ] {
            assert!(action.is_available_on(GNOME), "{action:?}");
            assert!(!action.is_available_on(OTHER_LINUX), "{action:?}");
        }
    }

    #[test]
    fn text_and_applescript_need_macos() {
        for action in [
            Action::TypeText("hi".into()),
            Action::RunAppleScript("beep".into()),
        ] {
            for host in [Host::Windows, GNOME, OTHER_LINUX] {
                assert!(!action.is_available_on(host), "{action:?} on {host:?}");
            }
        }
        assert!(Action::RunShellCommand("true".into()).is_available_on(OTHER_LINUX));
    }

    #[test]
    fn sleep_has_no_windows_backend() {
        assert!(!Action::Sleep.is_available_on(Host::Windows));
        assert!(Action::Sleep.is_available_on(OTHER_LINUX));
    }

    #[test]
    fn linux_relabels_follow_the_desktop() {
        assert_eq!(
            Action::MissionControl.translation_key_on(GNOME),
            Some("actions.activities_overview")
        );
        assert_eq!(
            Action::MissionControl.translation_key_on(Host::MacOs),
            Some("actions.mission_control")
        );
        assert_eq!(
            Action::NextDesktop.translation_key_on(OTHER_LINUX),
            Some("actions.next_workspace")
        );
        assert_eq!(
            Action::NextDesktop.translation_key_on(Host::Windows),
            Some("actions.next_desktop")
        );
        assert_eq!(Action::TypeText("x".into()).translation_key_on(GNOME), None);
    }

    /// A fresh gesture button must not ship a direction that does nothing on
    /// the most common Linux desktop.
    #[cfg(target_os = "linux")]
    #[test]
    fn linux_gesture_defaults_all_fire_on_gnome() {
        use crate::binding::{GestureDirection, default_gesture_binding};

        for direction in GestureDirection::ALL {
            let action = default_gesture_binding(direction);
            assert!(action.is_available_on(GNOME), "{direction:?} -> {action:?}");
        }
    }

    #[test]
    fn every_catalog_action_keeps_a_key_on_every_host() {
        for host in ALL_HOSTS {
            for action in Action::catalog() {
                assert!(
                    action.translation_key_on(host).is_some(),
                    "{action:?} on {host:?}"
                );
            }
        }
    }
}
