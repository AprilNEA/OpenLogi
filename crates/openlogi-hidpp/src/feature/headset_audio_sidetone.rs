//! Implements `HeadsetAudioSidetone` (feature `0x0604`) for gaming headsets (e.g., PRO X 2 LIGHTSPEED).

use openlogi_hidpp_derive::Feature;

use crate::{feature::FeatureEndpoint, protocol::v20::Hidpp20Error};

/// Implements the `HeadsetAudioSidetone` / `0x0604` feature.
#[derive(Clone, Feature)]
#[creatable(id = 0x0604, version = 0)]
pub struct HeadsetAudioSidetoneFeature {
    /// The endpoint this feature talks to.
    endpoint: FeatureEndpoint,
}

impl HeadsetAudioSidetoneFeature {
    /// Returns the active sidetone level in percentage `0..=100`.
    pub async fn get_sidetone_level(&self, version: u8) -> Result<u8, Hidpp20Error> {
        let reply = self.endpoint.call(0, [0; 3]).await?;
        let payload = reply.extend_payload();

        let level = if version > 1 {
            // Version > 1: [mic_count, mic_id, reserved, level, ...]
            if payload.len() > 3 {
                payload[3]
            } else {
                0
            }
        } else {
            // Version <= 1: [mic_count, mic_id, level, ...]
            if payload.len() > 2 {
                payload[2]
            } else {
                0
            }
        };

        Ok(level)
    }

    /// Sets the sidetone level in percentage `0..=100`.
    pub async fn set_sidetone_level(&self, version: u8, level: u8) -> Result<(), Hidpp20Error> {
        let level = level.min(100);
        if version > 1 {
            // Version > 1: [mic_id = 0x01, reserved = 0xFF, level]
            self.endpoint.call(1, [0x01, 0xff, level]).await?;
        } else {
            // Version <= 1: [mic_id = 0x01, level, 0x00]
            self.endpoint.call(1, [0x01, level, 0x00]).await?;
        }
        Ok(())
    }
}
