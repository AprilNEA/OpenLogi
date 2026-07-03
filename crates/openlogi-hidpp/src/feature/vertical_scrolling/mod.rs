//! Implements `VerticalScrolling` (feature `0x2100`).

use std::sync::Arc;

use num_enum::TryFromPrimitive;

use crate::{
    channel::HidppChannel,
    feature::{CreatableFeature, Feature, FeatureEndpoint},
    protocol::v20::Hidpp20Error,
};

/// Roller type reported by `VerticalScrolling`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, TryFromPrimitive)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
#[non_exhaustive]
#[repr(u8)]
pub enum RollerType {
    /// Standard one- or two-dimensional roller.
    Standard = 0x01,
    /// 3G roller.
    ThreeG = 0x03,
    /// Micro-ratchet roller.
    MicroRatchet = 0x04,
    /// Touchpad scrolling.
    Touchpad = 0x05,
    /// Touchpad with natural scrolling enabled by default.
    TouchpadNaturalDefault = 0x06,
}

/// Number of lines scrolled for a wheel movement.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
#[non_exhaustive]
pub enum ScrollLines {
    /// Do not change the host system setting.
    SystemDefault,
    /// Scroll this many lines per movement.
    Lines(u8),
    /// Scroll a full page or screen per movement.
    Page,
}

/// Vertical scrolling roller information.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
#[non_exhaustive]
pub struct RollerInfo {
    /// Roller type.
    pub roller_type: RollerType,
    /// Number of ratchets per wheel turn.
    pub ratchets_per_turn: u8,
    /// Scroll-line behavior.
    pub scroll_lines: ScrollLines,
}

/// Implements the `VerticalScrolling` / `0x2100` feature.
#[derive(Clone)]
pub struct VerticalScrollingFeature {
    /// The endpoint this feature talks to.
    endpoint: FeatureEndpoint,
}

impl CreatableFeature for VerticalScrollingFeature {
    const ID: u16 = 0x2100;
    const STARTING_VERSION: u8 = 0;

    fn new(chan: Arc<HidppChannel>, device_index: u8, feature_index: u8) -> Self {
        Self {
            endpoint: FeatureEndpoint::new(chan, device_index, feature_index),
        }
    }
}

impl Feature for VerticalScrollingFeature {}

impl VerticalScrollingFeature {
    /// Retrieves roller information.
    pub async fn get_roller_info(&self) -> Result<RollerInfo, Hidpp20Error> {
        let payload = self.endpoint.call(0, [0; 3]).await?.extend_payload();
        RollerInfo::from_payload(payload)
    }
}

impl RollerInfo {
    fn from_payload(payload: [u8; 16]) -> Result<Self, Hidpp20Error> {
        Ok(Self {
            roller_type: RollerType::try_from(payload[0])
                .map_err(|_| Hidpp20Error::UnsupportedResponse)?,
            ratchets_per_turn: payload[1],
            scroll_lines: ScrollLines::from(payload[2]),
        })
    }
}

impl From<u8> for ScrollLines {
    fn from(value: u8) -> Self {
        match value {
            0x00 => Self::SystemDefault,
            0xff => Self::Page,
            lines => Self::Lines(lines),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{RollerInfo, RollerType, ScrollLines};

    #[test]
    fn parses_roller_info() {
        let mut payload = [0; 16];
        payload[0] = 0x04;
        payload[1] = 24;
        payload[2] = 0xff;

        let info = RollerInfo::from_payload(payload).unwrap();

        assert_eq!(info.roller_type, RollerType::MicroRatchet);
        assert_eq!(info.ratchets_per_turn, 24);
        assert_eq!(info.scroll_lines, ScrollLines::Page);
    }
}
