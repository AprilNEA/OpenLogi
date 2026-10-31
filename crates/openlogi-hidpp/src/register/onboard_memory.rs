//! The onboard memory of HID++ 1.0 gaming mice (G500, G700, G9), read
//! through register `0xA2`.
//!
//! Reverse engineered; follows libratbag's `hidpp10_read_memory` and
//! `hidpp10_read_page` (`src/hidpp10.c`, MIT). Memory is split into
//! [`PAGE_SIZE`]-byte pages: page 0 is RAM, page 1 holds the
//! [`directory::ProfileDirectory`], and pages 2 to [`LAST_PAGE`] are flash,
//! where the profiles live (for the G700, [`g700::G700Profile`]). Every
//! page ends in a CRC over the rest of it, stored big-endian.
//!
//! A read is a short `GET_LONG_REGISTER` to `0xA2` naming a page and an
//! offset within it in 16-bit words; the long reply carries the sixteen bytes
//! that start there.

pub mod directory;
pub mod g700;

use crate::{channel::HidppChannel, protocol::v10::Hidpp10Error};

/// The register's address.
const REGISTER: u8 = 0xa2;

/// The size of one memory page in bytes.
pub const PAGE_SIZE: usize = 512;

/// The highest page number libratbag addresses on any of these mice.
pub const LAST_PAGE: u8 = 31;

/// The bytes one [`read_memory`] call returns.
pub const READ_CHUNK: usize = 16;

/// Where the page CRC sits: the last two bytes.
const CRC_OFFSET: usize = PAGE_SIZE - 2;

/// One page of onboard memory, exactly as the device stores it.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct Page {
    #[cfg_attr(feature = "serde", serde(with = "serde_page_bytes"))]
    bytes: [u8; PAGE_SIZE],
}

impl Page {
    /// Wraps the bytes of one page.
    #[must_use]
    pub fn from_bytes(bytes: [u8; PAGE_SIZE]) -> Self {
        Self { bytes }
    }

    /// The page's bytes, CRC included.
    #[must_use]
    pub fn as_bytes(&self) -> &[u8; PAGE_SIZE] {
        &self.bytes
    }

    /// The CRC the page carries in its last two bytes.
    #[must_use]
    pub fn stored_crc(&self) -> u16 {
        u16::from_be_bytes([self.bytes[CRC_OFFSET], self.bytes[CRC_OFFSET + 1]])
    }

    /// The CRC the page's content calls for.
    #[must_use]
    pub fn computed_crc(&self) -> u16 {
        crc16_ccitt_false(&self.bytes[..CRC_OFFSET])
    }

    /// Whether the stored CRC matches the content. libratbag notes the mouse
    /// still plays a profile whose CRC is wrong, so a mismatch is reported,
    /// not refused.
    #[must_use]
    pub fn has_valid_crc(&self) -> bool {
        self.stored_crc() == self.computed_crc()
    }

    /// Rewrites the stored CRC to match the content.
    pub fn update_crc(&mut self) {
        let crc = self.computed_crc();
        self.bytes[CRC_OFFSET..].copy_from_slice(&crc.to_be_bytes());
    }

    /// The bytes at `start..start + N`.
    fn array<const N: usize>(&self, start: usize) -> [u8; N] {
        let mut out = [0; N];
        out.copy_from_slice(&self.bytes[start..start + N]);
        out
    }
}

/// CRC-16/CCITT-FALSE (polynomial `0x1021`, initial value `0xFFFF`, no
/// reflection, no final XOR). libratbag's `hidpp_crc_ccitt`, which it credits
/// to Logitech, is the same function in its table-less nibble form; the
/// standard check value pins this one in the tests.
fn crc16_ccitt_false(data: &[u8]) -> u16 {
    data.iter().fold(0xffff, |crc, &byte| {
        let mut crc = crc ^ (u16::from(byte) << 8);
        for _ in 0..8 {
            crc = if crc & 0x8000 == 0 {
                crc << 1
            } else {
                (crc << 1) ^ 0x1021
            };
        }
        crc
    })
}

/// Reads [`READ_CHUNK`] bytes of `page`, starting `word_offset` 16-bit words
/// into it.
///
/// Every reply to this register has the same header, so a reply that arrives
/// after its own request timed out answers the next read in its place. A
/// caller that must trust the bytes checks the page CRC or reads twice.
pub async fn read_memory(
    chan: &HidppChannel,
    device_index: u8,
    page: u8,
    word_offset: u8,
) -> Result<[u8; READ_CHUNK], Hidpp10Error> {
    chan.read_long_register(device_index, REGISTER, [page, word_offset, 0x00])
        .await
}

/// Reads a whole page, one [`READ_CHUNK`] at a time. The CRC is not checked
/// here; see [`Page::has_valid_crc`].
pub async fn read_page(
    chan: &HidppChannel,
    device_index: u8,
    page: u8,
) -> Result<Page, Hidpp10Error> {
    let mut bytes = [0; PAGE_SIZE];
    for (word_offset, chunk) in (0..=u8::MAX)
        .step_by(READ_CHUNK / 2)
        .zip(bytes.as_chunks_mut::<READ_CHUNK>().0)
    {
        *chunk = read_memory(chan, device_index, page, word_offset).await?;
    }
    Ok(Page::from_bytes(bytes))
}

#[cfg(feature = "serde")]
mod serde_page_bytes {
    use super::PAGE_SIZE;

    pub(super) fn serialize<S: serde::Serializer>(
        bytes: &[u8; PAGE_SIZE],
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        serializer.serialize_bytes(bytes)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::{PAGE_SIZE, Page, crc16_ccitt_false, read_page};
    use crate::{
        channel::{
            HidppMessage,
            mock::{MockRawHidChannel, channel_with_reader},
        },
        protocol::v10::{ErrorType, Hidpp10Error, MessageType},
    };

    #[test]
    fn crc_matches_the_ccitt_false_check_value() {
        assert_eq!(crc16_ccitt_false(b"123456789"), 0x29b1);
        assert_eq!(crc16_ccitt_false(&[]), 0xffff);
    }

    #[test]
    fn page_crc_covers_everything_but_the_last_two_bytes() {
        let mut bytes = [0xff; PAGE_SIZE];
        bytes[0] = 0x02;
        let mut page = Page::from_bytes(bytes);
        assert!(!page.has_valid_crc(), "an all-0xFF tail is no CRC");

        page.update_crc();
        assert!(page.has_valid_crc());
        assert_eq!(&page.as_bytes()[..PAGE_SIZE - 2], &bytes[..PAGE_SIZE - 2]);
        // CRC-16/CCITT-FALSE of `02` and 509 bytes of `FF`, computed apart
        // from this implementation.
        assert_eq!(page.stored_crc(), 0x3b0a);
        assert_eq!(
            page.as_bytes()[PAGE_SIZE - 2..],
            [0x3b, 0x0a],
            "stored big-endian"
        );

        let mut corrupted = page.as_bytes().to_owned();
        corrupted[100] ^= 0x01;
        assert!(!Page::from_bytes(corrupted).has_valid_crc());
    }

    fn memory_reply(device: u8, data: [u8; 16]) -> HidppMessage {
        let mut reply = [0u8; 19];
        reply[0] = device;
        reply[1] = MessageType::GetLongRegister.into();
        reply[2] = 0xa2;
        reply[3..].copy_from_slice(&data);
        HidppMessage::Long(reply)
    }

    #[test]
    fn a_page_is_thirty_two_reads_eight_words_apart() {
        futures::executor::block_on(async {
            let (raw, handle) = MockRawHidChannel::new();
            let channel = Arc::new(channel_with_reader(raw).await);
            for chunk in 0..32u8 {
                handle.queue_response(memory_reply(1, [chunk; 16]));
            }

            let page = read_page(&channel, 1, 5).await.unwrap();
            for (chunk, bytes) in page.as_bytes().as_chunks::<16>().0.iter().enumerate() {
                assert!(
                    bytes.iter().all(|&b| usize::from(b) == chunk),
                    "chunk {chunk}"
                );
            }
            let written = handle.written_reports();
            assert_eq!(written.len(), 32);
            for (chunk, report) in written.iter().enumerate() {
                assert_eq!(
                    report[..],
                    [0x10, 1, 0x83, 0xa2, 5, u8::try_from(chunk * 8).unwrap(), 0],
                    "chunk {chunk}"
                );
            }
        });
    }

    #[test]
    fn a_refused_chunk_fails_the_page() {
        futures::executor::block_on(async {
            let (raw, handle) = MockRawHidChannel::new();
            let channel = Arc::new(channel_with_reader(raw).await);
            handle.queue_response(memory_reply(1, [0; 16]));
            handle.queue_response(HidppMessage::Short([
                1,
                MessageType::Error.into(),
                MessageType::GetLongRegister.into(),
                0xa2,
                ErrorType::InvalidValue.into(),
                0x00,
            ]));

            assert!(matches!(
                read_page(&channel, 1, 40).await,
                Err(Hidpp10Error::RegisterAccess(ErrorType::InvalidValue))
            ));
            assert_eq!(handle.written_reports().len(), 2, "stops at the refusal");
        });
    }
}
