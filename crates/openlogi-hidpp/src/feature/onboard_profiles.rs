//! Implements a diagnostics-only accessor for the `OnboardProfiles` feature
//! (ID `0x8100`) that Logitech's G-series gaming mice and keyboards expose
//! instead of `ReprogControls` (`0x1b00`–`0x1b04`) for on-device profile and
//! button-assignment storage.
//!
//! No official Logitech HID++ spec for this feature was available while
//! writing this, so only the wire request (function `0`, no arguments) is
//! implemented — the response byte layout is deliberately **not** decoded
//! here. [`OnboardProfilesFeature::get_info_raw`] hands back the raw payload
//! so it can be captured from real hardware and the field layout derived (and
//! verified against an authoritative source) before any typed accessor is
//! added. Do not add field parsing to this file without a citable spec or
//! cross-referenced multi-device confirmation.

use openlogi_hidpp_derive::Feature;

use crate::{feature::FeatureEndpoint, protocol::v20::Hidpp20Error};

/// Implements the `OnboardProfiles` / `0x8100` feature.
#[derive(Clone, Feature)]
#[creatable(id = 0x8100, version = 0)]
pub struct OnboardProfilesFeature {
    /// The endpoint this feature talks to.
    endpoint: FeatureEndpoint,
}

impl OnboardProfilesFeature {
    /// Calls function `0` (by HID++2.0 convention the "get info/capabilities"
    /// function on nearly every feature in this crate) with no arguments and
    /// returns the raw, undecoded response payload.
    pub async fn get_info_raw(&self) -> Result<[u8; 16], Hidpp20Error> {
        Ok(self.endpoint.call(0, [0; 3]).await?.extend_payload())
    }
}
