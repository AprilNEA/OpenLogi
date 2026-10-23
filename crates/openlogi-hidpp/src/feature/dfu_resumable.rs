//! Implements the `DfuResumable` feature (ID `0x00D1`) that writes a firmware
//! image into a device's inactive application bank while the device keeps
//! running, then activates it.
//!
//! No public Logitech specification covers this feature. The layout here
//! follows fwupd's `logitech-hidpp` plugin (`fu-logitech-hidpp-rdfu.rs`,
//! LGPL-2.1-or-later, by Denis Pynkin / Collabora) and was confirmed against an
//! MX Master 4 (firmware `RBM` 27.3.19) reinstalling its official image over a
//! Bolt receiver. Treat every byte offset below as reverse-engineered.
//!
//! See `docs/FIRMWARE_UPDATES.md` at the repository root for the complete
//! flashing sequence, validation boundaries and hardware evidence.

use std::time::Duration;

use num_enum::TryFromPrimitive;
use openlogi_hidpp_derive::Feature;

use crate::{
    feature::{DecodeEvent, EventSource, FeatureEndpoint},
    protocol::v20::{self, Hidpp20Error},
};

/// Bytes in one `transferDfuData` packet.
pub const DFU_PACKET_LEN: usize = 16;

/// Bytes in the magic string `startDfu` names the image with.
pub const DFU_MAGIC_LEN: usize = 10;

/// Function ids of the feature.
const GET_CAPABILITIES: u8 = 0;
const START_DFU: u8 = 1;
const GET_DFU_STATUS: u8 = 2;
const APPLY_DFU: u8 = 3;
const TRANSFER_DFU_DATA: u8 = 4;

/// Implements the `DfuResumable` / `0x00D1` feature.
#[derive(Feature)]
#[creatable(id = 0x00d1, version = 0)]
pub struct DfuResumableFeature {
    /// The endpoint this feature talks to.
    endpoint: FeatureEndpoint,

    /// Publishes the status reports the device broadcasts after a
    /// [`DfuStatus::TransferWait`].
    events: EventSource<DfuStatusReport>,
}

impl DfuResumableFeature {
    /// Reads which update modes the device supports.
    pub async fn get_capabilities(&self) -> Result<DfuCapabilities, Hidpp20Error> {
        let payload = self
            .endpoint
            .call_long(GET_CAPABILITIES, [0; 16])
            .await?
            .extend_payload();
        Ok(DfuCapabilities::from_bits_retain(payload[0]))
    }

    /// Opens an update of firmware `entity` with the image named `magic`.
    pub async fn start_dfu(
        &self,
        entity: u8,
        magic: [u8; DFU_MAGIC_LEN],
    ) -> Result<DfuStatusReport, Hidpp20Error> {
        let mut args = [0; 16];
        args[0] = entity;
        args[1..=DFU_MAGIC_LEN].copy_from_slice(&magic);
        self.status_call(START_DFU, args).await
    }

    /// Reads where the update of firmware `entity` stands.
    pub async fn get_dfu_status(&self, entity: u8) -> Result<DfuStatusReport, Hidpp20Error> {
        let mut args = [0; 16];
        args[0] = entity;
        self.status_call(GET_DFU_STATUS, args).await
    }

    /// Sends the next packet of the block the device asked for.
    pub async fn transfer_dfu_data(
        &self,
        packet: [u8; DFU_PACKET_LEN],
    ) -> Result<DfuStatusReport, Hidpp20Error> {
        self.status_call(TRANSFER_DFU_DATA, packet).await
    }

    /// Activates the transferred image of `entity`, forcing the reboot.
    ///
    /// The device restarts into the new bank without answering, so this does
    /// not wait for a response.
    pub async fn apply_dfu(&self, entity: u8) -> Result<(), Hidpp20Error> {
        let mut args = [0; 16];
        args[0] = entity;
        args[1] = DfuApplyFlags::FORCE.bits();
        self.endpoint.notify_long(APPLY_DFU, args).await
    }

    async fn status_call(
        &self,
        function: u8,
        args: [u8; 16],
    ) -> Result<DfuStatusReport, Hidpp20Error> {
        // REVERSE-ENGINEERED: RDFU may answer a command with its status
        // broadcast (software ID 0, event sub-ID 0) instead of echoing the
        // request header. Only this feature uses that response convention.
        let header = self.endpoint.header(function);
        let raw = self
            .endpoint
            .chan
            .send_write_through(
                v20::Message::Long(header, args).into(),
                move |raw| {
                    let response = v20::Message::from(*raw);
                    let received = response.header();
                    response.feature_error_for(header).is_some()
                        || received == header
                        || (received.device_index == header.device_index
                            && received.feature_index == header.feature_index
                            && received.software_id.to_lo() == 0
                            && received.function_id.to_lo() == 0)
                },
                Duration::from_secs(8),
            )
            .await?;
        let response = v20::Message::from(raw);
        if let Some(error) = response.feature_error_for(header) {
            return Err(error);
        }
        DfuStatusReport::from_message(&response).ok_or(Hidpp20Error::UnsupportedResponse)
    }
}

bitflags::bitflags! {
    /// What `getCapabilities` reports in its first byte.
    #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
    #[cfg_attr(feature = "serde", derive(serde::Serialize))]
    pub struct DfuCapabilities: u8 {
        /// An interrupted transfer continues from the block the device names.
        const RESUMABLE = 1;
        /// The device can hold a transferred image until a later apply.
        const DEFERRABLE = 1 << 1;
        /// `applyDfu` may force the reboot.
        const FORCIBLE = 1 << 2;
    }
}

bitflags::bitflags! {
    /// The flags byte of `applyDfu`.
    #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
    struct DfuApplyFlags: u8 {
        /// Reboot into the new image now.
        const FORCE = 1 << 1;
    }
}

/// A status reply or broadcast: which entity it is about and its state.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct DfuStatusReport {
    /// The firmware entity the update targets.
    pub entity: u8,
    /// Where the update stands.
    pub status: DfuStatus,
}

impl DfuStatusReport {
    fn from_message(message: &v20::Message) -> Option<Self> {
        match message {
            v20::Message::Short(_, payload) => Self::decode(payload),
            v20::Message::Long(_, payload) => Self::decode(payload),
        }
    }

    /// Decodes `[entity, code, params…]`, rejecting unknown codes and missing
    /// bytes before reading status parameters.
    fn decode(payload: &[u8]) -> Option<Self> {
        let entity = *payload.first()?;
        let param_u16 = || Some(u16::from_be_bytes([*payload.get(2)?, *payload.get(3)?]));
        let status = match *payload.get(1)? {
            0x01 => DfuStatus::NotStarted,
            0x02 => DfuStatus::TransferReady {
                block: param_u16()?,
            },
            0x03 => DfuStatus::TransferWait {
                delay_ms: param_u16()?,
            },
            0x04 => DfuStatus::TransferComplete,
            0x05 => DfuStatus::ApplyPending,
            0x06 => DfuStatus::PacketAck {
                next_packet: u32::from_be_bytes(payload.get(2..6)?.try_into().ok()?),
            },
            0x07 => DfuStatus::Aborted,
            code => DfuStatus::Error(DfuError::try_from(code).ok()?),
        };
        Some(Self { entity, status })
    }
}

impl DecodeEvent for DfuStatusReport {
    fn decode_message(message: &v20::Message) -> Option<Self> {
        // Length depends on the status code, so preserve the original short
        // or long payload instead of the default zero-extended event path.
        (message.header().function_id.to_lo() == 0)
            .then(|| Self::from_message(message))
            .flatten()
    }

    /// The device broadcasts the next status on sub-id `0` once a wait ends.
    fn decode(sub_id: u8, payload: &[u8; 16]) -> Option<Self> {
        (sub_id == 0).then(|| Self::decode(payload)).flatten()
    }
}

/// The state an update reports.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub enum DfuStatus {
    /// No update is open for the entity.
    NotStarted,
    /// Send block `block`, from its first packet.
    TransferReady {
        /// The zero-based block the device wants next.
        block: u16,
    },
    /// The device is busy (typically committing a block). Wait for the
    /// broadcast that follows, at most a small multiple of `delay_ms`.
    TransferWait {
        /// How long the device expects to be busy.
        delay_ms: u16,
    },
    /// Every block arrived; the image can be applied.
    TransferComplete,
    /// A transferred image is waiting to be applied.
    ApplyPending,
    /// The packet arrived; send packet `next_packet` of the current block.
    PacketAck {
        /// The zero-based packet the device wants next.
        next_packet: u32,
    },
    /// The device abandoned the update.
    Aborted,
    /// The device refused the request.
    Error(DfuError),
}

/// Why the device refused a DFU request.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, TryFromPrimitive)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
#[repr(u8)]
pub enum DfuError {
    /// The image's magic string does not name this device's firmware.
    InvalidMagic = 0x80,
    /// The entity cannot be updated.
    InvalidEntity = 0x81,
    /// The device is busy with something else.
    DeviceBusy = 0x82,
    /// Writing to flash failed.
    OperationFailed = 0x83,
    /// The request is not supported.
    NotSupported = 0x84,
    /// The request does not fit the update's current state.
    StateError = 0x85,
    /// The packet does not belong to the expected block.
    InvalidBlock = 0x86,
    /// Unspecified failure.
    Generic = 0xff,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn header() -> v20::MessageHeader {
        v20::MessageHeader {
            device_index: 3,
            feature_index: 8,
            function_id: crate::nibble::U4::from_lo(0),
            software_id: crate::nibble::U4::from_lo(0),
        }
    }

    #[test]
    fn truncated_status_events_are_not_zero_extended_into_valid_states() {
        for code in [0x02, 0x03, 0x06] {
            let short = v20::Message::Short(header(), [1, code, 0]);
            assert_eq!(
                <DfuStatusReport as DecodeEvent>::decode_message(&short),
                None
            );
        }
    }

    #[test]
    fn truncated_status_responses_are_unsupported() {
        use crate::channel::mock::{MockRawHidChannel, channel_with_reader};
        use crate::feature::CreatableFeature;
        use std::sync::Arc;

        futures::executor::block_on(async {
            let (raw, handle) = MockRawHidChannel::new();
            let channel = Arc::new(channel_with_reader(raw).await);
            let feature = DfuResumableFeature::new(channel, 3, 8);
            for echo in [false, true] {
                let mut reply = header();
                if echo {
                    reply.function_id = crate::nibble::U4::from_lo(2);
                    reply.software_id = crate::nibble::U4::from_lo(1);
                }
                for code in [0x02, 0x03, 0x06] {
                    handle.queue_response(v20::Message::Short(reply, [1, code, 0]).into());
                    assert!(matches!(
                        feature.get_dfu_status(1).await,
                        Err(Hidpp20Error::UnsupportedResponse)
                    ));
                }
                for (code, status) in [(1, DfuStatus::NotStarted), (4, DfuStatus::TransferComplete)]
                {
                    handle.queue_response(v20::Message::Short(reply, [1, code, 0]).into());
                    assert_eq!(feature.get_dfu_status(1).await.unwrap().status, status);
                }
            }
        });
    }

    #[test]
    fn status_parameter_lengths_are_checked_before_decoding() {
        for bytes in [
            &[1, 2, 0, 0][..],
            &[1, 3, 0, 0][..],
            &[1, 6, 0, 0, 0, 0][..],
        ] {
            for length in 0..bytes.len() {
                assert_eq!(DfuStatusReport::decode(&bytes[..length]), None);
            }
            assert!(DfuStatusReport::decode(bytes).is_some());
        }
        for code in [1, 4, 5, 7, 0x80] {
            let short = v20::Message::Short(header(), [1, code, 0]);
            assert!(<DfuStatusReport as DecodeEvent>::decode_message(&short).is_some());
        }
    }

    fn payload(bytes: &[u8]) -> [u8; 16] {
        let mut payload = [0; 16];
        payload[..bytes.len()].copy_from_slice(bytes);
        payload
    }

    /// Replies captured from an MX Master 4 reinstalling 27.3.19 into entity 1.
    #[test]
    fn decodes_the_recorded_status_replies() {
        let decode = |bytes: &[u8]| DfuStatusReport::decode(&payload(bytes)).map(|r| r.status);
        assert_eq!(decode(&[1, 0x01]), Some(DfuStatus::NotStarted));
        assert_eq!(
            decode(&[1, 0x02, 0x00, 0x01]),
            Some(DfuStatus::TransferReady { block: 1 })
        );
        assert_eq!(
            decode(&[1, 0x03, 0x01, 0x13]),
            Some(DfuStatus::TransferWait { delay_ms: 275 })
        );
        assert_eq!(
            decode(&[1, 0x06, 0, 0, 0x02, 0xdd]),
            Some(DfuStatus::PacketAck { next_packet: 733 })
        );
        assert_eq!(decode(&[1, 0x04]), Some(DfuStatus::TransferComplete));
    }

    #[test]
    fn keeps_the_entity_and_decodes_device_errors() {
        let report = DfuStatusReport::decode(&payload(&[2, 0x86])).expect("known error code");
        assert_eq!(report.entity, 2);
        assert_eq!(report.status, DfuStatus::Error(DfuError::InvalidBlock));
    }

    #[test]
    fn an_unknown_code_is_not_silently_mapped() {
        assert_eq!(DfuStatusReport::decode(&payload(&[1, 0x08])), None);
        assert_eq!(DfuStatusReport::decode(&payload(&[1, 0x90])), None);
    }

    #[test]
    fn only_sub_id_zero_is_a_status_broadcast() {
        let raw = payload(&[1, 0x02, 0x00, 0x05]);
        assert_eq!(
            <DfuStatusReport as DecodeEvent>::decode(0, &raw).map(|r| r.status),
            Some(DfuStatus::TransferReady { block: 5 })
        );
        assert_eq!(<DfuStatusReport as DecodeEvent>::decode(1, &raw), None);
    }

    #[test]
    fn capabilities_keep_unknown_bits() {
        let caps = DfuCapabilities::from_bits_retain(0x0f);
        assert!(caps.contains(DfuCapabilities::RESUMABLE | DfuCapabilities::FORCIBLE));
        assert_eq!(caps.bits(), 0x0f);
    }
}
