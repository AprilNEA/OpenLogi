//! Implements the `DualPlatform` feature (ID `0x4530`) that selects which of two
//! OS platforms a device sends HID codes for.
//!
//! This is the predecessor of [`MultiPlatform`](super::multi_platform)
//! (`0x4531`); a device exposing `0x4531` should be driven through that feature
//! instead.

use num_enum::{IntoPrimitive, TryFromPrimitive};
use openlogi_hidpp_derive::Feature;

use crate::{
    feature::{DecodeEvent, EventSource, FeatureEndpoint},
    protocol::v20::Hidpp20Error,
};

/// `getPlatform`. Logi Options+ 2.8 (`Feature4530DualPlatform::GetPlatform`)
/// and Solaar (`read_fnid=0x00`) both read with function 0.
const FN_GET_PLATFORM: u8 = 0;

/// `setPlatform`. Solaar writes with function 2, but Logi Options+ 2.8
/// (`Feature4530DualPlatform::SetPlatform`) uses function 1, and no capture
/// settles which is right. Function 2 stays: Options+ knows only these two
/// functions, so if it is right, function 2 is out of range and the device
/// answers with an error instead of taking a write.
const FN_SET_PLATFORM: u8 = 2;

/// The platform a [`DualPlatformFeature`] device is configured for.
///
/// The selection is persistent and chosen by the user during pairing or by short
/// pressing an OS-selection button; there is no default.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, IntoPrimitive, TryFromPrimitive)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
#[non_exhaustive]
#[repr(u8)]
pub enum DualPlatformSelection {
    /// iOS or macOS.
    IosOrMac = 0,
    /// Android or Windows.
    AndroidOrWindows = 1,
}

/// An event emitted by [`DualPlatformFeature`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
#[non_exhaustive]
pub enum DualPlatformEvent {
    /// The user changed the platform via an OS-selection button.
    PlatformChanged(DualPlatformSelection),
}

/// Implements the `DualPlatform` / `0x4530` feature.
#[derive(Feature)]
#[creatable(id = 0x4530, version = 0)]
pub struct DualPlatformFeature {
    /// The endpoint this feature talks to.
    endpoint: FeatureEndpoint,

    /// Publishes decoded events to listeners.
    events: EventSource<DualPlatformEvent>,
}

impl DecodeEvent for DualPlatformEvent {
    fn decode(sub_id: u8, payload: &[u8; 16]) -> Option<Self> {
        // PlatformChange is the only event and carries sub-id 0.
        if sub_id != 0 {
            return None;
        }
        DualPlatformSelection::try_from(payload[0])
            .ok()
            .map(DualPlatformEvent::PlatformChanged)
    }
}

impl DualPlatformFeature {
    /// Retrieves the current platform setting.
    pub async fn get_platform(&self) -> Result<DualPlatformSelection, Hidpp20Error> {
        let payload = self
            .endpoint
            .call(FN_GET_PLATFORM, [0; 3])
            .await?
            .extend_payload();
        DualPlatformSelection::try_from(payload[0]).map_err(|_| Hidpp20Error::UnsupportedResponse)
    }

    /// Sets the platform and returns the device's echo of the new setting.
    ///
    /// This does not trigger a [`DualPlatformEvent::PlatformChanged`] event.
    pub async fn set_platform(
        &self,
        platform: DualPlatformSelection,
    ) -> Result<DualPlatformSelection, Hidpp20Error> {
        let payload = self
            .endpoint
            .call(FN_SET_PLATFORM, [platform.into(), 0, 0])
            .await?
            .extend_payload();
        DualPlatformSelection::try_from(payload[0]).map_err(|_| Hidpp20Error::UnsupportedResponse)
    }
}

#[cfg(test)]
mod tests {
    use super::DualPlatformSelection;

    #[test]
    fn maps_platform_wire_values() {
        assert_eq!(
            DualPlatformSelection::try_from(0).unwrap(),
            DualPlatformSelection::IosOrMac
        );
        assert_eq!(
            DualPlatformSelection::try_from(1).unwrap(),
            DualPlatformSelection::AndroidOrWindows
        );
        DualPlatformSelection::try_from(2).unwrap_err();
        assert_eq!(u8::from(DualPlatformSelection::AndroidOrWindows), 1);
    }

    #[test]
    fn reads_the_platform_with_function_zero() {
        // Function 1 is setPlatform in the Options+ function table, so a read
        // sent there would switch the keyboard to the iOS/macOS layout.
        use std::sync::Arc;

        use super::{DualPlatformFeature, FN_GET_PLATFORM};
        use crate::channel::mock::{MockRawHidChannel, channel_with_reader};
        use crate::feature::CreatableFeature;
        use crate::nibble::U4;
        use crate::protocol::v20::{Message, MessageHeader};

        futures::executor::block_on(async {
            let (raw, handle) = MockRawHidChannel::new();
            let channel = Arc::new(channel_with_reader(raw).await);
            let feature = DualPlatformFeature::new(channel, 3, 8);
            let header = MessageHeader {
                device_index: 3,
                feature_index: 8,
                function_id: U4::from_lo(FN_GET_PLATFORM),
                software_id: U4::from_lo(1),
            };
            let mut read = Box::pin(feature.get_platform());
            assert!(futures::poll!(read.as_mut()).is_pending());

            // Report: [id, device, feature, function|swid, payload...].
            let [request] = &handle.written_reports()[..] else {
                panic!("one request");
            };
            assert_eq!(request[3] >> 4, 0);
            assert_eq!(request[4..7], [0, 0, 0]);

            let mut payload = [0; 16];
            payload[0] = 1;
            handle
                .send_incoming(Message::Long(header, payload).into())
                .await;
            assert_eq!(read.await.unwrap(), DualPlatformSelection::AndroidOrWindows);
        });
    }
}
