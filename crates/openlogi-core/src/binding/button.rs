//! Rebindable mouse/keyboard button identifiers.

use std::fmt;

use serde::{Deserialize, Serialize};

/// One of the user-rebindable hotspots on a Logi mouse. The order matches the
/// physical layout from front to side; [`ButtonId::ALL`] is consumed by the
/// default-binding generator and the popover trigger list.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
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
    /// Keyboard F-row "Search" control (`0x1b04` CID `0x00d4`,
    /// `MultiPlatform_Search`) — F4 on the Signature series.
    KeySearch,
    /// Keyboard "Dictation" control (CID `0x0103`) — F5 on the Signature series.
    KeyDictation,
    /// Keyboard "Emoji" control (CID `0x0108`) — F6 on the Signature series.
    KeyEmoji,
    /// Keyboard "Screen Capture" control (CID `0x010a`) — F7 on the Signature
    /// series.
    KeyScreenCapture,
    /// Keyboard "Mute Microphone" control (CID `0x011c`) — F8 on the Signature
    /// series.
    KeyMicMute,
    /// Keyboard "Play/Pause" control (CID `0x00e5`) — F9 on the Signature series.
    KeyPlayPause,
    /// Keyboard "Mute" control (CID `0x00e7`) — F10 on the Signature series.
    KeyMute,
    /// Keyboard "Volume Down" control (CID `0x00e8`) — F11 on the Signature
    /// series.
    KeyVolumeDown,
    /// Keyboard "Volume Up" control (CID `0x00e9`) — F12 on the Signature
    /// series.
    KeyVolumeUp,
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
    /// G502 X Plus G6 — hold-to-shift ("sniper") DPI. Captured over HID++
    /// `0x8110` spy events, not the OS hook. Kept out of [`ButtonId::ALL`] so
    /// MX popovers stay clean; seeded only for [`G502_X_PLUS_CONFIG_KEY`].
    DpiShift,
    /// G502 X Plus G8 — DPI up. Spy-captured like [`ButtonId::DpiShift`].
    DpiUp,
    /// G502 X Plus G7 — DPI down. Spy-captured like [`ButtonId::DpiShift`].
    DpiDown,
    /// G502 X Plus G9 — onboard profile cycle. Spy-captured like
    /// [`ButtonId::DpiShift`].
    ///
    /// Declared last: the TOML config and any serialized form encode the
    /// variant identifier / index, so new buttons are append-only.
    ProfileCycle,
}

/// Lightspeed / wired G502 X Plus (`wpid=4099`). The `config_key` is
/// `format!("{:x}{:04x}", extended_model_id, model_ids[0])` with ext = 0.
pub const G502_X_PLUS_CONFIG_KEY: &str = "04099";

impl ButtonId {
    /// Every rebindable button in declaration (physical front-to-side) order —
    /// the iteration source for default-binding seeding and the popover
    /// trigger list.
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

    /// Extra G502 X Plus buttons captured through `0x8110` Mouse Button Spy.
    /// Kept out of [`ButtonId::ALL`] so MX-line popovers and default seeding
    /// stay unchanged. [`crate::bindings::button_bindings_for`] seeds these
    /// only for a [`super::SpyModel`] row — today [`G502_X_PLUS_CONFIG_KEY`],
    /// either passed directly or recovered from the device's persisted identity.
    pub const SPY_BUTTONS: [ButtonId; 4] = [
        ButtonId::DpiShift,
        ButtonId::DpiUp,
        ButtonId::DpiDown,
        ButtonId::ProfileCycle,
    ];

    /// Extra spy-remappable buttons for this HID++ model id, if it has a
    /// locked [`super::SpyModel`] row. Used by the GUI to decide whether DPI
    /// Up/Down and the other extras exist — MX and undocumented cousins get
    /// [`None`].
    #[must_use]
    pub fn spy_buttons_for_config_key(key: &str) -> Option<&'static [ButtonId]> {
        super::spy::SpyModel::for_hidpp_key(key).map(|model| model.extra_buttons)
    }

    /// The divertable keyboard F-row controls, in F-row order. Kept out of
    /// [`ButtonId::ALL`]: that array seeds mouse defaults and the mouse
    /// popover trigger list, while keyboard keys stay native unless the user
    /// binds them (an unbound key is never diverted).
    pub const KEYBOARD_KEYS: [ButtonId; 9] = [
        ButtonId::KeySearch,
        ButtonId::KeyDictation,
        ButtonId::KeyEmoji,
        ButtonId::KeyScreenCapture,
        ButtonId::KeyMicMute,
        ButtonId::KeyPlayPause,
        ButtonId::KeyMute,
        ButtonId::KeyVolumeDown,
        ButtonId::KeyVolumeUp,
    ];

    /// Whether this button is one the OS hook (macOS `CGEventTap` / Linux evdev)
    /// remaps: Middle, Back, or Forward. The primary L/R clicks always pass
    /// through (suppressing them would brick the mouse), and the DPI / thumb /
    /// dedicated gesture controls aren't visible to the OS hook at all (they're
    /// captured over HID++). These are exactly the buttons that can become an
    /// OS-hook gesture button, so the hook's remap gate and the gesture-owner
    /// projection share this one definition.
    #[must_use]
    pub fn is_os_hook_button(self) -> bool {
        matches!(
            self,
            ButtonId::MiddleClick | ButtonId::Back | ButtonId::Forward
        )
    }

    /// Whether this button is a HID++ gesture source — a control that is
    /// captured over HID++ raw-XY diversion (never the OS hook) and can
    /// therefore own the gesture role with swipe directions: the dedicated
    /// gesture button, or the MX Master 4 haptic panel. The capture layer maps
    /// each to its control ID.
    #[must_use]
    pub fn is_hidpp_gesture_source(self) -> bool {
        matches!(self, ButtonId::GestureButton | ButtonId::HapticPanel)
    }

    /// Human-readable label for popovers and tooltips.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
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
            ButtonId::KeySearch => "Search Key",
            ButtonId::KeyDictation => "Dictation Key",
            ButtonId::KeyEmoji => "Emoji Key",
            ButtonId::KeyScreenCapture => "Screen Capture Key",
            ButtonId::KeyMicMute => "Mic Mute Key",
            ButtonId::KeyPlayPause => "Play/Pause Key",
            ButtonId::KeyMute => "Mute Key",
            ButtonId::KeyVolumeDown => "Volume Down Key",
            ButtonId::KeyVolumeUp => "Volume Up Key",
            ButtonId::HapticPanel => "Haptic Panel",
            ButtonId::DpiShift => "DPI Shift",
            ButtonId::DpiUp => "DPI Up",
            ButtonId::DpiDown => "DPI Down",
            ButtonId::ProfileCycle => "Profile Cycle",
        }
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
            ButtonId::KeySearch => "keyboard.search_key",
            ButtonId::KeyDictation => "keyboard.dictation_key",
            ButtonId::KeyEmoji => "keyboard.emoji_key",
            ButtonId::KeyScreenCapture => "keyboard.screen_capture_key",
            ButtonId::KeyMicMute => "keyboard.mic_mute_key",
            ButtonId::KeyPlayPause => "keyboard.play_pause_key",
            ButtonId::KeyMute => "keyboard.mute_key",
            ButtonId::KeyVolumeDown => "keyboard.volume_down_key",
            ButtonId::KeyVolumeUp => "keyboard.volume_up_key",
            ButtonId::HapticPanel => "actions.haptic_panel",
            ButtonId::DpiShift => "actions.dpi_shift",
            ButtonId::DpiUp => "actions.dpi_up",
            ButtonId::DpiDown => "actions.dpi_down",
            ButtonId::ProfileCycle => "actions.profile_cycle",
        }
    }
}

impl fmt::Display for ButtonId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.label())
    }
}
