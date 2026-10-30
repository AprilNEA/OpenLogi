//! Implements the `OnboardProfiles` feature (ID `0x8100`).
//!
//! Gaming mice keep their DPI, report rate and button settings in profiles
//! stored on the device. While a stored profile drives the device
//! ([`OnboardMode::Onboard`]) its firmware refuses host writes of those
//! settings — a PRO X SUPERLIGHT 2 answers every `0x2202`
//! `setSensorDpiParameters` with `LOGITECH_INTERNAL` — and in
//! [`OnboardMode::Host`] the host owns them.
//!
//! Only the mode functions are implemented. Their function IDs and mode values
//! follow Solaar's and libratbag's reverse-engineered decoding, confirmed on a
//! PRO X SUPERLIGHT 2, not an official spec.

use num_enum::{IntoPrimitive, TryFromPrimitive};
use openlogi_hidpp_derive::Feature;

use crate::{feature::FeatureEndpoint, protocol::v20::Hidpp20Error};

/// Who owns the settings an onboard profile stores.
///
/// The wire value `0` of `setOnboardMode` means "leave the mode unchanged"; it
/// is a write-only sentinel and never a mode the device reports, so it has no
/// variant here.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, IntoPrimitive, TryFromPrimitive)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
#[non_exhaustive]
#[repr(u8)]
pub enum OnboardMode {
    /// The active stored profile owns the settings.
    Onboard = 1,
    /// The host owns the settings.
    Host = 2,
}

/// Implements the `OnboardProfiles` / `0x8100` feature.
#[derive(Clone, Feature)]
#[creatable(id = 0x8100, version = 0)]
pub struct OnboardProfilesFeature {
    /// The endpoint this feature talks to.
    endpoint: FeatureEndpoint,
}

impl OnboardProfilesFeature {
    /// Sets who owns the settings an onboard profile stores.
    pub async fn set_onboard_mode(&self, mode: OnboardMode) -> Result<(), Hidpp20Error> {
        self.endpoint.call(1, [u8::from(mode), 0, 0]).await?;
        Ok(())
    }

    /// Retrieves who owns the settings an onboard profile stores.
    pub async fn get_onboard_mode(&self) -> Result<OnboardMode, Hidpp20Error> {
        let payload = self.endpoint.call(2, [0; 3]).await?.extend_payload();
        OnboardMode::try_from(payload[0]).map_err(|_| Hidpp20Error::UnsupportedResponse)
    }
}
