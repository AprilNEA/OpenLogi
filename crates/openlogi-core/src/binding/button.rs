//! Rebindable mouse/keyboard button identifiers.

use std::borrow::Cow;
use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Deserializer, Serialize, Serializer, de};
use thiserror::Error;

use super::control::Cid;

/// One of the user-rebindable controls on a Logi device: a mouse hotspot, or
/// a keyboard key addressed by its HID++ `0x1b04` control ID.
///
/// The mouse variants are declared in physical front-to-side order;
/// [`ButtonId::ALL`] is consumed by the default-binding generator and the
/// popover trigger list. Keyboard keys are [`ButtonId::Control`] and stay out
/// of that list: they seed no default and are never diverted until bound.
///
/// # Serialization
///
/// Persisted as a string — the variant name for a mouse button
/// (`"MiddleClick"`), the catalog name for a known control
/// (`"KeyScreenCapture"`) or `"control:0x0141"` for any other — so a
/// `ButtonId` can key a TOML table. See [`FromStr`](Self::from_str).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum ButtonId {
    /// The primary button. Rebindable in the config schema, but the OS hook
    /// never suppresses it — see [`ButtonId::is_os_hook_button`].
    LeftClick,
    /// The secondary button. Like [`ButtonId::LeftClick`], it always passes
    /// through the OS hook.
    RightClick,
    /// The wheel click — one of the three buttons the OS hook remaps.
    MiddleClick,
    /// The thumb-side "back" button (mouse button 4), remapped by the OS hook.
    Back,
    /// The thumb-side "forward" button (mouse button 5), remapped by the OS hook.
    Forward,
    /// The "ModeShift" button under the wheel — typically used for SmartShift /
    /// DPI cycle. Named `DpiToggle` for historical reasons.
    DpiToggle,
    /// The horizontal thumb wheel's click. Kept in [`ButtonId::ALL`] so its
    /// default still seeds and dispatches when the wheel is diverted, even
    /// though the mouse model surfaces one paired rotation control instead of
    /// the click (see `mouse_model::geometry`).
    Thumbwheel,
    /// Rotating the thumb wheel "up" (positive rotation). Bound, by default, to
    /// continuous horizontal scroll; see the agent-core `watchers`-side dispatch.
    ThumbwheelScrollUp,
    /// Rotating the thumb wheel "down" (negative rotation).
    ThumbwheelScrollDown,
    /// The HID++ gesture button on MX-line devices. The press itself
    /// fires the bound action; swipe directions are P1.5 territory.
    GestureButton,
    /// The MX Master 4 Haptic Sense Panel — the touch-sensitive thumb rest
    /// (Logi metadata slot `ASSIGNMENT_NAME_SHOW_RADIAL_MENU`, HID++ CID
    /// `0x01a0`). A separate physical control from [`ButtonId::GestureButton`];
    /// captured over HID++ like it, and eligible as the gesture owner.
    HapticPanel,
    /// Tilting the main wheel left — `0x1b04` CID `0x005b` ("Left Scroll"),
    /// Logi metadata slot `SLOT_NAME_LEFT_SCROLL_BUTTON`. A distinct control
    /// from the thumb wheel: it is a plain divertable button, not a rotation,
    /// and it lives on the main wheel of mice like the MX Anywhere 2S.
    WheelTiltLeft,
    /// Tilting the main wheel right — `0x1b04` CID `0x005d` ("Right Scroll"),
    /// Logi metadata slot `SLOT_NAME_RIGHT_SCROLL_BUTTON`. Counterpart to
    /// [`ButtonId::WheelTiltLeft`].
    WheelTiltRight,
    /// G-series button G6.
    G6,
    /// G-series button G7.
    G7,
    /// G-series button G8.
    G8,
    /// G-series button G9.
    G9,
    /// A keyboard key, by the HID++ `0x1b04` control it reports as. Diverted
    /// over HID++ while bound (the OS hook never sees these keys); left on its
    /// native firmware function otherwise.
    ///
    /// Declared last so the derived ordering keeps every mouse button ahead of
    /// the keys; among keys the order is by control ID, so a GUI that wants
    /// physical F-row order sorts by catalog position instead.
    Control(Cid),
}

impl ButtonId {
    /// Every rebindable mouse button in declaration (physical front-to-side)
    /// order — the iteration source for default-binding seeding and the
    /// popover trigger list. Keyboard controls are deliberately absent: an
    /// unbound key is never diverted, so there is nothing to seed.
    pub const ALL: [ButtonId; 13] = [
        ButtonId::LeftClick,
        ButtonId::RightClick,
        ButtonId::MiddleClick,
        ButtonId::WheelTiltLeft,
        ButtonId::WheelTiltRight,
        ButtonId::Back,
        ButtonId::Forward,
        ButtonId::DpiToggle,
        ButtonId::Thumbwheel,
        ButtonId::ThumbwheelScrollUp,
        ButtonId::ThumbwheelScrollDown,
        ButtonId::GestureButton,
        ButtonId::HapticPanel,
    ];

    /// G-series buttons, kept out of [`Self::ALL`].
    pub const G_BUTTONS: [ButtonId; 4] = [ButtonId::G6, ButtonId::G7, ButtonId::G8, ButtonId::G9];

    /// The keyboard key bound to `0x1b04` control `cid`.
    #[must_use]
    pub const fn control(cid: u16) -> Self {
        Self::Control(Cid::new(cid))
    }

    /// The `0x1b04` control this button diverts as, for a keyboard key.
    #[must_use]
    pub const fn cid(self) -> Option<Cid> {
        match self {
            Self::Control(cid) => Some(cid),
            _ => None,
        }
    }

    /// Whether this button is one the OS hook (macOS `CGEventTap` / Linux evdev)
    /// remaps: Middle, Back, or Forward. The primary L/R clicks always pass
    /// through (suppressing them would brick the mouse), and the DPI / thumb /
    /// dedicated gesture controls aren't visible to the OS hook at all (they're
    /// captured over HID++). Stored gesture bindings also project through this
    /// set so Middle Click configurations written by v0.8.0 keep working;
    /// [`Self::supports_gesture_mode`] separately controls which buttons may be
    /// newly promoted by the UI.
    #[must_use]
    pub fn is_os_hook_button(self) -> bool {
        matches!(
            self,
            ButtonId::MiddleClick | ButtonId::Back | ButtonId::Forward
        )
    }

    /// Whether this button may use the OS hook's hold-and-swipe gesture path.
    /// Back and Forward are the only eligible controls: Middle Click belongs
    /// to the main wheel, whose controls intentionally remain single-action.
    #[must_use]
    pub fn is_os_hook_gesture_source(self) -> bool {
        matches!(self, ButtonId::Back | ButtonId::Forward)
    }

    /// Whether this button is a HID++ gesture source — a control that is
    /// captured over HID++ raw-XY diversion (never the OS hook) and can
    /// therefore own the gesture role with swipe directions: DPI/ModeShift,
    /// the dedicated gesture button, or the MX Master 4 haptic panel. The
    /// capture layer maps each to its control ID and checks the device's
    /// advertised raw-XY capability before arming it.
    #[must_use]
    pub fn is_hidpp_gesture_source(self) -> bool {
        matches!(
            self,
            ButtonId::DpiToggle | ButtonId::GestureButton | ButtonId::HapticPanel
        )
    }

    /// Whether OpenLogi offers gesture mode for this logical control. Wheel
    /// controls, the primary clicks and keyboard keys stay single-action by
    /// product policy; device-specific HID++ capability checks may further
    /// narrow this set at capture time.
    #[must_use]
    pub fn supports_gesture_mode(self) -> bool {
        self.is_os_hook_gesture_source() || self.is_hidpp_gesture_source()
    }

    /// The persisted name of every mouse button. Both directions of the
    /// string form read this one table; `unit_name` is a match without a
    /// wildcard over the unit variants, so a new variant that is missing here
    /// fails to compile rather than failing to parse.
    const UNIT_NAMES: [(ButtonId, &'static str); 17] = [
        (ButtonId::LeftClick, "LeftClick"),
        (ButtonId::RightClick, "RightClick"),
        (ButtonId::MiddleClick, "MiddleClick"),
        (ButtonId::Back, "Back"),
        (ButtonId::Forward, "Forward"),
        (ButtonId::DpiToggle, "DpiToggle"),
        (ButtonId::Thumbwheel, "Thumbwheel"),
        (ButtonId::ThumbwheelScrollUp, "ThumbwheelScrollUp"),
        (ButtonId::ThumbwheelScrollDown, "ThumbwheelScrollDown"),
        (ButtonId::GestureButton, "GestureButton"),
        (ButtonId::HapticPanel, "HapticPanel"),
        (ButtonId::WheelTiltLeft, "WheelTiltLeft"),
        (ButtonId::WheelTiltRight, "WheelTiltRight"),
        (ButtonId::G6, "G6"),
        (ButtonId::G7, "G7"),
        (ButtonId::G8, "G8"),
        (ButtonId::G9, "G9"),
    ];

    /// The name a mouse button is persisted under. Keyboard controls have no
    /// fixed name here; see [`Cid::config_name`].
    const fn unit_name(self) -> Option<&'static str> {
        Some(match self {
            ButtonId::LeftClick => Self::UNIT_NAMES[0].1,
            ButtonId::RightClick => Self::UNIT_NAMES[1].1,
            ButtonId::MiddleClick => Self::UNIT_NAMES[2].1,
            ButtonId::Back => Self::UNIT_NAMES[3].1,
            ButtonId::Forward => Self::UNIT_NAMES[4].1,
            ButtonId::DpiToggle => Self::UNIT_NAMES[5].1,
            ButtonId::Thumbwheel => Self::UNIT_NAMES[6].1,
            ButtonId::ThumbwheelScrollUp => Self::UNIT_NAMES[7].1,
            ButtonId::ThumbwheelScrollDown => Self::UNIT_NAMES[8].1,
            ButtonId::GestureButton => Self::UNIT_NAMES[9].1,
            ButtonId::HapticPanel => Self::UNIT_NAMES[10].1,
            ButtonId::WheelTiltLeft => Self::UNIT_NAMES[11].1,
            ButtonId::WheelTiltRight => Self::UNIT_NAMES[12].1,
            ButtonId::G6 => Self::UNIT_NAMES[13].1,
            ButtonId::G7 => Self::UNIT_NAMES[14].1,
            ButtonId::G8 => Self::UNIT_NAMES[15].1,
            ButtonId::G9 => Self::UNIT_NAMES[16].1,
            ButtonId::Control(_) => return None,
        })
    }

    /// The name this button is written with in `config.toml`.
    #[must_use]
    pub fn config_name(self) -> Cow<'static, str> {
        match self {
            ButtonId::Control(cid) => cid.config_name(),
            other => Cow::Borrowed(other.unit_name().unwrap_or_default()),
        }
    }

    /// Human-readable label for popovers and tooltips.
    #[must_use]
    pub fn label(self) -> Cow<'static, str> {
        Cow::Borrowed(match self {
            ButtonId::LeftClick => "Left Click",
            ButtonId::RightClick => "Right Click",
            ButtonId::MiddleClick => "Middle Click",
            ButtonId::WheelTiltLeft => "Tilt Left",
            ButtonId::WheelTiltRight => "Tilt Right",
            ButtonId::Back => "Back",
            ButtonId::Forward => "Forward",
            ButtonId::DpiToggle => "DPI Toggle",
            ButtonId::Thumbwheel => "Thumb Wheel",
            ButtonId::ThumbwheelScrollUp => "Thumb Wheel Up",
            ButtonId::ThumbwheelScrollDown => "Thumb Wheel Down",
            ButtonId::GestureButton => "Gesture Button",
            ButtonId::HapticPanel => "Haptic Panel",
            ButtonId::G6 => "G6",
            ButtonId::G7 => "G7",
            ButtonId::G8 => "G8",
            ButtonId::G9 => "G9",
            ButtonId::Control(cid) => return cid.label(),
        })
    }

    /// Stable catalog key for the localized button label.
    #[must_use]
    pub fn translation_key(self) -> &'static str {
        match self {
            ButtonId::LeftClick => "actions.left_click",
            ButtonId::RightClick => "actions.right_click",
            ButtonId::MiddleClick => "actions.middle_click",
            ButtonId::WheelTiltLeft => "actions.tilt_left",
            ButtonId::WheelTiltRight => "actions.tilt_right",
            ButtonId::Back => "actions.back",
            ButtonId::Forward => "actions.forward",
            ButtonId::DpiToggle => "actions.dpi_toggle",
            ButtonId::Thumbwheel => "pointer.thumb_wheel",
            ButtonId::ThumbwheelScrollUp => "pointer.thumb_wheel_up",
            ButtonId::ThumbwheelScrollDown => "pointer.thumb_wheel_down",
            ButtonId::GestureButton => "actions.gesture_button",
            ButtonId::HapticPanel => "actions.haptic_panel",
            ButtonId::G6 => "actions.g6",
            ButtonId::G7 => "actions.g7",
            ButtonId::G8 => "actions.g8",
            ButtonId::G9 => "actions.g9",
            ButtonId::Control(cid) => cid.translation_key(),
        }
    }
}

impl fmt::Display for ButtonId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.label())
    }
}

/// A persisted button name that names no button.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("unknown button `{0}`")]
pub struct ParseButtonIdError(pub String);

impl FromStr for ButtonId {
    type Err = ParseButtonIdError;

    /// The inverse of [`ButtonId::config_name`]: a mouse button's variant
    /// name, a known control's catalog name, or `control:0x…`.
    fn from_str(name: &str) -> Result<Self, Self::Err> {
        ButtonId::UNIT_NAMES
            .into_iter()
            .find_map(|(button, unit_name)| (unit_name == name).then_some(button))
            .or_else(|| Cid::from_config_name(name).map(ButtonId::Control))
            .ok_or_else(|| ParseButtonIdError(name.to_owned()))
    }
}

impl Serialize for ButtonId {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.config_name())
    }
}

impl<'de> Deserialize<'de> for ButtonId {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        String::deserialize(deserializer)?
            .parse()
            .map_err(de::Error::custom)
    }
}
