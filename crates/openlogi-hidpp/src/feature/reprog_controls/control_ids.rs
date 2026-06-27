//! Named [`ControlId`] constants from the official `0x1b04`
//! control-id list (`x1b04_control_ids_list`).
//!
//! Covers control IDs `0xB8`..=`0x161`; the classic mouse and keyboard control
//! IDs below `0xB8` are not enumerated in that list.

use super::ControlId;

/// `Second L-Click` (`0xB8`).
pub const SECOND_L_CLICK: ControlId = ControlId(0xB8);

/// `Fn + Second L-Click` (`0xB9`).
pub const FN_SECOND_L_CLICK: ControlId = ControlId(0xB9);

/// `MultiPlatform App Switch` (`0xBA`).
pub const MULTIPLATFORM_APP_SWITCH: ControlId = ControlId(0xBA);

/// `MultiPlatform Home` (`0xBB`).
pub const MULTIPLATFORM_HOME: ControlId = ControlId(0xBB);

/// `MultiPlatform Menu` (`0xBC`).
pub const MULTIPLATFORM_MENU: ControlId = ControlId(0xBC);

/// `MultiPlatform Back` (`0xBD`).
pub const MULTIPLATFORM_BACK: ControlId = ControlId(0xBD);

/// `MultiPlatform Insert` (`0xBE`).
pub const MULTIPLATFORM_INSERT: ControlId = ControlId(0xBE);

/// `Screen Capture / Print Screen` (`0xBF`).
pub const SCREEN_CAPTURE_PRINT_SCREEN: ControlId = ControlId(0xBF);

/// `Fn + Down` (`0xC0`).
pub const FN_DOWN: ControlId = ControlId(0xC0);

/// `Fn + Up` (`0xC1`).
pub const FN_UP: ControlId = ControlId(0xC1);

/// `Multiplatform Lock` (`0xC2`).
pub const MULTIPLATFORM_LOCK: ControlId = ControlId(0xC2);

/// `App Switch Gesture` (`0xC3`).
pub const APP_SWITCH_GESTURE: ControlId = ControlId(0xC3);

/// `Smart Shift` (`0xC4`).
pub const SMART_SHIFT: ControlId = ControlId(0xC4);

/// `Microphone` (`0xC5`).
pub const MICROPHONE: ControlId = ControlId(0xC5);

/// `Wifi` (`0xC6`).
pub const WIFI: ControlId = ControlId(0xC6);

/// `Brightness Down` (`0xC7`).
pub const BRIGHTNESS_DOWN: ControlId = ControlId(0xC7);

/// `Brightness Up` (`0xC8`).
pub const BRIGHTNESS_UP: ControlId = ControlId(0xC8);

/// `Display out ( project screen )` (`0xC9`).
pub const DISPLAY_OUT: ControlId = ControlId(0xC9);

/// `View Open Apps` (`0xCA`).
pub const VIEW_OPEN_APPS: ControlId = ControlId(0xCA);

/// `View all apps` (`0xCB`).
pub const VIEW_ALL_APPS: ControlId = ControlId(0xCB);

/// `Switch App` (`0xCC`).
pub const SWITCH_APP: ControlId = ControlId(0xCC);

/// `Fn inversion change` (`0xCD`).
pub const FN_INVERSION_CHANGE: ControlId = ControlId(0xCD);

/// `MultiPlatform back` (`0xCE`).
pub const MULTIPLATFORM_BACK_206: ControlId = ControlId(0xCE);

/// `Multiplatform forward` (`0xCF`).
pub const MULTIPLATFORM_FORWARD: ControlId = ControlId(0xCF);

/// `Multiplatform gesture button` (`0xD0`).
pub const MULTIPLATFORM_GESTURE_BUTTON: ControlId = ControlId(0xD0);

/// `Host Switch channel 1` (`0xD1`).
pub const HOST_SWITCH_CHANNEL_1: ControlId = ControlId(0xD1);

/// `Host Switch channel 2` (`0xD2`).
pub const HOST_SWITCH_CHANNEL_2: ControlId = ControlId(0xD2);

/// `Host Switch channel 3` (`0xD3`).
pub const HOST_SWITCH_CHANNEL_3: ControlId = ControlId(0xD3);

/// `Multiplatform search` (`0xD4`).
pub const MULTIPLATFORM_SEARCH: ControlId = ControlId(0xD4);

/// `Multiplatform Home / Mission Control` (`0xD5`).
pub const MULTIPLATFORM_HOME_MISSION_CONTROL: ControlId = ControlId(0xD5);

/// `Multiplatform Menu / Show/Hide Virtual Keyboard / Launchpad` (`0xD6`).
pub const MULTIPLATFORM_MENU_SHOW_HIDE_VIRTUAL_KEYBOARD_LAUNCHPAD: ControlId = ControlId(0xD6);

/// `Virtual Gesture Button` (`0xD7`).
pub const VIRTUAL_GESTURE_BUTTON: ControlId = ControlId(0xD7);

/// `Cursor Button Long press` (`0xD8`).
pub const CURSOR_BUTTON_LONG_PRESS: ControlId = ControlId(0xD8);

/// `Next Button` (`0xD9`).
pub const NEXT_BUTTON: ControlId = ControlId(0xD9);

/// `Next Button Longpress` (`0xDA`).
pub const NEXT_BUTTON_LONG_PRESS: ControlId = ControlId(0xDA);

/// `Back` (`0xDB`).
pub const BACK: ControlId = ControlId(0xDB);

/// `Back Button Longpress` (`0xDC`).
pub const BACK_BUTTON_LONG_PRESS: ControlId = ControlId(0xDC);

/// `Multi Platform Language Switch` (`0xDD`).
pub const MULTI_PLATFORM_LANGUAGE_SWITCH: ControlId = ControlId(0xDD);

/// `F Lock` (`0xDE`).
pub const F_LOCK: ControlId = ControlId(0xDE);

/// `Switch Highlight` (`0xDF`).
pub const SWITCH_HIGHLIGHT: ControlId = ControlId(0xDF);

/// `Mission Control / Task View` (`0xE0`).
pub const MISSION_CONTROL_TASK_VIEW: ControlId = ControlId(0xE0);

/// `Dashboard (Launchpad) / Action Center` (`0xE1`).
pub const DASHBOARD_LAUNCHPAD_ACTION_CENTER: ControlId = ControlId(0xE1);

/// `Backlight -` (`0xE2`).
pub const BACKLIGHT_MINUS: ControlId = ControlId(0xE2);

/// `Backlight +` (`0xE3`).
pub const BACKLIGHT_PLUS: ControlId = ControlId(0xE3);

/// `Re-programmable Previous Track` (`0xE4`).
pub const RE_PROGRAMMABLE_PREVIOUS_TRACK: ControlId = ControlId(0xE4);

/// `Re-programmable Play/Pause` (`0xE5`).
pub const RE_PROGRAMMABLE_PLAY_PAUSE: ControlId = ControlId(0xE5);

/// `Re-programmable Next Track` (`0xE6`).
pub const RE_PROGRAMMABLE_NEXT_TRACK: ControlId = ControlId(0xE6);

/// `Re-programmable Mute` (`0xE7`).
pub const RE_PROGRAMMABLE_MUTE: ControlId = ControlId(0xE7);

/// `Re-programmable Volume Down` (`0xE8`).
pub const RE_PROGRAMMABLE_VOLUME_DOWN: ControlId = ControlId(0xE8);

/// `Re-programmable Volume Up` (`0xE9`).
pub const RE_PROGRAMMABLE_VOLUME_UP: ControlId = ControlId(0xE9);

/// `App (Contextual Menu) / Right Click` (`0xEA`).
pub const APP_CONTEXTUAL_MENU_RIGHT_CLICK: ControlId = ControlId(0xEA);

/// `Right Arrow` (`0xEB`).
pub const RIGHT_ARROW: ControlId = ControlId(0xEB);

/// `Left Arrow` (`0xEC`).
pub const LEFT_ARROW: ControlId = ControlId(0xEC);

/// `DPI Change` (`0xED`).
pub const DPI_CHANGE: ControlId = ControlId(0xED);

/// `New Tab` (`0xEE`).
pub const NEW_TAB: ControlId = ControlId(0xEE);

/// `F2` (`0xEF`).
pub const F2: ControlId = ControlId(0xEF);

/// `F3` (`0xF0`).
pub const F3: ControlId = ControlId(0xF0);

/// `F4` (`0xF1`).
pub const F4: ControlId = ControlId(0xF1);

/// `F5` (`0xF2`).
pub const F5: ControlId = ControlId(0xF2);

/// `F6` (`0xF3`).
pub const F6: ControlId = ControlId(0xF3);

/// `F7` (`0xF4`).
pub const F7: ControlId = ControlId(0xF4);

/// `F8` (`0xF5`).
pub const F8: ControlId = ControlId(0xF5);

/// `F1` (`0xF6`).
pub const F1: ControlId = ControlId(0xF6);

/// `Next Color Effect` (`0xF7`).
pub const NEXT_COLOR_EFFECT: ControlId = ControlId(0xF7);

/// `Increase Color Effect Speed` (`0xF8`).
pub const INCREASE_COLOR_EFFECT_SPEED: ControlId = ControlId(0xF8);

/// `Decrease Color Effect Speed` (`0xF9`).
pub const DECREASE_COLOR_EFFECT_SPEED: ControlId = ControlId(0xF9);

/// `Load Lighting Custom Profile` (`0xFA`).
pub const LOAD_LIGHTING_CUSTOM_PROFILE: ControlId = ControlId(0xFA);

/// `Laser button short press` (`0xFB`).
pub const LASER_BUTTON_SHORT_PRESS: ControlId = ControlId(0xFB);

/// `Laser button long press` (`0xFC`).
pub const LASER_BUTTON_LONG_PRESS: ControlId = ControlId(0xFC);

/// `DPI switch` (`0xFD`).
pub const DPI_SWITCH: ControlId = ControlId(0xFD);

/// `MultiPlatform Home / Show Desktop` (`0xFE`).
pub const MULTIPLATFORM_HOME_SHOW_DESKTOP: ControlId = ControlId(0xFE);

/// `MultiPlatform App Switch / Show Dashboard` (`0xFF`).
pub const MULTIPLATFORM_APP_SWITCH_SHOW_DASHBOARD: ControlId = ControlId(0xFF);

/// `MultiPlatform App Switch` (`0x100`).
pub const MULTIPLATFORM_APP_SWITCH_256: ControlId = ControlId(0x100);

/// `Fn Inversion Hot Key` (`0x101`).
pub const FN_INVERSION_HOT_KEY: ControlId = ControlId(0x101);

/// `LeftAndRightClick` (`0x102`).
pub const LEFT_AND_RIGHT_CLICK: ControlId = ControlId(0x102);

/// `Voice Dictation` (`0x103`).
pub const VOICE_DICTATION: ControlId = ControlId(0x103);

/// `Smiling face with heart shaped eyes` (`0x104`).
pub const SMILING_FACE_WITH_HEART_SHAPED_EYES: ControlId = ControlId(0x104);

/// `Loudly Crying face` (`0x105`).
pub const LOUDLY_CRYING_FACE: ControlId = ControlId(0x105);

/// `Emoji Smiley` (`0x106`).
pub const EMOJI_SMILEY: ControlId = ControlId(0x106);

/// `Emoji smiley with tears` (`0x107`).
pub const EMOJI_SMILEY_WITH_TEARS: ControlId = ControlId(0x107);

/// `Open emoji panel` (`0x108`).
pub const OPEN_EMOJI_PANEL: ControlId = ControlId(0x108);

/// `Multiplatform App Switch/Launchpad` (`0x109`).
pub const MULTIPLATFORM_APP_SWITCH_LAUNCHPAD: ControlId = ControlId(0x109);

/// `Snipping tool` (`0x10A`).
pub const SNIPPING_TOOL: ControlId = ControlId(0x10A);

/// `Grave accent` (`0x10B`).
pub const GRAVE_ACCENT: ControlId = ControlId(0x10B);

/// `Tab key` (`0x10C`).
pub const TAB_KEY: ControlId = ControlId(0x10C);

/// `Caps Lock` (`0x10D`).
pub const CAPS_LOCK: ControlId = ControlId(0x10D);

/// `Left Shift` (`0x10E`).
pub const LEFT_SHIFT: ControlId = ControlId(0x10E);

/// `Left Control` (`0x10F`).
pub const LEFT_CONTROL: ControlId = ControlId(0x10F);

/// `Left Option / Start` (`0x110`).
pub const LEFT_OPTION_START: ControlId = ControlId(0x110);

/// `Left Command / Alt` (`0x111`).
pub const LEFT_COMMAND_ALT: ControlId = ControlId(0x111);

/// `Right Command / Alt` (`0x112`).
pub const RIGHT_COMMAND_ALT: ControlId = ControlId(0x112);

/// `Right Option / Start` (`0x113`).
pub const RIGHT_OPTION_START: ControlId = ControlId(0x113);

/// `Right Control` (`0x114`).
pub const RIGHT_CONTROL: ControlId = ControlId(0x114);

/// `Right Shift` (`0x115`).
pub const RIGHT_SHIFT: ControlId = ControlId(0x115);

/// `Insert` (`0x116`).
pub const INSERT: ControlId = ControlId(0x116);

/// `Delete` (`0x117`).
pub const DELETE: ControlId = ControlId(0x117);

/// `Home` (`0x118`).
pub const HOME: ControlId = ControlId(0x118);

/// `End` (`0x119`).
pub const END: ControlId = ControlId(0x119);

/// `Page Up` (`0x11A`).
pub const PAGE_UP: ControlId = ControlId(0x11A);

/// `Page Down` (`0x11B`).
pub const PAGE_DOWN: ControlId = ControlId(0x11B);

/// `Mute microphone` (`0x11C`).
pub const MUTE_MICROPHONE: ControlId = ControlId(0x11C);

/// `Do not disturb` (`0x11D`).
pub const DO_NOT_DISTURB: ControlId = ControlId(0x11D);

/// `Backslash` (`0x11E`).
pub const BACKSLASH: ControlId = ControlId(0x11E);

/// `Refresh` (`0x11F`).
pub const REFRESH: ControlId = ControlId(0x11F);

/// `Close Tab` (`0x120`).
pub const CLOSE_TAB: ControlId = ControlId(0x120);

/// `Lang Switch` (`0x121`).
pub const LANG_SWITCH: ControlId = ControlId(0x121);

/// `Standard key A` (`0x122`).
pub const STANDARD_KEY_A: ControlId = ControlId(0x122);

/// `Standard key B` (`0x123`).
pub const STANDARD_KEY_B: ControlId = ControlId(0x123);

/// `Standard key C` (`0x124`).
pub const STANDARD_KEY_C: ControlId = ControlId(0x124);

/// `Standard key D` (`0x125`).
pub const STANDARD_KEY_D: ControlId = ControlId(0x125);

/// `Standard key E` (`0x126`).
pub const STANDARD_KEY_E: ControlId = ControlId(0x126);

/// `Standard key F` (`0x127`).
pub const STANDARD_KEY_F: ControlId = ControlId(0x127);

/// `Standard key G` (`0x128`).
pub const STANDARD_KEY_G: ControlId = ControlId(0x128);

/// `Standard key H` (`0x129`).
pub const STANDARD_KEY_H: ControlId = ControlId(0x129);

/// `Standard key I` (`0x12A`).
pub const STANDARD_KEY_I: ControlId = ControlId(0x12A);

/// `Standard key J` (`0x12B`).
pub const STANDARD_KEY_J: ControlId = ControlId(0x12B);

/// `Standard key K` (`0x12C`).
pub const STANDARD_KEY_K: ControlId = ControlId(0x12C);

/// `Standard key L` (`0x12D`).
pub const STANDARD_KEY_L: ControlId = ControlId(0x12D);

/// `Standard key M` (`0x12E`).
pub const STANDARD_KEY_M: ControlId = ControlId(0x12E);

/// `Standard key N` (`0x12F`).
pub const STANDARD_KEY_N: ControlId = ControlId(0x12F);

/// `Standard key O` (`0x130`).
pub const STANDARD_KEY_O: ControlId = ControlId(0x130);

/// `Standard key P` (`0x131`).
pub const STANDARD_KEY_P: ControlId = ControlId(0x131);

/// `Standard key Q` (`0x132`).
pub const STANDARD_KEY_Q: ControlId = ControlId(0x132);

/// `Standard key R` (`0x133`).
pub const STANDARD_KEY_R: ControlId = ControlId(0x133);

/// `Standard key S` (`0x134`).
pub const STANDARD_KEY_S: ControlId = ControlId(0x134);

/// `Standard key T` (`0x135`).
pub const STANDARD_KEY_T: ControlId = ControlId(0x135);

/// `Standard key U` (`0x136`).
pub const STANDARD_KEY_U: ControlId = ControlId(0x136);

/// `Standard key V` (`0x137`).
pub const STANDARD_KEY_V: ControlId = ControlId(0x137);

/// `Standard key W` (`0x138`).
pub const STANDARD_KEY_W: ControlId = ControlId(0x138);

/// `Standard key X` (`0x139`).
pub const STANDARD_KEY_X: ControlId = ControlId(0x139);

/// `Standard key Y` (`0x13A`).
pub const STANDARD_KEY_Y: ControlId = ControlId(0x13A);

/// `Standard key Z` (`0x13B`).
pub const STANDARD_KEY_Z: ControlId = ControlId(0x13B);

/// `Right Option / Start` (`0x13C`).
pub const RIGHT_OPTION_START_316: ControlId = ControlId(0x13C);

/// `Left Option` (`0x13D`).
pub const LEFT_OPTION: ControlId = ControlId(0x13D);

/// `Right Option` (`0x13E`).
pub const RIGHT_OPTION: ControlId = ControlId(0x13E);

/// `Left Cmd` (`0x13F`).
pub const LEFT_CMD: ControlId = ControlId(0x13F);

/// `Right Cmd` (`0x140`).
pub const RIGHT_CMD: ControlId = ControlId(0x140);

/// `Play/Pause (Double track)` (`0x141`).
pub const PLAY_PAUSE_DOUBLE_TRACK: ControlId = ControlId(0x141);

/// `Standard 0` (`0x142`).
pub const STANDARD_0: ControlId = ControlId(0x142);

/// `Standard 1` (`0x143`).
pub const STANDARD_1: ControlId = ControlId(0x143);

/// `Standard 2` (`0x144`).
pub const STANDARD_2: ControlId = ControlId(0x144);

/// `Standard 3` (`0x145`).
pub const STANDARD_3: ControlId = ControlId(0x145);

/// `Standard 4` (`0x146`).
pub const STANDARD_4: ControlId = ControlId(0x146);

/// `Standard 5` (`0x147`).
pub const STANDARD_5: ControlId = ControlId(0x147);

/// `Standard 6` (`0x148`).
pub const STANDARD_6: ControlId = ControlId(0x148);

/// `Standard 7` (`0x149`).
pub const STANDARD_7: ControlId = ControlId(0x149);

/// `Standard 8` (`0x14A`).
pub const STANDARD_8: ControlId = ControlId(0x14A);

/// `Standard 9` (`0x14B`).
pub const STANDARD_9: ControlId = ControlId(0x14B);

/// `Standard Esc` (`0x14C`).
pub const STANDARD_ESC: ControlId = ControlId(0x14C);

/// `Standard F9` (`0x14D`).
pub const STANDARD_F9: ControlId = ControlId(0x14D);

/// `Standard F10` (`0x14E`).
pub const STANDARD_F10: ControlId = ControlId(0x14E);

/// `Standard F11` (`0x14F`).
pub const STANDARD_F11: ControlId = ControlId(0x14F);

/// `Standard F12` (`0x150`).
pub const STANDARD_F12: ControlId = ControlId(0x150);

/// `Standard up arrow` (`0x151`).
pub const STANDARD_UP_ARROW: ControlId = ControlId(0x151);

/// `Standard down arrow` (`0x152`).
pub const STANDARD_DOWN_ARROW: ControlId = ControlId(0x152);

/// `Standard '/~` (`0x153`).
pub const STANDARD_GRAVE_TILDE: ControlId = ControlId(0x153);

/// `Fn` (`0x154`).
pub const FN: ControlId = ControlId(0x154);

/// `Standard Enter` (`0x155`).
pub const STANDARD_ENTER: ControlId = ControlId(0x155);

/// `Standard backspace` (`0x156`).
pub const STANDARD_BACKSPACE: ControlId = ControlId(0x156);

/// `Std. = or +` (`0x157`).
pub const STD_EQUALS_OR_PLUS: ControlId = ControlId(0x157);

/// `std minus` (`0x158`).
pub const STD_MINUS: ControlId = ControlId(0x158);

/// `bluetooth` (`0x159`).
pub const BLUETOOTH: ControlId = ControlId(0x159);

/// `Standard [ / {` (`0x15A`).
pub const STANDARD_LBRACKET_LBRACE: ControlId = ControlId(0x15A);

/// `Standard ] / }` (`0x15B`).
pub const STANDARD_RBRACKET_RBRACE: ControlId = ControlId(0x15B);

/// `Standard ; / :` (`0x15C`).
pub const STANDARD_SEMICOLON_COLON: ControlId = ControlId(0x15C);

/// `Standard ' / "` (`0x15D`).
pub const STANDARD_APOSTROPHE_QUOTE: ControlId = ControlId(0x15D);

/// `Standard / / ?` (`0x15E`).
pub const STANDARD_SLASH_QUESTION: ControlId = ControlId(0x15E);

/// `Standard . / >` (`0x15F`).
pub const STANDARD_DOT_GT: ControlId = ControlId(0x15F);

/// `Standard , / <` (`0x160`).
pub const STANDARD_COMMA_LT: ControlId = ControlId(0x160);

/// `Standard Space` (`0x161`).
pub const STANDARD_SPACE: ControlId = ControlId(0x161);
