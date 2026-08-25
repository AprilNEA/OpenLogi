//! Implements a diagnostics-only accessor for the `OnboardProfiles` feature
//! (ID `0x8100`) that Logitech's G-series gaming mice and keyboards expose
//! instead of `ReprogControls` (`0x1b00`–`0x1b04`) for on-device profile and
//! button-assignment storage.
//!
//! No official Logitech HID++ spec for this feature was available while
//! writing this. Field layout and function ids instead come from
//! [libratbag's hard-fork-quality reverse engineering][libratbag] (the same
//! caliber of source this crate already credits via Solaar elsewhere):
//! [`hidpp20.h`][libratbag-h] for the `getInfo` response struct and
//! [`hidpp20.c`][libratbag-c] for the per-feature function ids and the
//! `MEMORY_READ` sector-read loop. [`OnboardProfilesFeature::get_info`] and
//! [`OnboardProfilesFeature::read_sector`] mirror those two verbatim and have
//! been cross-checked against a real G502 X LIGHTSPEED and G502 LIGHTSPEED
//! (`button_count = 11` matches both mice's physical button count; the 5
//! reserved trailing bytes both read as zero).
//!
//! [`ButtonBinding::parse`] decodes one 4-byte button-binding entry —
//! libratbag's `union hidpp20_button_binding` — again cited verbatim from
//! `hidpp20.h`'s `HIDPP20_BUTTON_*` constants. The *framing* (profile
//! sector 1's button table starting at byte offset 32, 13 populated entries
//! there followed by an `0xff`-padded run, then an identical second copy at
//! offset 96 — almost certainly a G-Shift secondary layer that mirrors the
//! primary one because G-Shift isn't configured) is an **empirical
//! observation from two real profile-sector reads**, not a documented
//! protocol constant, and not general across `profile_format_id` values or
//! device generations. Callers decoding a live sector must not assume offset
//! 32 without confirming it against that device's own directory/profile
//! read first.
//!
//! [`OnboardProfilesFeature::write_sector`] mirrors libratbag's
//! `hidpp20_onboard_profiles_write_sector` (`MEMORY_ADDR_WRITE` /
//! `MEMORY_WRITE` / `MEMORY_WRITE_END`, `hidpp20.c`) and [`crc_ccitt`] mirrors
//! its `hidpp_crc_ccitt` (`hidpp-generic.c`) verbatim, both cited by exact
//! source line, not summarized. [`crc_ccitt`] is independently verified, not
//! just cited: run against the *first `sector_size - 2` bytes* of both real
//! profile sectors this crate captured, it reproduces each sector's own
//! trailing 2 bytes exactly (see the unit tests below) — strong evidence the
//! CRC placement (last 2 bytes of the sector, big-endian, over everything
//! before it) and the algorithm are both right, not just plausible.
//!
//! What this file deliberately does **not** implement: the rest of a
//! profile's layout (name, DPI table, report rate, LED config — everything
//! in libratbag's `struct hidpp20_profile` besides the button array and the
//! CRC trailer).
//!
//! [libratbag]: https://github.com/libratbag/libratbag
//! [libratbag-h]: https://github.com/libratbag/libratbag/blob/master/src/hidpp20.h
//! [libratbag-c]: https://github.com/libratbag/libratbag/blob/master/src/hidpp20.c

#[cfg(test)]
mod tests;

use num_enum::{IntoPrimitive, TryFromPrimitive};
use openlogi_hidpp_derive::Feature;

use crate::{feature::FeatureEndpoint, protocol::v20::Hidpp20Error};

/// HID++2.0 function id for `CMD_ONBOARD_PROFILES_MEMORY_READ` (libratbag
/// `hidpp20.c`), reading 16 bytes of onboard memory at a time.
const FUNCTION_MEMORY_READ: u8 = 5;
/// HID++2.0 function id for `CMD_ONBOARD_PROFILES_MEMORY_ADDR_WRITE`.
const FUNCTION_MEMORY_ADDR_WRITE: u8 = 6;
/// HID++2.0 function id for `CMD_ONBOARD_PROFILES_MEMORY_WRITE`.
const FUNCTION_MEMORY_WRITE: u8 = 7;
/// HID++2.0 function id for `CMD_ONBOARD_PROFILES_MEMORY_WRITE_END`.
const FUNCTION_MEMORY_WRITE_END: u8 = 8;

/// Seed libratbag's `hidpp_crc_ccitt` (`hidpp-generic.c`) uses.
const CRC_CCITT_SEED: u16 = 0xffff;

/// Implements the `OnboardProfiles` / `0x8100` feature.
#[derive(Clone, Feature)]
#[creatable(id = 0x8100, version = 0)]
pub struct OnboardProfilesFeature {
    /// The endpoint this feature talks to.
    endpoint: FeatureEndpoint,
}

impl OnboardProfilesFeature {
    /// Calls function `0` (`CMD_ONBOARD_PROFILES_GET_PROFILES_DESCR`) and
    /// decodes the response into [`OnboardProfilesInfo`].
    pub async fn get_info(&self) -> Result<OnboardProfilesInfo, Hidpp20Error> {
        let payload = self.endpoint.call(0, [0; 3]).await?.extend_payload();
        Ok(OnboardProfilesInfo {
            memory_model_id: payload[0],
            profile_format_id: payload[1],
            macro_format_id: payload[2],
            profile_count: payload[3],
            profile_count_oob: payload[4],
            button_count: payload[5],
            sector_count: payload[6],
            sector_size: u16::from_be_bytes([payload[7], payload[8]]),
            mechanical_layout: payload[9],
            various_info: payload[10],
        })
    }

    /// Reads `sector_size` bytes of onboard memory starting at `sector`,
    /// 16 bytes per `MEMORY_READ` request.
    ///
    /// Mirrors libratbag's `hidpp20_onboard_profiles_read_sector` exactly,
    /// including its handling of a `sector_size` that isn't a multiple of 16:
    /// the last request is shifted back so it still ends exactly at
    /// `sector_size`, overlapping the previous chunk by a few bytes rather
    /// than reading past the sector.
    ///
    /// `sector_size` must be at least 16 — every onboard-profile device
    /// known to libratbag reports one in the hundreds of bytes (this crate
    /// observed 255 on a G502 X LIGHTSPEED and a G502 LIGHTSPEED).
    pub async fn read_sector(
        &self,
        sector: u16,
        sector_size: u16,
    ) -> Result<Vec<u8>, Hidpp20Error> {
        debug_assert!(sector_size >= 16, "sector_size must be at least 16");
        let mut data = vec![0u8; usize::from(sector_size)];
        let mut offset = 0u16;
        loop {
            let read_offset = if sector_size - offset < 16 {
                sector_size - 16
            } else {
                offset
            };

            let [sector_hi, sector_lo] = sector.to_be_bytes();
            let [offset_hi, offset_lo] = read_offset.to_be_bytes();
            let mut args = [0u8; 16];
            args[..4].copy_from_slice(&[sector_hi, sector_lo, offset_hi, offset_lo]);

            let chunk = self
                .endpoint
                .call_long(FUNCTION_MEMORY_READ, args)
                .await?
                .extend_payload();
            let start = usize::from(read_offset);
            data[start..start + 16].copy_from_slice(&chunk);

            if offset + 16 >= sector_size {
                break;
            }
            offset += 16;
        }
        Ok(data)
    }

    /// Writes a full sector's worth of onboard memory: computes and stamps
    /// the trailing CRC (see [`crc_ccitt`]), then `MEMORY_ADDR_WRITE` +
    /// repeated `MEMORY_WRITE` + `MEMORY_WRITE_END`, mirroring libratbag's
    /// `hidpp20_onboard_profiles_write_sector` exactly — including sending
    /// `data.len()` rounded up to a 16-byte boundary in `MEMORY_WRITE`
    /// chunks (the tail is padded with `0xff`) while `MEMORY_ADDR_WRITE`'s
    /// `count` field still carries the unpadded `data.len()`, because that
    /// mismatch is what libratbag's own field-tested implementation does.
    ///
    /// `data` must be at least 2 bytes (for the CRC) — the last 2 bytes are
    /// overwritten unconditionally with the freshly computed CRC before
    /// sending, so callers do not need to (and should not) stamp it
    /// themselves.
    ///
    /// This performs a real write to onboard flash. Read the sector back
    /// afterwards and compare against what was intended — this crate does
    /// not do that verification for the caller.
    pub async fn write_sector(&self, sector: u16, data: &mut [u8]) -> Result<(), Hidpp20Error> {
        debug_assert!(data.len() >= 2, "data must hold at least the CRC trailer");
        let crc = crc_ccitt(&data[..data.len() - 2]);
        let trailer_start = data.len() - 2;
        data[trailer_start..].copy_from_slice(&crc.to_be_bytes());

        let sector_size = u16::try_from(data.len()).unwrap_or(u16::MAX);
        let [sector_hi, sector_lo] = sector.to_be_bytes();
        let [size_hi, size_lo] = sector_size.to_be_bytes();
        let mut start_args = [0u8; 16];
        start_args[..6].copy_from_slice(&[sector_hi, sector_lo, 0, 0, size_hi, size_lo]);
        self.endpoint
            .call_long(FUNCTION_MEMORY_ADDR_WRITE, start_args)
            .await?;

        let padded_len = data.len().next_multiple_of(16);
        let mut padded = vec![0xffu8; padded_len];
        padded[..data.len()].copy_from_slice(data);
        for &args in padded.as_chunks::<16>().0 {
            self.endpoint.call_long(FUNCTION_MEMORY_WRITE, args).await?;
        }

        self.endpoint
            .call(FUNCTION_MEMORY_WRITE_END, [0; 3])
            .await?;
        Ok(())
    }
}

/// Port of libratbag's `hidpp_crc_ccitt` (`hidpp-generic.c`). See the module
/// docs for how this was verified against real onboard-profile sectors.
#[must_use]
pub fn crc_ccitt(data: &[u8]) -> u16 {
    let mut crc = CRC_CCITT_SEED;
    for &byte in data {
        let temp = (crc >> 8) ^ u16::from(byte);
        crc <<= 8;
        let mut quick = temp ^ (temp >> 4);
        crc ^= quick;
        quick <<= 5;
        crc ^= quick;
        quick <<= 7;
        crc ^= quick;
    }
    crc
}

/// Decoded response of `CMD_ONBOARD_PROFILES_GET_PROFILES_DESCR` (function
/// `0`) — mirrors libratbag's `struct hidpp20_onboard_profiles_info`
/// verbatim, field for field.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
#[non_exhaustive]
pub struct OnboardProfilesInfo {
    /// Identifies the onboard memory layout generation (e.g. libratbag's
    /// `HIDPP20_USER_PROFILES_G402` family vs. later ones).
    pub memory_model_id: u8,
    /// Identifies the byte layout of one profile sector.
    pub profile_format_id: u8,
    /// Identifies the byte layout of macro data referenced from a profile.
    pub macro_format_id: u8,
    /// Number of user-writable profile slots.
    pub profile_count: u8,
    /// Number of profile slots pre-populated out of the box (ROM defaults).
    pub profile_count_oob: u8,
    /// Number of physical buttons the device's profiles carry bindings for.
    pub button_count: u8,
    /// Number of addressable memory sectors.
    pub sector_count: u8,
    /// Bytes per sector — the unit [`OnboardProfilesFeature::read_sector`]
    /// reads.
    pub sector_size: u16,
    /// Device-specific mechanical layout identifier.
    pub mechanical_layout: u8,
    /// Device-specific info bitfield; libratbag does not decode this further.
    pub various_info: u8,
}

/// One entry from the profile-directory sector (`0x0000`, present on every
/// device libratbag documents). Not itself a libratbag struct — libratbag
/// reads the address inline in `hidpp20_onboard_profiles_initialize` rather
/// than naming a type for it — but the 4-byte-entry framing and the address
/// field match `get_unaligned_be_u16(d)` there, and this crate cross-checked
/// it against two real profile directories: exactly `profile_count` entries
/// populated (addresses `1..=profile_count`, i.e. profile `N` lives in
/// sector `N`), followed by `0xff`-padding to the sector end.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
#[non_exhaustive]
pub struct ProfileDirectoryEntry {
    /// The sector this profile's data lives in — pass to
    /// [`OnboardProfilesFeature::read_sector`].
    pub address: u16,
    /// Third byte of the entry. Observed nonzero on some populated entries
    /// and zero on others inconsistently with [`OnboardProfilesInfo`]'s
    /// `profile_count_oob`, so its meaning is **not** established — exposed
    /// raw rather than guessed at.
    pub flag: u8,
}

/// Parses the profile-directory sector (sector `0x0000`) into its populated
/// entries, stopping at the first unpopulated (`0xff 0xff`-address) entry —
/// see [`ProfileDirectoryEntry`] for how confident that framing is.
#[must_use]
pub fn parse_profile_directory(sector0: &[u8]) -> Vec<ProfileDirectoryEntry> {
    sector0
        .as_chunks::<4>()
        .0
        .iter()
        .map_while(|entry| {
            let address = u16::from_be_bytes([entry[0], entry[1]]);
            (address != 0xffff).then_some(ProfileDirectoryEntry {
                address,
                flag: entry[2],
            })
        })
        .collect()
}

/// `HIDPP20_BUTTON_SPECIAL_*` opcodes (libratbag `hidpp20.h`), used inside
/// [`ButtonBinding::Special`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, IntoPrimitive, TryFromPrimitive)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
#[non_exhaustive]
#[repr(u8)]
pub enum SpecialFunction {
    /// `HIDPP20_BUTTON_SPECIAL_NOOP`.
    Noop = 0x00,
    /// `HIDPP20_BUTTON_SPECIAL_TILT_LEFT` — wheel tilt left.
    TiltLeft = 0x01,
    /// `HIDPP20_BUTTON_SPECIAL_TILT_RIGHT` — wheel tilt right.
    TiltRight = 0x02,
    /// `HIDPP20_BUTTON_SPECIAL_NEXT_DPI`.
    NextDpi = 0x03,
    /// `HIDPP20_BUTTON_SPECIAL_PREV_DPI`.
    PrevDpi = 0x04,
    /// `HIDPP20_BUTTON_SPECIAL_CYCLE_DPI`.
    CycleDpi = 0x05,
    /// `HIDPP20_BUTTON_SPECIAL_DEFAULT_DPI`.
    DefaultDpi = 0x06,
    /// `HIDPP20_BUTTON_SPECIAL_SHIFT_DPI` — hold for a temporary DPI (a
    /// "sniper button").
    ShiftDpi = 0x07,
    /// `HIDPP20_BUTTON_SPECIAL_NEXT_PROFILE`.
    NextProfile = 0x08,
    /// `HIDPP20_BUTTON_SPECIAL_PREV_PROFILE`.
    PrevProfile = 0x09,
    /// `HIDPP20_BUTTON_SPECIAL_CYCLE_PROFILE`.
    CycleProfile = 0x0a,
    /// `HIDPP20_BUTTON_SPECIAL_GSHIFT` — hold for the secondary button
    /// layer.
    GShift = 0x0b,
    /// `HIDPP20_BUTTON_SPECIAL_BATTERY_INDICATOR`.
    BatteryIndicator = 0x0c,
    /// `HIDPP20_BUTTON_SPECIAL_ENABLE_PROFILE`.
    EnableProfile = 0x0d,
    /// `HIDPP20_BUTTON_SPECIAL_PERFORMANCE_SWITCH`.
    PerformanceSwitch = 0x0e,
    /// `HIDPP20_BUTTON_SPECIAL_HOST` — switch paired host.
    Host = 0x0f,
    /// `HIDPP20_BUTTON_SPECIAL_SCROLL_DOWN`.
    ScrollDown = 0x10,
    /// `HIDPP20_BUTTON_SPECIAL_SCROLL_UP`.
    ScrollUp = 0x11,
}

/// One decoded entry from a profile's button-binding table — 4 bytes on the
/// wire, libratbag's `union hidpp20_button_binding`. Tag/subtype/opcode
/// values are cited verbatim from libratbag's `hidpp20.h`
/// (`HIDPP20_BUTTON_*` constants) — see the module docs for how this framing
/// was cross-checked against real hardware.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
#[non_exhaustive]
pub enum ButtonBinding {
    /// `HIDPP20_BUTTON_HID_TYPE` / `_MOUSE` — a standard HID mouse-button
    /// bitmask (bit `N` set means mouse button `N + 1`).
    Mouse {
        /// The bitmask.
        buttons: u16,
    },
    /// `HIDPP20_BUTTON_HID_TYPE` / `_KEYBOARD` — a standard HID keyboard
    /// key, with modifier flags.
    Keyboard {
        /// HID keyboard modifier bitmask (ctrl/shift/alt/gui, left/right).
        modifier_flags: u8,
        /// HID keyboard usage id.
        key: u8,
    },
    /// `HIDPP20_BUTTON_HID_TYPE` / `_CONSUMER_CONTROL` — a standard HID
    /// consumer-control usage code (media keys and similar).
    ConsumerControl {
        /// HID consumer-page usage id.
        usage: u16,
    },
    /// `HIDPP20_BUTTON_SPECIAL` — a device-specific function.
    Special(SpecialFunction),
    /// `HIDPP20_BUTTON_MACRO` — a macro stored elsewhere on the device.
    Macro {
        /// Which macro page it lives on.
        page: u8,
        /// Byte offset within that page.
        offset: u8,
    },
    /// `HIDPP20_BUTTON_DISABLED` (`0xff`) — the slot carries no binding.
    Disabled,
}

impl ButtonBinding {
    /// Decodes one 4-byte button-binding entry.
    ///
    /// # Errors
    ///
    /// Returns [`Hidpp20Error::UnsupportedResponse`] for a `type`/`subtype`
    /// combination not in libratbag's `HIDPP20_BUTTON_*` constants, or a
    /// `HIDPP20_BUTTON_SPECIAL` opcode [`SpecialFunction`] doesn't cover —
    /// per this crate's rule of surfacing an unknown wire value as an error
    /// rather than silently guessing at it.
    pub fn parse(entry: [u8; 4]) -> Result<Self, Hidpp20Error> {
        const HID_TYPE: u8 = 0x80;
        const SPECIAL_TYPE: u8 = 0x90;
        const MACRO_TYPE: u8 = 0x00;
        const DISABLED_TYPE: u8 = 0xff;
        const HID_SUBTYPE_MOUSE: u8 = 0x01;
        const HID_SUBTYPE_KEYBOARD: u8 = 0x02;
        const HID_SUBTYPE_CONSUMER_CONTROL: u8 = 0x03;

        match entry[0] {
            DISABLED_TYPE => Ok(Self::Disabled),
            MACRO_TYPE => Ok(Self::Macro {
                page: entry[1],
                offset: entry[3],
            }),
            HID_TYPE => match entry[1] {
                HID_SUBTYPE_MOUSE => Ok(Self::Mouse {
                    buttons: u16::from_be_bytes([entry[2], entry[3]]),
                }),
                HID_SUBTYPE_KEYBOARD => Ok(Self::Keyboard {
                    modifier_flags: entry[2],
                    key: entry[3],
                }),
                HID_SUBTYPE_CONSUMER_CONTROL => Ok(Self::ConsumerControl {
                    usage: u16::from_be_bytes([entry[2], entry[3]]),
                }),
                _ => Err(Hidpp20Error::UnsupportedResponse),
            },
            SPECIAL_TYPE => SpecialFunction::try_from(entry[1])
                .map(Self::Special)
                .map_err(|_| Hidpp20Error::UnsupportedResponse),
            _ => Err(Hidpp20Error::UnsupportedResponse),
        }
    }
}
