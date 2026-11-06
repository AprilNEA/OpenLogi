//! OS-level pointer speed application.
//!
//! Applies a pointer speed factor at the compositor level, like Logi Options+
//! does on macOS. This is independent of HID++ device support.

use openlogi_core::compositor::Compositor;
use std::process::Command;

/// Apply pointer speed to the OS compositor.
/// Returns Ok(()) if the command was executed successfully, or an error description.
pub fn apply(compositor: Compositor, sensitivity: f32) -> Result<(), String> {
    let cmd = compositor.pointer_speed_command(sensitivity);
    let Some(cmd) = cmd else {
        return Err(format!("unsupported compositor: {:?}", compositor));
    };
    if cmd.is_empty() {
        return Err("empty command".to_string());
    }
    let output = Command::new(&cmd[0])
        .args(&cmd[1..])
        .output()
        .map_err(|e| format!("failed to run {}: {}", cmd[0], e))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("{} failed: {}", cmd[0], stderr.trim()));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use openlogi_core::compositor::Compositor;

    #[test]
    fn unknown_compositor_returns_error() {
        let result = apply(Compositor::Unknown, 0.5);
        assert!(result.is_err());
    }

    #[test]
    fn apply_hyprland_command_structure() {
        // We can't actually run hyprctl in tests, but we can verify
        // the command is generated correctly
        let cmd = Compositor::Hyprland.pointer_speed_command(-0.2);
        assert!(cmd.is_some());
        let cmd = cmd.unwrap();
        assert_eq!(cmd[0], "hyprctl");
        assert_eq!(cmd[1], "keyword");
        assert_eq!(cmd[2], "input:sensitivity");
    }
}