//! The HOT upload: how data reaches the device's RAM page.
//!
//! Reverse engineered; follows libratbag's `hidpp10_send_hot_payload` and
//! the helpers it calls (`src/hidpp10.c`, MIT). A short `SET_REGISTER` of
//! `01 00 00` to the HOT control register `0xA1` starts a transfer. The data
//! then travels in long reports that are not register accesses: sub id
//! `0x92` opens the transfer with a header naming the destination and the
//! size and carries the first seven bytes, sub id `0x93` carries the next
//! sixteen at a time. Where a register access names its address, these carry
//! a sequence number counted from 0. The device answers each with a report
//! of sub id `0x50` whose first parameter byte repeats that number.
//!
//! libratbag's names stop at "HOT"; what the letters stand for is not known.

use std::time::Duration;

use tracing::debug;

use super::{HALF_PAGE, HALF_PAGE_LEN, RAM_PAGE, write::StepError};
use crate::{
    channel::{HidppChannel, HidppMessage, LONG_REPORT_LENGTH},
    protocol::v10::{ErrorType, Hidpp10Error, MessageType},
};

/// The HOT control register's address.
const CONTROL_REGISTER: u8 = 0xa1;

/// What a write to [`CONTROL_REGISTER`] carries to start a transfer.
const RESET: [u8; 3] = [0x01, 0x00, 0x00];

/// The sub id of the report that opens a transfer.
const FIRST_CHUNK: u8 = 0x92;

/// The sub id of every later report of a transfer.
const NEXT_CHUNK: u8 = 0x93;

/// The sub id of the device's answer to a chunk.
const ACK: u8 = 0x50;

/// The first byte of the transfer header; libratbag always sends 1.
const TRANSFER_ID: u8 = 0x01;

/// The bytes of a long report after device index, sub id and sequence
/// number.
const CHUNK_PAYLOAD: usize = LONG_REPORT_LENGTH - 4;

/// The size of the transfer header: id, page, word offset, two zero bytes,
/// the size big-endian, two zero bytes.
const HEADER_LENGTH: usize = 9;

/// The data bytes the opening report has room for after the header.
const FIRST_CHUNK_DATA: usize = CHUNK_PAYLOAD - HEADER_LENGTH;

/// One long report of a transfer: device index, sub id, sequence number,
/// then the payload.
type ChunkReport = [u8; LONG_REPORT_LENGTH - 1];

/// The long reports that carry `data` to the start of [`RAM_PAGE`], in the
/// order they go out. The last one is zero-padded, as libratbag sends it.
fn chunk_reports(device_index: u8, data: &[u8; HALF_PAGE]) -> Vec<ChunkReport> {
    let (first, rest) = data.split_at(FIRST_CHUNK_DATA);
    let size = HALF_PAGE_LEN.to_be_bytes();

    let mut opening: ChunkReport = [0; LONG_REPORT_LENGTH - 1];
    opening[..3].copy_from_slice(&[device_index, FIRST_CHUNK, 0]);
    opening[3..3 + HEADER_LENGTH].copy_from_slice(&[
        TRANSFER_ID,
        RAM_PAGE,
        0x00,
        0x00,
        0x00,
        size[0],
        size[1],
        0x00,
        0x00,
    ]);
    opening[3 + HEADER_LENGTH..].copy_from_slice(first);

    let mut reports = vec![opening];
    for (sequence, chunk) in (1..=u8::MAX).zip(rest.chunks(CHUNK_PAYLOAD)) {
        let mut report: ChunkReport = [0; LONG_REPORT_LENGTH - 1];
        report[..3].copy_from_slice(&[device_index, NEXT_CHUNK, sequence]);
        report[3..3 + chunk.len()].copy_from_slice(chunk);
        reports.push(report);
    }
    reports
}

/// Uploads `data` to the start of [`RAM_PAGE`], waiting at most
/// `ack_timeout` for each chunk's acknowledgement.
pub(super) async fn upload(
    chan: &HidppChannel,
    device_index: u8,
    data: &[u8; HALF_PAGE],
    ack_timeout: Duration,
) -> Result<(), StepError> {
    chan.write_register(device_index, CONTROL_REGISTER, RESET)
        .await
        .map_err(StepError::Reset)?;
    for report in chunk_reports(device_index, data) {
        let [_, sub_id, sequence, ..] = report;
        let reply = chan
            .send_with_timeout(
                HidppMessage::Long(report),
                move |reply| answers_chunk(device_index, sub_id, sequence, reply),
                ack_timeout,
            )
            .await
            .map_err(|source| StepError::Chunk {
                chunk: sequence,
                source: source.into(),
            })?;
        check_ack(sequence, &reply)?;
    }
    Ok(())
}

/// The first four bytes of a report: device index, sub id, then the bytes a
/// register access calls address and first parameter.
fn head(report: &HidppMessage) -> [u8; 4] {
    match report {
        HidppMessage::Short([a, b, c, d, ..]) | HidppMessage::Long([a, b, c, d, ..]) => {
            [*a, *b, *c, *d]
        }
    }
}

/// Whether `reply` answers the chunk `(sub_id, sequence)` of a transfer to
/// `device_index`: any acknowledgement, which [`check_ack`] then holds to
/// the sequence number, or an error report naming this chunk.
fn answers_chunk(device_index: u8, sub_id: u8, sequence: u8, reply: &HidppMessage) -> bool {
    let [device, reply_sub_id, third, fourth] = head(reply);
    device == device_index
        && (reply_sub_id == ACK
            || (reply_sub_id == u8::from(MessageType::Error)
                && third == sub_id
                && fourth == sequence))
}

/// The verdict on the reply [`answers_chunk`] accepted for chunk `sequence`.
fn check_ack(sequence: u8, reply: &HidppMessage) -> Result<(), StepError> {
    let [_, sub_id, second, acknowledged] = head(reply);
    if sub_id == u8::from(MessageType::Error) {
        let code = match reply {
            HidppMessage::Short(bytes) => bytes[4],
            HidppMessage::Long(bytes) => bytes[4],
        };
        let source = ErrorType::try_from(code).map_or(
            Hidpp10Error::UnsupportedResponse,
            Hidpp10Error::RegisterAccess,
        );
        return Err(StepError::Chunk {
            chunk: sequence,
            source,
        });
    }
    if acknowledged != sequence {
        return Err(StepError::UnexpectedAck {
            chunk: sequence,
            acknowledged,
        });
    }
    debug!(sequence, second, "HOT chunk acknowledged");
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::{sync::Arc, time::Duration};

    use super::{chunk_reports, upload};
    use crate::{
        channel::{
            HidppMessage,
            mock::{MockRawHidChannel, channel_with_reader},
        },
        protocol::v10::{ErrorType, Hidpp10Error},
        register::onboard_memory::{HALF_PAGE, write::StepError},
    };

    fn counting_data() -> [u8; HALF_PAGE] {
        std::array::from_fn(|i| u8::try_from(i).unwrap())
    }

    fn ack(sequence: u8) -> HidppMessage {
        HidppMessage::Short([1, 0x50, 0x01, sequence, 0x00, 0x00])
    }

    fn reset_ack() -> HidppMessage {
        HidppMessage::Short([1, 0x80, 0xa1, 0x00, 0x00, 0x00])
    }

    #[test]
    fn half_a_page_goes_out_in_seventeen_numbered_chunks() {
        let data = counting_data();
        let reports = chunk_reports(1, &data);
        assert_eq!(reports.len(), 17, "7 bytes, then 15 × 16, then 9");

        let opening = reports[0];
        assert_eq!(
            opening,
            [
                1, 0x92, 0x00, // device, opening sub id, sequence 0
                0x01, 0x00, 0x00, 0x00, 0x00, // id 1, RAM page 0, word 0, zero
                0x01, 0x00, 0x00, 0x00, // 256 bytes, big-endian, then zero
                0, 1, 2, 3, 4, 5, 6, // the first seven data bytes
            ]
        );

        let mut carried = opening[12..].to_vec();
        for (index, report) in reports.iter().enumerate().skip(1) {
            assert_eq!(report[..3], [1, 0x93, u8::try_from(index).unwrap()]);
            carried.extend_from_slice(&report[3..]);
        }
        assert_eq!(carried[..HALF_PAGE], data[..], "the data in order");
        assert!(
            carried[HALF_PAGE..].iter().all(|&b| b == 0),
            "the last chunk is zero-padded"
        );
    }

    #[test]
    fn every_chunk_waits_for_its_acknowledgement() {
        futures::executor::block_on(async {
            let (raw, handle) = MockRawHidChannel::new();
            let channel = Arc::new(channel_with_reader(raw).await);
            handle.queue_response(reset_ack());
            for sequence in 0..17 {
                handle.queue_response(ack(sequence));
            }

            upload(&channel, 1, &counting_data(), Duration::from_secs(1))
                .await
                .unwrap();

            let written = handle.written_reports();
            assert_eq!(written.len(), 18);
            assert_eq!(written[0], [0x10, 1, 0x80, 0xa1, 0x01, 0x00, 0x00], "reset");
            assert_eq!(written[1][..4], [0x11, 1, 0x92, 0x00]);
            assert_eq!(written[17][..4], [0x11, 1, 0x93, 16]);
        });
    }

    #[test]
    fn an_acknowledgement_for_another_chunk_stops_the_upload() {
        futures::executor::block_on(async {
            let (raw, handle) = MockRawHidChannel::new();
            let channel = Arc::new(channel_with_reader(raw).await);
            handle.queue_response(reset_ack());
            handle.queue_response(ack(0));
            handle.queue_response(ack(0));

            let result = upload(&channel, 1, &counting_data(), Duration::from_secs(1)).await;

            assert!(
                matches!(
                    result,
                    Err(StepError::UnexpectedAck {
                        chunk: 1,
                        acknowledged: 0
                    })
                ),
                "{result:?}"
            );
            assert_eq!(handle.written_reports().len(), 3, "no chunk after it");
        });
    }

    #[test]
    fn an_unacknowledged_chunk_times_out() {
        futures::executor::block_on(async {
            let (raw, handle) = MockRawHidChannel::new();
            let channel = Arc::new(channel_with_reader(raw).await);
            handle.queue_response(reset_ack());
            handle.queue_response(ack(0));

            let result = upload(&channel, 1, &counting_data(), Duration::from_millis(50)).await;

            assert!(
                matches!(
                    result,
                    Err(StepError::Chunk {
                        chunk: 1,
                        source: Hidpp10Error::Channel(_)
                    })
                ),
                "{result:?}"
            );
            assert_eq!(handle.written_reports().len(), 3);
        });
    }

    #[test]
    fn a_refused_chunk_is_its_error() {
        futures::executor::block_on(async {
            let (raw, handle) = MockRawHidChannel::new();
            let channel = Arc::new(channel_with_reader(raw).await);
            handle.queue_response(reset_ack());
            // A HID++ 1.0 error names the request's sub id and, where a
            // register access has its address, the chunk's sequence number.
            handle.queue_response(HidppMessage::Short([
                1,
                0x8f,
                0x92,
                0x00,
                ErrorType::InvalidValue.into(),
                0x00,
            ]));

            let result = upload(&channel, 1, &counting_data(), Duration::from_secs(1)).await;

            assert!(
                matches!(
                    result,
                    Err(StepError::Chunk {
                        chunk: 0,
                        source: Hidpp10Error::RegisterAccess(ErrorType::InvalidValue)
                    })
                ),
                "{result:?}"
            );
        });
    }

    #[test]
    fn a_refused_reset_sends_no_chunk() {
        futures::executor::block_on(async {
            let (raw, handle) = MockRawHidChannel::new();
            let channel = Arc::new(channel_with_reader(raw).await);
            handle.queue_response(HidppMessage::Short([
                1,
                0x8f,
                0x80,
                0xa1,
                ErrorType::InvalidAddress.into(),
                0x00,
            ]));

            let result = upload(&channel, 1, &counting_data(), Duration::from_secs(1)).await;

            assert!(
                matches!(
                    result,
                    Err(StepError::Reset(Hidpp10Error::RegisterAccess(
                        ErrorType::InvalidAddress
                    )))
                ),
                "{result:?}"
            );
            assert_eq!(handle.written_reports().len(), 1);
        });
    }
}
