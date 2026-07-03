//! Implements the `DualPlatform` feature (ID `0x4530`) that selects which of two
//! OS platforms a device sends HID codes for.
//!
//! This is the predecessor of [`MultiPlatform`](super::multi_platform)
//! (`0x4531`); a device exposing `0x4531` should be driven through that feature
//! instead.

use std::sync::Arc;

use num_enum::{IntoPrimitive, TryFromPrimitive};

use crate::{
    channel::{HidppChannel, MessageListenerGuard},
    event::EventEmitter,
    feature::{CreatableFeature, EmittingFeature, Feature, FeatureEndpoint, event_payload},
    protocol::v20::Hidpp20Error,
};

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
pub struct DualPlatformFeature {
    /// The endpoint this feature talks to.
    endpoint: FeatureEndpoint,

    /// The emitter used to publish decoded events.
    emitter: Arc<EventEmitter<DualPlatformEvent>>,

    /// Removes the message listener when the feature is dropped.
    _msg_listener: MessageListenerGuard,
}

impl CreatableFeature for DualPlatformFeature {
    const ID: u16 = 0x4530;
    const STARTING_VERSION: u8 = 0;

    fn new(chan: Arc<HidppChannel>, device_index: u8, feature_index: u8) -> Self {
        let emitter = Arc::new(EventEmitter::new());

        let listener = chan.add_msg_listener_guarded({
            let emitter = Arc::clone(&emitter);

            move |raw, matched| {
                let Some((func, payload)) =
                    event_payload(raw, matched, device_index, feature_index)
                else {
                    return;
                };
                // PlatformChange is the only event and carries sub-id 0.
                if func.to_lo() != 0 {
                    return;
                }
                if let Ok(platform) = DualPlatformSelection::try_from(payload[0]) {
                    emitter.emit(DualPlatformEvent::PlatformChanged(platform));
                }
            }
        });

        Self {
            endpoint: FeatureEndpoint::new(chan, device_index, feature_index),
            emitter,
            _msg_listener: listener,
        }
    }
}

impl Feature for DualPlatformFeature {}

impl EmittingFeature<DualPlatformEvent> for DualPlatformFeature {
    fn listen(&self) -> async_channel::Receiver<DualPlatformEvent> {
        self.emitter.create_receiver()
    }
}

impl DualPlatformFeature {
    /// Retrieves the current platform setting.
    pub async fn get_platform(&self) -> Result<DualPlatformSelection, Hidpp20Error> {
        // `getPlatform` is function 1 in this feature, not the usual 0.
        let payload = self.endpoint.call(1, [0; 3]).await?.extend_payload();
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
            .call(2, [platform.into(), 0, 0])
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
        assert!(DualPlatformSelection::try_from(2).is_err());
        assert_eq!(u8::from(DualPlatformSelection::AndroidOrWindows), 1);
    }
}
