use super::*;
use crate::channel::scripted::{ScriptedRawHidChannel, feature_error, scripted_channel};
use hidpp::feature::CreatableFeature;
use profile::{
    ButtonAction, ButtonPatch, Layer, ProfilePatch, checksum_valid, crc16, patch_profile,
};

fn sector() -> Vec<u8> {
    let mut bytes = vec![0xff; 255];
    bytes[..13].copy_from_slice(&[1, 0, 1, 0x20, 3, 0x40, 6, 0, 0, 0, 0, 0, 0]);
    bytes[32..36].copy_from_slice(&[0x80, 1, 0, 1]);
    bytes[208..219].copy_from_slice(&[15, 23, 42, 6, 7, 8, 9, 10, 100, 3, 17]);
    let crc = crc16(&bytes[..253]);
    bytes[253..].copy_from_slice(&crc.to_be_bytes());
    bytes
}

fn captured() -> GamingBackup {
    let mut directory = vec![0xff; 255];
    directory[..4].copy_from_slice(&[0, 1, 1, 0xff]);
    let crc = crc16(&directory[..253]);
    directory[253..].copy_from_slice(&crc.to_be_bytes());
    GamingBackup {
        schema_version: 1,
        route: DeviceRoute::Direct {
            vendor_id: 0x046d,
            product_id: 0xc098,
        },
        memory_model: 1,
        profile_format: 3,
        macro_format: 1,
        profile_count: 1,
        button_count: 11,
        sector_size: 255,
        mode: 1,
        active_profile: 1,
        sectors: vec![directory, sector()],
    }
}

#[test]
fn edit_rejects_stale_memory_and_wrong_physical_route() {
    let original = captured();
    let edit = ProfileEdit {
        schema_version: 1,
        original: original.clone(),
        sector: 1,
        patch: ProfilePatch::default(),
    };
    edit.verify_current(&original).unwrap();
    let mut changed = original.clone();
    changed.sectors[1][200] ^= 1;
    edit.verify_current(&changed)
        .expect_err("a concurrent edit must not be overwritten");
    changed = original;
    changed.route = DeviceRoute::Direct {
        vendor_id: 0x046d,
        product_id: 0xc099,
    };
    edit.verify_current(&changed)
        .expect_err("another product is not the captured target");
}

#[test]
fn corrupted_directory_and_outside_sector_cannot_be_edited() {
    let mut edit = ProfileEdit {
        schema_version: 1,
        original: captured(),
        sector: 0,
        patch: ProfilePatch::default(),
    };
    edit.render().expect_err("directory is not a profile");
    edit.sector = 255;
    edit.render()
        .expect_err("out-of-range sector must not panic");
    edit.sector = 1;
    edit.original.sectors[0][0] ^= 1;
    edit.render()
        .expect_err("invalid directory checksum must not be trusted");
}

#[test]
fn editing_255_byte_sector_preserves_unknown_fields_and_normal_layer() {
    let original = sector();
    let patch = ProfilePatch {
        report_rate_hz: Some(500),
        buttons: vec![ButtonPatch {
            index: 4,
            layer: Layer::Shifted,
            action: ButtonAction::Key {
                usage: 75,
                modifiers: 0,
            },
        }],
        ..ProfilePatch::default()
    };
    let after = patch_profile(&original, 11, &patch).unwrap();
    assert_eq!(after.len(), 255);
    assert_eq!(after[0], 2);
    assert_eq!(&after[112..116], &[0x80, 2, 0, 75]);
    assert_eq!(&after[32..96], &original[32..96]);
    assert_eq!(&after[208..253], &original[208..253]);
    assert!(checksum_valid(&after));
}

#[test]
fn no_op_preserves_every_byte() {
    let original = sector();
    assert_eq!(
        patch_profile(&original, 11, &ProfilePatch::default()).unwrap(),
        original
    );
}

#[test]
fn invalid_edits_never_modify_the_input() {
    let original = sector();
    for patch in [
        ProfilePatch {
            report_rate_hz: Some(999),
            ..ProfilePatch::default()
        },
        ProfilePatch {
            dpi: Some([0; 5]),
            ..ProfilePatch::default()
        },
        ProfilePatch {
            default_dpi_slot: Some(5),
            ..ProfilePatch::default()
        },
        ProfilePatch {
            dpi: Some([801, 1600, 0, 0, 0]),
            ..ProfilePatch::default()
        },
        ProfilePatch {
            name: Some("a".repeat(24)),
            ..ProfilePatch::default()
        },
        ProfilePatch {
            buttons: vec![ButtonPatch {
                index: 0,
                layer: Layer::Normal,
                action: ButtonAction::Disabled,
            }],
            ..ProfilePatch::default()
        },
        ProfilePatch {
            buttons: vec![ButtonPatch {
                index: 11,
                layer: Layer::Normal,
                action: ButtonAction::Disabled,
            }],
            ..ProfilePatch::default()
        },
    ] {
        patch_profile(&original, 11, &patch).expect_err("invalid input must be rejected");
        assert_eq!(original, sector());
    }
}

#[test]
fn bad_crc_and_unknown_json_fields_are_rejected() {
    let mut corrupted = sector();
    corrupted[100] ^= 1;
    patch_profile(&corrupted, 11, &ProfilePatch::default())
        .expect_err("invalid input must be rejected");
    serde_json::from_str::<ProfilePatch>(r#"{"dpii":[800,1600,0,0,0]}"#)
        .expect_err("invalid input must be rejected");
}

fn reply(request: &[u8]) -> Option<Vec<u8>> {
    if request.len() < 7 {
        return None;
    }
    let mut response = vec![0; 20];
    response[0] = 0x11;
    response[1..4].copy_from_slice(&request[1..4]);
    if request[2] == 9 && request[3] >> 4 == 5 {
        let offset = u16::from_be_bytes([request[6], request[7]]);
        let bytes = sector();
        response[4..].copy_from_slice(&bytes[usize::from(offset)..usize::from(offset) + 16]);
    }
    Some(response)
}

#[tokio::test]
async fn reads_last_255_byte_sector_block_at_239_without_mode_writes() {
    let (raw, handle) = ScriptedRawHidChannel::with_responder(reply);
    let feature = OnboardProfilesFeature::new(scripted_channel(raw).await, 255, 9);
    assert_eq!(feature.read_sector(1, 255).await.unwrap(), sector());
    let reports = handle.written_reports();
    let memory: Vec<_> = reports.iter().filter(|r| r[2] == 9).collect();
    assert_eq!(memory.len(), 16);
    assert!(memory.iter().all(|r| r[0] == 0x11 && r[3] >> 4 == 5));
    assert_eq!(&memory.last().unwrap()[4..8], &[0, 1, 0, 239]);
}

#[tokio::test]
async fn write_count_excludes_final_padding_and_commit_follows_data() {
    let (raw, handle) = ScriptedRawHidChannel::with_responder(reply);
    let feature = OnboardProfilesFeature::new(scripted_channel(raw).await, 255, 9);
    let bytes = sector();
    feature.write_sector(3, &bytes).await.unwrap();
    let reports = handle.written_reports();
    let memory: Vec<_> = reports.iter().filter(|r| r[2] == 9).collect();
    assert_eq!(memory.len(), 18);
    assert_eq!(memory[0][3] >> 4, 6);
    assert_eq!(&memory[0][4..10], &[0, 3, 0, 0, 0, 255]);
    assert_eq!(&memory[16][4..19], &bytes[240..255]);
    assert_eq!(memory[16][19], 0xff);
    assert_eq!(memory[17][3] >> 4, 8);
}

fn fail_data(request: &[u8]) -> Option<Vec<u8>> {
    if request.len() >= 7 && request[2] == 9 && request[3] >> 4 == 7 {
        Some(feature_error(request, 4))
    } else {
        reply(request)
    }
}

#[tokio::test]
async fn failed_data_transfer_is_not_retried_or_committed() {
    let (raw, handle) = ScriptedRawHidChannel::with_responder(fail_data);
    let feature = OnboardProfilesFeature::new(scripted_channel(raw).await, 255, 9);
    feature
        .write_sector(3, &sector())
        .await
        .expect_err("invalid input must be rejected");
    let functions: Vec<_> = handle
        .written_reports()
        .iter()
        .filter(|r| r[2] == 9)
        .map(|r| r[3] >> 4)
        .collect();
    assert_eq!(functions, [6, 7]);
}
