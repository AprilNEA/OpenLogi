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

use std::{
    fmt,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use num_enum::TryFromPrimitive;
use openlogi_hidpp_derive::Feature;

use crate::{
    channel::MessageListenerGuard,
    feature::{DecodeEvent, FeatureEndpoint, InboundReport, inbound_report},
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
    ) -> Result<DfuReply, Hidpp20Error> {
        let mut args = [0; 16];
        args[0] = entity;
        args[1..=DFU_MAGIC_LEN].copy_from_slice(&magic);
        self.status_call(START_DFU, args).await
    }

    /// Reads where the update of firmware `entity` stands.
    pub async fn get_dfu_status(&self, entity: u8) -> Result<DfuReply, Hidpp20Error> {
        let mut args = [0; 16];
        args[0] = entity;
        self.status_call(GET_DFU_STATUS, args).await
    }

    /// Sends the next packet of the block the device asked for.
    pub async fn transfer_dfu_data(
        &self,
        packet: [u8; DFU_PACKET_LEN],
    ) -> Result<DfuReply, Hidpp20Error> {
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

    async fn status_call(&self, function: u8, args: [u8; 16]) -> Result<DfuReply, Hidpp20Error> {
        // The recorded MX Master 4 transfer answered every command with its
        // own request header and broadcast status (software ID 0, event 0)
        // only after a `TransferWait` reply. A reply is therefore matched by
        // header alone: a broadcast never answers a command, and a late reply
        // to an abandoned command is quarantined by the channel.
        let header = self.endpoint.header(function);
        let (sender, broadcasts) = async_channel::unbounded();
        let replied = Arc::new(AtomicBool::new(false));
        let own = self.endpoint.chan.request_sw_ids();
        // Registered before the write: the read loop dispatches reports in
        // arrival order, so the broadcasts seen after this command's own reply
        // are exactly the ones the device sent after answering it. The channel
        // never hands it a reply that answered an earlier same-header command.
        let listener = self.endpoint.chan.add_msg_listener_guarded({
            let replied = Arc::clone(&replied);
            move |raw, matched| {
                if matched {
                    if v20::Message::from(raw).header() == header {
                        replied.store(true, Ordering::SeqCst);
                    }
                    return;
                }
                if !replied.load(Ordering::SeqCst) {
                    return;
                }
                if let Some(InboundReport::Broadcast(message)) =
                    inbound_report(raw, matched, header.device_index, header.feature_index, own)
                    && let Some(report) = DfuStatusReport::decode_message(&message)
                {
                    let _ = sender.try_send(report);
                }
            }
        });
        let raw = self
            .endpoint
            .chan
            .send_write_through(
                v20::Message::Long(header, args).into(),
                move |raw| {
                    let response = v20::Message::from(*raw);
                    response.feature_error_for(header).is_some() || response.header() == header
                },
                REPLY_TIMEOUT,
            )
            .await?;
        let response = v20::Message::from(raw);
        if let Some(error) = response.feature_error_for(header) {
            return Err(error);
        }
        let report =
            DfuStatusReport::from_message(&response).ok_or(Hidpp20Error::UnsupportedResponse)?;
        Ok(DfuReply {
            report,
            broadcasts,
            _listener: listener,
        })
    }
}

/// How long a command waits for its own reply. Tests shorten it so a
/// deliberately unanswered command does not cost the full budget.
#[cfg(not(test))]
const REPLY_TIMEOUT: Duration = Duration::from_secs(8);
#[cfg(test)]
const REPLY_TIMEOUT: Duration = Duration::from_millis(200);

/// A command's status reply, with the status broadcasts the device sent after
/// it.
pub struct DfuReply {
    /// The status this command's own reply carried.
    pub report: DfuStatusReport,
    broadcasts: async_channel::Receiver<DfuStatusReport>,
    /// Keeps collecting broadcasts until the reply is dropped.
    _listener: MessageListenerGuard,
}

impl DfuReply {
    /// The next status broadcast the device sent after this reply, which is
    /// how it ends a [`DfuStatus::TransferWait`]. A broadcast that arrived
    /// before the reply belongs to an earlier state and is never returned.
    /// `None` once the channel closes.
    pub async fn next_broadcast(&self) -> Option<DfuStatusReport> {
        self.broadcasts.recv().await.ok()
    }
}

impl fmt::Debug for DfuReply {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DfuReply")
            .field("report", &self.report)
            .finish_non_exhaustive()
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
    use crate::{
        channel::{
            ChannelObservation, HidppChannel, HidppMessage,
            mock::{MockRawHidChannel, MockRawHidHandle, channel_with_reader},
        },
        feature::CreatableFeature,
    };

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

    fn mock_feature() -> (DfuResumableFeature, MockRawHidHandle) {
        futures::executor::block_on(async {
            let (raw, handle) = MockRawHidChannel::new();
            let channel = Arc::new(channel_with_reader(raw).await);
            (DfuResumableFeature::new(channel, 3, 8), handle)
        })
    }

    /// The header of the `index`th report the feature wrote.
    fn written_header(handle: &MockRawHidHandle, index: usize) -> v20::MessageHeader {
        let written = handle.written_reports();
        v20::Message::from(HidppMessage::read_raw(&written[index]).unwrap()).header()
    }

    /// Waits until the feature has written `count` reports.
    async fn written(handle: &MockRawHidHandle, count: usize) {
        while handle.written_reports().len() < count {
            futures_timer::Delay::new(Duration::from_millis(1)).await;
        }
    }

    fn status(header: v20::MessageHeader, payload: &[u8]) -> HidppMessage {
        let mut long = [0; 16];
        long[..payload.len()].copy_from_slice(payload);
        v20::Message::Long(header, long).into()
    }

    #[test]
    fn truncated_status_responses_are_unsupported() {
        let (feature, handle) = mock_feature();
        futures::executor::block_on(async {
            let mut reply = header();
            reply.function_id = crate::nibble::U4::from_lo(2);
            reply.software_id = crate::nibble::U4::from_lo(1);
            for code in [0x02, 0x03, 0x06] {
                handle.queue_response(v20::Message::Short(reply, [1, code, 0]).into());
                assert!(matches!(
                    feature.get_dfu_status(1).await,
                    Err(Hidpp20Error::UnsupportedResponse)
                ));
            }
            for (code, status) in [(1, DfuStatus::NotStarted), (4, DfuStatus::TransferComplete)] {
                handle.queue_response(v20::Message::Short(reply, [1, code, 0]).into());
                assert_eq!(
                    feature.get_dfu_status(1).await.unwrap().report.status,
                    status
                );
            }
        });
    }

    /// A broadcast that lands while a command waits is not its answer, and a
    /// later wait never sees it; only broadcasts after the reply end a wait.
    #[test]
    fn a_broadcast_never_answers_a_command_or_ends_a_later_wait() {
        let (feature, handle) = mock_feature();
        futures::executor::block_on(async {
            handle.park_writes();
            let command = feature.transfer_dfu_data([0; DFU_PACKET_LEN]);
            let device = async {
                written(&handle, 1).await;
                // Late, from an earlier wait: it asks for a block this
                // command's reply must not be confused with.
                handle
                    .send_incoming(status(header(), &[1, 0x02, 0x00, 0x07]))
                    .await;
                handle.queue_response(status(written_header(&handle, 0), &[1, 0x03, 0x00, 0x10]));
                handle.release_writes();
            };
            let (reply, ()) = futures::join!(command, device);
            let reply = reply.unwrap();
            assert_eq!(
                reply.report.status,
                DfuStatus::TransferWait { delay_ms: 16 }
            );
            assert!(
                reply.broadcasts.try_recv().is_err(),
                "a broadcast from before the reply cannot end its wait"
            );
            handle
                .send_incoming(status(header(), &[1, 0x02, 0x00, 0x01]))
                .await;
            assert_eq!(
                reply.next_broadcast().await.unwrap().status,
                DfuStatus::TransferReady { block: 1 }
            );
        });
    }

    /// A packet command sent while the read loop is still dispatching the
    /// previous packet's reply — same header — does not take that reply for
    /// its own, so a broadcast before its own reply cannot end its wait.
    #[test]
    fn a_follow_up_command_does_not_inherit_the_previous_reply() {
        futures::executor::block_on(async {
            // Holds the read loop after it resolves the first command, until
            // the follow-up command has registered its listener and written.
            let (written_follow_up, gate) = std::sync::mpsc::channel::<()>();
            let gate = std::sync::Mutex::new(Some(gate));
            let observer = move |observation: ChannelObservation| {
                if let ChannelObservation::IncomingReport {
                    request_id: Some(_),
                    ..
                } = observation
                    && let Some(gate) = gate.lock().unwrap().take()
                {
                    let _ = gate.recv_timeout(Duration::from_secs(1));
                }
            };
            let (raw, handle) = MockRawHidChannel::new();
            let channel = HidppChannel::from_raw_channel_with_observer(raw, Arc::new(observer))
                .await
                .unwrap();
            let feature = DfuResumableFeature::new(Arc::new(channel), 3, 8);

            handle.park_writes();
            let first = feature.transfer_dfu_data([0; DFU_PACKET_LEN]);
            let device = async {
                written(&handle, 1).await;
                handle.queue_response(status(written_header(&handle, 0), &[1, 0x06, 0, 0, 0, 1]));
                handle.release_writes();
            };
            let (first, ()) = futures::join!(first, device);
            assert_eq!(
                first.unwrap().report.status,
                DfuStatus::PacketAck { next_packet: 1 }
            );

            handle.park_writes();
            let follow_up = feature.transfer_dfu_data([1; DFU_PACKET_LEN]);
            let device = async {
                written(&handle, 2).await;
                assert_eq!(written_header(&handle, 1), written_header(&handle, 0));
                written_follow_up.send(()).unwrap();
                // Sent before the follow-up's reply: it must not end its wait.
                handle
                    .send_incoming(status(header(), &[1, 0x02, 0x00, 0x07]))
                    .await;
                handle.queue_response(status(written_header(&handle, 1), &[1, 0x03, 0x00, 0x10]));
                handle.release_writes();
            };
            let (reply, ()) = futures::join!(follow_up, device);
            let reply = reply.unwrap();
            assert_eq!(
                reply.report.status,
                DfuStatus::TransferWait { delay_ms: 16 }
            );
            assert!(
                reply.broadcasts.try_recv().is_err(),
                "a broadcast from before the reply cannot end its wait"
            );
        });
    }

    /// A reply that arrives after its command timed out answers neither a
    /// different command nor a re-ask of the same one.
    #[test]
    fn a_reply_after_its_timeout_is_not_the_next_commands_answer() {
        let (feature, handle) = mock_feature();
        futures::executor::block_on(async {
            assert!(matches!(
                feature.get_dfu_status(1).await,
                Err(Hidpp20Error::Channel(_))
            ));
            let abandoned = written_header(&handle, 0);
            let late = status(abandoned, &[1, 0x06, 0, 0, 0, 0x09]);

            handle.park_writes();
            let command = feature.transfer_dfu_data([0; DFU_PACKET_LEN]);
            let device = async {
                written(&handle, 2).await;
                handle.send_incoming(late).await;
                handle.queue_response(status(written_header(&handle, 1), &[1, 0x06, 0, 0, 0, 1]));
                handle.release_writes();
            };
            let (reply, ()) = futures::join!(command, device);
            assert_eq!(
                reply.unwrap().report.status,
                DfuStatus::PacketAck { next_packet: 1 }
            );
        });

        // The same header again: the channel holds the re-ask until the
        // abandoned reply lands and is discarded.
        let (feature, handle) = mock_feature();
        futures::executor::block_on(async {
            assert!(matches!(
                feature.get_dfu_status(1).await,
                Err(Hidpp20Error::Channel(_))
            ));
            let abandoned = written_header(&handle, 0);
            let reask = feature.get_dfu_status(1);
            let device = async {
                futures_timer::Delay::new(Duration::from_millis(20)).await;
                handle.queue_response(status(abandoned, &[1, 0x01]));
                handle
                    .send_incoming(status(abandoned, &[1, 0x06, 0, 0, 0, 0x09]))
                    .await;
            };
            let (reply, ()) = futures::join!(reask, device);
            assert_eq!(reply.unwrap().report.status, DfuStatus::NotStarted);
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
