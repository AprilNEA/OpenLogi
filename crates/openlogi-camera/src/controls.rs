//! Platform-independent control vocabulary shared by every UVC backend
//! (IOKit on macOS, DirectShow on Windows, stubs elsewhere).

use thiserror::Error;

/// One adjustable camera control, mapped to a UVC selector by each backend.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CameraControl {
    Zoom,
    Focus,
    Exposure,
    PowerLineFrequency,
    LowLightCompensation,
    Brightness,
    Contrast,
    Saturation,
    Sharpness,
    WhiteBalance,
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
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AutoToggle {
    Focus,
    Exposure,
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
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AutoState {
    pub current: bool,
    pub default: bool,
}

/// Everything the controls UI needs, read in a single device-open: each
/// supported control's range and each supported auto toggle's state.
#[derive(Debug, Clone, Default)]
pub struct CameraState {
    pub controls: Vec<(CameraControl, ControlRange)>,
    pub autos: Vec<(AutoToggle, AutoState)>,
}

/// The device's reported range and current value for a control.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ControlRange {
    pub min: i32,
    pub max: i32,
    pub default: i32,
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
    use super::ControlRange;

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
}

/// Why a UVC control operation failed.
#[derive(Debug, Clone, Error)]
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
}
