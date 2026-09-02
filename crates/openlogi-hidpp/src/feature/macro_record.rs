//! Implements `MacroRecord` (feature `0x8030`).
//!
//! The MR state-event payload used here was verified on Logitech gaming
//! keyboards and is retained as an explicitly reverse-engineered protocol fact.

use std::sync::Arc;

use crate::{
    channel::HidppChannel,
    feature::{CreatableFeature, DecodeEvent, EmittingFeature, EventSource, Feature},
};

/// An event emitted by [`MacroRecordFeature`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
#[non_exhaustive]
pub enum MacroRecordEvent {
    /// Full pressed/released snapshot of the MR key.
    StateChanged(bool),
}

impl DecodeEvent for MacroRecordEvent {
    fn decode(sub_id: u8, payload: &[u8; 16]) -> Option<Self> {
        (sub_id == 0).then_some(Self::StateChanged(payload[0] != 0))
    }
}

/// Implements the `MacroRecord` / `0x8030` event feature.
pub struct MacroRecordFeature {
    /// Publishes decoded MR-key state snapshots.
    events: EventSource<MacroRecordEvent>,
}

impl CreatableFeature for MacroRecordFeature {
    const ID: u16 = 0x8030;
    const STARTING_VERSION: u8 = 0;

    fn new(chan: Arc<HidppChannel>, device_index: u8, feature_index: u8) -> Self {
        Self {
            events: EventSource::attach(&chan, device_index, feature_index),
        }
    }
}

impl Feature for MacroRecordFeature {}

impl EmittingFeature<MacroRecordEvent> for MacroRecordFeature {
    fn listen(&self) -> async_channel::Receiver<MacroRecordEvent> {
        self.events.listen()
    }
}

#[cfg(test)]
mod tests {
    use super::MacroRecordEvent;
    use crate::feature::DecodeEvent;

    #[test]
    fn decodes_mr_press_and_release() {
        let mut payload = [0; 16];
        payload[0] = 1;
        assert_eq!(
            MacroRecordEvent::decode(0, &payload),
            Some(MacroRecordEvent::StateChanged(true))
        );

        payload[0] = 0;
        assert_eq!(
            MacroRecordEvent::decode(0, &payload),
            Some(MacroRecordEvent::StateChanged(false))
        );
        assert_eq!(MacroRecordEvent::decode(1, &payload), None);
    }
}
