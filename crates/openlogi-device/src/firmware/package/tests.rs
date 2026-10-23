use super::*;
use serde_json::{Value, json};

fn record(op: u8, address: u32, data: &[u8]) -> Vec<u8> {
    let mut bytes = vec![0; 16];
    bytes[0] = op;
    bytes[1..5].copy_from_slice(&address.to_be_bytes());
    bytes[5..9].copy_from_slice(&u32::try_from(data.len()).unwrap().to_be_bytes());
    bytes.extend_from_slice(data);
    bytes.resize(bytes.len().next_multiple_of(16), 0);
    bytes
}

pub(crate) fn container() -> Value {
    let mut contents = Vec::new();
    let mut payloads = serde_json::Map::new();
    for entity in [1_u8, 2] {
        let key = format!("bank{entity}");
        contents.push(json!({ "name": "RBM27", "magicStr": "0x52424d32375f44310000", "entity": entity.to_string(), "type": "0", "build": "19", "revision": "3", "extraVer": "0", "modelId": "0x00000000b042", "payload": key }));
        let mut init = [0; 16];
        init[0] = entity;
        init[1..11].copy_from_slice(b"RBM27_D1\0\0");
        let mut block = record(4, 0, &init);
        block.extend(record(
            1,
            if entity == 1 { 0x8000 } else { 0x41000 },
            &[0xaa; 16],
        ));
        payloads.insert(key, json!({ "blocks": [{ "data": hex::encode(block) }] }));
    }
    json!({ "fileVersion": "1", "contents": contents, "payloads": payloads })
}

#[test]
fn accepts_both_banks_and_preserves_the_exact_bytes() {
    let package = FirmwarePackage::parse(&serde_json::to_vec(&container()).unwrap()).unwrap();
    assert_eq!(package.images.len(), 2);
    assert_eq!(package.images[0].blocks[0].len(), 64);
    assert_eq!(&package.images[0].blocks[0][48..], &[0xaa; 16]);
}

#[test]
fn rejects_wrong_model_duplicate_entity_missing_payload_and_version_disagreement() {
    for edit in [0, 1, 2, 3, 4] {
        let mut json = container();
        match edit {
            0 => json["contents"][0]["modelId"] = json!("0xb048"),
            1 => json["contents"][1]["entity"] = json!("1"),
            2 => json["contents"][0]["payload"] = json!("missing"),
            3 => json["contents"][1]["build"] = json!("20"),
            _ => json["contents"][0]["magicStr"] = json!("0x00"),
        }
        assert!(
            FirmwarePackage::parse(&serde_json::to_vec(&json).unwrap()).is_err(),
            "edit {edit}"
        );
    }
}

#[test]
fn rejects_out_of_bank_writes_truncation_unknown_ops_and_foreign_init() {
    for edit in [0, 1, 2, 3, 4] {
        let mut json = container();
        let mut data = hex::decode(
            json["payloads"]["bank1"]["blocks"][0]["data"]
                .as_str()
                .unwrap(),
        )
        .unwrap();
        match edit {
            0 => data[33..37].copy_from_slice(&0x41000_u32.to_be_bytes()),
            1 => {
                data.pop();
            }
            2 => data[32] = 99,
            3 => data[16] = 2,
            _ => data[37..41].copy_from_slice(&u32::MAX.to_be_bytes()),
        }
        json["payloads"]["bank1"]["blocks"][0]["data"] = json!(hex::encode(data));
        assert!(
            FirmwarePackage::parse(&serde_json::to_vec(&json).unwrap()).is_err(),
            "edit {edit}"
        );
    }
}

/// Optional developer check against the vendor image; no package bytes are
/// stored in the repository. This reads a file and performs no device I/O.
#[test]
#[ignore = "set OPENLOGI_TEST_RDFU_PATH to the official 27.3.19 RDFU image"]
fn parses_the_official_vendor_image() {
    let path = std::env::var_os("OPENLOGI_TEST_RDFU_PATH").expect("official RDFU file path");
    let bytes = std::fs::read(path).unwrap();
    let package = FirmwarePackage::parse(&bytes).unwrap();
    assert_eq!(package.images.len(), 2);
    assert_eq!(
        package.images[0].version,
        FirmwareVersion {
            number: 27,
            revision: 3,
            build: 19
        }
    );
    for image in &package.images {
        assert_eq!(image.blocks.len(), 20);
    }
    assert_eq!(
        package.images[0].blocks.iter().map(Vec::len).sum::<usize>(),
        222_912
    );
}

#[test]
fn streamed_container_uses_the_parser_size_policy() {
    let error = FirmwarePackage::read_from(std::io::repeat(b' ').take(MAX_CONTAINER_BYTES + 1))
        .unwrap_err();
    assert_eq!(error, invalid("container exceeds size limit"));
}
