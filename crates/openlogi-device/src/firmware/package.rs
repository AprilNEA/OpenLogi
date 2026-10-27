//! Validates the reverse-engineered RDFU container before any DFU command.

use std::collections::{HashMap, HashSet};
use std::io::Read;

use hidpp::feature::dfu_resumable::{DFU_MAGIC_LEN, DFU_PACKET_LEN};
use openlogi_core::hid::FirmwareVersion;
use serde::Deserialize;
use thiserror::Error;

const MAX_CONTAINER_BYTES: u64 = 8 * 1024 * 1024;

/// A structurally validated RDFU package. Authenticity is the downloader's
/// responsibility: only parse bytes extracted from a verified pinned archive.
#[derive(Debug, Clone)]
pub struct FirmwarePackage {
    pub(super) images: Vec<FirmwareImage>,
}

#[derive(Debug, Clone)]
pub(super) struct FirmwareImage {
    pub entity: u8,
    pub product_id: u16,
    pub prefix: String,
    pub version: FirmwareVersion,
    pub magic: [u8; DFU_MAGIC_LEN],
    pub blocks: Vec<Vec<u8>>,
}

/// An invalid, unsupported or internally inconsistent RDFU package.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("invalid firmware package: {0}")]
pub struct PackageError(String);

fn invalid(message: &str) -> PackageError {
    PackageError(message.into())
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Container {
    file_version: String,
    contents: Vec<Content>,
    payloads: HashMap<String, Payload>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Content {
    name: String,
    magic_str: String,
    entity: String,
    #[serde(rename = "type")]
    kind: String,
    build: String,
    revision: String,
    extra_ver: String,
    model_id: String,
    payload: String,
}

#[derive(Deserialize)]
struct Payload {
    blocks: Vec<Block>,
}

#[derive(Deserialize)]
struct Block {
    data: String,
}

impl FirmwarePackage {
    /// Reads and validates a container with the same size policy as [`Self::parse`].
    /// Archive callers stream through this owner instead of independently bounding
    /// decompression or parsing an unchecked firmware container.
    pub fn read_from(reader: impl Read) -> Result<Self, PackageError> {
        let mut bytes = Vec::new();
        reader
            .take(MAX_CONTAINER_BYTES + 1)
            .read_to_end(&mut bytes)
            .map_err(|error| PackageError(error.to_string()))?;
        Self::parse(&bytes)
    }

    /// Parses and validates application images for the supported dual-bank
    /// format. Rejects duplicate entities, malformed records, foreign bank
    /// writes, and disagreement between the manifest and init record.
    pub fn parse(bytes: &[u8]) -> Result<Self, PackageError> {
        if bytes.len() as u64 > MAX_CONTAINER_BYTES {
            return Err(invalid("container exceeds size limit"));
        }
        let container: Container =
            serde_json::from_slice(bytes).map_err(|error| PackageError(error.to_string()))?;
        if container.file_version != "1" || container.contents.len() != 2 {
            return Err(invalid("unsupported container version or bank count"));
        }
        let mut seen = HashSet::new();
        let mut payload_names = HashSet::new();
        let mut images = Vec::new();
        for content in container.contents {
            let entity = content
                .entity
                .parse()
                .map_err(|_| invalid("invalid entity"))?;
            if !matches!(entity, 1 | 2) || !seen.insert(entity) {
                return Err(invalid("ambiguous or unsupported application bank"));
            }
            if content.kind != "0" || content.extra_ver != "0" {
                return Err(invalid("unsupported image type"));
            }
            // REVERSE-ENGINEERED: the only reviewed layout is MX Master 4's
            // RBM27 dual-bank map. New layouts need explicit evidence here.
            let product_id = u64::from_str_radix(
                content
                    .model_id
                    .strip_prefix("0x")
                    .ok_or_else(|| invalid("model ID"))?,
                16,
            )
            .map_err(|_| invalid("model ID"))?;
            if product_id != 0xb042 || content.name != "RBM27" {
                return Err(invalid("unsupported model or firmware line"));
            }
            let version = FirmwareVersion {
                number: 27,
                revision: content.revision.parse().map_err(|_| invalid("revision"))?,
                build: content.build.parse().map_err(|_| invalid("build"))?,
            };
            let magic: [u8; DFU_MAGIC_LEN] = hex::decode(
                content
                    .magic_str
                    .strip_prefix("0x")
                    .ok_or_else(|| invalid("magic"))?,
            )
            .map_err(|_| invalid("magic"))?
            .try_into()
            .map_err(|_| invalid("magic length"))?;
            if &magic != b"RBM27_D1\0\0" || !payload_names.insert(content.payload.clone()) {
                return Err(invalid("invalid magic or shared bank payload"));
            }
            let payload = container
                .payloads
                .get(&content.payload)
                .ok_or_else(|| invalid("missing payload"))?;
            if payload.blocks.is_empty() || payload.blocks.len() > usize::from(u16::MAX) {
                return Err(invalid("invalid block count"));
            }
            let mut init_count = 0;
            let mut write_count = 0;
            let mut blocks = Vec::new();
            for block in &payload.blocks {
                let data = hex::decode(&block.data).map_err(|_| invalid("block is not hex"))?;
                validate_records(&data, entity, &magic, &mut init_count, &mut write_count)?;
                blocks.push(data);
            }
            if init_count != 1 || write_count == 0 || blocks[0][0] != 4 {
                return Err(invalid("missing initial record or flash data"));
            }
            images.push(FirmwareImage {
                entity,
                product_id: 0xb042,
                prefix: "RBM".into(),
                version,
                magic,
                blocks,
            });
        }
        if images[0].version != images[1].version {
            return Err(invalid("bank versions disagree"));
        }
        Ok(Self { images })
    }
}

fn validate_records(
    data: &[u8],
    entity: u8,
    magic: &[u8; DFU_MAGIC_LEN],
    init_count: &mut usize,
    write_count: &mut usize,
) -> Result<(), PackageError> {
    if data.is_empty() || !data.len().is_multiple_of(DFU_PACKET_LEN) {
        return Err(invalid("empty or unaligned block"));
    }
    let (low, high) = if entity == 1 {
        (0x8000_u32, 0x41000_u32)
    } else {
        (0x41000, 0x7a000)
    };
    let mut offset = 0;
    while offset < data.len() {
        let header = data
            .get(offset..offset + 16)
            .ok_or_else(|| invalid("truncated record"))?;
        let address = u32::from_be_bytes([header[1], header[2], header[3], header[4]]);
        let size = u32::from_be_bytes([header[5], header[6], header[7], header[8]]);
        let padded = usize::try_from(size)
            .ok()
            .and_then(|n| n.checked_add(15))
            .map(|n| n / 16 * 16)
            .ok_or_else(|| invalid("oversized record"))?;
        let end = offset
            .checked_add(16)
            .and_then(|n| n.checked_add(padded))
            .ok_or_else(|| invalid("oversized record"))?;
        let body = data
            .get(offset + 16..end)
            .ok_or_else(|| invalid("truncated record body"))?;
        if header[9..] != [0; 7] || !matches!(header[0], 1 | 2 | 3 | 4 | 5 | 7) {
            return Err(invalid("unsupported record"));
        }
        match header[0] {
            1 => {
                if size == 0
                    || address < low
                    || address.checked_add(size).is_none_or(|end| end > high)
                {
                    return Err(invalid("flash write outside selected bank"));
                }
                *write_count += 1;
            }
            4 => {
                if size != 16 || body[0] != entity || &body[1..11] != magic {
                    return Err(invalid("initialization record disagrees with manifest"));
                }
                *init_count += 1;
            }
            _ => {}
        }
        offset = end;
    }
    Ok(())
}

#[cfg(test)]
pub(super) mod tests;
