//! Named [`TaskId`] constants from the official `0x1b04` task-id
//! list (`x1b04_tasks_ids_list`).
//!
//! A task ID is the behaviour a control performs; it is assigned to a
//! [`ControlId`](super::ControlId) via the device's remapping features.

use super::TaskId;

/// `Switch presentation ( switch screen)` (`0x93`).
pub const SWITCH_PRESENTATION_SWITCH_SCREEN: TaskId = TaskId(0x93);

/// `Minimize window` (`0x94`).
pub const MINIMIZE_WINDOW: TaskId = TaskId(0x94);

/// `Maximize window` (`0x95`).
pub const MAXIMIZE_WINDOW: TaskId = TaskId(0x95);

/// `MultiPlatform App Switch` (`0x96`).
pub const MULTIPLATFORM_APP_SWITCH: TaskId = TaskId(0x96);

/// `MultiPlatform Home` (`0x97`).
pub const MULTIPLATFORM_HOME: TaskId = TaskId(0x97);

/// `MultiPlatform Menu` (`0x98`).
pub const MULTIPLATFORM_MENU: TaskId = TaskId(0x98);

/// `MultiPlatform Back` (`0x99`).
pub const MULTIPLATFORM_BACK: TaskId = TaskId(0x99);

/// `Mac switch language` (`0x9A`).
pub const MAC_SWITCH_LANGUAGE: TaskId = TaskId(0x9A);

/// `Mac screen Capture` (`0x9B`).
pub const MAC_SCREEN_CAPTURE: TaskId = TaskId(0x9B);

/// `Gesture Button` (`0x9C`).
pub const GESTURE_BUTTON: TaskId = TaskId(0x9C);

/// `Smart Shift` (`0x9D`).
pub const SMART_SHIFT: TaskId = TaskId(0x9D);

/// `AppExpose` (`0x9E`).
pub const APP_EXPOSE: TaskId = TaskId(0x9E);

/// `SmartZoom` (`0x9F`).
pub const SMART_ZOOM: TaskId = TaskId(0x9F);

/// `Lookup` (`0xA0`).
pub const LOOKUP: TaskId = TaskId(0xA0);

/// `Microphone on/off` (`0xA1`).
pub const MICROPHONE_ON_OFF: TaskId = TaskId(0xA1);

/// `Wifi on/off` (`0xA2`).
pub const WIFI_ON_OFF: TaskId = TaskId(0xA2);

/// `Brightness down` (`0xA3`).
pub const BRIGHTNESS_DOWN: TaskId = TaskId(0xA3);

/// `Brightness up` (`0xA4`).
pub const BRIGHTNESS_UP: TaskId = TaskId(0xA4);

/// `Display out` (`0xA5`).
pub const DISPLAY_OUT: TaskId = TaskId(0xA5);

/// `View Open Apps` (`0xA6`).
pub const VIEW_OPEN_APPS: TaskId = TaskId(0xA6);

/// `View All Open Apps` (`0xA7`).
pub const VIEW_ALL_OPEN_APPS: TaskId = TaskId(0xA7);

/// `AppSwitch` (`0xA8`).
pub const APP_SWITCH: TaskId = TaskId(0xA8);

/// `Gesture Button Navigation` (`0xA9`).
pub const GESTURE_BUTTON_NAVIGATION: TaskId = TaskId(0xA9);

/// `Fn inversion` (`0xAA`).
pub const FN_INVERSION: TaskId = TaskId(0xAA);

/// `Multiplatform Back` (`0xAB`).
pub const MULTIPLATFORM_BACK_171: TaskId = TaskId(0xAB);

/// `Multiplatform Forward` (`0xAC`).
pub const MULTIPLATFORM_FORWARD: TaskId = TaskId(0xAC);

/// `Multiplatform Gesture button` (`0xAD`).
pub const MULTIPLATFORM_GESTURE_BUTTON: TaskId = TaskId(0xAD);

/// `HostSwitch channel 1` (`0xAE`).
pub const HOST_SWITCH_CHANNEL_1: TaskId = TaskId(0xAE);

/// `HostSwitch channel 2` (`0xAF`).
pub const HOST_SWITCH_CHANNEL_2: TaskId = TaskId(0xAF);

/// `HostSwitch channel 3` (`0xB0`).
pub const HOST_SWITCH_CHANNEL_3: TaskId = TaskId(0xB0);

/// `Multiplatform Search` (`0xB1`).
pub const MULTIPLATFORM_SEARCH: TaskId = TaskId(0xB1);

/// `Multiplatform Home / Mission Control` (`0xB2`).
pub const MULTIPLATFORM_HOME_MISSION_CONTROL: TaskId = TaskId(0xB2);

/// `Multiplatform Menu / Launchpad` (`0xB3`).
pub const MULTIPLATFORM_MENU_LAUNCHPAD: TaskId = TaskId(0xB3);

/// `Virtual Gesture Button` (`0xB4`).
pub const VIRTUAL_GESTURE_BUTTON: TaskId = TaskId(0xB4);

/// `Cursor` (`0xB5`).
pub const CURSOR: TaskId = TaskId(0xB5);

/// `keyboard right arrow` (`0xB6`).
pub const KEYBOARD_RIGHT_ARROW: TaskId = TaskId(0xB6);

/// `SW custom highlight` (`0xB7`).
pub const SW_CUSTOM_HIGHLIGHT: TaskId = TaskId(0xB7);

/// `keyboard left arrow` (`0xB8`).
pub const KEYBOARD_LEFT_ARROW: TaskId = TaskId(0xB8);

/// `TBD` (`0xB9`).
pub const TBD: TaskId = TaskId(0xB9);

/// `Multiplatform Language Switch` (`0xBA`).
pub const MULTIPLATFORM_LANGUAGE_SWITCH: TaskId = TaskId(0xBA);

/// `SWCustomHighligt2` (`0xBB`).
pub const SW_CUSTOM_HIGHLIGT_2: TaskId = TaskId(0xBB);

/// `FastForward` (`0xBC`).
pub const FAST_FORWARD: TaskId = TaskId(0xBC);

/// `FastBackward` (`0xBD`).
pub const FAST_BACKWARD: TaskId = TaskId(0xBD);

/// `SwitchHighlighting` (`0xBE`).
pub const SWITCH_HIGHLIGHTING: TaskId = TaskId(0xBE);

/// `Mission Control / Task View` (`0xBF`).
pub const MISSION_CONTROL_TASK_VIEW: TaskId = TaskId(0xBF);

/// `Dashboard (Launchpad) / Action Center` (`0xC0`).
pub const DASHBOARD_LAUNCHPAD_ACTION_CENTER: TaskId = TaskId(0xC0);

/// `Backlight - (FW internal function)` (`0xC1`).
pub const BACKLIGHT_MINUS_FW_INTERNAL_FUNCTION: TaskId = TaskId(0xC1);

/// `Backlight + (FW internal function)` (`0xC2`).
pub const BACKLIGHT_PLUS_FW_INTERNAL_FUNCTION: TaskId = TaskId(0xC2);

/// `Right Click / App (Contextual Menu)` (`0xC3`).
pub const RIGHT_CLICK_APP_CONTEXTUAL_MENU: TaskId = TaskId(0xC3);

/// `DPI Change` (`0xC4`).
pub const DPI_CHANGE: TaskId = TaskId(0xC4);

/// `New Tab` (`0xC5`).
pub const NEW_TAB: TaskId = TaskId(0xC5);

/// `F2` (`0xC6`).
pub const F2: TaskId = TaskId(0xC6);

/// `F3` (`0xC7`).
pub const F3: TaskId = TaskId(0xC7);

/// `F4` (`0xC8`).
pub const F4: TaskId = TaskId(0xC8);

/// `F5` (`0xC9`).
pub const F5: TaskId = TaskId(0xC9);

/// `F6` (`0xCA`).
pub const F6: TaskId = TaskId(0xCA);

/// `F7` (`0xCB`).
pub const F7: TaskId = TaskId(0xCB);

/// `F8` (`0xCC`).
pub const F8: TaskId = TaskId(0xCC);

/// `F1` (`0xCD`).
pub const F1: TaskId = TaskId(0xCD);

/// `laser button` (`0xCE`).
pub const LASER_BUTTON: TaskId = TaskId(0xCE);

/// `laser button long press` (`0xCF`).
pub const LASER_BUTTON_LONG_PRESS: TaskId = TaskId(0xCF);

/// `start presentation` (`0xD0`).
pub const START_PRESENTATION: TaskId = TaskId(0xD0);

/// `blank screen` (`0xD1`).
pub const BLANK_SCREEN: TaskId = TaskId(0xD1);

/// `DPI switch` (`0xD2`).
pub const DPI_SWITCH: TaskId = TaskId(0xD2);

/// `MultiPlatform Home / Show Desktop` (`0xD3`).
pub const MULTIPLATFORM_HOME_SHOW_DESKTOP: TaskId = TaskId(0xD3);

/// `MultiPlatform App Switch / Dashboard` (`0xD4`).
pub const MULTIPLATFORM_APP_SWITCH_DASHBOARD: TaskId = TaskId(0xD4);

/// `MultiPlatform App Switch` (`0xD5`).
pub const MULTIPLATFORM_APP_SWITCH_213: TaskId = TaskId(0xD5);

/// `Fn Inversion` (`0xD6`).
pub const FN_INVERSION_214: TaskId = TaskId(0xD6);

/// `LeftAndRightClick` (`0xD7`).
pub const LEFT_AND_RIGHT_CLICK: TaskId = TaskId(0xD7);

/// `Voice Dictation` (`0xD8`).
pub const VOICE_DICTATION: TaskId = TaskId(0xD8);

/// `Emoji - Smiling face with heart shaped eyes` (`0xD9`).
pub const EMOJI_SMILING_FACE_WITH_HEART_SHAPED_EYES: TaskId = TaskId(0xD9);

/// `Emoji - Loudly Crying face` (`0xDA`).
pub const EMOJI_LOUDLY_CRYING_FACE: TaskId = TaskId(0xDA);

/// `Emoji - Smiley` (`0xDB`).
pub const EMOJI_SMILEY: TaskId = TaskId(0xDB);

/// `Emoji - Smiley with tears` (`0xDC`).
pub const EMOJI_SMILEY_WITH_TEARS: TaskId = TaskId(0xDC);

/// `Open emoji panel` (`0xDD`).
pub const OPEN_EMOJI_PANEL: TaskId = TaskId(0xDD);

/// `Multiplatform App Switch/Launchpad` (`0xDE`).
pub const MULTIPLATFORM_APP_SWITCH_LAUNCHPAD: TaskId = TaskId(0xDE);

/// `Snipping tool` (`0xDF`).
pub const SNIPPING_TOOL: TaskId = TaskId(0xDF);

/// `Grave accent` (`0xE0`).
pub const GRAVE_ACCENT: TaskId = TaskId(0xE0);

/// `Standard Tab key` (`0xE1`).
pub const STANDARD_TAB_KEY: TaskId = TaskId(0xE1);

/// `Caps lock` (`0xE2`).
pub const CAPS_LOCK: TaskId = TaskId(0xE2);

/// `Left Shift` (`0xE3`).
pub const LEFT_SHIFT: TaskId = TaskId(0xE3);

/// `Left Control` (`0xE4`).
pub const LEFT_CONTROL: TaskId = TaskId(0xE4);

/// `Left Option /Start` (`0xE5`).
pub const LEFT_OPTION_START: TaskId = TaskId(0xE5);

/// `Left Command/Alt` (`0xE6`).
pub const LEFT_COMMAND_ALT: TaskId = TaskId(0xE6);

/// `Right Command/Alt` (`0xE7`).
pub const RIGHT_COMMAND_ALT: TaskId = TaskId(0xE7);

/// `Right Option/Start` (`0xE8`).
pub const RIGHT_OPTION_START: TaskId = TaskId(0xE8);

/// `Right Control` (`0xE9`).
pub const RIGHT_CONTROL: TaskId = TaskId(0xE9);

/// `Right Shift` (`0xEA`).
pub const RIGHT_SHIFT: TaskId = TaskId(0xEA);

/// `Insert` (`0xEB`).
pub const INSERT: TaskId = TaskId(0xEB);

/// `Delete` (`0xEC`).
pub const DELETE: TaskId = TaskId(0xEC);

/// `Home` (`0xED`).
pub const HOME: TaskId = TaskId(0xED);

/// `End` (`0xEE`).
pub const END: TaskId = TaskId(0xEE);

/// `Page Up` (`0xEF`).
pub const PAGE_UP: TaskId = TaskId(0xEF);

/// `Page Down` (`0xF0`).
pub const PAGE_DOWN: TaskId = TaskId(0xF0);

/// `Mute microphone` (`0xF1`).
pub const MUTE_MICROPHONE: TaskId = TaskId(0xF1);

/// `Do not disturb` (`0xF2`).
pub const DO_NOT_DISTURB: TaskId = TaskId(0xF2);

/// `Backslash` (`0xF3`).
pub const BACKSLASH: TaskId = TaskId(0xF3);

/// `Refresh` (`0xF4`).
pub const REFRESH: TaskId = TaskId(0xF4);

/// `Close Tab` (`0xF5`).
pub const CLOSE_TAB: TaskId = TaskId(0xF5);

/// `Lang Switch` (`0xF6`).
pub const LANG_SWITCH: TaskId = TaskId(0xF6);

/// `Standard alphabetical key` (`0xF7`).
pub const STANDARD_ALPHABETICAL_KEY: TaskId = TaskId(0xF7);

/// `Right Option / Start` (`0xF8`).
pub const RIGHT_OPTION_START_248: TaskId = TaskId(0xF8);

/// `Left Option` (`0xF9`).
pub const LEFT_OPTION: TaskId = TaskId(0xF9);

/// `Right Option` (`0xFA`).
pub const RIGHT_OPTION: TaskId = TaskId(0xFA);

/// `Left cmd` (`0xFB`).
pub const LEFT_CMD: TaskId = TaskId(0xFB);

/// `Right cmd` (`0xFC`).
pub const RIGHT_CMD: TaskId = TaskId(0xFC);

#[cfg(test)]
mod tests {
    use super::TaskId;
    use crate::feature::reprog_controls::control_ids;

    #[test]
    fn task_id_round_trips_through_u16() {
        assert_eq!(u16::from(TaskId::from(0x93)), 0x93);
        assert_eq!(TaskId::from(0x93), super::SWITCH_PRESENTATION_SWITCH_SCREEN);
    }

    #[test]
    fn known_constants_have_expected_values() {
        assert_eq!(super::RIGHT_CMD, TaskId(0xfc));
        assert_eq!(control_ids::SMART_SHIFT.0, 0xc4);
        assert_eq!(control_ids::DPI_SWITCH.0, 0xfd);
    }
}
