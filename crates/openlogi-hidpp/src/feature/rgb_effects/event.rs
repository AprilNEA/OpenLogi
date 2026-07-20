//! Events emitted by the `RgbEffects` feature (`0x8071`).

use super::types::{
    ActivityEventType, CLUSTER_EFFECT_PARAM_COUNT, PowerModeTarget, RgbPersistence, be16,
};

/// Bit offset of the power-mode target in the cluster-effect flags byte.
const POWER_TARGET_SHIFT: u8 = 2;
/// Mask of the 2-bit fields packed into the cluster-effect flags byte.
const FLAGS_FIELD_MASK: u8 = 0b11;

/// An event emitted by [`RgbEffectsFeature`](super::RgbEffectsFeature).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
#[non_exhaustive]
pub enum RgbEffectsEvent {
    /// A period effect reached a synchronization point.
    EffectSync {
        /// Cluster the event applies to; `0xff` means all clusters.
        cluster_index: u8,
        /// Current timing position within the period, in milliseconds.
        effect_counter: u16,
    },
    /// User activity started or its absence timed out.
    UserActivity(ActivityEventType),
    /// A cluster's effect changed; mirrors a `setRgbClusterEffect` request.
    ClusterChanged {
        /// Index of the cluster.
        cluster_index: u8,
        /// Index of the effect within the cluster.
        cluster_effect_index: u8,
        /// The effect parameters (meaning depends on the effect).
        params: [u8; CLUSTER_EFFECT_PARAM_COUNT],
        /// Persistence the effect was applied with.
        persistence: RgbPersistence,
        /// Power-mode target the effect applies to.
        power_mode: PowerModeTarget,
    },
}

/// Decodes an unsolicited `0x8071` event payload by its sub-id.
pub(super) fn decode_event(sub_id: u8, payload: &[u8; 16]) -> Option<RgbEffectsEvent> {
    match sub_id {
        0 => Some(RgbEffectsEvent::EffectSync {
            cluster_index: payload[0],
            effect_counter: be16(payload, 1),
        }),
        1 => Some(RgbEffectsEvent::UserActivity(ActivityEventType::from(
            payload[0],
        ))),
        2 => {
            let mut params = [0; CLUSTER_EFFECT_PARAM_COUNT];
            params.copy_from_slice(&payload[2..2 + CLUSTER_EFFECT_PARAM_COUNT]);
            // The flags byte mirrors setRgbClusterEffect: persistence in the low
            // two bits, power-mode target in bits 2..=3.
            let flags = payload[2 + CLUSTER_EFFECT_PARAM_COUNT];
            Some(RgbEffectsEvent::ClusterChanged {
                cluster_index: payload[0],
                cluster_effect_index: payload[1],
                params,
                persistence: RgbPersistence::from_bits_retain(flags & FLAGS_FIELD_MASK),
                power_mode: PowerModeTarget::from((flags >> POWER_TARGET_SHIFT) & FLAGS_FIELD_MASK),
            })
        }
        _ => None,
    }
}
