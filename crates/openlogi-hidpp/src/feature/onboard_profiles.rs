//! Implements the `OnboardProfiles` feature (ID `0x8100`) that selects whether
//! the device's own stored profiles or the host drives its settings.

use num_enum::{IntoPrimitive, TryFromPrimitive};
use openlogi_hidpp_derive::Feature;

use crate::{feature::FeatureEndpoint, protocol::v20::Hidpp20Error};

/// Implements the `OnboardProfiles` / `0x8100` feature.
#[derive(Feature)]
#[creatable(id = 0x8100, version = 0)]
pub struct OnboardProfilesFeature {
    /// The endpoint this feature talks to.
    endpoint: FeatureEndpoint,
}

impl OnboardProfilesFeature {
    /// Retrieves which settings source currently drives the device.
    pub async fn get_onboard_mode(&self) -> Result<OnboardMode, Hidpp20Error> {
        let payload = self.endpoint.call(2, [0; 3]).await?.extend_payload();
        OnboardMode::try_from(payload[0]).map_err(|_| Hidpp20Error::UnsupportedResponse)
    }

    /// Selects which settings source drives the device.
    ///
    /// The mode is volatile by design, so that a device keeps working
    /// standalone on a host that has no software: it returns to
    /// [`OnboardMode::Onboard`] when it reconnects, wakes, or is power-cycled.
    /// A host that needs [`OnboardMode::Host`] therefore re-asserts it over the
    /// device's lifetime instead of setting it once.
    pub async fn set_onboard_mode(&self, mode: OnboardMode) -> Result<(), Hidpp20Error> {
        self.endpoint.call(1, [mode.into(), 0, 0]).await?;
        Ok(())
    }
}

/// The settings source driving a device that has onboard profiles.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, IntoPrimitive, TryFromPrimitive)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
#[non_exhaustive]
#[repr(u8)]
pub enum OnboardMode {
    /// The device runs a profile out of its own memory: it applies the DPI
    /// stages, report rate and button map stored there, and rejects host
    /// writes to the settings that profile owns.
    Onboard = 0x01,
    /// The host drives the settings and the stored profile is not applied.
    Host = 0x02,
}
