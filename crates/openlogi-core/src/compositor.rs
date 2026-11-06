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
    pub fn pointer_speed_command(&self, sensitivity: f32) -> Option<Vec<String>> {
        match self {
            Compositor::Hyprland => Some(vec![
                "hyprctl".to_string(),
                "keyword".to_string(),
                "input:sensitivity".to_string(),
                sensitivity.to_string(),
            ]),
            Compositor::Sway => Some(vec![
                "swaymsg".to_string(),
                "input".to_string(),
                "*".to_string(),
                "accel_profile".to_string(),
                "adaptive".to_string(),
            ]),
            Compositor::Gnome => Some(vec![
                "gsettings".to_string(),
                "set".to_string(),
                "org.gnome.desktop.peripherals.mouse".to_string(),
                "speed".to_string(),
                sensitivity.to_string(),
            ]),
            Compositor::X11 => Some(vec![
                "xinput".to_string(),
                "--set-prop".to_string(),
                "*".to_string(),
                "libinput Accel Speed".to_string(),
                sensitivity.to_string(),
            ]),
            _ => None,
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
        assert_eq!(cmd[0], "hyprctl");
    }

    #[test]
    fn unknown_command_is_none() {
        let cmd = Compositor::Unknown.pointer_speed_command(0.5);
        assert!(cmd.is_none());
    }
}