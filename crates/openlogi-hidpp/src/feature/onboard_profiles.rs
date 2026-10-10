//! Implements the `OnboardProfiles` feature (ID `0x8100`) — only the mode
//! switch between the device's stored profiles and host control.
//!
//! The function numbers and mode values follow Solaar's implementation
//! (`setOnboardMode` = function 1, `getOnboardMode` = function 2), not a
//! published spec, and were confirmed on a PRO X3 SUPERSTRIKE: in onboard mode
//! it rejects `0x2202` DPI writes with `LogitechInternal`, in host mode it
//! takes them.

use num_enum::{IntoPrimitive, TryFromPrimitive};
use openlogi_hidpp_derive::Feature;

use crate::{feature::FeatureEndpoint, protocol::v20::Hidpp20Error};

/// Who controls the settings an onboard profile stores (DPI stages, report
/// rate, button assignments).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, IntoPrimitive, TryFromPrimitive)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
#[non_exhaustive]
#[repr(u8)]
pub enum OnboardMode {
    /// The active onboard profile applies; host writes to the settings it
    /// stores are rejected.
    Onboard = 1,
    /// The host controls those settings; onboard profiles are inactive.
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
    /// Retrieves the current onboard mode.
    pub async fn get_onboard_mode(&self) -> Result<OnboardMode, Hidpp20Error> {
        let payload = self.endpoint.call(2, [0; 3]).await?.extend_payload();
        OnboardMode::try_from(payload[0]).map_err(|_| Hidpp20Error::UnsupportedResponse)
    }

    /// Switches to `mode`.
    pub async fn set_onboard_mode(&self, mode: OnboardMode) -> Result<(), Hidpp20Error> {
        self.endpoint.call(1, [mode.into(), 0, 0]).await?;
        Ok(())
    }
}
