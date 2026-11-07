use std::collections::BTreeMap;

use openlogi_core::config::{KeyRemap, KeyTarget, LightingKey, MediaKey};
use openlogi_device_registry::key_remap::key_sources;

use super::{crc32, layer_table, set_key_remap_on_channel, table_calls};
use crate::channel::scripted::{ScriptedRawHidChannel, scripted_channel};
use crate::write::WriteError;

fn key(name: &str) -> LightingKey {
    name.parse().expect("a key name")
}

fn g515() -> openlogi_device_registry::key_remap::KeySources {
    key_sources([0, 0x40b4, 0]).expect("the G515")
}

fn hex(text: &str) -> Vec<u8> {
    (0..text.len())
        .step_by(2)
        .map(|at| u8::from_str_radix(&text[at..at + 2], 16).expect("hex"))
        .collect()
}

#[test]
fn the_crc_is_zlibs() {
    assert_eq!(crc32(&[]), 0);
    assert_eq!(crc32(&[0, 0]), 0x41d9_12ff);
}

/// The table and calls G HUB wrote for F1 → A on the base layer.
#[test]
fn one_key_matches_g_hub_bytes() {
    let layer = BTreeMap::from([(key("F1"), KeyTarget::Key(key("A")))]);
    let (table, missing) = layer_table(&layer, g515());
    assert!(missing.is_empty());
    assert_eq!(table, hex("00011001008000000080020004"));
    let calls = table_calls(1, &table);
    assert_eq!(calls.len(), 3);
    assert_eq!((calls[0].0, &calls[0].1[..2]), (2, &[0x00, 0x0d][..]));
    assert_eq!((calls[1].0, &calls[1].1[..13]), (3, table.as_slice()));
    assert_eq!(calls[1].1[13..], [0; 3]);
    assert_eq!(
        (calls[2].0, &calls[2].1[..11]),
        (9, hex("1b05010200000d6f6e868e").as_slice())
    );
    assert_eq!(calls[2].1[11..], [0; 5]);
}

/// G HUB's three-key base table: F1 → A, Tab → Esc and Menu → G-Shift,
/// sorted by source and split over three pieces.
#[test]
fn several_keys_sort_by_source_and_span_pieces() {
    let layer = BTreeMap::from([
        (key("Menu"), KeyTarget::GShift),
        (key("Tab"), KeyTarget::Key(key("Esc"))),
        (key("F1"), KeyTarget::Key(key("A"))),
    ]);
    let (table, _) = layer_table(&layer, g515());
    assert_eq!(
        table,
        hex("0003100100800000008002000420010080000000800200294c01008000000080040002")
    );
    let calls = table_calls(1, &table);
    assert_eq!(
        calls
            .iter()
            .map(|(function, _)| *function)
            .collect::<Vec<_>>(),
        [2, 3, 3, 3, 9]
    );
    assert_eq!(calls[4].1[..11], hex("1b0501020000237cc3075b")[..]);
}

#[test]
fn media_keys_and_empty_layers_encode_as_g_hub_does() {
    let layer = BTreeMap::from([(key("PrintScreen"), KeyTarget::Media(MediaKey::Mute))]);
    let (table, _) = layer_table(&layer, g515());
    assert_eq!(
        table,
        hex("0001160100800000008003 00e2".replace(' ', "").as_str())
    );
    let (empty, _) = layer_table(&BTreeMap::new(), g515());
    assert_eq!(empty, [0, 0]);
    assert_eq!(table_calls(3, &empty)[1].1, [0; 16]);
    assert_eq!(
        table_calls(3, &empty)[2].1[..11],
        hex("1b05030200000241d912ff")[..]
    );
}

#[test]
fn keys_the_keyboard_lacks_are_left_out() {
    let layer = BTreeMap::from([
        (key("NumLock"), KeyTarget::Key(key("A"))),
        (key("F1"), KeyTarget::Key(key("B"))),
    ]);
    let (table, missing) = layer_table(&layer, g515());
    assert_eq!(missing, [key("NumLock")]);
    assert_eq!(table[..2], [0, 1]);
}

const INFO_INDEX: u8 = 0x02;
const FKC_INDEX: u8 = 0x10;
const FILES_INDEX: u8 = 0x0f;

/// A G515 with `0x0003`, `0x1b05` and `0x8101`; with `known` false it
/// reports a model id no table lists.
fn g515_device(request: &[u8], known: bool) -> Option<Vec<u8>> {
    if request.len() < 7 || !matches!(request[0], 0x10 | 0x11) {
        return None;
    }
    let mut payload = [0u8; 16];
    match (request[2], request[3] >> 4) {
        (0x00, 0x01) => payload[0] = 4,
        (0x00, 0x00) => {
            payload[0] = match u16::from_be_bytes([request[4], request[5]]) {
                0x0003 => INFO_INDEX,
                0x1b05 => FKC_INDEX,
                0x8101 => FILES_INDEX,
                _ => 0,
            };
        }
        (INFO_INDEX, 0x00) => {
            let model: u16 = if known { 0x40b4 } else { 0x4321 };
            payload[9..11].copy_from_slice(&model.to_be_bytes());
        }
        (FILES_INDEX, _) => {}
        _ => return None,
    }
    let mut response = vec![0u8; 20];
    response[0] = 0x11;
    response[1..4].copy_from_slice(&request[1..4]);
    response[4..].copy_from_slice(&payload);
    Some(response)
}

#[tokio::test]
async fn a_remap_hands_the_keyboard_to_the_host_then_writes_every_layer() {
    let (raw, handle) = ScriptedRawHidChannel::with_responder(|request| g515_device(request, true));
    let channel = scripted_channel(raw).await;
    let remap = KeyRemap {
        base: BTreeMap::from([(key("F1"), KeyTarget::Key(key("A")))]),
        ..KeyRemap::default()
    };

    let skipped = set_key_remap_on_channel(&channel, 1, &remap)
        .await
        .expect("written");

    assert!(skipped.is_empty());
    let files: Vec<(u8, Vec<u8>)> = handle
        .written_reports()
        .iter()
        .filter(|report| report[2] == FILES_INDEX)
        .map(|report| (report[3] >> 4, report[4..].to_vec()))
        .collect();
    let functions: Vec<u8> = files.iter().map(|(function, _)| *function).collect();
    assert_eq!(functions, [6, 2, 3, 9, 2, 3, 9, 2, 3, 9]);
    assert_eq!(files[0].1[0], 0x05, "host mode first");
    assert_eq!(files[3].1[..11], hex("1b05010200000d6f6e868e")[..]);
    assert_eq!(files[6].1[..11], hex("1b05020200000241d912ff")[..]);
    assert_eq!(files[9].1[..11], hex("1b05030200000241d912ff")[..]);
}

#[tokio::test]
async fn an_unknown_keyboard_is_left_alone() {
    let (raw, handle) =
        ScriptedRawHidChannel::with_responder(|request| g515_device(request, false));
    let channel = scripted_channel(raw).await;

    let error = set_key_remap_on_channel(&channel, 1, &KeyRemap::default())
        .await
        .expect_err("no source table");

    assert_eq!(
        error,
        WriteError::FeatureUnsupported {
            feature_hex: 0x1b05
        }
    );
    assert!(
        !handle
            .written_reports()
            .iter()
            .any(|report| report[2] == FILES_INDEX),
        "nothing is written"
    );
}
