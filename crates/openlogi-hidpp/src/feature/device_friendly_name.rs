//! Implements the `DeviceFriendlyName` feature (ID `0x0007`) that provides
//! functionality to set and retrieve a custom device name.

use openlogi_hidpp_derive::Feature;
use tracing::warn;

use crate::{
    feature::FeatureEndpoint,
    protocol::v20::{ErrorType, Hidpp20Error},
};

/// Name bytes one `setFriendlyName` request carries after its char index.
const NAME_CHUNK_LEN: usize = 15;

/// Implements the `DeviceFriendlyName` / `0x0007` feature.
#[derive(Clone, Feature)]
#[creatable(id = 0x0007, version = 0)]
pub struct DeviceFriendlyNameFeature {
    /// The endpoint this feature talks to.
    endpoint: FeatureEndpoint,
}

impl DeviceFriendlyNameFeature {
    /// Retrieves the length data of the friendly device name feature.
    pub async fn get_friendly_name_length(&self) -> Result<DeviceFriendlyNameLength, Hidpp20Error> {
        let payload = self.endpoint.call(0, [0; 3]).await?.extend_payload();

        Ok(DeviceFriendlyNameLength {
            name_length: payload[0],
            name_max_length: payload[1],
            default_name_length: payload[2],
        })
    }

    /// Retrieves a chunk of characters of the friendly name of the device,
    /// starting at a specific index (inclusive).
    ///
    /// This function will always retrieve 15 bytes, filling up the rest with
    /// zeroes if the chunk is shorter than that.
    ///
    /// Use this function in conjunction with [`Self::get_friendly_name_length`]
    /// to retrieve the whole friendly name of the device.\
    /// A convenience wrapper implementing this functionality is provided as
    /// [`Self::get_whole_friendly_name`].
    pub async fn get_friendly_name(&self, index: u8) -> Result<[u8; 15], Hidpp20Error> {
        self.read_name_chunk(1, index).await
    }

    /// Retrieves the whole friendly name of the device by first calling
    /// [`Self::get_friendly_name_length`] once and then repeatedly calling
    /// [`Self::get_friendly_name`] until all characters were received.
    pub async fn get_whole_friendly_name(&self) -> Result<String, Hidpp20Error> {
        let count = self.get_friendly_name_length().await?.name_length;
        let mut string = String::with_capacity(count as usize);

        let mut len = 0;
        while len < count as usize {
            #[expect(
                clippy::cast_possible_truncation,
                reason = "len < count as usize and count is a u8, so len always fits in u8"
            )]
            let index = len as u8;
            let part = self.get_friendly_name(index).await?;
            string.push_str(str::from_utf8(&part).map_err(|_| Hidpp20Error::UnsupportedResponse)?);
            len = string.len();
        }

        Ok(string.trim_end_matches(char::from(0)).to_string())
    }

    /// Retrieves a chunk of characters of the default friendly name of the
    /// device, starting at a specific index (inclusive).
    ///
    /// This function will always retrieve 15 bytes, filling up the rest with
    /// zeroes if the chunk is shorter than that.
    ///
    /// Use this function in conjunction with [`Self::get_friendly_name_length`]
    /// to retrieve the whole default friendly name of the device.\
    /// A convenience wrapper implementing this functionality is provided as
    /// [`Self::get_whole_default_friendly_name`].
    pub async fn get_default_friendly_name(&self, index: u8) -> Result<[u8; 15], Hidpp20Error> {
        self.read_name_chunk(2, index).await
    }

    /// Reads one 15-byte name chunk at `index` with `function` (1: friendly
    /// name, 2: default name). Byte 0 of the reply echoes the char index; a
    /// mismatch is logged and the chunk is still returned, as Options+ does,
    /// since a firmware that echoes wrongly would otherwise lose its name.
    async fn read_name_chunk(&self, function: u8, index: u8) -> Result<[u8; 15], Hidpp20Error> {
        let payload = self
            .endpoint
            .call(function, [index, 0x00, 0x00])
            .await?
            .extend_payload();
        let echoed = payload[0];
        if echoed != index {
            warn!(
                function,
                index, echoed, "name read echoed another char index"
            );
        }
        let mut chunk = [0; NAME_CHUNK_LEN];
        chunk.copy_from_slice(&payload[1..=NAME_CHUNK_LEN]);
        Ok(chunk)
    }

    /// Retrieves the whole default friendly name of the device by first calling
    /// [`Self::get_friendly_name_length`] once and then repeatedly calling
    /// [`Self::get_default_friendly_name`] until all characters were received.
    pub async fn get_whole_default_friendly_name(&self) -> Result<String, Hidpp20Error> {
        let count = self.get_friendly_name_length().await?.default_name_length;
        let mut string = String::with_capacity(count as usize);

        let mut len = 0;
        while len < count as usize {
            #[expect(
                clippy::cast_possible_truncation,
                reason = "len < count as usize and count is a u8, so len always fits in u8"
            )]
            let index = len as u8;
            let part = self.get_default_friendly_name(index).await?;
            string.push_str(str::from_utf8(&part).map_err(|_| Hidpp20Error::UnsupportedResponse)?);
            len = string.len();
        }

        Ok(string.trim_end_matches(char::from(0)).to_string())
    }

    /// Sets a chunk of the friendly device name, starting at a specific index
    /// (inclusive).
    ///
    /// If the index and chunk combination would exceed the
    /// [`DeviceFriendlyNameLength::name_max_length`], the name is automatically
    /// truncated by the device.
    ///
    /// Returns the char index the device echoes back. It is not the name's
    /// length: Logi Options+ compares it with the index it sent to check where
    /// the chunk landed.
    ///
    /// A convenience wrapper setting the whole friendly device name at once is
    /// provided as [`Self::set_whole_device_name`].
    pub async fn set_friendly_name(&self, index: u8, chunk: [u8; 15]) -> Result<u8, Hidpp20Error> {
        let mut data = [0u8; 16];
        data[0] = index;
        data[1..].copy_from_slice(&chunk);

        let payload = self.endpoint.call_long(3, data).await?.extend_payload();

        Ok(payload[0])
    }

    /// Sets the whole friendly device name, truncating the value to a maximum
    /// of [`DeviceFriendlyNameLength::name_max_length`] bytes, cut back to a
    /// UTF-8 character boundary so the stored name still decodes.
    ///
    /// This method calls [`Self::get_friendly_name_length`] first to retrieve
    /// the maximum length and then calls [`Self::set_friendly_name`] once per
    /// 15-byte chunk, at char indices 0, 15, 30, and so on. A chunk the device
    /// echoes at another index is logged, as Logi Options+ does, and the write
    /// carries on. An empty name writes nothing; use
    /// [`Self::reset_friendly_name`] to restore the default.
    ///
    /// Returns the number of name bytes written.
    pub async fn set_whole_device_name(&self, name: String) -> Result<u8, Hidpp20Error> {
        let max_len = usize::from(self.get_friendly_name_length().await?.name_max_length);
        let mut end = name.len().min(max_len);
        while !name.is_char_boundary(end) {
            end -= 1;
        }
        let bytes = &name.as_bytes()[..end];

        for (chunk_number, part) in bytes.chunks(NAME_CHUNK_LEN).enumerate() {
            let index = byte_count(chunk_number * NAME_CHUNK_LEN)?;
            let mut chunk = [0u8; NAME_CHUNK_LEN];
            chunk[..part.len()].copy_from_slice(part);
            let echoed = self.set_friendly_name(index, chunk).await?;
            if echoed != index {
                warn!(index, echoed, "setFriendlyName echoed another char index");
            }
        }

        byte_count(bytes.len())
    }

    /// Resets the friendly device name to the default one.
    ///
    /// Returns the total length of the name after resetting it,
    pub async fn reset_friendly_name(&self) -> Result<u8, Hidpp20Error> {
        Ok(self.endpoint.call(4, [0; 3]).await?.extend_payload()[0])
    }
}

/// Converts a byte offset or count within a name to the feature's `u8`.
///
/// Names are cut to `name_max_length`, itself a `u8`, so this only fails if
/// that invariant breaks.
fn byte_count(value: usize) -> Result<u8, Hidpp20Error> {
    u8::try_from(value).map_err(|_| Hidpp20Error::Feature(ErrorType::InvalidArgument))
}

/// Represents the length data as returned by
/// [`DeviceFriendlyNameFeature::get_friendly_name_length`].
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
#[non_exhaustive]
pub struct DeviceFriendlyNameLength {
    /// The current length of the friendly device name.
    pub name_length: u8,

    /// The maximum length of the friendly device name.
    pub name_max_length: u8,

    /// The length of the default friendly device name.
    pub default_name_length: u8,
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::{DeviceFriendlyNameFeature, NAME_CHUNK_LEN};
    use crate::{
        channel::mock::{MockRawHidChannel, MockRawHidHandle, channel_with_reader},
        feature::CreatableFeature,
        nibble::U4,
        protocol::v20::{Message, MessageHeader},
    };

    const DEVICE_INDEX: u8 = 2;
    const FEATURE_INDEX: u8 = 5;

    fn reply(function: u8, head: &[u8]) -> Message {
        let header = MessageHeader {
            device_index: DEVICE_INDEX,
            feature_index: FEATURE_INDEX,
            function_id: U4::from_lo(function),
            software_id: U4::from_lo(1),
        };
        let mut payload = [0; 16];
        payload[..head.len()].copy_from_slice(head);
        Message::Long(header, payload)
    }

    /// Queues the length reply and one echo per chunk, then writes `name`.
    fn write_name(name: &str, max_len: u8, echoes: &[u8]) -> (u8, MockRawHidHandle) {
        futures::executor::block_on(async {
            let (raw, handle) = MockRawHidChannel::new();
            let channel = Arc::new(channel_with_reader(raw).await);
            let feature = DeviceFriendlyNameFeature::new(channel, DEVICE_INDEX, FEATURE_INDEX);
            handle.queue_response(reply(0, &[4, max_len, 4]).into());
            for &echo in echoes {
                handle.queue_response(reply(3, &[echo]).into());
            }
            let written = feature
                .set_whole_device_name(name.to_owned())
                .await
                .unwrap();
            (written, handle)
        })
    }

    /// The `(char index, name bytes)` of every setFriendlyName request.
    /// Reports are `[id, device, feature, function|swid, payload...]`.
    fn chunks_sent(handle: &MockRawHidHandle) -> Vec<(u8, Vec<u8>)> {
        handle
            .written_reports()
            .iter()
            .filter(|report| report[3] >> 4 == 3)
            .map(|report| (report[4], report[5..5 + NAME_CHUNK_LEN].to_vec()))
            .collect()
    }

    fn padded(text: &[u8]) -> Vec<u8> {
        let mut chunk = text.to_vec();
        chunk.resize(NAME_CHUNK_LEN, 0);
        chunk
    }

    #[test]
    fn writes_each_chunk_at_its_own_offset() {
        // The device echoes the char index, so a long name used to land
        // every chunk at index 0.
        let name = "Desk mouse, MX Master";
        let (written, handle) = write_name(name, 32, &[0, 15]);

        assert_eq!(
            chunks_sent(&handle),
            [
                (0, name.as_bytes()[..15].to_vec()),
                (15, padded(&name.as_bytes()[15..])),
            ]
        );
        assert_eq!(written, 21);
    }

    #[test]
    fn cuts_a_long_name_back_to_a_character_boundary() {
        // Sixteen bytes would split the two-byte "é".
        let name = "Fifteen bytes..é";
        let (written, handle) = write_name(name, 16, &[0]);

        assert_eq!(written, 15);
        assert_eq!(chunks_sent(&handle), [(0, b"Fifteen bytes..".to_vec())]);
    }

    #[test]
    fn keeps_its_own_offsets_when_the_echo_differs() {
        let name = "Keyboard on the left side";
        let (written, handle) = write_name(name, 32, &[7, 7]);

        let offsets: Vec<u8> = chunks_sent(&handle)
            .iter()
            .map(|(index, _)| *index)
            .collect();
        assert_eq!(offsets, [0, 15]);
        assert_eq!(written, 25);
    }

    /// Answers one name-chunk read with `reply` and returns the chunk.
    fn read_chunk(function: u8, index: u8, reply_head: &[u8]) -> [u8; NAME_CHUNK_LEN] {
        futures::executor::block_on(async {
            let (raw, handle) = MockRawHidChannel::new();
            let channel = Arc::new(channel_with_reader(raw).await);
            let feature = DeviceFriendlyNameFeature::new(channel, DEVICE_INDEX, FEATURE_INDEX);
            handle.queue_response(reply(function, reply_head).into());
            let chunk = if function == 1 {
                feature.get_friendly_name(index).await
            } else {
                feature.get_default_friendly_name(index).await
            };
            let sent: Vec<u8> = handle
                .written_reports()
                .iter()
                .map(|report| report[4])
                .collect();
            assert_eq!(sent, [index], "the request carries the char index");
            chunk.unwrap()
        })
    }

    #[test]
    fn reads_the_name_bytes_after_the_echoed_index() {
        let mut head = vec![15];
        head.extend_from_slice(b"Master 3S");
        assert_eq!(read_chunk(1, 15, &head), padded(b"Master 3S").as_slice());
        let mut head = vec![0];
        head.extend_from_slice(b"MX Keys");
        assert_eq!(read_chunk(2, 0, &head), padded(b"MX Keys").as_slice());
    }

    #[test]
    fn keeps_a_name_chunk_whose_echo_differs() {
        // Options+ only logs a mismatched char index and keeps the chunk;
        // refusing it would lose the name of a firmware that echoes wrongly.
        let mut head = vec![0];
        head.extend_from_slice(b"Desk mouse");
        assert_eq!(read_chunk(1, 15, &head), padded(b"Desk mouse").as_slice());
        assert_eq!(read_chunk(2, 15, &head), padded(b"Desk mouse").as_slice());
    }
}
