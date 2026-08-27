//! Implements the `OnboardProfiles` feature (ID `0x8100`).
//!
//! Gaming mice such as the G502 store DPI, report rate, and per-button
//! bindings in flash sectors rather than exposing HID++ `0x1b04`
//! ReprogControls. Host software must keep the device in onboard mode and
//! rewrite the active profile sector for a remap to stick.
//!
//! Function addresses and the packed G402-family profile layout follow
//! Logitech's HID++ 2.0 documentation and the libratbag `hidpp20`
//! implementation. Sector CRC is CRC-16/CCITT-FALSE (poly `0x1021`, init
//! `0xFFFF`). Button-binding offsets inside that layout were reverse-engineered
//! (`buttons` starts at byte 32) and are marked below. G502 Proteus Spectrum
//! reports profile format `0x02` (G303), which shares that button table.

use openlogi_hidpp_derive::Feature;

use crate::{feature::FeatureEndpoint, protocol::v20::Hidpp20Error};

/// HID++ `0x8100` onboard-vs-host execution mode.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OnboardMode {
    /// Leave the current mode unchanged (write-only sentinel).
    NoChange,
    /// Firmware executes the onboard profile (required for profile writes).
    Onboard,
    /// Host owns the buttons; onboard profile contents are ignored.
    Host,
}

impl OnboardMode {
    const fn from_wire(byte: u8) -> Result<Self, Hidpp20Error> {
        match byte {
            0x00 => Ok(Self::NoChange),
            0x01 => Ok(Self::Onboard),
            0x02 => Ok(Self::Host),
            _ => Err(Hidpp20Error::UnsupportedResponse),
        }
    }

    const fn to_wire(self) -> u8 {
        match self {
            Self::NoChange => 0x00,
            Self::Onboard => 0x01,
            Self::Host => 0x02,
        }
    }
}

/// Memory-model identifier from `getProfilesDescription`.
///
/// `0x01` is the G402-family layout used by G502 Proteus Spectrum / Hero.
pub const MEMORY_MODEL_G402: u8 = 0x01;

/// Profile-format identifier for the original G402 256-byte profile.
pub const PROFILE_FORMAT_G402: u8 = 0x01;

/// Profile-format identifier for G303 / G403 / G502 Proteus (same button table).
pub const PROFILE_FORMAT_G303: u8 = 0x02;

/// Profile-format identifier for G900.
pub const PROFILE_FORMAT_G900: u8 = 0x03;

/// Profile-format identifier for G915.
pub const PROFILE_FORMAT_G915: u8 = 0x04;

/// Profile-format identifier for G502 X.
pub const PROFILE_FORMAT_G502X: u8 = 0x05;

/// Directory sector that lists user-profile flash addresses.
pub const USER_PROFILE_DIRECTORY: u16 = 0x0000;

/// Byte offset of `buttons[0]` in a G402-format profile sector.
///
/// Reverse-engineered from libratbag's packed `hidpp20_internal_profile`
/// (report rate + DPI table + colour + power fields occupy bytes 0..=31).
pub const G402_BUTTONS_OFFSET: usize = 32;

/// Size of one onboard button binding.
pub const BUTTON_BINDING_LEN: usize = 4;

/// Maximum G402-format button slots (`buttons[16]`).
pub const G402_BUTTON_SLOTS: usize = 16;

/// Directory-entry size in the user-profile directory sector.
pub const PROFILE_DIRECTORY_ENTRY_LEN: usize = 4;

/// Implements the `OnboardProfiles` / `0x8100` feature.
#[derive(Clone, Feature)]
#[creatable(id = 0x8100, version = 0)]
pub struct OnboardProfilesFeature {
    /// The endpoint this feature talks to.
    endpoint: FeatureEndpoint,
}

/// Description of onboard profile flash geometry, from function `0x00`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ProfilesDescription {
    /// Memory-model identifier (`0x01` = G402).
    pub memory_model_id: u8,
    /// Profile-format identifier (`0x01` = G402, `0x02` = G303/G502 Proteus,
    /// `0x05` = G502 X).
    pub profile_format_id: u8,
    /// Macro-format identifier.
    pub macro_format_id: u8,
    /// Number of user profiles.
    pub profile_count: u8,
    /// Number of ROM / out-of-box profiles.
    pub rom_profile_count: u8,
    /// Physical buttons stored in each profile.
    pub button_count: u8,
    /// Number of flash sectors.
    pub sector_count: u8,
    /// Bytes per sector, typically 16 or 256.
    pub sector_size: u16,
    /// Mechanical-layout bitfield (G-shift / DPI-shift flags).
    pub mechanical_layout: u8,
    /// Corded / wireless capability bits.
    pub various_info: u8,
}

impl ProfilesDescription {
    /// Parse the 16-byte `getProfilesDescription` payload.
    ///
    /// `sector_size` is big-endian at bytes 7..=8 (unaligned on the wire).
    pub fn from_payload(payload: &[u8]) -> Result<Self, Hidpp20Error> {
        if payload.len() < 11 {
            return Err(Hidpp20Error::UnsupportedResponse);
        }
        Ok(Self {
            memory_model_id: payload[0],
            profile_format_id: payload[1],
            macro_format_id: payload[2],
            profile_count: payload[3],
            rom_profile_count: payload[4],
            button_count: payload[5],
            sector_count: payload[6],
            sector_size: u16::from_be_bytes([payload[7], payload[8]]),
            mechanical_layout: payload[9],
            various_info: payload[10],
        })
    }

    /// Whether this description is the G402 memory model we can read/write.
    #[must_use]
    pub const fn is_g402_memory(self) -> bool {
        self.memory_model_id == MEMORY_MODEL_G402
    }

    /// Whether the profile body uses the packed G402-family layout whose
    /// `buttons[]` table starts at [`G402_BUTTONS_OFFSET`]. G502 Proteus
    /// Spectrum reports format `0x02` (G303); G402 is `0x01`. Later G900 /
    /// G915 / G502 X formats keep the same button table.
    #[must_use]
    pub const fn has_g402_button_table(self) -> bool {
        matches!(
            self.profile_format_id,
            PROFILE_FORMAT_G402
                | PROFILE_FORMAT_G303
                | PROFILE_FORMAT_G900
                | PROFILE_FORMAT_G915
                | PROFILE_FORMAT_G502X
        )
    }

    /// Whether the profile body uses the original G402 format id.
    #[must_use]
    pub const fn is_g402_profile(self) -> bool {
        self.profile_format_id == PROFILE_FORMAT_G402
    }
}

impl OnboardProfilesFeature {
    /// Read flash geometry and profile counts.
    pub async fn get_profiles_description(&self) -> Result<ProfilesDescription, Hidpp20Error> {
        let payload = self.endpoint.call(0, [0; 3]).await?.extend_payload();
        ProfilesDescription::from_payload(&payload)
    }

    /// Switch between onboard and host mode.
    ///
    /// Profile sector writes are ignored unless the device is in
    /// [`OnboardMode::Onboard`].
    pub async fn set_onboard_mode(&self, mode: OnboardMode) -> Result<(), Hidpp20Error> {
        self.endpoint.call(1, [mode.to_wire(), 0x00, 0x00]).await?;
        Ok(())
    }

    /// Read the current onboard/host mode.
    pub async fn get_onboard_mode(&self) -> Result<OnboardMode, Hidpp20Error> {
        let payload = self.endpoint.call(2, [0; 3]).await?.extend_payload();
        OnboardMode::from_wire(payload[0])
    }

    /// Select the active onboard profile.
    ///
    /// `index` is 0-based. The firmware function takes a 1-based index in
    /// parameter byte 1 (libratbag `set_current_profile`).
    pub async fn set_current_profile(&self, index: u8) -> Result<(), Hidpp20Error> {
        self.endpoint
            .call(3, [0x00, index.saturating_add(1), 0x00])
            .await?;
        Ok(())
    }

    /// Read the firmware-reported active profile index (parameter byte 1).
    ///
    /// Some G502-family devices report this 1-based; the caller applies
    /// [`decode_active_profile_index`].
    pub async fn get_current_profile_raw(&self) -> Result<u8, Hidpp20Error> {
        let payload = self.endpoint.call(4, [0; 3]).await?.extend_payload();
        Ok(payload[1])
    }

    /// Read `sector_size` bytes from `sector`, 16 bytes at a time.
    pub async fn read_sector(
        &self,
        sector: u16,
        sector_size: u16,
    ) -> Result<Vec<u8>, Hidpp20Error> {
        if sector_size < 16 || !sector_size.is_multiple_of(16) {
            return Err(Hidpp20Error::UnsupportedResponse);
        }
        let mut data = vec![0u8; usize::from(sector_size)];
        let mut offset: u16 = 0;
        while offset < sector_size {
            // Firmware rejects a read that would pass `sector_size`; when
            // fewer than 16 bytes remain, reread the last aligned window.
            let read_at = if sector_size - offset < 16 {
                sector_size - 16
            } else {
                offset
            };
            let chunk = self.read_chunk(sector, read_at).await?;
            let dest = usize::from(read_at);
            data[dest..dest + 16].copy_from_slice(&chunk);
            offset = read_at.saturating_add(16);
        }
        Ok(data)
    }

    async fn read_chunk(&self, sector: u16, offset: u16) -> Result<[u8; 16], Hidpp20Error> {
        let mut args = [0u8; 16];
        args[0..2].copy_from_slice(&sector.to_be_bytes());
        args[2..4].copy_from_slice(&offset.to_be_bytes());
        Ok(self.endpoint.call_long(5, args).await?.extend_payload())
    }

    /// Write a full sector, optionally refreshing the trailing CRC.
    pub async fn write_sector(
        &self,
        sector: u16,
        mut data: Vec<u8>,
        write_crc: bool,
    ) -> Result<(), Hidpp20Error> {
        let sector_size =
            u16::try_from(data.len()).map_err(|_| Hidpp20Error::UnsupportedResponse)?;
        if sector_size < 16 || !sector_size.is_multiple_of(16) {
            return Err(Hidpp20Error::UnsupportedResponse);
        }
        if write_crc {
            let crc = crc_ccitt(&data[..data.len() - 2]);
            let last = data.len() - 2;
            data[last..].copy_from_slice(&crc.to_be_bytes());
        }
        self.write_start(sector, 0, sector_size).await?;
        for chunk in data.as_chunks::<16>().0 {
            self.endpoint.call_long(7, *chunk).await?;
        }
        self.endpoint.call(8, [0; 3]).await?;
        Ok(())
    }

    async fn write_start(
        &self,
        sector: u16,
        sub_address: u16,
        count: u16,
    ) -> Result<(), Hidpp20Error> {
        let mut args = [0u8; 16];
        args[0..2].copy_from_slice(&sector.to_be_bytes());
        args[2..4].copy_from_slice(&sub_address.to_be_bytes());
        args[4..6].copy_from_slice(&count.to_be_bytes());
        self.endpoint.call_long(6, args).await?;
        Ok(())
    }
}

/// CRC-16/CCITT-FALSE over `data` (poly `0x1021`, init `0xFFFF`, xorout 0).
#[must_use]
pub fn crc_ccitt(data: &[u8]) -> u16 {
    let mut crc: u16 = 0xffff;
    for &byte in data {
        crc ^= u16::from(byte) << 8;
        for _ in 0..8 {
            if crc & 0x8000 == 0 {
                crc <<= 1;
            } else {
                crc = (crc << 1) ^ 0x1021;
            }
        }
    }
    crc
}

/// Whether `data`'s trailing two bytes match [`crc_ccitt`] of the rest.
#[must_use]
pub fn sector_crc_valid(data: &[u8]) -> bool {
    if data.len() < 2 {
        return false;
    }
    let split = data.len() - 2;
    let expected = u16::from_be_bytes([data[split], data[split + 1]]);
    crc_ccitt(&data[..split]) == expected
}

/// Convert a firmware-reported profile index into a 0-based slot.
///
/// G502 Proteus Spectrum / Hero report a 1-based index (`INDEX_OFFSET` in
/// libratbag). `one_based` is true for those PIDs; other 0x8100 devices keep
/// the raw value.
#[must_use]
pub fn decode_active_profile_index(raw: u8, profile_count: u8, one_based: bool) -> u8 {
    if profile_count == 0 {
        return 0;
    }
    let max = profile_count.saturating_sub(1);
    if one_based {
        raw.saturating_sub(1).min(max)
    } else {
        raw.min(max)
    }
}

/// USB product IDs whose `getCurrentProfile` index is 1-based.
#[must_use]
pub const fn profile_index_is_one_based(product_id: u16) -> bool {
    matches!(
        product_id,
        0xc332 // G502 Proteus Spectrum
            | 0xc07d // G502 (registry / Proteus Core)
            | 0xc08b // G502 Hero
    )
}

/// One 4-byte onboard button binding.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ButtonBinding {
    /// Raw little-layout bytes as stored in the profile sector.
    pub bytes: [u8; BUTTON_BINDING_LEN],
}

impl ButtonBinding {
    /// Disabled / no-op binding (`type = 0xFF`).
    #[must_use]
    pub const fn disabled() -> Self {
        Self {
            bytes: [0xff, 0x00, 0x00, 0x00],
        }
    }

    /// HID mouse button `n` (1 = left … 5 = forward).
    #[must_use]
    pub fn hid_mouse(button: u8) -> Self {
        let bit = 1u16 << u16::from(button.saturating_sub(1));
        let [hi, lo] = bit.to_be_bytes();
        Self {
            bytes: [0x80, 0x01, hi, lo],
        }
    }

    /// HID keyboard report: modifier bitmap + usage ID.
    #[must_use]
    pub const fn hid_keyboard(modifiers: u8, usage: u8) -> Self {
        Self {
            bytes: [0x80, 0x02, modifiers, usage],
        }
    }

    /// HID consumer-control usage (big-endian).
    #[must_use]
    pub const fn hid_consumer(usage: u16) -> Self {
        let [hi, lo] = usage.to_be_bytes();
        Self {
            bytes: [0x80, 0x03, hi, lo],
        }
    }

    /// Firmware special opcode (`0x90`).
    #[must_use]
    pub const fn special(opcode: u8) -> Self {
        Self {
            bytes: [0x90, opcode, 0x00, 0x00],
        }
    }

    /// Binding type byte.
    #[must_use]
    pub const fn kind(self) -> u8 {
        self.bytes[0]
    }
}

/// Firmware special opcodes (`type = 0x90`).
pub mod special {
    /// Tilt wheel left.
    pub const TILT_LEFT: u8 = 0x01;
    /// Tilt wheel right.
    pub const TILT_RIGHT: u8 = 0x02;
    /// Next DPI preset.
    pub const NEXT_DPI: u8 = 0x03;
    /// Previous DPI preset.
    pub const PREV_DPI: u8 = 0x04;
    /// Cycle DPI presets.
    pub const CYCLE_DPI: u8 = 0x05;
    /// DPI shift / sniper (hold).
    pub const SHIFT_DPI: u8 = 0x07;
    /// Next onboard profile.
    pub const NEXT_PROFILE: u8 = 0x08;
    /// Previous onboard profile.
    pub const PREV_PROFILE: u8 = 0x09;
    /// Cycle onboard profiles.
    pub const CYCLE_PROFILE: u8 = 0x0a;
    /// G-shift modifier.
    pub const GSHIFT: u8 = 0x0b;
    /// Vertical scroll down.
    pub const SCROLL_DOWN: u8 = 0x10;
    /// Vertical scroll up.
    pub const SCROLL_UP: u8 = 0x11;
}

/// Patch G402-format `buttons[slot]` in a profile sector.
///
/// Reverse-engineered offset: see [`G402_BUTTONS_OFFSET`].
pub fn patch_g402_button(sector: &mut [u8], slot: usize, binding: ButtonBinding) -> bool {
    let start = G402_BUTTONS_OFFSET + slot * BUTTON_BINDING_LEN;
    let end = start + BUTTON_BINDING_LEN;
    if end > sector.len().saturating_sub(2) || slot >= G402_BUTTON_SLOTS {
        return false;
    }
    sector[start..end].copy_from_slice(&binding.bytes);
    true
}

/// Read G402-format `buttons[slot]` from a profile sector.
#[must_use]
pub fn read_g402_button(sector: &[u8], slot: usize) -> Option<ButtonBinding> {
    let start = G402_BUTTONS_OFFSET + slot * BUTTON_BINDING_LEN;
    let end = start + BUTTON_BINDING_LEN;
    if end > sector.len() {
        return None;
    }
    let mut bytes = [0u8; BUTTON_BINDING_LEN];
    bytes.copy_from_slice(&sector[start..end]);
    Some(ButtonBinding { bytes })
}

/// Parse the user-profile directory (sector 0). Each entry is a big-endian
/// sector address plus an enabled flag at byte 2 (`0x02` = enabled).
#[must_use]
pub fn parse_profile_directory(data: &[u8], profile_count: u8) -> Vec<ProfileDirectoryEntry> {
    let n = usize::from(profile_count);
    let mut entries = Vec::with_capacity(n);
    for i in 0..n {
        let start = i * PROFILE_DIRECTORY_ENTRY_LEN;
        let end = start + PROFILE_DIRECTORY_ENTRY_LEN;
        if end > data.len() {
            break;
        }
        let address = u16::from_be_bytes([data[start], data[start + 1]]);
        if address == 0xffff {
            break;
        }
        entries.push(ProfileDirectoryEntry {
            address,
            enabled: data[start + 2] != 0,
        });
    }
    entries
}

/// One row of the onboard profile directory.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ProfileDirectoryEntry {
    /// Flash sector holding this profile.
    pub address: u16,
    /// Whether the profile is enabled.
    pub enabled: bool,
}

#[cfg(test)]
mod tests {
    use super::{
        ButtonBinding, G402_BUTTONS_OFFSET, ProfilesDescription, crc_ccitt,
        decode_active_profile_index, parse_profile_directory, patch_g402_button,
        profile_index_is_one_based, read_g402_button, sector_crc_valid, special,
    };

    #[test]
    fn crc_ccitt_false_matches_the_published_vector() {
        assert_eq!(crc_ccitt(b"123456789"), 0x29b1);
    }

    #[test]
    fn sector_crc_round_trips() {
        let mut data = vec![0u8; 16];
        data[..5].copy_from_slice(b"hello");
        let crc = crc_ccitt(&data[..14]);
        data[14..].copy_from_slice(&crc.to_be_bytes());
        assert!(sector_crc_valid(&data));
        data[0] ^= 1;
        assert!(!sector_crc_valid(&data));
    }

    #[test]
    fn parses_g402_profiles_description() {
        let mut payload = [0u8; 16];
        payload[0] = 0x01;
        payload[1] = 0x01;
        payload[2] = 0x01;
        payload[3] = 0x03;
        payload[4] = 0x01;
        payload[5] = 0x0b;
        payload[6] = 0x05;
        payload[7..9].copy_from_slice(&256u16.to_be_bytes());
        let desc = ProfilesDescription::from_payload(&payload).unwrap();
        assert!(desc.is_g402_memory());
        assert!(desc.is_g402_profile());
        assert_eq!(desc.profile_count, 3);
        assert_eq!(desc.button_count, 11);
        assert_eq!(desc.sector_size, 256);
    }

    #[test]
    fn g303_profile_format_shares_the_g402_button_table() {
        let mut payload = [0u8; 16];
        payload[0] = 0x01;
        payload[1] = 0x02; // G502 Proteus Spectrum / G403
        payload[5] = 0x0b;
        payload[7..9].copy_from_slice(&256u16.to_be_bytes());
        let desc = ProfilesDescription::from_payload(&payload).unwrap();
        assert!(desc.is_g402_memory());
        assert!(!desc.is_g402_profile());
        assert!(desc.has_g402_button_table());
    }

    #[test]
    fn g502_product_ids_are_one_based() {
        assert!(profile_index_is_one_based(0xc332));
        assert_eq!(decode_active_profile_index(1, 3, true), 0);
        assert_eq!(decode_active_profile_index(3, 3, true), 2);
        assert_eq!(decode_active_profile_index(0, 3, false), 0);
        assert_eq!(decode_active_profile_index(2, 3, false), 2);
    }

    #[test]
    fn patches_g402_button_slot() {
        let mut sector = vec![0u8; 256];
        let binding = ButtonBinding::special(special::NEXT_DPI);
        assert!(patch_g402_button(&mut sector, 6, binding));
        assert_eq!(read_g402_button(&sector, 6).unwrap(), binding);
        assert_eq!(
            &sector[G402_BUTTONS_OFFSET + 24..G402_BUTTONS_OFFSET + 28],
            &binding.bytes
        );
    }

    #[test]
    fn hid_mouse_binding_uses_a_big_endian_bitfield() {
        assert_eq!(ButtonBinding::hid_mouse(1).bytes, [0x80, 0x01, 0x00, 0x01]);
        assert_eq!(ButtonBinding::hid_mouse(4).bytes, [0x80, 0x01, 0x00, 0x08]);
    }

    #[test]
    fn parses_profile_directory_entries() {
        let mut data = vec![0u8; 16];
        data[0..2].copy_from_slice(&1u16.to_be_bytes());
        data[2] = 2;
        data[4..6].copy_from_slice(&2u16.to_be_bytes());
        data[6] = 2;
        data[8..10].copy_from_slice(&0xffffu16.to_be_bytes());
        let entries = parse_profile_directory(&data, 3);
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].address, 1);
        assert!(entries[0].enabled);
    }
}
