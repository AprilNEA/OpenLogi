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
    /// Keyboard "Calculator" control (CID `0x000a`, firmware task
    /// `CALCULATOR`) — Logi metadata slot `SLOT_NAME_CALCULATOR`. Not an F-row
    /// key: it sits in the hotkey cluster above/right of the numpad on boards
    /// like the ERGO K860 and MX Keys S, and is absent from the Signature
    /// series.
    ///
    /// In the keyboard's macOS mode the firmware emits no HID usage at all for
    /// this key, so diversion is the only way to reach it — an OS-level hook
    /// has no event to intercept. Logitech documents the same constraint from
    /// the other side, listing the Calculator key as one that requires their
    /// software on macOS while working out of the box on Windows.
    ///
    KeyCalculator,
    /// Keyboard "Previous Track" control (CID `0x00e4`) — F7 on the ERGO K860
    /// and MX Keys.
    KeyPreviousTrack,
    /// Keyboard "Next Track" control (CID `0x00e6`) — F9 on the ERGO K860 and
    /// MX Keys.
    KeyNextTrack,
    /// Keyboard "App Contextual Menu / Right Click" control (CID `0x00ea`) —
    /// the menu key in the ERGO K860's top-right hotkey cluster. In the
    /// keyboard's macOS mode the firmware sends a right mouse click for it.
    KeyContextMenu,
    /// Keyboard "Screen Lock" control (CID `0x006f`) — the lock key in the ERGO
    /// K860's top-right hotkey cluster. In macOS mode the firmware sends
    /// Cmd+Ctrl+Q for it.
    KeyScreenLock,
    /// Keyboard "Show Desktop" control (CID `0x006e`) — F5 on the ERGO K860.
    KeyShowDesktop,
    /// Keyboard "Mission Control / Task View" control (CID `0x00e0`) — F3 on
    /// the ERGO K860.
    KeyTaskView,
    /// Keyboard "App Switch" control (CID `0x0100`, `Multiplatform_App_Switch`)
    /// — F4 on the ERGO K860.
    KeyAppSwitch,
    /// Keyboard "Brightness Down" control (CID `0x00c7`) — F1 on the ERGO K860
    /// and MX Keys.
    KeyBrightnessDown,
    /// Keyboard "Brightness Up" control (CID `0x00c8`) — F2 on the ERGO K860
    /// and MX Keys.
    ///
    /// Declared last: the TOML config and any serialized form encode the
    /// variant identifier / index, so new buttons are append-only.
    KeyBrightnessUp,
}

/// The divertable keyboard controls OpenLogi models, as
/// `(0x1b04 control ID, ButtonId)` pairs — the one table both the agent's
/// diversion and the settings app's key layout read. CID values match Logitech's control
/// catalog (cross-checked against Solaar's `special_keys.py`); the F-row
/// positions are the Signature-series layout, except the Calculator key, which
/// is a hotkey beside the numpad rather than an F-row key.
pub const KEYBOARD_KEY_CIDS: [(u16, ButtonId); 20] = [
    (0x00d4, ButtonId::KeySearch),
    (0x0103, ButtonId::KeyDictation),
    (0x0108, ButtonId::KeyEmoji),
    (0x010a, ButtonId::KeyScreenCapture),
    (0x011c, ButtonId::KeyMicMute),
    (0x00e5, ButtonId::KeyPlayPause),
    (0x00e7, ButtonId::KeyMute),
    (0x00e8, ButtonId::KeyVolumeDown),
    (0x00e9, ButtonId::KeyVolumeUp),
    // Last, matching the order of `ButtonId::KEYBOARD_KEYS`. In the keyboard's
    // macOS mode the firmware emits nothing for this control, so diversion is
    // the only way to reach it at all; in Windows mode it natively sends
    // consumer usage `0x0192` (`AL Calculator`), which diversion suppresses —
    // and, as for every key here, only once the user binds it.
    (0x000a, ButtonId::KeyCalculator),
    // ERGO K860 / MX Keys hotkeys, CIDs from the K860's own 0x1b04 table and
    // named per Solaar's `special_keys.py`. Print Screen is the same
    // screen-capture function as the Signature F7 under a different CID, so
    // it shares that button's binding.
    (0x00bf, ButtonId::KeyScreenCapture),
    (0x00e4, ButtonId::KeyPreviousTrack),
    (0x00e6, ButtonId::KeyNextTrack),
    (0x00ea, ButtonId::KeyContextMenu),
    (0x006f, ButtonId::KeyScreenLock),
    (0x006e, ButtonId::KeyShowDesktop),
    (0x00e0, ButtonId::KeyTaskView),
    (0x0100, ButtonId::KeyAppSwitch),
    (0x00c7, ButtonId::KeyBrightnessDown),
    (0x00c8, ButtonId::KeyBrightnessUp),
];

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

    /// The divertable keyboard F-row controls, in F-row order. Kept out of
    /// [`ButtonId::ALL`]: that array seeds mouse defaults and the mouse
    /// popover trigger list, while keyboard keys stay native unless the user
    /// binds them (an unbound key is never diverted).
    pub const KEYBOARD_KEYS: [ButtonId; 19] = [
        ButtonId::KeySearch,
        ButtonId::KeyDictation,
        ButtonId::KeyEmoji,
        ButtonId::KeyScreenCapture,
        ButtonId::KeyMicMute,
        ButtonId::KeyPlayPause,
        ButtonId::KeyMute,
        ButtonId::KeyVolumeDown,
        ButtonId::KeyVolumeUp,
        // Last: not part of the Signature F-row that seeded this list — the
        // Calculator key lives in the hotkey cluster beside the numpad. Kept in
        // the same order as [`KEYBOARD_KEY_CIDS`].
        ButtonId::KeyCalculator,
        // ERGO K860 / MX Keys controls absent from the Signature F-row.
        ButtonId::KeyPreviousTrack,
        ButtonId::KeyNextTrack,
        ButtonId::KeyContextMenu,
        ButtonId::KeyScreenLock,
        ButtonId::KeyShowDesktop,
        ButtonId::KeyTaskView,
        ButtonId::KeyAppSwitch,
        ButtonId::KeyBrightnessDown,
        ButtonId::KeyBrightnessUp,
    ];

    /// The keyboard key a `0x1b04` control ID drives, or `None` for a control
    /// OpenLogi does not model. Several controls may share one key.
    #[must_use]
    pub fn for_keyboard_control(cid: u16) -> Option<Self> {
        KEYBOARD_KEY_CIDS
            .iter()
            .find(|(control, _)| *control == cid)
            .map(|(_, key)| *key)
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
    /// controls and the primary clicks stay single-action by product policy;
    /// device-specific HID++ capability checks may further narrow this set at
    /// capture time.
    #[must_use]
    pub fn supports_gesture_mode(self) -> bool {
        self.is_os_hook_gesture_source() || self.is_hidpp_gesture_source()
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
            ButtonId::KeyCalculator => "Calculator Key",
            ButtonId::KeyPreviousTrack => "Previous Track Key",
            ButtonId::KeyNextTrack => "Next Track Key",
            ButtonId::KeyContextMenu => "Context Menu Key",
            ButtonId::KeyScreenLock => "Screen Lock Key",
            ButtonId::KeyShowDesktop => "Show Desktop Key",
            ButtonId::KeyTaskView => "Task View Key",
            ButtonId::KeyAppSwitch => "App Switch Key",
            ButtonId::KeyBrightnessDown => "Brightness Down Key",
            ButtonId::KeyBrightnessUp => "Brightness Up Key",
            ButtonId::HapticPanel => "Haptic Panel",
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
            ButtonId::KeyCalculator => "keyboard.calculator_key",
            ButtonId::KeyPreviousTrack => "keyboard.previous_track_key",
            ButtonId::KeyNextTrack => "keyboard.next_track_key",
            ButtonId::KeyContextMenu => "keyboard.context_menu_key",
            ButtonId::KeyScreenLock => "keyboard.screen_lock_key",
            ButtonId::KeyShowDesktop => "keyboard.show_desktop_key",
            ButtonId::KeyTaskView => "keyboard.task_view_key",
            ButtonId::KeyAppSwitch => "keyboard.app_switch_key",
            ButtonId::KeyBrightnessDown => "keyboard.brightness_down_key",
            ButtonId::KeyBrightnessUp => "keyboard.brightness_up_key",
            ButtonId::HapticPanel => "actions.haptic_panel",
        }
    }
}

impl fmt::Display for ButtonId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.label())
    }
}
