//! Detection of the running compositor / desktop environment.
//!
//! Used to apply OS-level settings (like pointer speed) via the appropriate
//! compositor-specific command.

/// The compositor or desktop environment detected at runtime.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Compositor {
    /// Hyprland compositor.
    Hyprland,
    /// Sway compositor.
    Sway,
    /// GNOME desktop environment.
    Gnome,
    /// KDE Plasma desktop environment.
    Kde,
    /// X11 display server.
    X11,
    /// Unknown or unsupported compositor.
    Unknown,
}

impl Compositor {
    /// Detect the running compositor from environment variables.
    #[must_use]
    pub fn detect() -> Self {
        if std::env::var_os("WAYLAND_DISPLAY").is_some() {
            if let Ok(desktop) = std::env::var("XDG_CURRENT_DESKTOP") {
                let desktop = desktop.to_lowercase();
                if desktop.contains("hyprland") {
                    return Compositor::Hyprland;
                }
                if desktop.contains("sway") {
                    return Compositor::Sway;
                }
                if desktop.contains("gnome") {
                    return Compositor::Gnome;
                }
                if desktop.contains("kde") || desktop.contains("plasma") {
                    return Compositor::Kde;
                }
            }
            if std::env::var_os("HYPRLAND_INSTANCE_SIGNATURE").is_some() {
                return Compositor::Hyprland;
            }
            if std::env::var_os("SWAYSOCK").is_some() {
                return Compositor::Sway;
            }
            return Compositor::Unknown;
        }
        if std::env::var_os("DISPLAY").is_some() {
            return Compositor::X11;
        }
        Compositor::Unknown
    }

    /// The command to apply pointer speed for this compositor.
    /// Returns None if no command is known.
    #[must_use]
    pub fn pointer_speed_command(&self, sensitivity: f32) -> Option<Vec<String>> {
        match self {
            // Hyprland ships two config parsers and only one accepts a runtime
            // set per install: `hyprctl keyword` (legacy) is rejected outright
            // by the Lua parser ("keyword can't work with non-legacy
            // parsers"), where `hyprctl eval` runs Lua — but eval does not
            // exist upstream. Both fail with exit 0, so the fallback cannot be
            // decided from the exit code; run both and let each install honor
            // the one it understands. `hl.config` merges, so the eval only
            // touches `sensitivity`.
            Compositor::Hyprland => Some(vec![
                "sh".to_string(),
                "-c".to_string(),
                format!(
                    "hyprctl eval \"hl.config({{input={{sensitivity={sensitivity}}}}})\"; \
                     hyprctl keyword input:sensitivity {sensitivity}"
                ),
            ]),
            Compositor::Sway => Some(vec![
                "swaymsg".to_string(),
                "input".to_string(),
                "*".to_string(),
                "pointer_accel".to_string(),
                sensitivity.to_string(),
            ]),
            Compositor::Gnome => Some(vec![
                "gsettings".to_string(),
                "set".to_string(),
                "org.gnome.desktop.peripherals.mouse".to_string(),
                "speed".to_string(),
                sensitivity.to_string(),
            ]),
            Compositor::X11 => Some(vec![
                "sh".to_string(),
                "-c".to_string(),
                format!(
                    "dev=$(xinput list --name-only | grep -iE 'mouse|touchpad|pointer' | head -1); \
                     [ -n \"$dev\" ] && xinput --set-prop \"$dev\" 'libinput Accel Speed' {}",
                    sensitivity
                ),
            ]),
            _ => None,
        }
    }
}

/// Generate the compositor command for a given sensitivity value.
/// Returns None if the compositor is unknown or doesn't support pointer speed.
#[must_use]
pub fn apply_pointer_speed(compositor: Compositor, sensitivity: f32) -> Option<Vec<String>> {
    compositor.pointer_speed_command(sensitivity)
}

#[cfg(test)]
mod apply_tests {
    use super::*;

    #[test]
    fn hyprland_generates_correct_command() {
        let cmd = apply_pointer_speed(Compositor::Hyprland, -0.2).unwrap();
        let (shell, script) = (cmd[0].as_str(), cmd[1].as_str());
        assert_eq!((shell, cmd.len()), ("sh", 3));
        assert_eq!(script, "-c");
        // The Lua-parser runtime set and the legacy keyword both go out; each
        // Hyprland install honors the one its config parser understands.
        assert_eq!(
            cmd[2],
            "hyprctl eval \"hl.config({input={sensitivity=-0.2}})\"; \
             hyprctl keyword input:sensitivity -0.2"
        );
    }

    #[test]
    fn unknown_returns_none() {
        assert!(apply_pointer_speed(Compositor::Unknown, 0.5).is_none());
    }

    #[test]
    fn all_known_compositors_have_commands() {
        for c in [
            Compositor::Hyprland,
            Compositor::Sway,
            Compositor::Gnome,
            Compositor::X11,
        ] {
            assert!(
                c.pointer_speed_command(0.0).is_some(),
                "{c:?} has no command"
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detect_does_not_panic() {
        let c = Compositor::detect();
        let _ = c;
    }

    #[test]
    fn hyprland_command_is_some() {
        let cmd = Compositor::Hyprland.pointer_speed_command(0.5);
        assert!(cmd.is_some());
        let cmd = cmd.unwrap();
        assert_eq!((cmd[0].as_str(), cmd[1].as_str()), ("sh", "-c"));
        assert!(cmd[2].contains("sensitivity=0.5"));
    }

    #[test]
    fn unknown_command_is_none() {
        let cmd = Compositor::Unknown.pointer_speed_command(0.5);
        assert!(cmd.is_none());
    }
}
