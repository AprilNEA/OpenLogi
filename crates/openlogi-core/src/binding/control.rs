//! HID++ `0x1b04` reprogrammable controls as rebindable buttons.
//!
//! A Logitech keyboard's media and shortcut keys are not F-keys to the host:
//! each is a *control* the firmware identifies by a 16-bit control ID (CID),
//! reports in its `0x1b04` table with `divertable` and friends, and — once
//! diverted — delivers as a HID++ notification instead of a keystroke. The
//! CID is therefore the key's real identity, on the wire, in Options+'s own
//! slot names (`c266` is `0x010A`), and here.
//!
//! [`Cid`] carries that identity — the codebase's own name for a `0x1b04`
//! control ID. The [`KNOWN_CONTROLS`] catalog gives
//! the controls OpenLogi has met a stable config name, an English label and a
//! locale key; a control outside the catalog is still a first-class
//! [`ButtonId`](super::ButtonId) — it is written as `control:0x0141` and shown
//! by its number — so a keyboard OpenLogi has never seen is bindable the day it
//! ships, and adding a catalog row is a data change, not a new enum variant.

use std::borrow::Cow;
use std::fmt;

/// One HID++ `0x1b04` control, by its firmware control ID.
///
/// Distinct from the HID++ layer's own `ControlId` on purpose: this is the
/// config-facing identity of a rebindable control, and the two crates never
/// depend on each other.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Cid(u16);

impl Cid {
    /// Wrap a raw `0x1b04` control ID.
    #[must_use]
    pub const fn new(cid: u16) -> Self {
        Self(cid)
    }

    /// The raw control ID as the firmware reports it.
    #[must_use]
    pub const fn raw(self) -> u16 {
        self.0
    }

    /// The catalog row for this control, when OpenLogi knows it.
    #[must_use]
    pub fn known(self) -> Option<&'static KnownControl> {
        KNOWN_CONTROLS.iter().find(|control| control.id == self)
    }

    /// The control a config name denotes: a catalog name such as
    /// `KeyScreenCapture`, or the `control:0x…` form any control has.
    #[must_use]
    pub fn from_config_name(name: &str) -> Option<Self> {
        if let Some(hex) = name.strip_prefix(CONFIG_PREFIX) {
            let digits = hex.strip_prefix("0x")?;
            // `from_str_radix` would also accept a leading sign; the grammar
            // is hex digits only.
            if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_hexdigit()) {
                return None;
            }
            return u16::from_str_radix(digits, 16).ok().map(Self);
        }
        KNOWN_CONTROLS
            .iter()
            .find(|control| control.name == name)
            .map(|control| control.id)
    }

    /// The name this control is written with in `config.toml`: its catalog
    /// name when it has one, else `control:0x…`. Stable — a control that later
    /// gains a catalog row keeps loading under its numeric name.
    #[must_use]
    pub fn config_name(self) -> Cow<'static, str> {
        match self.known() {
            Some(control) => Cow::Borrowed(control.name),
            None => Cow::Owned(format!("{CONFIG_PREFIX}{:#06x}", self.0)),
        }
    }

    /// English label for logs and non-localized contexts. An uncataloged
    /// control is named by number — `Control 0x01f3`, deliberately not the
    /// `control:0x01f3` config form, so a label pasted into `config.toml`
    /// fails to parse instead of half-working.
    #[must_use]
    pub fn label(self) -> Cow<'static, str> {
        match self.known() {
            Some(control) => Cow::Borrowed(control.label),
            None => Cow::Owned(format!("Control {}", self.hex())),
        }
    }

    /// Stable catalog key for the localized label. An uncataloged control
    /// resolves to [`UNKNOWN_CONTROL_TRANSLATION_KEY`], the plain word
    /// "Control"; callers that show the number append [`Cid::hex`].
    #[must_use]
    pub fn translation_key(self) -> &'static str {
        self.known()
            .map_or(UNKNOWN_CONTROL_TRANSLATION_KEY, |control| {
                control.translation_key
            })
    }

    /// The control ID as `0x01f3`, for labels of uncataloged controls.
    #[must_use]
    pub fn hex(self) -> String {
        format!("{:#06x}", self.0)
    }
}

impl fmt::Display for Cid {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.label())
    }
}

/// Prefix of the numeric config form, `control:0x0141`.
const CONFIG_PREFIX: &str = "control:";

/// Locale key for a control with no catalog row: the plain word "Control",
/// to which the GUI appends [`Cid::hex`]. Deliberately not an interpolating
/// message — every caller resolves button keys with a bare `tr!`.
pub const UNKNOWN_CONTROL_TRANSLATION_KEY: &str = "keyboard.control_key";

/// A control OpenLogi knows by name.
#[derive(Debug, PartialEq, Eq)]
pub struct KnownControl {
    /// The firmware control ID.
    pub id: Cid,
    /// The name it is written with in `config.toml`. Persisted, so append-only
    /// and never renamed: the nine `Key…` names predate the catalog and are
    /// kept byte-for-byte.
    pub name: &'static str,
    /// English label.
    pub label: &'static str,
    /// Locale catalog key for the label.
    pub translation_key: &'static str,
}

const fn known(
    cid: u16,
    name: &'static str,
    label: &'static str,
    translation_key: &'static str,
) -> KnownControl {
    KnownControl {
        id: Cid(cid),
        name,
        label,
        translation_key,
    }
}

/// Every control OpenLogi names, in the order a keyboard's function row lays
/// them out (brightness, backlight, then the shortcut and media keys, then the
/// keys above the numpad). CIDs are Logitech's own (the `0x1b04` control-ID
/// list, cross-checked against Solaar's `special_keys.py` and the slot IDs in
/// Logi's device metadata).
pub const KNOWN_CONTROLS: &[KnownControl] = &[
    known(
        0x00c7,
        "KeyBrightnessDown",
        "Brightness Down Key",
        "keyboard.brightness_down_key",
    ),
    known(
        0x00c8,
        "KeyBrightnessUp",
        "Brightness Up Key",
        "keyboard.brightness_up_key",
    ),
    known(
        0x00e2,
        "KeyBacklightDown",
        "Backlight Down Key",
        "keyboard.backlight_down_key",
    ),
    known(
        0x00e3,
        "KeyBacklightUp",
        "Backlight Up Key",
        "keyboard.backlight_up_key",
    ),
    known(
        0x00e0,
        "KeyMissionControl",
        "Mission Control Key",
        "keyboard.mission_control_key",
    ),
    known(
        0x00e1,
        "KeyLaunchpad",
        "Launchpad Key",
        "keyboard.launchpad_key",
    ),
    known(
        0x006e,
        "KeyShowDesktop",
        "Show Desktop Key",
        "keyboard.show_desktop_key",
    ),
    known(0x00d4, "KeySearch", "Search Key", "keyboard.search_key"),
    known(
        0x0103,
        "KeyDictation",
        "Dictation Key",
        "keyboard.dictation_key",
    ),
    known(0x0108, "KeyEmoji", "Emoji Key", "keyboard.emoji_key"),
    known(
        0x010a,
        "KeyScreenCapture",
        "Screen Capture Key",
        "keyboard.screen_capture_key",
    ),
    known(
        0x00bf,
        "KeyPrintScreen",
        "Print Screen Key",
        "keyboard.print_screen_key",
    ),
    known(
        0x011c,
        "KeyMicMute",
        "Mic Mute Key",
        "keyboard.mic_mute_key",
    ),
    known(
        0x00e4,
        "KeyPreviousTrack",
        "Previous Track Key",
        "keyboard.previous_track_key",
    ),
    known(
        0x00e5,
        "KeyPlayPause",
        "Play/Pause Key",
        "keyboard.play_pause_key",
    ),
    known(
        0x0141,
        "KeyPlayPauseTrack",
        "Play/Pause Key (track)",
        "keyboard.play_pause_track_key",
    ),
    known(
        0x00e6,
        "KeyNextTrack",
        "Next Track Key",
        "keyboard.next_track_key",
    ),
    known(0x00e7, "KeyMute", "Mute Key", "keyboard.mute_key"),
    known(
        0x00e8,
        "KeyVolumeDown",
        "Volume Down Key",
        "keyboard.volume_down_key",
    ),
    known(
        0x00e9,
        "KeyVolumeUp",
        "Volume Up Key",
        "keyboard.volume_up_key",
    ),
    known(
        0x000a,
        "KeyCalculator",
        "Calculator Key",
        "keyboard.calculator_key",
    ),
    known(0x006f, "KeyLock", "Lock Key", "keyboard.lock_key"),
    known(
        0x011d,
        "KeyDoNotDisturb",
        "Do Not Disturb Key",
        "keyboard.do_not_disturb_key",
    ),
    known(
        0x0104,
        "KeyEmojiHeartEyes",
        "Heart-Eyes Emoji Key",
        "keyboard.emoji_heart_eyes_key",
    ),
    known(
        0x0105,
        "KeyEmojiCrying",
        "Crying Emoji Key",
        "keyboard.emoji_crying_key",
    ),
    known(
        0x0106,
        "KeyEmojiSmiley",
        "Smiley Emoji Key",
        "keyboard.emoji_smiley_key",
    ),
    known(
        0x0107,
        "KeyEmojiTears",
        "Tears-of-Joy Emoji Key",
        "keyboard.emoji_tears_key",
    ),
    known(0x00ea, "KeyLookUp", "Look Up Key", "keyboard.look_up_key"),
    known(
        0x0100,
        "KeyAppSwitch",
        "App Switch Key",
        "keyboard.app_switch_key",
    ),
    known(
        0x0109,
        "KeyAppSwitchLaunchpad",
        "App Switch / Launchpad Key",
        "keyboard.app_switch_launchpad_key",
    ),
    known(0x0118, "KeyHome", "Home Key", "keyboard.home_key"),
    known(0x0119, "KeyEnd", "End Key", "keyboard.end_key"),
    known(0x011a, "KeyPageUp", "Page Up Key", "keyboard.page_up_key"),
    known(
        0x011b,
        "KeyPageDown",
        "Page Down Key",
        "keyboard.page_down_key",
    ),
    // The K380 / K400 multiplatform row. Its Back key (`0x00bd`) is a member
    // of the Back family `openlogi_device::reprog_controls::BACK_CIDS` owns
    // and dispatches as `ButtonId::Back`, so it is deliberately not a catalog
    // row here: a second name for one control would make the same key two
    // buttons. It still loads and binds as `control:0x00bd`.
    known(
        0x00ba,
        "KeyMultiPlatformAppSwitch",
        "App Switch Key (multiplatform)",
        "keyboard.multiplatform_app_switch_key",
    ),
    known(
        0x00bb,
        "KeyMultiPlatformHome",
        "Home Key (multiplatform)",
        "keyboard.multiplatform_home_key",
    ),
    known(
        0x00bc,
        "KeyMultiPlatformMenu",
        "Menu Key (multiplatform)",
        "keyboard.multiplatform_menu_key",
    ),
    known(
        0x00be,
        "KeyMultiPlatformInsert",
        "Insert Key (multiplatform)",
        "keyboard.multiplatform_insert_key",
    ),
    known(
        0x00dd,
        "KeyLanguageSwitch",
        "Language Switch Key",
        "keyboard.language_switch_key",
    ),
    // The POP Icon Keys' four shape keys.
    known(0x01a3, "KeyCircle", "Circle Key", "keyboard.circle_key"),
    known(
        0x01a4,
        "KeyTriangle",
        "Triangle Key",
        "keyboard.triangle_key",
    ),
    known(0x01a5, "KeyDiamond", "Diamond Key", "keyboard.diamond_key"),
    known(0x01a6, "KeyStar", "Star Key", "keyboard.star_key"),
];

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::*;

    #[test]
    fn catalog_names_and_ids_are_unique() {
        let ids: BTreeSet<_> = KNOWN_CONTROLS.iter().map(|c| c.id).collect();
        let names: BTreeSet<_> = KNOWN_CONTROLS.iter().map(|c| c.name).collect();
        let keys: BTreeSet<_> = KNOWN_CONTROLS.iter().map(|c| c.translation_key).collect();
        assert_eq!(ids.len(), KNOWN_CONTROLS.len());
        assert_eq!(names.len(), KNOWN_CONTROLS.len());
        assert_eq!(keys.len(), KNOWN_CONTROLS.len());
        for control in KNOWN_CONTROLS {
            assert!(
                Cid::from_config_name(control.name).is_some(),
                "{} must parse back as a config name",
                control.name
            );
        }
    }

    #[test]
    fn config_names_round_trip_for_catalogued_and_raw_controls() {
        for control in KNOWN_CONTROLS {
            assert_eq!(
                Cid::from_config_name(&control.id.config_name()),
                Some(control.id)
            );
        }
        let raw = Cid::new(0x01f3);
        assert_eq!(raw.config_name(), "control:0x01f3");
        assert_eq!(Cid::from_config_name("control:0x01f3"), Some(raw));
        assert_eq!(Cid::from_config_name("control:0x1F3"), Some(raw));
        // The numeric form names a catalogued control too — a file written
        // before the row existed keeps loading.
        assert_eq!(
            Cid::from_config_name("control:0x010a"),
            Some(Cid::new(0x010a))
        );
    }

    #[test]
    fn config_name_parser_rejects_everything_else() {
        for bogus in [
            "",
            "Key",
            "KeyBogus",
            "control:",
            "control:0x",
            "control:010a",
            "control:0xZZ",
            "control:0x+1a3",
            "control:0x-1",
            "control:0x1ffff",
            "0x010a",
        ] {
            assert_eq!(Cid::from_config_name(bogus), None, "{bogus:?}");
        }
    }

    #[test]
    fn the_nine_original_key_names_are_preserved() {
        // These have been persisted in config.toml since 0.7; renaming any of
        // them is a migration event.
        for (cid, name) in [
            (0x00d4, "KeySearch"),
            (0x0103, "KeyDictation"),
            (0x0108, "KeyEmoji"),
            (0x010a, "KeyScreenCapture"),
            (0x011c, "KeyMicMute"),
            (0x00e5, "KeyPlayPause"),
            (0x00e7, "KeyMute"),
            (0x00e8, "KeyVolumeDown"),
            (0x00e9, "KeyVolumeUp"),
        ] {
            assert_eq!(Cid::new(cid).config_name(), name);
        }
    }

    #[test]
    fn unknown_controls_label_and_localize_by_number() {
        let raw = Cid::new(0x01f3);
        assert_eq!(raw.label(), "Control 0x01f3");
        assert_eq!(raw.to_string(), "Control 0x01f3");
        assert_eq!(raw.hex(), "0x01f3");
        assert_ne!(
            raw.label(),
            raw.config_name(),
            "a label must not be a valid config name"
        );
        assert_eq!(raw.translation_key(), UNKNOWN_CONTROL_TRANSLATION_KEY);
        assert_eq!(
            Cid::new(0x010a).translation_key(),
            "keyboard.screen_capture_key"
        );
    }
}
