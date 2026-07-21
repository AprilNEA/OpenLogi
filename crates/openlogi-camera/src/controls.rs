//! Platform-independent control vocabulary shared by every UVC backend
//! (IOKit on macOS, DirectShow on Windows, stubs elsewhere).

/// One adjustable camera control, mapped to a UVC selector by each backend.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CameraControl {
    Zoom,
    Focus,
    Exposure,
    Brightness,
    Contrast,
    Saturation,
    Sharpness,
    WhiteBalance,
    Tint,
}

impl CameraControl {
    /// Every control, in the order the UI lists them (lens first, then image).
    pub const ALL: [Self; 9] = [
        Self::Zoom,
        Self::Focus,
        Self::Exposure,
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
}

/// Why a UVC control operation failed.
#[derive(Debug, Clone)]
pub enum ControlError {
    /// No matching camera device (or it exposes no controllable unit).
    NotFound,
    /// The selected camera can't be uniquely identified: its unique id didn't
    /// resolve to a USB location and more than one Logitech camera is attached,
    /// so a write could hit the wrong device. Fails closed instead of guessing.
    Ambiguous,
    /// The camera rejected or didn't support the control — or the platform
    /// has no UVC control backend at all.
    Unsupported,
    /// A platform API call failed (open, bind, or the control transfer).
    Io(String),
}

impl std::fmt::Display for ControlError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotFound => write!(f, "no matching UVC device"),
            Self::Ambiguous => write!(f, "camera could not be uniquely identified"),
            Self::Unsupported => write!(f, "camera does not support that control"),
            Self::Io(s) => write!(f, "platform error: {s}"),
        }
    }
}

impl std::error::Error for ControlError {}
