//! Implements `OnboardProfiles` (feature `0x8100`).
//!
//! Read-only diagnostic surface only: OpenLogi does not manage on-device
//! profile memory (see `openlogi diag mouse-buttons`). The layout below is
//! reverse-engineered, cross-checked against two independent sources —
//! libratbag (`src/hidpp20.c` / `hidpp20.h`) and `cvuchener/hidpp`
//! (`hidpp20/IOnboardProfiles.h`) — which agree on the function IDs and the
//! descriptor layout. No official Logitech spec document was found for this
//! feature.

use num_enum::TryFromPrimitive;
use openlogi_hidpp_derive::Feature;

use crate::{feature::FeatureEndpoint, protocol::v20::Hidpp20Error};

/// The device's onboard/host mode, as returned by `getMode`.
///
/// `0` ("no change") is a write-only sentinel on `setMode` and never a state
/// a device reports here — an unrecognised byte, including `0`, is
/// [`Hidpp20Error::UnsupportedResponse`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, TryFromPrimitive)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
#[repr(u8)]
pub enum OnboardMode {
    /// The device applies its onboard profile (DPI stages, button bindings,
    /// macros, RGB) without host involvement.
    Onboard = 1,
    /// The device sends only generic button/DPI events; onboard-profile
    /// bindings do not apply. `0x8110`'s `MouseButtonSpy` button mapping
    /// (functions 3/4, not implemented here) only takes effect in this mode.
    Host = 2,
}

/// The `0x8100` descriptor returned by `getDescription` — memory layout and
/// per-device profile geometry, not a profile's contents.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
#[non_exhaustive]
pub struct ProfilesDescription {
    /// Vendor memory-model identifier.
    pub memory_model_id: u8,
    /// Onboard profile blob format version — load-bearing for any future
    /// profile-memory work: the 256-byte layout other tools reverse-engineered
    /// is specific to one `profile_format_id` value per device family.
    pub profile_format_id: u8,
    /// Macro blob format version.
    pub macro_format_id: u8,
    /// Number of user-writable onboard profiles.
    pub profile_count: u8,
    /// Number of out-of-box (read-only/factory) profiles.
    pub profile_count_oob: u8,
    /// Number of physical buttons the profile format has slots for.
    pub button_count: u8,
    /// Number of addressable memory sectors.
    pub sector_count: u8,
    /// Bytes per memory sector.
    pub sector_size: u16,
    /// Vendor mechanical-layout code.
    pub mechanical_layout: u8,
    /// Vendor-defined additional info byte.
    pub various_info: u8,
}

impl ProfilesDescription {
    fn from_payload(payload: [u8; 16]) -> Self {
        Self {
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
        }
    }
}

/// Implements the `OnboardProfiles` / `0x8100` feature.
///
/// Only the read-only diagnostic functions are implemented (`getDescription`,
/// `getMode`). Mode switching and the sector-addressed memory read/write
/// functions are deliberately not implemented — OpenLogi does not manage
/// on-device profile memory.
#[derive(Feature)]
#[creatable(id = 0x8100, version = 0)]
pub struct OnboardProfilesFeature {
    endpoint: FeatureEndpoint,
}

impl OnboardProfilesFeature {
    /// Reads the profile-memory descriptor (`getDescription`, function 0).
    pub async fn get_description(&self) -> Result<ProfilesDescription, Hidpp20Error> {
        let payload = self.endpoint.call(0, [0; 3]).await?.extend_payload();
        Ok(ProfilesDescription::from_payload(payload))
    }

    /// Reads the device's current onboard/host mode (`getMode`, function 2).
    pub async fn get_mode(&self) -> Result<OnboardMode, Hidpp20Error> {
        let byte = self.endpoint.call(2, [0; 3]).await?.extend_payload()[0];
        OnboardMode::try_from(byte).map_err(|_| Hidpp20Error::UnsupportedResponse)
    }
}
