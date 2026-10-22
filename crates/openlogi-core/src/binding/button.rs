//! Rebindable mouse/keyboard button identifiers.

use std::borrow::Cow;
use std::fmt;
use std::num::NonZeroU8;
use std::str::FromStr;

use openlogi_device_registry::keypad::{KeyPosition, MX_CREATIVE_KEYPAD_GRID};
use serde::{Deserialize, Deserializer, Serialize, Serializer, de};
use thiserror::Error;

use super::control::Cid;

/// One of the user-rebindable controls on a Logi device: a mouse hotspot,
/// a keyboard key addressed by its HID++ `0x1b04` control ID, or a keypad grid
/// key addressed by its [`GridSlot`] — a [`GridKey`] position on a
/// [`KeypadPage`].
///
/// The mouse variants are declared in physical front-to-side order;
/// [`ButtonId::ALL`] is consumed by the default-binding generator and the
/// popover trigger list. Keyboard keys are [`ButtonId::Control`] and grid keys
/// are [`ButtonId::Grid`], both out of that list: keyboard keys seed no default
/// and are never diverted until bound, and grid keys are observed only — the
/// firmware reports them on a vendor report whatever the binding says.
///
/// # Serialization
///
/// Persisted as a string — the variant name for a mouse button
/// (`"MiddleClick"`), the catalog name for a known control
/// (`"KeyScreenCapture"`) or `"control:0x0141"` for any other, and the
/// position name for a grid key (`"gridMiddle"`, `"page2:gridMiddle"` on a
/// later page) — so a `ButtonId` can key a TOML table. See
/// [`FromStr`](Self::from_str).
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
    /// A key on a keypad's 3×3 LCD grid, on one of the keypad's binding pages.
    /// These keys report through a vendor report rather than `0x1b04`, so they
    /// have no control ID and no divertable state: bound or not, they emit
    /// nothing of their own to the host, and OpenLogi only ever observes them.
    ///
    /// Capture is physical: a session observes and reports a key as its
    /// first-page slot ([`ButtonId::grid`]), and the page the keypad is on
    /// decides which slot's binding that press dispatches.
    Grid(GridSlot),
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

    /// The physical grid key `key` — its first-page slot, the identity a
    /// capture session reports a press as.
    #[must_use]
    pub const fn grid(key: GridKey) -> Self {
        Self::Grid(GridSlot {
            page: KeypadPage::FIRST,
            key,
        })
    }

    /// The page and position this button addresses, for a keypad grid key.
    #[must_use]
    pub const fn grid_slot(self) -> Option<GridSlot> {
        match self {
            Self::Grid(slot) => Some(slot),
            _ => None,
        }
    }

    /// Whether this button is one the OS hook (macOS `CGEventTap` / Linux evdev)
    /// remaps: Middle, Back, or Forward. The primary L/R clicks always pass
    /// through (suppressing them would brick the mouse), and the DPI / thumb /
    /// dedicated gesture controls aren't visible to the OS hook at all (they're
    /// captured over HID++). Stored gesture bindings also project through this
    /// set, including Middle Click gestures.
    #[must_use]
    pub fn is_os_hook_button(self) -> bool {
        matches!(
            self,
            ButtonId::MiddleClick | ButtonId::Back | ButtonId::Forward
        )
    }

    /// Whether this button may use the OS hook's hold-and-swipe gesture path.
    /// Middle, Back, and Forward are eligible controls. Wheel rotation and
    /// tilt remain single-action controls.
    #[must_use]
    pub fn is_os_hook_gesture_source(self) -> bool {
        matches!(
            self,
            ButtonId::MiddleClick | ButtonId::Back | ButtonId::Forward
        )
    }

    /// Whether holding this button may turn the main wheel into a virtual
    /// thumb wheel ([`Binding::WheelHold`](super::Binding::WheelHold)). Back
    /// and Forward are the only eligible controls: they are where devices
    /// report raw-wheel support (`0x1b04` `RAW_WHEEL`), and where Logi
    /// Options+ offers its virtual thumb wheel.
    #[must_use]
    pub fn supports_wheel_hold(self) -> bool {
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
    /// rotation and tilt, the primary clicks and keyboard keys stay single-action by
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
    const UNIT_NAMES: [(ButtonId, &'static str); 13] = [
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
            // Grid keys and keyboard controls have no fixed name in this
            // table; see [`GridKey::config_name`] and [`Cid::config_name`].
            ButtonId::Grid(_) | ButtonId::Control(_) => return None,
        })
    }

    /// The name this button is written with in `config.toml`.
    #[must_use]
    pub fn config_name(self) -> Cow<'static, str> {
        match self {
            ButtonId::Control(cid) => cid.config_name(),
            ButtonId::Grid(slot) => slot.config_name(),
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
            ButtonId::Grid(slot) => return slot.label(),
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
            // The page is context the caller shows beside the key, not part
            // of the key's name.
            ButtonId::Grid(slot) => slot.key.translation_key(),
            ButtonId::Control(cid) => cid.translation_key(),
        }
    }
}

impl fmt::Display for ButtonId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.label())
    }
}

/// One position of a keypad's 3×3 key grid — currently the MX Creative
/// Keypad's LCD keys, whose identity is their position because, unlike
/// `0x1b04` controls, they carry no control ID.
///
/// The variants declare the firmware's own numbering: row-major from the
/// top-left, so [`index`](Self::index) is exactly the byte the vendor report
/// carries. That pinning was verified by a mapping run of the
/// `openlogi-hid` grid-key probe (`crates/openlogi-hid/examples/mx_keypad_grid_probe.rs`,
/// 2026-10): pressing the keys in physical row-major order reported the index
/// sequence `1..=9`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum GridKey {
    /// Row-major index 1.
    TopLeft,
    /// Row-major index 2.
    TopMiddle,
    /// Row-major index 3.
    TopRight,
    /// Row-major index 4.
    MiddleLeft,
    /// Row-major index 5.
    Middle,
    /// Row-major index 6.
    MiddleRight,
    /// Row-major index 7.
    BottomLeft,
    /// Row-major index 8.
    BottomMiddle,
    /// Row-major index 9.
    BottomRight,
}

impl GridKey {
    /// The nine keys in wire-index order — the iteration source for GUI grids
    /// and for building a device's bindable key set.
    pub const ALL: [GridKey; 9] = [
        Self::TopLeft,
        Self::TopMiddle,
        Self::TopRight,
        Self::MiddleLeft,
        Self::Middle,
        Self::MiddleRight,
        Self::BottomLeft,
        Self::BottomMiddle,
        Self::BottomRight,
    ];

    /// Physical position as `(row, col)`, zero-based from the top-left. The
    /// inverse of [`index`](Self::index): the firmware numbers keys row-major,
    /// so a GUI drawing the device's keycap grid must not re-derive the
    /// mapping from another number.
    #[must_use]
    pub const fn position(self) -> KeyPosition {
        MX_CREATIVE_KEYPAD_GRID.cell(self.index() as u16 - 1)
    }

    /// The 1-based index the device's vendor report carries for this key.
    #[must_use]
    pub const fn index(self) -> u8 {
        match self {
            Self::TopLeft => 1,
            Self::TopMiddle => 2,
            Self::TopRight => 3,
            Self::MiddleLeft => 4,
            Self::Middle => 5,
            Self::MiddleRight => 6,
            Self::BottomLeft => 7,
            Self::BottomMiddle => 8,
            Self::BottomRight => 9,
        }
    }

    /// The key with `index` as its wire index, or `None` off the grid.
    #[must_use]
    pub const fn from_index(index: u8) -> Option<Self> {
        match index {
            1 => Some(Self::TopLeft),
            2 => Some(Self::TopMiddle),
            3 => Some(Self::TopRight),
            4 => Some(Self::MiddleLeft),
            5 => Some(Self::Middle),
            6 => Some(Self::MiddleRight),
            7 => Some(Self::BottomLeft),
            8 => Some(Self::BottomMiddle),
            9 => Some(Self::BottomRight),
            _ => None,
        }
    }

    /// The persisted name, `grid` + the CamelCase position.
    #[must_use]
    pub const fn config_name(self) -> &'static str {
        match self {
            Self::TopLeft => "gridTopLeft",
            Self::TopMiddle => "gridTopMiddle",
            Self::TopRight => "gridTopRight",
            Self::MiddleLeft => "gridMiddleLeft",
            Self::Middle => "gridMiddle",
            Self::MiddleRight => "gridMiddleRight",
            Self::BottomLeft => "gridBottomLeft",
            Self::BottomMiddle => "gridBottomMiddle",
            Self::BottomRight => "gridBottomRight",
        }
    }

    /// The inverse of [`Self::config_name`].
    #[must_use]
    pub fn from_config_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|key| key.config_name() == name)
    }

    /// Human-readable label for popovers and tooltips.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::TopLeft => "Grid Key (Top Left)",
            Self::TopMiddle => "Grid Key (Top Middle)",
            Self::TopRight => "Grid Key (Top Right)",
            Self::MiddleLeft => "Grid Key (Middle Left)",
            Self::Middle => "Grid Key (Middle)",
            Self::MiddleRight => "Grid Key (Middle Right)",
            Self::BottomLeft => "Grid Key (Bottom Left)",
            Self::BottomMiddle => "Grid Key (Bottom Middle)",
            Self::BottomRight => "Grid Key (Bottom Right)",
        }
    }

    /// Stable catalog key for the localized key label.
    #[must_use]
    pub const fn translation_key(self) -> &'static str {
        match self {
            Self::TopLeft => "keyboard.grid_top_left",
            Self::TopMiddle => "keyboard.grid_top_middle",
            Self::TopRight => "keyboard.grid_top_right",
            Self::MiddleLeft => "keyboard.grid_middle_left",
            Self::Middle => "keyboard.grid_middle",
            Self::MiddleRight => "keyboard.grid_middle_right",
            Self::BottomLeft => "keyboard.grid_bottom_left",
            Self::BottomMiddle => "keyboard.grid_bottom_middle",
            Self::BottomRight => "keyboard.grid_bottom_right",
        }
    }
}

/// One of a keypad's binding pages, numbered from 1.
///
/// The keypad's nine keys carry one binding set per page; the page buttons
/// (or any key bound to [`Action::NextKeypadPage`](super::Action::NextKeypadPage)
/// / [`Action::PreviousKeypadPage`](super::Action::PreviousKeypadPage)) move
/// between them. A device's page *count* is a page too: the last one.
///
/// Persisted and sent as its number.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct KeypadPage(NonZeroU8);

impl KeypadPage {
    /// Page 1 — the only page a keypad has until more are added.
    pub const FIRST: Self = Self(NonZeroU8::MIN);
    /// The most pages a keypad can have, matching Logi Options+.
    pub const MAX: Self = match NonZeroU8::new(15) {
        Some(max) => Self(max),
        None => unreachable!(),
    };

    /// Page `number`, or `None` outside `1..=MAX`.
    #[must_use]
    pub const fn new(number: u8) -> Option<Self> {
        match NonZeroU8::new(number) {
            Some(page) if number <= Self::MAX.number() => Some(Self(page)),
            _ => None,
        }
    }

    /// The page's number, from 1.
    #[must_use]
    pub const fn number(self) -> u8 {
        self.0.get()
    }

    /// The page after this one on a keypad whose last page is `last`,
    /// wrapping to page 1. A page past `last` (the count shrank under it)
    /// also lands on page 1.
    #[must_use]
    pub const fn next_within(self, last: Self) -> Self {
        if self.number() >= last.number() {
            Self::FIRST
        } else {
            Self(self.0.saturating_add(1))
        }
    }

    /// The page before this one on a keypad whose last page is `last`,
    /// wrapping from page 1 to `last`. A page past `last` lands on `last`.
    #[must_use]
    pub const fn previous_within(self, last: Self) -> Self {
        if self.number() == 1 || self.number() > last.number() {
            last
        } else {
            match NonZeroU8::new(self.number() - 1) {
                Some(page) => Self(page),
                None => last,
            }
        }
    }

    /// Every page from 1 through `self`.
    pub fn through(self) -> impl Iterator<Item = Self> {
        (1..=self.number()).filter_map(Self::new)
    }
}

impl Default for KeypadPage {
    fn default() -> Self {
        Self::FIRST
    }
}

impl fmt::Display for KeypadPage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.number())
    }
}

impl Serialize for KeypadPage {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_u8(self.number())
    }
}

impl<'de> Deserialize<'de> for KeypadPage {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let number = u8::deserialize(deserializer)?;
        Self::new(number).ok_or_else(|| {
            de::Error::custom(format!(
                "keypad page {number} is outside 1..={}",
                Self::MAX.number()
            ))
        })
    }
}

/// A keypad grid key on one binding page — what a grid binding is keyed by.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct GridSlot {
    /// The binding page.
    pub page: KeypadPage,
    /// The physical key.
    pub key: GridKey,
}

/// Prefix of a later page's grid name: `page2:gridMiddle`.
const PAGE_PREFIX: &str = "page";

impl GridSlot {
    /// The persisted name: the key's position name on page 1, and
    /// `page<n>:` + it on a later page.
    #[must_use]
    pub fn config_name(self) -> Cow<'static, str> {
        if self.page == KeypadPage::FIRST {
            Cow::Borrowed(self.key.config_name())
        } else {
            Cow::Owned(format!(
                "{PAGE_PREFIX}{}:{}",
                self.page,
                self.key.config_name()
            ))
        }
    }

    /// The inverse of [`Self::config_name`]. A page-1 slot has exactly one
    /// spelling: `page1:` is not accepted, so a config cannot bind one slot
    /// twice under two names.
    #[must_use]
    pub fn from_config_name(name: &str) -> Option<Self> {
        let Some((page, key)) = name
            .strip_prefix(PAGE_PREFIX)
            .and_then(|rest| rest.split_once(':'))
        else {
            return GridKey::from_config_name(name).map(|key| Self {
                page: KeypadPage::FIRST,
                key,
            });
        };
        // Digits only: `u8::from_str` would also take a leading `+`.
        let page = page
            .bytes()
            .all(|byte| byte.is_ascii_digit())
            .then(|| page.parse().ok())
            .flatten()
            .and_then(KeypadPage::new)
            .filter(|page| *page != KeypadPage::FIRST)?;
        Some(Self {
            page,
            key: GridKey::from_config_name(key)?,
        })
    }

    /// Human-readable label: the key's, plus its page past the first.
    #[must_use]
    pub fn label(self) -> Cow<'static, str> {
        if self.page == KeypadPage::FIRST {
            Cow::Borrowed(self.key.label())
        } else {
            Cow::Owned(format!("{}, Page {}", self.key.label(), self.page))
        }
    }
}

/// A persisted button name that names no button.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("unknown button `{0}`")]
pub struct ParseButtonIdError(pub String);

impl FromStr for ButtonId {
    type Err = ParseButtonIdError;

    /// The inverse of [`ButtonId::config_name`]: a mouse button's variant
    /// name, a grid slot's name, a known control's catalog name, or
    /// `control:0x…`.
    fn from_str(name: &str) -> Result<Self, Self::Err> {
        ButtonId::UNIT_NAMES
            .into_iter()
            .find_map(|(button, unit_name)| (unit_name == name).then_some(button))
            .or_else(|| GridSlot::from_config_name(name).map(Self::Grid))
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
