//! Implements the `SolarKeyboardDashboard` feature (ID `0x4301`) for Logitech's
//! solar keyboards (e.g. the K750): scheduling light-measure reports, overriding
//! the CheckLight LED, and receiving battery / light broadcast events.

pub mod event;

#[cfg(test)]
mod tests;

use std::sync::Arc;

use num_enum::{IntoPrimitive, TryFromPrimitive};

pub use event::{SolarEvent, SolarStatus};

use crate::{
    channel::{HidppChannel, MessageListenerGuard},
    event::EventEmitter,
    feature::{CreatableFeature, EmittingFeature, Feature, FeatureEndpoint, event_payload},
    protocol::v20::Hidpp20Error,
};

/// A CheckLight LED color for [`set_led`](SolarDashboardFeature::set_led).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, IntoPrimitive, TryFromPrimitive)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
#[non_exhaustive]
#[repr(u8)]
pub enum LedId {
    /// All LEDs off.
    Off = 0,
    /// Red.
    Red = 1,
    /// Orange.
    Orange = 2,
    /// Green.
    Green = 3,
}

/// Implements the `SolarKeyboardDashboard` / `0x4301` feature.
pub struct SolarDashboardFeature {
    /// The endpoint this feature talks to.
    endpoint: FeatureEndpoint,

    /// The emitter used to publish decoded events.
    emitter: Arc<EventEmitter<SolarEvent>>,

    /// Removes the message listener when the feature is dropped.
    _msg_listener: MessageListenerGuard,
}

impl CreatableFeature for SolarDashboardFeature {
    const ID: u16 = 0x4301;
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
                if let Some(event) = event::decode_event(func.to_lo(), &payload) {
                    emitter.emit(event);
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

impl Feature for SolarDashboardFeature {}

impl EmittingFeature<SolarEvent> for SolarDashboardFeature {
    fn listen(&self) -> async_channel::Receiver<SolarEvent> {
        self.emitter.create_receiver()
    }
}

impl SolarDashboardFeature {
    /// Schedules [`SolarEvent::LightMeasure`] reports.
    ///
    /// `max_reports` is the number of reports to send and `report_period` their
    /// spacing in seconds. Passing `0` for either cancels reporting.
    pub async fn set_light_measure(
        &self,
        max_reports: u8,
        report_period: u8,
    ) -> Result<(), Hidpp20Error> {
        self.endpoint
            .call(0, [max_reports, report_period, 0])
            .await?;
        Ok(())
    }

    /// Lights the CheckLight LED in the given color for a firmware-defined
    /// duration.
    ///
    /// Intended to override the firmware's own CheckLight display in response to a
    /// [`SolarEvent::CheckLightButton`]; the firmware waits 250 ms before showing
    /// its own status, so call this within that window.
    pub async fn set_led(&self, led: LedId) -> Result<(), Hidpp20Error> {
        self.endpoint.call(1, [led.into(), 0, 0]).await?;
        Ok(())
    }
}
