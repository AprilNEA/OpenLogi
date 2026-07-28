//! Implements the `DisableKeysByUsage` feature (ID `0x4522`) that disables or
//! enables arbitrary keyboard keys by HID usage.
//!
//! Unlike [`DisableKeys`](super::disable_keys) (`0x4521`), which toggles a fixed
//! set of lock keys, this feature operates on any 8-bit keyboard HID usage.

use std::sync::Arc;

use crate::{
    channel::HidppChannel,
    feature::{CreatableFeature, Feature, FeatureEndpoint},
    protocol::v20::{ErrorType, Hidpp20Error},
};

/// Number of usage bytes carried by one long-report request.
const USAGES_PER_PACKET: usize = 16;

/// Implements the `DisableKeysByUsage` / `0x4522` feature.
#[derive(Clone)]
pub struct DisableKeysByUsageFeature {
    /// The endpoint this feature talks to.
    endpoint: FeatureEndpoint,
}

impl CreatableFeature for DisableKeysByUsageFeature {
    const ID: u16 = 0x4522;
    const STARTING_VERSION: u8 = 0;

    fn new(chan: Arc<HidppChannel>, device_index: u8, feature_index: u8) -> Self {
        Self {
            endpoint: FeatureEndpoint::new(chan, device_index, feature_index),
        }
    }
}

impl Feature for DisableKeysByUsageFeature {}

impl DisableKeysByUsageFeature {
    /// Retrieves the maximum number of usages that can be disabled at once.
    pub async fn get_capabilities(&self) -> Result<u8, Hidpp20Error> {
        Ok(self.endpoint.call(0, [0; 3]).await?.extend_payload()[0])
    }

    /// Disables the given 8-bit keyboard HID `usages`.
    ///
    /// Disabling is **cumulative**: these usages are added to the disabled set
    /// rather than replacing it. A usage of `0` is the list terminator and cannot
    /// itself be disabled. More usages than fit in one request are sent over
    /// several requests, which the device still accumulates.
    pub async fn disable_keys(&self, usages: &[u8]) -> Result<(), Hidpp20Error> {
        validate_usages(usages)?;
        for packet in usage_packets(usages) {
            self.endpoint.call_long(1, packet).await?;
        }
        Ok(())
    }

    /// Enables (removes from the disabled set) the given 8-bit keyboard HID
    /// `usages`.
    ///
    /// Enabling a usage that is not disabled is a no-op. A usage of `0`
    /// terminates the list.
    pub async fn enable_keys(&self, usages: &[u8]) -> Result<(), Hidpp20Error> {
        validate_usages(usages)?;
        for packet in usage_packets(usages) {
            self.endpoint.call_long(2, packet).await?;
        }
        Ok(())
    }

    /// Re-enables every keyboard key.
    pub async fn enable_all_keys(&self) -> Result<(), Hidpp20Error> {
        self.endpoint.call(3, [0; 3]).await?;
        Ok(())
    }
}

fn validate_usages(usages: &[u8]) -> Result<(), Hidpp20Error> {
    if usages.contains(&0) {
        return Err(Hidpp20Error::Feature(ErrorType::InvalidArgument));
    }
    Ok(())
}

/// Splits `usages` into long-report packets of up to [`USAGES_PER_PACKET`]
/// bytes.
///
/// A packet shorter than the full width is zero-padded, which doubles as the
/// `0x00` end-of-list terminator; a packet filling every byte carries no
/// terminator, as the device treats a full packet as exactly that many usages.
fn usage_packets(usages: &[u8]) -> Vec<[u8; USAGES_PER_PACKET]> {
    usages
        .chunks(USAGES_PER_PACKET)
        .map(|chunk| {
            let mut packet = [0u8; USAGES_PER_PACKET];
            packet[..chunk.len()].copy_from_slice(chunk);
            packet
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use std::assert_matches;

    use super::{usage_packets, validate_usages};
    use crate::protocol::v20::{ErrorType, Hidpp20Error};

    #[test]
    fn empty_usage_list_sends_no_packets() {
        assert!(usage_packets(&[]).is_empty());
    }

    #[test]
    fn short_list_is_zero_terminated() {
        let packets = usage_packets(&[0x39, 0x3a, 0x3b]);
        assert_eq!(packets.len(), 1);
        assert_eq!(packets[0][..3], [0x39, 0x3a, 0x3b]);
        // The remaining bytes are the 0x00 terminator / padding.
        assert!(packets[0][3..].iter().all(|&b| b == 0));
    }

    #[test]
    fn rejects_zero_usage_before_packetizing() {
        assert_matches!(
            validate_usages(&[0x39, 0, 0x3a]),
            Err(Hidpp20Error::Feature(ErrorType::InvalidArgument))
        );
    }

    #[test]
    fn full_packet_has_no_terminator() {
        let usages: Vec<u8> = (1..=16).collect();
        let packets = usage_packets(&usages);
        assert_eq!(packets.len(), 1);
        assert_eq!(packets[0], usages.as_slice());
    }

    #[test]
    fn overflow_splits_into_cumulative_packets() {
        let usages: Vec<u8> = (1..=18).collect();
        let packets = usage_packets(&usages);
        assert_eq!(packets.len(), 2);
        assert_eq!(packets[0], (1..=16).collect::<Vec<u8>>().as_slice());
        assert_eq!(packets[1][..2], [17, 18]);
        assert!(packets[1][2..].iter().all(|&b| b == 0));
    }
}
