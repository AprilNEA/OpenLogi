//! Firmware key remapping through HID++ `0x1b05` key customisation (the
//! G515 LS TKL).
//!
//! The keyboard keeps one remap table per layer — base, Fn, G-Shift — as a
//! file in the `0x8101` profile file system. Reverse-engineered from G HUB:
//!
//! * `0x8101` function 6 with `05` hands the keyboard to the host; tables
//!   written then take effect at once and last until the keyboard powers off.
//! * Function 2 starts a file of N bytes (`u16`, big-endian), function 3
//!   carries it in 16-byte pieces, and function 9 binds it to `0x1b05` and a
//!   layer, with its length and CRC-32.
//! * A table is a big-endian `u16` count, then 11 bytes per key: the key's
//!   source index (see [`openlogi_device_registry::key_remap`]), seven bytes
//!   G HUB always writes the same, and the target — `02 00 usage` for a
//!   keyboard key, `03` and a big-endian consumer usage for a media key,
//!   `04 00 02` for G-Shift.

use std::sync::Arc;

use hidpp::{
    channel::HidppChannel,
    device::Device,
    feature::{CreatableFeature as _, device_information::DeviceInformationFeature},
    nibble::U4,
    protocol::v20::{self, Hidpp20Error},
};
use openlogi_core::config::{KeyRemap, KeyTarget, LightingKey};
use openlogi_device_registry::key_remap::{KeySources, key_sources};
use tracing::{debug, warn};

use crate::SharedChannel;

use super::{HidppOperation, WriteError, classify_hidpp_error, open_feature};

/// HID++ key customisation, whose tables this module writes.
pub const KEY_CUSTOMIZATION: u16 = 0x1b05;
/// HID++ profile management, the file system the tables live in.
pub const PROFILE_MANAGEMENT: u16 = 0x8101;

/// The bytes between a key's source index and its target, as G HUB writes
/// them for every key.
const ENTRY_MIDDLE: [u8; 7] = [0x01, 0x00, 0x80, 0x00, 0x00, 0x00, 0x80];
/// `0x8101` function 6 argument that hands the keyboard to the host.
const HOST_MODE: u8 = 0x05;

/// The three bytes a table entry ends with for `target`.
fn target_bytes(target: KeyTarget) -> [u8; 3] {
    match target {
        KeyTarget::Key(key) => [0x02, 0x00, key.usage()],
        KeyTarget::Media(media) => {
            let [hi, lo] = media.usage().to_be_bytes();
            [0x03, hi, lo]
        }
        KeyTarget::GShift => [0x04, 0x00, 0x02],
    }
}

/// One layer's table for a keyboard whose keys have `sources`, in source
/// order, and the keys it left out because the keyboard has no such key.
fn layer_table(
    layer: &std::collections::BTreeMap<LightingKey, KeyTarget>,
    sources: KeySources,
) -> (Vec<u8>, Vec<LightingKey>) {
    let mut entries = Vec::with_capacity(layer.len());
    let mut missing = Vec::new();
    for (&key, &target) in layer {
        match sources.source(key.usage()) {
            Some(source) => entries.push((source, target)),
            None => missing.push(key),
        }
    }
    entries.sort_by_key(|&(source, _)| source);
    let count = u16::try_from(entries.len()).unwrap_or(u16::MAX);
    let mut table = count.to_be_bytes().to_vec();
    for (source, target) in entries {
        table.push(source);
        table.extend_from_slice(&ENTRY_MIDDLE);
        table.extend_from_slice(&target_bytes(target));
    }
    (table, missing)
}

/// The CRC-32 (IEEE, as zlib computes it) `0x8101` checks a file against.
fn crc32(data: &[u8]) -> u32 {
    !data.iter().fold(u32::MAX, |crc, &byte| {
        (0..8).fold(crc ^ u32::from(byte), |crc, _| {
            if crc & 1 == 1 {
                (crc >> 1) ^ 0xedb8_8320
            } else {
                crc >> 1
            }
        })
    })
}

/// The `0x8101` calls that write `table` as the table of `layer` (1 base,
/// 2 Fn, 3 G-Shift): `(function, parameters)`.
fn table_calls(layer: u8, table: &[u8]) -> Vec<(u8, [u8; 16])> {
    let len = u16::try_from(table.len()).unwrap_or(u16::MAX);
    let [len_hi, len_lo] = len.to_be_bytes();
    let mut calls = Vec::with_capacity(table.len() / 16 + 3);
    let mut start = [0; 16];
    start[..2].copy_from_slice(&[len_hi, len_lo]);
    calls.push((2, start));
    for piece in table.chunks(16) {
        let mut data = [0; 16];
        data[..piece.len()].copy_from_slice(piece);
        calls.push((3, data));
    }
    let [k_hi, k_lo] = KEY_CUSTOMIZATION.to_be_bytes();
    let mut bind = [0; 16];
    bind[..7].copy_from_slice(&[k_hi, k_lo, layer, 0x02, 0x00, len_hi, len_lo]);
    bind[7..11].copy_from_slice(&crc32(table).to_be_bytes());
    calls.push((9, bind));
    calls
}

/// Write `remap` to the keyboard on `shared`: every layer, so a layer the
/// map leaves empty clears what an earlier write put there. Keys the
/// keyboard has no source for are skipped and returned.
///
/// # Errors
///
/// [`WriteError::FeatureUnsupported`] when the keyboard lacks `0x1b05` or
/// `0x8101`, or its key sources are unknown; any transport or feature error
/// a call meets.
pub async fn set_key_remap_on(
    shared: &SharedChannel,
    remap: &KeyRemap,
) -> Result<Vec<LightingKey>, WriteError> {
    set_key_remap_on_channel(shared.channel(), shared.device_index(), remap).await
}

pub(crate) async fn set_key_remap_on_channel(
    channel: &Arc<HidppChannel>,
    index: u8,
    remap: &KeyRemap,
) -> Result<Vec<LightingKey>, WriteError> {
    let mut device = Device::new(Arc::clone(channel), index)
        .await
        .map_err(|_| WriteError::DeviceUnreachable { index })?;
    let model_id = open_feature::<DeviceInformationFeature>(&mut device)
        .await?
        .get_device_info()
        .await
        .map_err(|error| {
            classify_hidpp_error(
                error,
                HidppOperation::ResolveFeature,
                DeviceInformationFeature::ID,
            )
        })?
        .model_id;
    let sources = key_sources(model_id).ok_or(WriteError::FeatureUnsupported {
        feature_hex: KEY_CUSTOMIZATION,
    })?;
    feature_index(&device, KEY_CUSTOMIZATION).await?;
    let files = feature_index(&device, PROFILE_MANAGEMENT).await?;

    let mut calls = vec![(6, {
        let mut host = [0; 16];
        host[0] = HOST_MODE;
        host
    })];
    let mut skipped = Vec::new();
    for (layer, keys) in (1..=3).zip(remap.layers()) {
        let (table, missing) = layer_table(keys, sources);
        skipped.extend(missing);
        calls.extend(table_calls(layer, &table));
    }
    for (function, params) in calls {
        call(channel, index, files, function, params).await?;
    }
    if !skipped.is_empty() {
        warn!(
            ?skipped,
            "keys this keyboard does not have were left out of its remap"
        );
    }
    debug!(index, "key remap written");
    Ok(skipped)
}

/// The index of `feature_id` on `device`.
async fn feature_index(device: &Device, feature_id: u16) -> Result<u8, WriteError> {
    device
        .root()
        .get_feature(feature_id)
        .await
        .map_err(|error| classify_hidpp_error(error, HidppOperation::ResolveFeature, feature_id))?
        .map(|info| info.index)
        .ok_or(WriteError::FeatureUnsupported {
            feature_hex: feature_id,
        })
}

/// One long `0x8101` call. A feature error names the function, since the
/// file system answers only with a code.
async fn call(
    channel: &Arc<HidppChannel>,
    index: u8,
    feature_index: u8,
    function: u8,
    params: [u8; 16],
) -> Result<(), WriteError> {
    let header = v20::MessageHeader {
        device_index: index,
        feature_index,
        function_id: U4::from_lo(function),
        software_id: channel.get_sw_id(),
    };
    match channel.send_v20(v20::Message::Long(header, params)).await {
        Ok(_) => Ok(()),
        Err(Hidpp20Error::Feature(error)) => Err(WriteError::Hidpp(format!(
            "0x8101 function {function} refused the key remap: {error:?}"
        ))),
        Err(error) => Err(WriteError::Hidpp(format!("{error:?}"))),
    }
}

#[cfg(test)]
mod tests;
