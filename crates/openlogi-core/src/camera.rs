//! Platform-independent control vocabulary shared by every UVC backend
//! (IOKit on macOS, DirectShow on Windows, stubs elsewhere).

use serde::{Deserialize, Serialize};
use thiserror::Error;

/// One adjustable camera control, mapped to a UVC selector by each backend.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CameraControl {
    /// Lens zoom in device units.
    Zoom,
    /// Manual lens focus.
    Focus,
    /// Manual sensor exposure.
    Exposure,
    /// Anti-flicker power-line frequency.
    PowerLineFrequency,
    /// Automatic low-light compensation.
    LowLightCompensation,
    /// Image brightness.
    Brightness,
    /// Image contrast.
    Contrast,
    /// Color saturation.
    Saturation,
    /// Image edge sharpness.
    Sharpness,
    /// Color temperature.
    WhiteBalance,
    /// Color tint.
    Tint,
}

impl CameraControl {
    /// Every control, in the order the UI lists them (lens first, then image).
    pub const ALL: [Self; 11] = [
        Self::Zoom,
        Self::Focus,
        Self::Exposure,
        Self::PowerLineFrequency,
        Self::LowLightCompensation,
        Self::Brightness,
        Self::Contrast,
        Self::Saturation,
        Self::Sharpness,
        Self::WhiteBalance,
        Self::Tint,
    ];

    /// Stable snake_case identifier used for config persistence and the CLI.
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::Zoom => "zoom",
            Self::Focus => "focus",
            Self::Exposure => "exposure",
            Self::PowerLineFrequency => "power_line_frequency",
            Self::LowLightCompensation => "low_light_compensation",
            Self::Brightness => "brightness",
            Self::Contrast => "contrast",
            Self::Saturation => "saturation",
            Self::Sharpness => "sharpness",
            Self::WhiteBalance => "white_balance",
            Self::Tint => "tint",
        }
    }

    /// The auto-mode toggle that gates this control, if the device has one.
    #[must_use]
    pub fn auto_toggle(self) -> Option<AutoToggle> {
        match self {
            Self::Focus => Some(AutoToggle::Focus),
            Self::Exposure => Some(AutoToggle::Exposure),
            Self::WhiteBalance => Some(AutoToggle::WhiteBalance),
            _ => None,
        }
    }
}

/// An auto-mode toggle paired with a manual control (focus / exposure / white
/// balance).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AutoToggle {
    /// Manual lens focus.
    Focus,
    /// Manual sensor exposure.
    Exposure,
    /// Color temperature.
    WhiteBalance,
}

impl AutoToggle {
    /// Every toggle, matching [`CameraControl::auto_toggle`] pairs.
    pub const ALL: [Self; 3] = [Self::Focus, Self::Exposure, Self::WhiteBalance];

    /// Stable snake_case identifier used for config persistence and the CLI.
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::Focus => "focus_auto",
            Self::Exposure => "exposure_auto",
            Self::WhiteBalance => "white_balance_auto",
        }
    }
}

/// One auto toggle's live and default state, read from the device.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct AutoState {
    /// Observed auto-mode state.
    pub current: bool,
    /// Device-reported default auto-mode state.
    pub default: bool,
}

/// Everything the controls UI needs, read in a single device-open: each
/// supported control's range and each supported auto toggle's state.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CameraState {
    /// Supported manual controls and measured ranges.
    pub controls: Vec<(CameraControl, ControlRange)>,
    /// Supported automatic controls and measured states.
    pub autos: Vec<(AutoToggle, AutoState)>,
}

/// One native device-open, with auto modes applied before manual values.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CameraCommand {
    /// Auto-mode changes.
    pub autos: Vec<(AutoToggle, bool)>,
    /// Manual control changes.
    pub values: Vec<(CameraControl, i32)>,
}

impl CameraCommand {
    /// Whether the observed device already satisfies the desired settings.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.autos.is_empty() && self.values.is_empty()
    }
}

impl CameraState {
    /// Compare persisted controls with measured support. Unknown controls remain inert.
    pub fn changes(
        &self,
        desired: &crate::config::CameraControls,
    ) -> Result<CameraCommand, ControlError> {
        let mut command = CameraCommand::default();
        for (toggle, state) in &self.autos {
            if let Some(value) = desired.0.get(toggle.name()) {
                let on = match value {
                    0 => false,
                    1 => true,
                    _ => {
                        return Err(ControlError::InvalidSettings(format!(
                            "{} requires 0 or 1",
                            toggle.name()
                        )));
                    }
                };
                if on != state.current {
                    command.autos.push((*toggle, on));
                }
            }
        }
        for (control, range) in &self.controls {
            let Some(value) = desired.0.get(control.name()) else {
                continue;
            };
            if !range.supports(*value) {
                return Err(ControlError::InvalidSettings(format!(
                    "{} does not support {value}",
                    control.name()
                )));
            }
            let auto = control.auto_toggle().and_then(|toggle| {
                self.autos
                    .iter()
                    .find(|(id, _)| *id == toggle)
                    .map(|(_, state)| {
                        desired
                            .0
                            .get(toggle.name())
                            .map_or(state.current, |v| *v == 1)
                    })
            });
            if auto != Some(true) && *value != range.current {
                command.values.push((*control, *value));
            }
        }
        Ok(command)
    }
}

/// The device's reported range and current value for a control.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ControlRange {
    /// Lowest supported value.
    pub min: i32,
    /// Highest supported value.
    pub max: i32,
    /// Device-reported reset value.
    pub default: i32,
    /// Value observed during the last read.
    pub current: i32,
    /// Bit `n` is set when discrete value `n` is supported. `None` means every
    /// value in the range is available.
    pub value_mask: Option<u32>,
}

impl ControlRange {
    /// Whether the device reports `value` as supported.
    #[must_use]
    pub fn supports(self, value: i32) -> bool {
        if !(self.min..=self.max).contains(&value) {
            return false;
        }
        self.value_mask.is_none_or(|mask| {
            u32::try_from(value)
                .ok()
                .filter(|value| *value < u32::BITS)
                .is_some_and(|value| mask & (1 << value) != 0)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::CameraControls;

    #[test]
    fn discrete_range_rejects_missing_values() {
        let range = ControlRange {
            min: 0,
            max: 3,
            default: 1,
            current: 1,
            value_mask: Some((1 << 1) | (1 << 3)),
        };

        assert!(range.supports(1));
        assert!(!range.supports(2));
        assert!(range.supports(3));
    }

    #[test]
    fn desired_camera_controls_respect_auto_modes_and_retain_unknown_settings() {
        let state = CameraState {
            controls: vec![(
                CameraControl::Focus,
                ControlRange {
                    min: 0,
                    max: 100,
                    default: 0,
                    current: 20,
                    value_mask: None,
                },
            )],
            autos: vec![(
                AutoToggle::Focus,
                AutoState {
                    current: true,
                    default: true,
                },
            )],
        };
        let mut desired = CameraControls(std::collections::BTreeMap::from([
            ("focus".into(), 50),
            ("future_control".into(), 500),
        ]));
        assert!(
            state.changes(&desired).unwrap().is_empty(),
            "manual settings are inert during autofocus"
        );
        desired.0.insert("focus_auto".into(), 0);
        assert_eq!(
            state.changes(&desired).unwrap(),
            CameraCommand {
                autos: vec![(AutoToggle::Focus, false)],
                values: vec![(CameraControl::Focus, 50)],
            }
        );
        assert_eq!(desired.0["future_control"], 500);
        desired.0.insert("focus".into(), 101);
        assert!(matches!(
            state.changes(&desired),
            Err(ControlError::InvalidSettings(_))
        ));
        desired.0.insert("focus".into(), 50);
        desired.0.insert("focus_auto".into(), 2);
        assert!(matches!(
            state.changes(&desired),
            Err(ControlError::InvalidSettings(_))
        ));
    }
}

/// Why a UVC control operation failed.
#[derive(Debug, Clone, Error, PartialEq, Eq, Serialize, Deserialize)]
pub enum ControlError {
    /// No matching camera device (or it exposes no controllable unit).
    #[error("no matching UVC device")]
    NotFound,
    /// The selected camera can't be uniquely identified: its unique id didn't
    /// resolve to a USB location and more than one Logitech camera is attached,
    /// so a write could hit the wrong device. Fails closed instead of guessing.
    #[error("camera could not be uniquely identified")]
    Ambiguous,
    /// The camera rejected or didn't support the control — or the platform
    /// has no UVC control backend at all.
    #[error("camera does not support that control")]
    Unsupported,
    /// A platform API call failed (open, bind, or the control transfer).
    #[error("platform error: {0}")]
    Io(String),
    /// A known setting violates the measured control contract.
    #[error("invalid camera settings: {0}")]
    InvalidSettings(String),
}

/// A connected USB Video Class camera.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Camera {
    /// Human-readable name, e.g. `"Logitech StreamCam"`.
    pub name: String,
    /// OS capture-layer identifier (AVFoundation `uniqueID`, DirectShow device
    /// path). Used to open preview/controls; may embed a USB location and so
    /// change when the camera is moved to another port.
    pub unique_id: String,
    /// USB `iSerialNumber` when the device reports one. Port-stable; preferred
    /// for persisted config keys via [`Self::config_key`].
    pub serial_number: Option<String>,
    /// USB vendor id (`0x046d` for Logitech).
    pub vendor_id: u16,
    /// USB product id (e.g. `0x0893` for the StreamCam).
    pub product_id: u16,
    /// Largest supported frame size `(width, height)`, when the OS reports the
    /// device's formats. Read from metadata only — no capture, no permission.
    pub max_resolution: Option<(u32, u32)>,
    /// Highest supported frame rate (fps) across all formats, when known.
    pub max_fps: Option<u32>,
}

impl Camera {
    /// Persistence key that is stable across USB ports.
    ///
    /// Prefers the USB serial when the device reports one. When it doesn't,
    /// falls back to a model-scoped key (`camera:vid:pid`) so settings survive
    /// a port change. Two serial-less units of the same model share this key
    /// (no stronger USB identity); the GUI keeps them as separate live cards
    /// via the OS capture id, not via this settings key.
    #[must_use]
    pub fn config_key(&self) -> String {
        if let Some(serial) = self
            .serial_number
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            format!(
                "camera:{:04x}:{:04x}:serial:{}",
                self.vendor_id,
                self.product_id,
                serial.to_ascii_lowercase()
            )
        } else {
            format!("camera:{:04x}:{:04x}", self.vendor_id, self.product_id)
        }
    }
}
