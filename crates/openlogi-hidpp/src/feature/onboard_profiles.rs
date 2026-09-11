//! Implements a mode-only slice of `OnboardProfiles` (ID `0x8100`).
//!
//! Function IDs and mode bytes are reverse-engineered from the public
//! descriptions of cvuchener/hidpp `IOnboardProfiles`. Logitech has not
//! published this feature in the public HID++ specs used by this crate, so
//! additions must be verified against hardware rather than guessed. This file
//! does not copy that project's code.
//!
//! Flash reads/writes, current-profile selection, and DPI-index functions are
//! intentionally unimplemented. Host mode is what makes `0x8110`
//! `SetMouseButtonMapping` apply; onboard profiles ignore that mapping.

use num_enum::{IntoPrimitive, TryFromPrimitive};
use openlogi_hidpp_derive::Feature;

use crate::{feature::FeatureEndpoint, protocol::v20::Hidpp20Error};

/// HID++ 2.0 function ids for `0x8100` (4-bit). Reverse-engineered; see the
/// module docs.
const FN_SET_MODE: u8 = 1;
const FN_GET_MODE: u8 = 2;

/// Whether the device applies onboard flash profiles or host-controlled
/// mappings.
///
/// `0` (`NoChange`) is a set-only wire sentinel and is not part of this enum:
/// [`OnboardProfilesFeature::get_mode`] rejects it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, IntoPrimitive, TryFromPrimitive)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
#[non_exhaustive]
#[repr(u8)]
pub enum OnboardProfilesMode {
    /// On-board memory mode. Firmware uses stored profiles; `0x8110` mapping
    /// writes do not apply.
    Onboard = 1,
    /// Host-controlled mode. Standard HID reports follow the `0x8110` mapping.
    /// Current-profile and onboard DPI-index writes are rejected.
    Host = 2,
}

/// Implements the `OnboardProfiles` / `0x8100` feature (mode switch only).
#[derive(Clone, Feature)]
#[creatable(id = 0x8100, version = 0)]
pub struct OnboardProfilesFeature {
    /// The endpoint this feature talks to.
    endpoint: FeatureEndpoint,
}

impl OnboardProfilesFeature {
    /// Current onboard-versus-host mode.
    pub async fn get_mode(&self) -> Result<OnboardProfilesMode, Hidpp20Error> {
        let payload = self
            .endpoint
            .call(FN_GET_MODE, [0; 3])
            .await?
            .extend_payload();
        onboard_mode_from_payload(&payload)
    }

    /// Switches between onboard profiles and host-controlled mappings.
    pub async fn set_mode(&self, mode: OnboardProfilesMode) -> Result<(), Hidpp20Error> {
        self.endpoint
            .call(FN_SET_MODE, [u8::from(mode), 0, 0])
            .await?;
        Ok(())
    }
}

fn onboard_mode_from_payload(payload: &[u8; 16]) -> Result<OnboardProfilesMode, Hidpp20Error> {
    OnboardProfilesMode::try_from(payload[0]).map_err(|_| Hidpp20Error::UnsupportedResponse)
}

#[cfg(test)]
mod tests {
    use super::{OnboardProfilesMode, onboard_mode_from_payload};
    use crate::protocol::v20::Hidpp20Error;

    fn payload_with_prefix(bytes: &[u8]) -> [u8; 16] {
        let mut payload = [0; 16];
        payload[..bytes.len()].copy_from_slice(bytes);
        payload
    }

    #[test]
    fn parses_onboard_and_host_modes() {
        assert_eq!(
            onboard_mode_from_payload(&payload_with_prefix(&[1])).unwrap(),
            OnboardProfilesMode::Onboard
        );
        assert_eq!(
            onboard_mode_from_payload(&payload_with_prefix(&[2])).unwrap(),
            OnboardProfilesMode::Host
        );
    }

    #[test]
    fn rejects_no_change_and_unknown_mode_bytes() {
        assert!(matches!(
            onboard_mode_from_payload(&payload_with_prefix(&[0])),
            Err(Hidpp20Error::UnsupportedResponse)
        ));
        assert!(matches!(
            onboard_mode_from_payload(&payload_with_prefix(&[3])),
            Err(Hidpp20Error::UnsupportedResponse)
        ));
    }
}
