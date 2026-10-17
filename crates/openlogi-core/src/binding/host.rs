//! Which host an action runs on, and which actions that host can perform.
//!
//! The catalog is one list for every platform, but `openlogi-inject` cannot
//! perform every entry everywhere: the window-manager actions exist only on
//! the Linux desktops that have a binding or API for them (GNOME's Super keys,
//! KWin's D-Bus effects, Hyprland's helpers), and `TypeText` has no Linux or
//! Windows backend yet. [`Action::is_available_on`] is the single table of
//! those gaps, read both by the pickers (to stop offering a binding that would
//! never fire) and by the Linux backend (to decide whether to fire it), so the
//! two cannot drift apart.

use super::action::{Action, WorkflowStep};

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

/// The Linux desktop, as far as action dispatch cares: each one is a separate
/// native table in `openlogi-inject`'s Linux backend.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum LinuxDesktop {
    /// GNOME Shell, including distro sessions such as `ubuntu:GNOME`.
    Gnome,
    /// KDE Plasma, driven through KWin's D-Bus API.
    Kde,
    /// Hyprland, driven through its own helpers.
    Hyprland,
    /// Any other desktop, or none detected.
    Other,
}

impl LinuxDesktop {
    /// Classify a session from `$XDG_CURRENT_DESKTOP` (a colon-separated list
    /// such as `ubuntu:GNOME` or `KDE`) and `$HYPRLAND_INSTANCE_SIGNATURE`.
    ///
    /// A non-empty Hyprland signature wins over the desktop list, which
    /// Hyprland sessions do not set consistently.
    #[must_use]
    pub fn from_session(current_desktop: &str, hyprland_signature: Option<&str>) -> Self {
        if hyprland_signature.is_some_and(|signature| !signature.is_empty()) {
            return Self::Hyprland;
        }
        let current_desktop = current_desktop.to_lowercase();
        if current_desktop.contains("kde") {
            Self::Kde
        } else if current_desktop.contains("gnome") {
            Self::Gnome
        } else {
            Self::Other
        }
    }

    /// The desktop of the running session, read from the process environment.
    #[must_use]
    pub fn current() -> Self {
        Self::from_session(
            &std::env::var("XDG_CURRENT_DESKTOP").unwrap_or_default(),
            std::env::var("HYPRLAND_INSTANCE_SIGNATURE").ok().as_deref(),
        )
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
                Action::TypeText(_) | Action::RunAppleScript(_) => false,
                // GNOME's Super / Super+D and KWin's Overview / show-desktop.
                // Nothing else binds them the same way (KDE opens KRunner on a
                // bare Super), so they are not sent there.
                Action::MissionControl | Action::ShowDesktop => {
                    matches!(desktop, LinuxDesktop::Gnome | LinuxDesktop::Kde)
                }
                // Only KWin has a single-app window spread ("ExposeClass");
                // GNOME's Activities shows every app, the wrong scope.
                Action::AppExpose => desktop == LinuxDesktop::Kde,
                // GNOME's Super+A and Hyprland's launcher helper.
                Action::LaunchpadShow => {
                    matches!(desktop, LinuxDesktop::Gnome | LinuxDesktop::Hyprland)
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

impl WorkflowStep {
    /// Whether `host` can perform this step; a step it cannot is skipped
    /// with a warning when the workflow runs. Follows the matching
    /// [`Action::is_available_on`] row.
    #[must_use]
    pub fn is_available_on(&self, host: Host) -> bool {
        // An empty payload stands in for the step's own; availability never
        // depends on it, and an empty `String` does not allocate.
        let action = match self {
            WorkflowStep::TypeText(_) => Action::TypeText(String::new()),
            WorkflowStep::RunAppleScript(_) => Action::RunAppleScript(String::new()),
            WorkflowStep::PressKey(_)
            | WorkflowStep::Delay { .. }
            | WorkflowStep::RunShellCommand(_) => return true,
        };
        action.is_available_on(host)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const GNOME: Host = Host::Linux(LinuxDesktop::Gnome);
    const KDE: Host = Host::Linux(LinuxDesktop::Kde);
    const HYPRLAND: Host = Host::Linux(LinuxDesktop::Hyprland);
    const OTHER_LINUX: Host = Host::Linux(LinuxDesktop::Other);
    const ALL_HOSTS: [Host; 7] = [
        Host::MacOs,
        Host::Windows,
        GNOME,
        KDE,
        HYPRLAND,
        OTHER_LINUX,
        Host::Other,
    ];

    #[test]
    fn current_desktop_is_parsed_case_insensitively_from_the_list() {
        for (value, expected) in [
            ("GNOME", LinuxDesktop::Gnome),
            ("ubuntu:GNOME", LinuxDesktop::Gnome),
            ("GNOME-Classic:GNOME", LinuxDesktop::Gnome),
            ("gnome", LinuxDesktop::Gnome),
            ("KDE", LinuxDesktop::Kde),
            ("kde", LinuxDesktop::Kde),
            ("niri", LinuxDesktop::Other),
            ("X-Cinnamon", LinuxDesktop::Other),
            ("", LinuxDesktop::Other),
        ] {
            assert_eq!(
                LinuxDesktop::from_session(value, None),
                expected,
                "XDG_CURRENT_DESKTOP={value:?}"
            );
        }
    }

    #[test]
    fn a_hyprland_signature_wins_over_the_desktop_list() {
        for desktop in ["Hyprland", "KDE", "GNOME", ""] {
            assert_eq!(
                LinuxDesktop::from_session(desktop, Some("abc_123")),
                LinuxDesktop::Hyprland,
                "{desktop}"
            );
        }
        // The desktop list alone does not select Hyprland: the native table
        // keys on the signature, as `openlogi-inject` does.
        assert_eq!(
            LinuxDesktop::from_session("Hyprland", None),
            LinuxDesktop::Other
        );
        assert_eq!(
            LinuxDesktop::from_session("KDE", Some("")),
            LinuxDesktop::Kde
        );
    }

    #[test]
    fn macos_performs_every_catalog_action() {
        for action in Action::catalog() {
            assert!(action.is_available_on(Host::MacOs), "{action:?}");
        }
    }

    /// The Linux window-manager rows, one column per desktop, mirroring the
    /// native tables in `openlogi-inject`'s Linux backend.
    #[test]
    fn linux_window_manager_actions_follow_each_desktops_native_table() {
        //            action                  GNOME  KDE    Hyprland Other
        let rows = [
            (Action::MissionControl, [true, true, false, false]),
            (Action::AppExpose, [false, true, false, false]),
            (Action::ShowDesktop, [true, true, false, false]),
            (Action::LaunchpadShow, [true, false, true, false]),
            (Action::PreviousDesktop, [true, true, true, true]),
            (Action::NextDesktop, [true, true, true, true]),
        ];
        for (action, expected) in rows {
            for (host, available) in [GNOME, KDE, HYPRLAND, OTHER_LINUX]
                .into_iter()
                .zip(expected)
            {
                assert_eq!(
                    action.is_available_on(host),
                    available,
                    "{action:?} on {host:?}"
                );
            }
        }
    }

    #[test]
    fn text_and_applescript_need_macos() {
        for action in [
            Action::TypeText("hi".into()),
            Action::RunAppleScript("beep".into()),
        ] {
            for host in [Host::Windows, GNOME, KDE, HYPRLAND, OTHER_LINUX] {
                assert!(!action.is_available_on(host), "{action:?} on {host:?}");
            }
        }
        assert!(Action::RunShellCommand("true".into()).is_available_on(OTHER_LINUX));
    }

    #[test]
    fn workflow_steps_follow_their_action_rows() {
        let combo: crate::binding::KeyCombo = "Ctrl+A".parse().expect("Ctrl+A is a valid combo");
        for host in ALL_HOSTS {
            assert_eq!(
                WorkflowStep::TypeText("hi".into()).is_available_on(host),
                Action::TypeText("hi".into()).is_available_on(host),
                "{host:?}"
            );
            assert_eq!(
                WorkflowStep::RunAppleScript("beep".into()).is_available_on(host),
                Action::RunAppleScript("beep".into()).is_available_on(host),
                "{host:?}"
            );
            assert!(WorkflowStep::PressKey(combo.clone()).is_available_on(host));
            assert!(WorkflowStep::Delay { millis: 10 }.is_available_on(host));
            assert!(WorkflowStep::RunShellCommand("true".into()).is_available_on(host));
        }
    }

    #[test]
    fn sleep_has_no_windows_backend() {
        assert!(!Action::Sleep.is_available_on(Host::Windows));
        assert!(Action::Sleep.is_available_on(OTHER_LINUX));
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
    fn linux_relabels_follow_the_desktop() {
        assert_eq!(
            Action::MissionControl.translation_key_on(GNOME),
            Some("actions.activities_overview")
        );
        assert_eq!(
            Action::MissionControl.translation_key_on(KDE),
            Some("actions.mission_control")
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
