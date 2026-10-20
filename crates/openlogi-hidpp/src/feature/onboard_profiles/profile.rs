//! The bytes of onboard profile memory: the profile directory and the
//! profile sectors — pure data, no I/O.
//!
//! Layout as libratbag (`union hidpp20_internal_profile`) and Solaar
//! (`OnboardProfile.from_bytes`) decode it; Logitech has not published it.
//! Offsets within a profile sector:
//!
//! | offset | field |
//! |---|---|
//! | `0x00` | report interval, ms |
//! | `0x01` | default DPI slot |
//! | `0x02` | DPI-shift slot |
//! | `0x03` | five DPI slots, `u16` little-endian |
//! | `0x20` | sixteen button bindings, then sixteen G-shift bindings |
//! | `0xa0` | name, 24 UTF-16LE code units, `0xff`-filled when unnamed |
//! | `0xd0` | two LED zones, 11 bytes each (format G303 and later) |
//! | `0xe6` | the same two zones' alternate effect |
//!
//! Every sector ends in a big-endian CRC-16/CCITT-FALSE over the bytes
//! before it. The firmware ignores a user sector whose CRC is wrong and
//! falls back to its factory profile, so every edit here re-seals the
//! sector, and edits are made in place: bytes this module does not name
//! (buttons, macros, fields of later formats) are written back unchanged.

use crc::{CRC_16_IBM_3740, Crc};

use super::types::{ProfileFormat, SectorAddress};
use crate::feature::color_led_effects::Rgb;

/// CRC-16/CCITT-FALSE (poly `0x1021`, init `0xffff`), which catalogues name
/// IBM-3740.
const SECTOR_CRC: Crc<u16> = Crc::<u16>::new(&CRC_16_IBM_3740);

/// How many DPI slots a profile holds.
pub const DPI_SLOTS: usize = 5;

/// How many LED zones a profile stores an effect for.
pub const LED_ZONES: usize = 2;

const REPORT_INTERVAL: usize = 0x00;
const DEFAULT_DPI_SLOT: usize = 0x01;
const SHIFT_DPI_SLOT: usize = 0x02;
const DPI: usize = 0x03;
const NAME: usize = 0xa0;
const NAME_LEN: usize = 48;
const LEDS: usize = 0xd0;
const ALT_LEDS: usize = 0xe6;
const LED_LEN: usize = 11;

/// The bytes a profile sector must hold before its CRC: through the end of
/// the alternate LED effects.
const PROFILE_FIELDS_END: usize = ALT_LEDS + LED_ZONES * LED_LEN;

/// Why a sector could not be decoded or edited.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum ProfileError {
    /// The sector is too short to hold the fields of its layout and a CRC.
    #[error("a {len}-byte sector cannot hold an onboard profile")]
    SectorTooShort {
        /// The sector length.
        len: usize,
    },
    /// The trailing CRC does not match the sector's bytes.
    #[error("sector CRC {stored:#06x} does not match its contents ({computed:#06x})")]
    CrcMismatch {
        /// The CRC stored in the sector.
        stored: u16,
        /// The CRC of the bytes before it.
        computed: u16,
    },
    /// A DPI slot index past [`DPI_SLOTS`].
    #[error("DPI slot {0} does not exist")]
    NoSuchDpiSlot(usize),
    /// An LED zone past [`LED_ZONES`], or any zone in a format without LEDs.
    #[error("LED zone {0} does not exist in this profile format")]
    NoSuchLedZone(usize),
    /// A report interval outside 1–8 ms.
    #[error("{0} ms is not a report interval")]
    ReportInterval(u8),
}

/// The CRC the firmware expects at the end of a sector whose other bytes
/// are `data`.
#[must_use]
pub fn sector_crc(data: &[u8]) -> u16 {
    SECTOR_CRC.checksum(data)
}

/// Whether `bytes` ends in the CRC of the bytes before it — whether the
/// firmware would take it as a written sector.
#[must_use]
pub fn is_sealed(bytes: &[u8]) -> bool {
    checked(bytes).is_ok()
}

/// Check that `bytes` ends in the CRC of the bytes before it.
fn checked(bytes: &[u8]) -> Result<(), ProfileError> {
    let Some(body_len) = bytes.len().checked_sub(2) else {
        return Err(ProfileError::SectorTooShort { len: bytes.len() });
    };
    let stored = u16::from_be_bytes([bytes[body_len], bytes[body_len + 1]]);
    let computed = sector_crc(&bytes[..body_len]);
    if stored == computed {
        Ok(())
    } else {
        Err(ProfileError::CrcMismatch { stored, computed })
    }
}

/// Recompute the trailing CRC of `bytes`.
fn seal(bytes: &mut [u8]) {
    let body_len = bytes.len() - 2;
    let crc = sector_crc(&bytes[..body_len]);
    bytes[body_len..].copy_from_slice(&crc.to_be_bytes());
}

/// One entry of the profile directory.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DirectoryEntry {
    /// Where the profile is stored.
    pub sector: SectorAddress,
    /// Whether the profile takes part in profile cycling.
    pub enabled: bool,
}

/// The user profile directory at [`SectorAddress::DIRECTORY`]: which user
/// sectors hold a profile.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProfileDirectory {
    entries: Vec<DirectoryEntry>,
}

impl ProfileDirectory {
    /// End-of-directory marker in place of a sector address.
    const END: u16 = 0xffff;

    /// A directory listing `entries`.
    #[must_use]
    pub fn new(entries: Vec<DirectoryEntry>) -> Self {
        Self { entries }
    }

    /// Decode the directory sector. `None` when it holds none: a device
    /// whose user memory was never written answers with an invalid CRC, or
    /// with an empty list, and then runs its factory profiles.
    #[must_use]
    pub fn decode(bytes: &[u8]) -> Option<Self> {
        checked(bytes).ok()?;
        let body = &bytes[..bytes.len() - 2];
        let entries: Vec<_> = body
            .as_chunks::<4>()
            .0
            .iter()
            .map_while(|entry| {
                let sector = u16::from_be_bytes([entry[0], entry[1]]);
                (sector != Self::END).then_some(DirectoryEntry {
                    sector: SectorAddress::new(sector),
                    enabled: entry[2] != 0,
                })
            })
            .collect();
        (!entries.is_empty()).then_some(Self { entries })
    }

    /// The listed profiles, in order.
    #[must_use]
    pub fn entries(&self) -> &[DirectoryEntry] {
        &self.entries
    }

    /// Encode the directory as a sealed sector of `sector_size` bytes. A
    /// directory too long for the sector is truncated to what fits before
    /// the end marker.
    #[must_use]
    pub fn encode(&self, sector_size: usize) -> Vec<u8> {
        let mut bytes = vec![0xff; sector_size];
        let fits = sector_size.saturating_sub(2 + 4) / 4;
        let mut at = 0;
        for entry in self.entries.iter().take(fits) {
            bytes[at..at + 2].copy_from_slice(&entry.sector.raw().to_be_bytes());
            bytes[at + 2] = u8::from(entry.enabled);
            bytes[at + 3] = 0;
            at += 4;
        }
        bytes[at..at + 4].copy_from_slice(&[0xff, 0xff, 0x00, 0x00]);
        seal(&mut bytes);
        bytes
    }
}

/// The effect one LED zone runs while the profile is active.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProfileLighting {
    /// The zone is dark.
    Off,
    /// One steady color.
    Fixed(Rgb),
    /// The zone cycles through the color wheel.
    Cycle {
        /// One full cycle, in milliseconds.
        period_ms: u16,
        /// Brightness, 1–100 percent.
        brightness: u8,
    },
    /// The zone fades one color in and out.
    Breathing {
        /// The color.
        color: Rgb,
        /// One full breath, in milliseconds.
        period_ms: u16,
        /// Peak brightness, 1–100 percent.
        brightness: u8,
    },
    /// An effect this module does not decode — kept byte for byte unless
    /// replaced.
    Other {
        /// The raw effect id.
        effect: u8,
    },
}

impl ProfileLighting {
    const OFF: u8 = 0x00;
    const FIXED: u8 = 0x01;
    const CYCLE: u8 = 0x03;
    const BREATHING: u8 = 0x0a;

    /// The wire's "0 means full brightness".
    fn brightness(raw: u8) -> u8 {
        if raw == 0 { 100 } else { raw.min(100) }
    }

    fn decode(bytes: &[u8; LED_LEN]) -> Self {
        let color = Rgb {
            red: bytes[1],
            green: bytes[2],
            blue: bytes[3],
        };
        match bytes[0] {
            Self::OFF => Self::Off,
            Self::FIXED => Self::Fixed(color),
            Self::CYCLE => Self::Cycle {
                period_ms: u16::from_be_bytes([bytes[6], bytes[7]]),
                brightness: Self::brightness(bytes[8]),
            },
            Self::BREATHING => Self::Breathing {
                color,
                period_ms: u16::from_be_bytes([bytes[4], bytes[5]]),
                brightness: Self::brightness(bytes[7]),
            },
            effect => Self::Other { effect },
        }
    }

    /// The 11 bytes of this effect, or `None` for [`Self::Other`], whose
    /// bytes only the sector holds.
    fn encode(self) -> Option<[u8; LED_LEN]> {
        let mut bytes = [0; LED_LEN];
        let wire_brightness = |b: u8| if b >= 100 { 0 } else { b.max(1) };
        match self {
            Self::Off => bytes[0] = Self::OFF,
            Self::Fixed(color) => {
                bytes[0] = Self::FIXED;
                bytes[1..4].copy_from_slice(&[color.red, color.green, color.blue]);
            }
            Self::Cycle {
                period_ms,
                brightness,
            } => {
                bytes[0] = Self::CYCLE;
                bytes[6..8].copy_from_slice(&period_ms.to_be_bytes());
                bytes[8] = wire_brightness(brightness);
            }
            Self::Breathing {
                color,
                period_ms,
                brightness,
            } => {
                bytes[0] = Self::BREATHING;
                bytes[1..4].copy_from_slice(&[color.red, color.green, color.blue]);
                bytes[4..6].copy_from_slice(&period_ms.to_be_bytes());
                bytes[7] = wire_brightness(brightness);
            }
            Self::Other { .. } => return None,
        }
        Some(bytes)
    }
}

/// One profile sector, edited in place.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProfileSector {
    format: ProfileFormat,
    bytes: Vec<u8>,
}

impl ProfileSector {
    /// Decode a user profile sector, refusing one whose CRC is wrong: the
    /// firmware does not run such a sector, so it describes nothing.
    pub fn decode(format: ProfileFormat, bytes: Vec<u8>) -> Result<Self, ProfileError> {
        let sector = Self::decode_factory(format, bytes)?;
        checked(&sector.bytes)?;
        Ok(sector)
    }

    /// Decode a factory (ROM) profile sector. Factory sectors are not
    /// CRC-sealed the way user sectors are, so the CRC is not checked; a
    /// copy written to a user sector is sealed by [`Self::encode`].
    pub fn decode_factory(format: ProfileFormat, bytes: Vec<u8>) -> Result<Self, ProfileError> {
        if bytes.len() < PROFILE_FIELDS_END + 2 {
            return Err(ProfileError::SectorTooShort { len: bytes.len() });
        }
        Ok(Self { format, bytes })
    }

    /// The sealed sector, ready to write.
    #[must_use]
    pub fn encode(&self) -> Vec<u8> {
        let mut bytes = self.bytes.clone();
        seal(&mut bytes);
        bytes
    }

    /// The report interval, in milliseconds.
    #[must_use]
    pub fn report_interval_ms(&self) -> u8 {
        self.bytes[REPORT_INTERVAL]
    }

    /// Set the report interval, 1–8 ms.
    pub fn set_report_interval_ms(&mut self, interval_ms: u8) -> Result<(), ProfileError> {
        if !(1..=8).contains(&interval_ms) {
            return Err(ProfileError::ReportInterval(interval_ms));
        }
        self.bytes[REPORT_INTERVAL] = interval_ms;
        Ok(())
    }

    /// The five DPI slots; `None` marks an unused one.
    #[must_use]
    pub fn dpi_slots(&self) -> [Option<u16>; DPI_SLOTS] {
        std::array::from_fn(|slot| {
            let at = DPI + 2 * slot;
            let dpi = u16::from_le_bytes([self.bytes[at], self.bytes[at + 1]]);
            (dpi != 0 && dpi != 0xffff).then_some(dpi)
        })
    }

    /// Set DPI slot `slot`, or clear it with `None`.
    pub fn set_dpi_slot(&mut self, slot: usize, dpi: Option<u16>) -> Result<(), ProfileError> {
        if slot >= DPI_SLOTS {
            return Err(ProfileError::NoSuchDpiSlot(slot));
        }
        let at = DPI + 2 * slot;
        self.bytes[at..at + 2].copy_from_slice(&dpi.unwrap_or(0).to_le_bytes());
        Ok(())
    }

    /// The slot the profile starts in.
    #[must_use]
    pub fn default_dpi_slot(&self) -> u8 {
        self.bytes[DEFAULT_DPI_SLOT]
    }

    /// Set the slot the profile starts in.
    pub fn set_default_dpi_slot(&mut self, slot: u8) -> Result<(), ProfileError> {
        if usize::from(slot) >= DPI_SLOTS {
            return Err(ProfileError::NoSuchDpiSlot(usize::from(slot)));
        }
        self.bytes[DEFAULT_DPI_SLOT] = slot;
        Ok(())
    }

    /// The slot the DPI-shift button selects while held.
    #[must_use]
    pub fn shift_dpi_slot(&self) -> u8 {
        self.bytes[SHIFT_DPI_SLOT]
    }

    /// The profile's name, or `None` when it carries the firmware default.
    #[must_use]
    pub fn name(&self) -> Option<String> {
        let raw = &self.bytes[NAME..NAME + NAME_LEN];
        if raw.iter().all(|&b| b == 0xff) {
            return None;
        }
        let units: Vec<u16> = raw
            .as_chunks::<2>()
            .0
            .iter()
            .map(|unit| u16::from_le_bytes(*unit))
            .take_while(|&unit| unit != 0 && unit != 0xffff)
            .collect();
        let name = String::from_utf16_lossy(&units);
        (!name.is_empty()).then_some(name)
    }

    /// How many LED zones this profile format stores: none in the original
    /// G402 format.
    #[must_use]
    pub fn led_zones(&self) -> usize {
        if self.format == ProfileFormat::G402 {
            0
        } else {
            LED_ZONES
        }
    }

    fn led_offset(&self, zone: usize) -> Result<usize, ProfileError> {
        if zone >= self.led_zones() {
            return Err(ProfileError::NoSuchLedZone(zone));
        }
        Ok(LEDS + zone * LED_LEN)
    }

    /// The effect LED zone `zone` runs.
    pub fn lighting(&self, zone: usize) -> Result<ProfileLighting, ProfileError> {
        let at = self.led_offset(zone)?;
        let mut bytes = [0; LED_LEN];
        bytes.copy_from_slice(&self.bytes[at..at + LED_LEN]);
        Ok(ProfileLighting::decode(&bytes))
    }

    /// Set the effect LED zone `zone` runs, and its alternate effect with
    /// it, as libratbag and G HUB do: the firmware switches to the alternate
    /// effect in states this crate does not model, and a zone set to off
    /// must stay off in all of them. [`ProfileLighting::Other`] names no
    /// bytes, so it leaves the zone as it is.
    pub fn set_lighting(
        &mut self,
        zone: usize,
        lighting: ProfileLighting,
    ) -> Result<(), ProfileError> {
        let at = self.led_offset(zone)?;
        let Some(bytes) = lighting.encode() else {
            return Ok(());
        };
        let alt = ALT_LEDS + zone * LED_LEN;
        self.bytes[at..at + LED_LEN].copy_from_slice(&bytes);
        self.bytes[alt..alt + LED_LEN].copy_from_slice(&bytes);
        Ok(())
    }
}
