//! The onboard profile page of the Logitech G700 / G700s.
//!
//! Reverse engineered; the layout follows libratbag's
//! `struct _hidpp10_profile_700` and `union _hidpp10_button_binding`
//! (`src/hidpp10.c`, MIT), the DPI table its `DpiRange=0:5700@23.53`
//! (`data/devices/logitech-g700*.device`). A profile fills the first
//! [`PROFILE_LENGTH`] bytes of its page:
//!
//! | Offset | Size | Content |
//! |---|---|---|
//! | `0x00` | 5 × 4 | DPI modes: X step, Y step, two LED bytes |
//! | `0x14` | 1 | default DPI mode |
//! | `0x15` | 3 | unknown (libratbag writes `80 01 10`) |
//! | `0x18` | 1 | report interval in milliseconds |
//! | `0x19` | 10 | unknown (libratbag writes `01 2c 02 58 64 ff bc 00 09 31`) |
//! | `0x23` | 13 × 3 | button bindings |
//! | `0x4A` | 425 | metadata: `LGS02` marker, name, macro names |
//!
//! libratbag overwrites the unknown bytes with constants when it writes a
//! profile. This codec never does: [`G700Profile`] keeps the page it was
//! parsed from and lays only its decoded fields back over it, so every byte
//! it does not understand — unknown fields, the tail, the CRC, binding
//! encodings it cannot name — comes back unchanged.

use std::num::NonZeroU16;

use bitflags::bitflags;
use num_enum::{FromPrimitive, IntoPrimitive};

use super::{MemoryWriter, Page, directory::ProfileDirectory};
use crate::{
    channel::HidppChannel, protocol::v10::Hidpp10Error, register::report_rate::ReportInterval,
};

/// How many profiles the G700 keeps.
pub const PROFILE_COUNT: usize = 5;

/// How many DPI modes one profile holds.
pub const DPI_MODE_COUNT: usize = 5;

/// How many button bindings one profile holds.
pub const BUTTON_COUNT: usize = 13;

/// How many named macros the metadata block holds.
pub const MACRO_NAME_COUNT: usize = 11;

/// The bytes of a page the profile occupies; the rest up to the CRC is
/// unused (`0xFF` on the pages libratbag writes).
pub const PROFILE_LENGTH: usize = 499;

/// The highest raw DPI step with a table entry.
pub const DPI_RAW_MAX: u8 = 242;

const DPI_MODES_OFFSET: usize = 0x00;
const DPI_MODE_SIZE: usize = 4;
const DEFAULT_DPI_OFFSET: usize = 0x14;
const REPORT_INTERVAL_OFFSET: usize = 0x18;
const BINDINGS_OFFSET: usize = 0x23;
const BINDING_SIZE: usize = 3;
const METADATA_OFFSET: usize = 0x4a;
const METADATA_MARKER: &[u8; 5] = b"LGS02";
const NAME_OFFSET: usize = METADATA_OFFSET + METADATA_MARKER.len();
const NAME_UNITS: usize = 23;
const MACRO_NAMES_OFFSET: usize = NAME_OFFSET + NAME_UNITS * 2;
const MACRO_NAME_UNITS: usize = 17;

const _: () = assert!(
    MACRO_NAMES_OFFSET + MACRO_NAME_COUNT * MACRO_NAME_UNITS * 2 == PROFILE_LENGTH,
    "the metadata block ends where the profile does"
);

/// The DPI a raw sensor step selects, or `None` for a step without a table
/// entry — `0`, which marks an unused DPI mode, or one above
/// [`DPI_RAW_MAX`].
///
/// The table is libratbag's `round(23.53 × raw / 25) × 25`, here in exact
/// integer arithmetic; the two agree on every step.
#[must_use]
pub fn dpi_from_raw(raw: u8) -> Option<NonZeroU16> {
    if raw > DPI_RAW_MAX {
        return None;
    }
    let dpi = (2353 * u32::from(raw) + 1250) / 2500 * 25;
    u16::try_from(dpi).ok().and_then(NonZeroU16::new)
}

/// The raw step whose DPI is closest to `dpi`, the lowest step on a tie.
/// The table steps by about 23.5 DPI and rounds to multiples of 25, so the
/// mapping is lossy: some neighbouring steps share a DPI.
#[must_use]
pub fn raw_from_dpi(dpi: u16) -> u8 {
    (1..=DPI_RAW_MAX)
        .min_by_key(|&raw| dpi_from_raw(raw).map_or(u16::MAX, |entry| entry.get().abs_diff(dpi)))
        .unwrap_or(1)
}

/// One DPI mode of a profile, in raw sensor steps.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct DpiMode {
    /// The X-axis step; see [`dpi_from_raw`].
    pub x: u8,
    /// The Y-axis step.
    pub y: u8,
    /// The four DPI-indicator LEDs, one nibble each: LED 1 in the low nibble
    /// of the first byte, LED 4 in the high nibble of the second.
    pub leds: [u8; 2],
}

impl DpiMode {
    fn from_bytes([x, y, led_lo, led_hi]: [u8; DPI_MODE_SIZE]) -> Self {
        Self {
            x,
            y,
            leds: [led_lo, led_hi],
        }
    }

    fn to_bytes(self) -> [u8; DPI_MODE_SIZE] {
        [self.x, self.y, self.leds[0], self.leds[1]]
    }

    /// The X-axis DPI, or `None` for an unused mode.
    #[must_use]
    pub fn x_dpi(self) -> Option<NonZeroU16> {
        dpi_from_raw(self.x)
    }

    /// The Y-axis DPI, or `None` for an unused mode.
    #[must_use]
    pub fn y_dpi(self) -> Option<NonZeroU16> {
        dpi_from_raw(self.y)
    }

    /// The state of DPI-indicator LED `1..=4` while this mode is active.
    /// `None` for any other LED number.
    #[must_use]
    pub fn led(self, number: usize) -> Option<DpiLed> {
        let byte = *self.leds.get(number.checked_sub(1)? / 2)?;
        let nibble = if number % 2 == 1 {
            byte & 0x0f
        } else {
            byte >> 4
        };
        Some(DpiLed::from(nibble))
    }
}

/// A DPI-indicator LED's nibble in a [`DpiMode`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, FromPrimitive, IntoPrimitive)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
#[repr(u8)]
pub enum DpiLed {
    /// Off.
    Off = 1,
    /// Lit.
    On = 2,
    /// A nibble libratbag never writes, kept as read.
    #[num_enum(catch_all)]
    Unknown(u8),
}

/// The buttons a [`Binding::MouseButtons`] presses: button `n` is bit
/// `n - 1`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct ButtonMask(pub u16);

impl ButtonMask {
    /// The pressed buttons, numbered from 1, in ascending order.
    pub fn buttons(self) -> impl Iterator<Item = u8> {
        (1..=16u8).filter(move |&n| self.0 & (1 << (n - 1)) != 0)
    }
}

bitflags! {
    /// The modifiers a [`Binding::Key`] holds: the HID keyboard modifier
    /// byte, which libratbag passes on unchanged. Unknown bits cannot occur —
    /// the byte has eight defined bits — but are retained regardless.
    #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
    #[cfg_attr(feature = "serde", derive(serde::Serialize))]
    pub struct KeyModifiers: u8 {
        /// Left Control.
        const LEFT_CTRL = 0x01;
        /// Left Shift.
        const LEFT_SHIFT = 0x02;
        /// Left Alt.
        const LEFT_ALT = 0x04;
        /// Left GUI (Super, Command, Windows).
        const LEFT_GUI = 0x08;
        /// Right Control.
        const RIGHT_CTRL = 0x10;
        /// Right Shift.
        const RIGHT_SHIFT = 0x20;
        /// Right Alt.
        const RIGHT_ALT = 0x40;
        /// Right GUI.
        const RIGHT_GUI = 0x80;
    }
}

/// A firmware function a [`Binding::Special`] runs. The codes are libratbag's
/// `hidpp10_profiles_specials` table.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, FromPrimitive, IntoPrimitive)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
#[repr(u16)]
pub enum Special {
    /// Scroll left (wheel tilt).
    WheelLeft = 0x01,
    /// Scroll right (wheel tilt).
    WheelRight = 0x02,
    /// Show the battery level on the DPI LEDs.
    BatteryLevel = 0x03,
    /// Next DPI mode, stopping at the last.
    ResolutionUp = 0x04,
    /// Next DPI mode, wrapping around.
    ResolutionCycleUp = 0x05,
    /// Previous DPI mode, stopping at the first.
    ResolutionDown = 0x08,
    /// Previous DPI mode, wrapping around.
    ResolutionCycleDown = 0x09,
    /// Next profile, stopping at the last.
    ProfileUp = 0x10,
    /// Next profile, wrapping around.
    ProfileCycleUp = 0x11,
    /// Previous profile, stopping at the first.
    ProfileDown = 0x20,
    /// Previous profile, wrapping around.
    ProfileCycleDown = 0x21,
    /// A code libratbag has no name for, kept as read.
    #[num_enum(catch_all)]
    Unknown(u16),
}

/// Where a [`Binding::Macro`]'s macro starts in onboard memory.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct MacroPointer {
    /// The memory page, below `0x80` — that is what tells a pointer from a
    /// typed binding.
    pub page: u8,
    /// The offset within the page, in 16-bit words.
    pub word_offset: u8,
}

const BINDING_MOUSE_BUTTONS: u8 = 0x81;
const BINDING_KEY: u8 = 0x82;
const BINDING_SPECIAL: u8 = 0x83;
const BINDING_CONSUMER: u8 = 0x84;
const BINDING_DISABLED: u8 = 0x8f;

/// What one button of a profile does. Three bytes on the wire: a type byte
/// and two of data, or, with the type byte's high bit clear, a pointer to a
/// macro.
///
/// Decoding is lossless: [`Self::to_bytes`] of a decoded binding returns the
/// bytes it came from. A form that would lose bytes — a disabled binding or
/// a macro pointer whose padding is not zero, a type byte libratbag does not
/// know — decodes as [`Self::Unknown`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub enum Binding {
    /// Type `0x81`: press these mouse buttons (little-endian mask).
    MouseButtons(ButtonMask),
    /// Type `0x82`: press a key, a HID keyboard usage, with modifiers.
    Key {
        /// The held modifiers.
        modifiers: KeyModifiers,
        /// The HID keyboard usage (page `0x07`).
        usage: u8,
    },
    /// Type `0x83`: a firmware function (little-endian code).
    Special(Special),
    /// Type `0x84`: a HID consumer-control usage (page `0x0C`, big-endian).
    Consumer(u16),
    /// Type `0x8F`: the button does nothing.
    Disabled,
    /// Run the macro stored at this address. Macros are not decoded here.
    Macro(MacroPointer),
    /// Bytes this codec cannot name, kept verbatim. Factory profiles carry
    /// such bindings on the two extra side buttons (libratbag issue #772).
    /// Constructing one with a known type byte is allowed; it encodes as
    /// given and decodes back as that type.
    Unknown([u8; 3]),
}

impl Binding {
    /// Decodes one binding.
    #[must_use]
    pub fn from_bytes(bytes: [u8; BINDING_SIZE]) -> Self {
        let [kind, first, second] = bytes;
        match kind {
            BINDING_MOUSE_BUTTONS => {
                Self::MouseButtons(ButtonMask(u16::from_le_bytes([first, second])))
            }
            BINDING_KEY => Self::Key {
                modifiers: KeyModifiers::from_bits_retain(first),
                usage: second,
            },
            BINDING_SPECIAL => Self::Special(Special::from(u16::from_le_bytes([first, second]))),
            BINDING_CONSUMER => Self::Consumer(u16::from_be_bytes([first, second])),
            BINDING_DISABLED if first == 0 && second == 0 => Self::Disabled,
            0x00..=0x7f if second == 0 => Self::Macro(MacroPointer {
                page: kind,
                word_offset: first,
            }),
            _ => Self::Unknown(bytes),
        }
    }

    /// Encodes the binding.
    #[must_use]
    pub fn to_bytes(self) -> [u8; BINDING_SIZE] {
        match self {
            Self::MouseButtons(ButtonMask(mask)) => {
                let [lo, hi] = mask.to_le_bytes();
                [BINDING_MOUSE_BUTTONS, lo, hi]
            }
            Self::Key { modifiers, usage } => [BINDING_KEY, modifiers.bits(), usage],
            Self::Special(special) => {
                let [lo, hi] = u16::from(special).to_le_bytes();
                [BINDING_SPECIAL, lo, hi]
            }
            Self::Consumer(usage) => {
                let [hi, lo] = usage.to_be_bytes();
                [BINDING_CONSUMER, hi, lo]
            }
            Self::Disabled => [BINDING_DISABLED, 0, 0],
            Self::Macro(MacroPointer { page, word_offset }) => [page, word_offset, 0],
            Self::Unknown(bytes) => bytes,
        }
    }
}

/// One G700 profile, decoded from its page.
///
/// The decoded fields are public and laid back over the original page by
/// [`Self::to_page`]; everything else stays as read. `to_page` keeps the
/// stored CRC too, so a caller that changed a field and means to write the
/// page calls [`Page::update_crc`] on the result.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct G700Profile {
    /// The DPI modes the resolution buttons step through.
    pub dpi_modes: [DpiMode; DPI_MODE_COUNT],
    /// The DPI mode the profile starts in, counted from 0. Profiles on a
    /// real G700 also carry `0x82` here, a value libratbag reads as an
    /// out-of-range mode; what the high bit means is not known, so the byte
    /// is kept as read.
    pub default_dpi_mode: u8,
    /// The report interval in milliseconds; see [`Self::report_interval`].
    pub report_interval_millis: u8,
    /// The thirteen button bindings, in libratbag's button order.
    pub bindings: [Binding; BUTTON_COUNT],
    page: Page,
}

impl G700Profile {
    /// Decodes the profile stored in `page`. Any page decodes; one that holds
    /// no profile just yields meaningless fields.
    #[must_use]
    pub fn parse(page: &Page) -> Self {
        Self {
            dpi_modes: std::array::from_fn(|mode| {
                DpiMode::from_bytes(page.array(DPI_MODES_OFFSET + mode * DPI_MODE_SIZE))
            }),
            default_dpi_mode: page.bytes[DEFAULT_DPI_OFFSET],
            report_interval_millis: page.bytes[REPORT_INTERVAL_OFFSET],
            bindings: std::array::from_fn(|button| {
                Binding::from_bytes(page.array(BINDINGS_OFFSET + button * BINDING_SIZE))
            }),
            page: page.clone(),
        }
    }

    /// The profile as a page: the page it was parsed from with the decoded
    /// fields written back. Byte-identical to that page while no field has
    /// changed.
    #[must_use]
    pub fn to_page(&self) -> Page {
        let mut page = self.page.clone();
        for (mode, dpi) in self.dpi_modes.iter().enumerate() {
            let start = DPI_MODES_OFFSET + mode * DPI_MODE_SIZE;
            page.bytes[start..start + DPI_MODE_SIZE].copy_from_slice(&dpi.to_bytes());
        }
        page.bytes[DEFAULT_DPI_OFFSET] = self.default_dpi_mode;
        page.bytes[REPORT_INTERVAL_OFFSET] = self.report_interval_millis;
        for (button, binding) in self.bindings.iter().enumerate() {
            let start = BINDINGS_OFFSET + button * BINDING_SIZE;
            page.bytes[start..start + BINDING_SIZE].copy_from_slice(&binding.to_bytes());
        }
        page
    }

    /// The report interval; [`Hidpp10Error::UnsupportedResponse`] for a zero
    /// byte, which names no rate.
    pub fn report_interval(&self) -> Result<ReportInterval, Hidpp10Error> {
        ReportInterval::try_from(self.report_interval_millis)
    }

    /// Whether the metadata block carries the `LGS02` marker that the names
    /// below need.
    #[must_use]
    pub fn has_metadata(&self) -> bool {
        self.page.bytes[METADATA_OFFSET..NAME_OFFSET] == METADATA_MARKER[..]
    }

    /// The profile's name, or `None` without the metadata marker.
    #[must_use]
    pub fn name(&self) -> Option<String> {
        self.has_metadata()
            .then(|| self.utf16_field(NAME_OFFSET, NAME_UNITS))
    }

    /// The name of macro slot `slot` (`0..`[`MACRO_NAME_COUNT`]), or `None`
    /// without the metadata marker or for a slot out of range.
    #[must_use]
    pub fn macro_name(&self, slot: usize) -> Option<String> {
        (self.has_metadata() && slot < MACRO_NAME_COUNT).then(|| {
            self.utf16_field(
                MACRO_NAMES_OFFSET + slot * MACRO_NAME_UNITS * 2,
                MACRO_NAME_UNITS,
            )
        })
    }

    /// A UTF-16LE string of at most `units` code units, ending at the first
    /// NUL. libratbag keeps only each unit's low byte, which garbles any
    /// non-ASCII name; this decodes the units properly.
    fn utf16_field(&self, start: usize, units: usize) -> String {
        let units = self.page.bytes[start..start + units * 2]
            .as_chunks::<2>()
            .0
            .iter()
            .map(|&unit| u16::from_le_bytes(unit))
            .take_while(|&unit| unit != 0);
        char::decode_utf16(units)
            .map(|c| c.unwrap_or(char::REPLACEMENT_CHARACTER))
            .collect()
    }
}

/// The G700's profile directory: [`ProfileDirectory::parse`] with the G700's
/// [`PROFILE_COUNT`].
#[must_use]
pub fn parse_directory(page: &Page) -> ProfileDirectory {
    ProfileDirectory::parse(page, PROFILE_COUNT)
}

/// A [`MemoryWriter`] for the G700 at `device_index`, with the G700's
/// [`PROFILE_COUNT`].
#[must_use]
pub fn memory_writer(chan: &HidppChannel, device_index: u8) -> MemoryWriter<'_> {
    MemoryWriter::new(chan, device_index, PROFILE_COUNT)
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroU16;

    use super::{
        BUTTON_COUNT, Binding, ButtonMask, DPI_RAW_MAX, DpiLed, DpiMode, G700Profile, KeyModifiers,
        MacroPointer, Special, dpi_from_raw, raw_from_dpi,
    };
    use crate::register::onboard_memory::{PAGE_SIZE, Page};

    fn pseudo_random_page(seed: u32) -> Page {
        let mut bytes = [0u8; PAGE_SIZE];
        let mut state = seed;
        for byte in &mut bytes {
            state ^= state << 13;
            state ^= state >> 17;
            state ^= state << 5;
            *byte = state.to_le_bytes()[1];
        }
        Page::from_bytes(bytes)
    }

    /// A profile shaped like the ones on a G700 (profile 2 of the mouse
    /// these tests were checked against, name "Профиль 3"), with the unused
    /// bytes erased.
    fn synthetic_profile_page() -> Page {
        let mut bytes = [0xff; PAGE_SIZE];
        let head: [u8; 0x23] = [
            0x24, 0x24, 0x12, 0x11, // 850 dpi, LED 1 on
            0x44, 0x44, 0x21, 0x11, // 1600 dpi
            0x59, 0x59, 0x22, 0x11, // 2100 dpi
            0x00, 0x00, 0x11, 0x11, // unused
            0x00, 0x00, 0x11, 0x11, // unused
            0x00, // default mode
            0x80, 0x01, 0x10, // unknown
            0x04, // 250 Hz
            0x01, 0x2c, 0x02, 0x58, 0x64, 0xff, 0xbc, 0x00, 0x09, 0x31,
        ];
        bytes[..head.len()].copy_from_slice(&head);
        let bindings: [[u8; 3]; BUTTON_COUNT] = [
            [0x81, 0x01, 0x00],
            [0x81, 0x02, 0x00],
            [0x81, 0x04, 0x00],
            [0x81, 0x40, 0x00],
            [0x81, 0x20, 0x00],
            [0x81, 0x08, 0x00],
            [0x81, 0x10, 0x00],
            [0x83, 0x03, 0x00],
            [0x81, 0x00, 0x04],
            [0x81, 0x00, 0x02],
            [0x83, 0x11, 0x00],
            [0x83, 0x01, 0x00],
            [0x83, 0x02, 0x00],
        ];
        for (button, binding) in bindings.iter().enumerate() {
            bytes[0x23 + button * 3..0x26 + button * 3].copy_from_slice(binding);
        }
        bytes[0x4a..0x4f].copy_from_slice(b"LGS02");
        bytes[0x4f..0x4f + 46].fill(0);
        for (unit, c) in "Профиль 3".encode_utf16().enumerate() {
            bytes[0x4f + unit * 2..0x51 + unit * 2].copy_from_slice(&c.to_le_bytes());
        }
        bytes[0x7d..499].fill(0);
        let mut page = Page::from_bytes(bytes);
        page.update_crc();
        page
    }

    #[test]
    fn any_page_round_trips_byte_for_byte() {
        for seed in [1, 0xdead_beef, 0x0bad_f00d, 42, 7] {
            let page = pseudo_random_page(seed);
            assert_eq!(G700Profile::parse(&page).to_page(), page, "seed {seed:#x}");
        }
        for fill in [0x00, 0xff] {
            let page = Page::from_bytes([fill; PAGE_SIZE]);
            assert_eq!(G700Profile::parse(&page).to_page(), page, "fill {fill:#x}");
        }
    }

    #[test]
    fn decodes_a_profile_page() {
        let page = synthetic_profile_page();
        let profile = G700Profile::parse(&page);

        let dpi: Vec<_> = profile
            .dpi_modes
            .iter()
            .map(|mode| mode.x_dpi().map(NonZeroU16::get))
            .collect();
        assert_eq!(dpi, [Some(850), Some(1600), Some(2100), None, None]);
        assert_eq!(profile.dpi_modes[0].led(1), Some(DpiLed::On));
        assert_eq!(profile.dpi_modes[0].led(2), Some(DpiLed::Off));
        assert_eq!(profile.dpi_modes[1].led(2), Some(DpiLed::On));
        assert_eq!(profile.dpi_modes[0].led(5), None);
        assert_eq!(profile.dpi_modes[0].led(0), None);
        assert_eq!(profile.default_dpi_mode, 0);
        assert_eq!(profile.report_interval().unwrap().rate_hz(), 250);
        assert_eq!(profile.name().as_deref(), Some("Профиль 3"));
        assert_eq!(profile.macro_name(0).as_deref(), Some(""));
        assert_eq!(profile.macro_name(11), None);
        assert_eq!(
            profile.bindings[3],
            Binding::MouseButtons(ButtonMask(0x0040))
        );
        assert_eq!(
            profile.bindings[8],
            Binding::MouseButtons(ButtonMask(0x0400))
        );
        assert_eq!(profile.bindings[7], Binding::Special(Special::BatteryLevel));
        assert_eq!(
            profile.bindings[10],
            Binding::Special(Special::ProfileCycleUp)
        );
        assert!(page.has_valid_crc());
        assert_eq!(profile.to_page(), page);
    }

    #[test]
    fn a_page_without_the_marker_has_no_names() {
        let mut bytes = *synthetic_profile_page().as_bytes();
        bytes[0x4a] = b'X';
        let profile = G700Profile::parse(&Page::from_bytes(bytes));
        assert!(!profile.has_metadata());
        assert_eq!(profile.name(), None);
        assert_eq!(profile.macro_name(0), None);
    }

    #[test]
    fn invalid_utf16_decodes_with_replacement_characters() {
        let mut bytes = *synthetic_profile_page().as_bytes();
        // A lone high surrogate, then "A".
        bytes[0x4f..0x53].copy_from_slice(&[0x00, 0xd8, 0x41, 0x00]);
        bytes[0x53..0x55].fill(0);
        let profile = G700Profile::parse(&Page::from_bytes(bytes));
        assert_eq!(profile.name().as_deref(), Some("\u{fffd}A"));
    }

    #[test]
    fn a_changed_field_lands_only_in_its_bytes() {
        let page = synthetic_profile_page();
        let mut profile = G700Profile::parse(&page);
        profile.bindings[12] = Binding::Disabled;
        profile.dpi_modes[4] = DpiMode {
            x: 0x10,
            y: 0x11,
            leds: [0x12, 0x21],
        };
        profile.report_interval_millis = 1;

        let changed: Vec<usize> = page
            .as_bytes()
            .iter()
            .zip(profile.to_page().as_bytes())
            .enumerate()
            .filter_map(|(offset, (old, new))| (old != new).then_some(offset))
            .collect();
        assert_eq!(changed, [0x10, 0x11, 0x12, 0x13, 0x18, 0x47, 0x48]);
    }

    #[test]
    fn each_binding_type_decodes_and_encodes_back() {
        for (wire, binding) in [
            (
                [0x81, 0x01, 0x00],
                Binding::MouseButtons(ButtonMask(0x0001)),
            ),
            (
                [0x81, 0x00, 0x80],
                Binding::MouseButtons(ButtonMask(0x8000)),
            ),
            (
                [0x82, 0x05, 0x04],
                Binding::Key {
                    modifiers: KeyModifiers::LEFT_CTRL | KeyModifiers::LEFT_ALT,
                    usage: 0x04,
                },
            ),
            (
                [0x83, 0x05, 0x00],
                Binding::Special(Special::ResolutionCycleUp),
            ),
            (
                [0x83, 0x21, 0x00],
                Binding::Special(Special::ProfileCycleDown),
            ),
            (
                [0x83, 0x06, 0x00],
                Binding::Special(Special::Unknown(0x0006)),
            ),
            (
                [0x83, 0x01, 0x01],
                Binding::Special(Special::Unknown(0x0101)),
            ),
            ([0x84, 0x00, 0xe9], Binding::Consumer(0x00e9)),
            ([0x84, 0x01, 0x83], Binding::Consumer(0x0183)),
            ([0x8f, 0x00, 0x00], Binding::Disabled),
            (
                [0x05, 0x10, 0x00],
                Binding::Macro(MacroPointer {
                    page: 5,
                    word_offset: 0x10,
                }),
            ),
            (
                [0x00, 0x00, 0x00],
                Binding::Macro(MacroPointer {
                    page: 0,
                    word_offset: 0,
                }),
            ),
            // Non-zero padding would be lost, so these stay raw.
            ([0x8f, 0x00, 0x01], Binding::Unknown([0x8f, 0x00, 0x01])),
            ([0x05, 0x10, 0x01], Binding::Unknown([0x05, 0x10, 0x01])),
            // Type bytes libratbag does not know.
            ([0x80, 0x12, 0x34], Binding::Unknown([0x80, 0x12, 0x34])),
            ([0x85, 0x00, 0x00], Binding::Unknown([0x85, 0x00, 0x00])),
            ([0xff, 0xff, 0xff], Binding::Unknown([0xff, 0xff, 0xff])),
        ] {
            assert_eq!(Binding::from_bytes(wire), binding, "{wire:02x?}");
            assert_eq!(binding.to_bytes(), wire, "{binding:?}");
        }
    }

    #[test]
    fn every_binding_encoding_round_trips() {
        for kind in 0..=u8::MAX {
            for first in [0x00, 0x01, 0x7f, 0x80, 0xff] {
                for second in [0x00, 0x01, 0x80, 0xff] {
                    let wire = [kind, first, second];
                    assert_eq!(Binding::from_bytes(wire).to_bytes(), wire, "{wire:02x?}");
                }
            }
        }
    }

    #[test]
    fn button_masks_list_their_buttons_from_one() {
        assert_eq!(ButtonMask(0x0001).buttons().collect::<Vec<_>>(), [1]);
        assert_eq!(
            ButtonMask(0x8401).buttons().collect::<Vec<_>>(),
            [1, 11, 16]
        );
        assert_eq!(ButtonMask(0).buttons().count(), 0);
    }

    #[test]
    fn the_dpi_table_follows_libratbag() {
        assert_eq!(dpi_from_raw(0), None, "an unused mode");
        assert_eq!(dpi_from_raw(DPI_RAW_MAX + 1), None);
        for (raw, dpi) in [
            (1, 25),
            (2, 50),
            (0x11, 400),
            (0x24, 850),
            (0x44, 1600),
            (0x59, 2100),
            (DPI_RAW_MAX, 5700),
        ] {
            assert_eq!(
                dpi_from_raw(raw).map(NonZeroU16::get),
                Some(dpi),
                "raw {raw:#x}"
            );
        }
    }

    #[test]
    fn dpi_to_raw_picks_the_nearest_step_and_the_lowest_on_a_tie() {
        for raw in 1..=DPI_RAW_MAX {
            let dpi = dpi_from_raw(raw).unwrap().get();
            let back = raw_from_dpi(dpi);
            assert_eq!(dpi_from_raw(back).unwrap().get(), dpi, "raw {raw}");
            assert!(
                back <= raw,
                "raw {raw} maps back to the lowest step with its DPI"
            );
        }
        // Steps 8 and 9 share 200 DPI; the lower one wins.
        assert_eq!(dpi_from_raw(8).unwrap().get(), 200);
        assert_eq!(dpi_from_raw(9).unwrap().get(), 200);
        assert_eq!(raw_from_dpi(200), 8);
        assert_eq!(raw_from_dpi(0), 1);
        assert_eq!(raw_from_dpi(410), 17);
        assert_eq!(raw_from_dpi(60_000), DPI_RAW_MAX);
    }
}
