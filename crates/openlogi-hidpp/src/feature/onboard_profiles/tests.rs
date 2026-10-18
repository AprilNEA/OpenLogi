//! Unit tests for `OnboardProfiles` framing and the profile memory codec.

use std::{assert_matches, sync::Arc};

use super::profile::{
    DPI_SLOTS, DirectoryEntry, ProfileDirectory, ProfileError, ProfileLighting, ProfileSector,
    sector_crc,
};
use super::{OnboardMode, OnboardProfilesFeature, ProfileFormat, SectorAddress};
use crate::{
    channel::mock::{MockRawHidChannel, MockRawHidHandle, channel_with_reader},
    feature::{CreatableFeature, color_led_effects::Rgb},
    nibble::U4,
    protocol::v20::{Hidpp20Error, Message, MessageHeader},
};

const DEVICE: u8 = 0xff;
const FEATURE_INDEX: u8 = 15;
const SECTOR_SIZE: usize = 255;

fn header(function: u8) -> MessageHeader {
    MessageHeader {
        device_index: DEVICE,
        feature_index: FEATURE_INDEX,
        function_id: U4::from_lo(function),
        software_id: U4::from_lo(1),
    }
}

async fn feature() -> (OnboardProfilesFeature, MockRawHidHandle) {
    let (raw, handle) = MockRawHidChannel::new();
    let channel = Arc::new(channel_with_reader(raw).await);
    (
        OnboardProfilesFeature::new(channel, DEVICE, FEATURE_INDEX),
        handle,
    )
}

/// A sealed G303-format profile: 1 ms, slots 400/800/1600/3200 with 800 the
/// default, logo zone fixed white, side zone breathing.
fn profile_bytes() -> Vec<u8> {
    let mut bytes = vec![0xff; SECTOR_SIZE];
    bytes[0] = 1;
    bytes[1] = 1;
    bytes[2] = 0;
    for (slot, dpi) in [400u16, 800, 1600, 3200, 0].into_iter().enumerate() {
        bytes[3 + 2 * slot..5 + 2 * slot].copy_from_slice(&dpi.to_le_bytes());
    }
    // A button binding the codec must carry through untouched.
    bytes[0x20..0x24].copy_from_slice(&[0x80, 0x01, 0x00, 0x01]);
    let name: Vec<u8> = "FPS"
        .encode_utf16()
        .chain(std::iter::repeat_n(0, 21))
        .flat_map(u16::to_le_bytes)
        .collect();
    bytes[0xa0..0xd0].copy_from_slice(&name);
    bytes[0xd0..0xdb].copy_from_slice(&[0x01, 0xff, 0xff, 0xff, 0, 0, 0, 0, 0, 0, 0]);
    bytes[0xdb..0xe6].copy_from_slice(&[0x0a, 0x00, 0x80, 0xff, 0x0f, 0xa0, 0, 0x32, 0, 0, 0]);
    let crc = sector_crc(&bytes[..SECTOR_SIZE - 2]);
    bytes[SECTOR_SIZE - 2..].copy_from_slice(&crc.to_be_bytes());
    bytes
}

#[test]
fn the_sector_crc_is_ccitt_false() {
    // The CRC catalogue's check value for CRC-16/CCITT-FALSE.
    assert_eq!(sector_crc(b"123456789"), 0x29b1);
}

#[test]
fn a_profile_decodes_its_fields() {
    let profile = ProfileSector::decode(ProfileFormat::G303, profile_bytes()).unwrap();
    assert_eq!(profile.report_interval_ms(), 1);
    assert_eq!(profile.default_dpi_slot(), 1);
    assert_eq!(
        profile.dpi_slots(),
        [Some(400), Some(800), Some(1600), Some(3200), None]
    );
    assert_eq!(profile.name().as_deref(), Some("FPS"));
    assert_eq!(
        profile.lighting(0).unwrap(),
        ProfileLighting::Fixed(Rgb {
            red: 0xff,
            green: 0xff,
            blue: 0xff
        })
    );
    assert_eq!(
        profile.lighting(1).unwrap(),
        ProfileLighting::Breathing {
            color: Rgb {
                red: 0,
                green: 0x80,
                blue: 0xff
            },
            period_ms: 4000,
            brightness: 50,
        }
    );
}

#[test]
fn a_corrupt_user_sector_is_refused_but_a_factory_one_is_read() {
    let mut bytes = profile_bytes();
    bytes[3] ^= 1;
    assert_matches!(
        ProfileSector::decode(ProfileFormat::G303, bytes.clone()),
        Err(ProfileError::CrcMismatch { .. })
    );
    let factory = ProfileSector::decode_factory(ProfileFormat::G303, bytes).unwrap();
    assert_eq!(factory.dpi_slots()[0], Some(401));
}

#[test]
fn a_short_sector_is_refused() {
    assert_matches!(
        ProfileSector::decode_factory(ProfileFormat::G303, vec![0; 64]),
        Err(ProfileError::SectorTooShort { len: 64 })
    );
}

#[test]
fn edits_reseal_the_sector_and_keep_every_other_byte() {
    let original = profile_bytes();
    let mut profile = ProfileSector::decode(ProfileFormat::G303, original.clone()).unwrap();
    profile.set_dpi_slot(4, Some(6400)).unwrap();
    profile.set_default_dpi_slot(4).unwrap();
    profile.set_report_interval_ms(2).unwrap();
    profile.set_lighting(0, ProfileLighting::Off).unwrap();

    let encoded = profile.encode();
    let reread = ProfileSector::decode(ProfileFormat::G303, encoded.clone()).unwrap();
    assert_eq!(reread.dpi_slots()[4], Some(6400));
    assert_eq!(reread.default_dpi_slot(), 4);
    assert_eq!(reread.report_interval_ms(), 2);
    assert_eq!(reread.lighting(0).unwrap(), ProfileLighting::Off);
    // The alternate effect of the zone goes dark with it.
    assert_eq!(encoded[0xe6], 0x00);

    let changed: Vec<usize> = (0..SECTOR_SIZE - 2)
        .filter(|&at| encoded[at] != original[at])
        .collect();
    // 6400 is 0x1900: only the high byte of slot 4 changes.
    let expected: Vec<usize> = [0, 1, 12]
        .into_iter()
        .chain(0xd0..=0xd3)
        .chain(0xe6..=0xf0)
        .collect();
    assert_eq!(changed, expected);
}

#[test]
fn out_of_range_edits_are_refused() {
    let mut profile = ProfileSector::decode(ProfileFormat::G303, profile_bytes()).unwrap();
    assert_eq!(
        profile.set_dpi_slot(DPI_SLOTS, Some(800)),
        Err(ProfileError::NoSuchDpiSlot(DPI_SLOTS))
    );
    assert_eq!(
        profile.set_default_dpi_slot(5),
        Err(ProfileError::NoSuchDpiSlot(5))
    );
    for invalid in [0, 9] {
        assert_eq!(
            profile.set_report_interval_ms(invalid),
            Err(ProfileError::ReportInterval(invalid))
        );
    }
    assert_eq!(
        profile.set_lighting(2, ProfileLighting::Off),
        Err(ProfileError::NoSuchLedZone(2))
    );
}

#[test]
fn the_g402_format_has_no_led_zones() {
    let profile = ProfileSector::decode(ProfileFormat::G402, profile_bytes()).unwrap();
    assert_eq!(profile.led_zones(), 0);
    assert_eq!(profile.lighting(0), Err(ProfileError::NoSuchLedZone(0)));
}

#[test]
fn lighting_round_trips_and_unknown_effects_stay_put() {
    let mut profile = ProfileSector::decode(ProfileFormat::G303, profile_bytes()).unwrap();
    let cycle = ProfileLighting::Cycle {
        period_ms: 10_000,
        brightness: 100,
    };
    profile.set_lighting(1, cycle).unwrap();
    assert_eq!(profile.lighting(1).unwrap(), cycle);

    let mut bytes = profile.encode();
    bytes[0xd0] = 0x0b; // ripple: decoded as Other
    let crc = sector_crc(&bytes[..SECTOR_SIZE - 2]);
    bytes[SECTOR_SIZE - 2..].copy_from_slice(&crc.to_be_bytes());
    let mut profile = ProfileSector::decode(ProfileFormat::G303, bytes.clone()).unwrap();
    assert_eq!(
        profile.lighting(0).unwrap(),
        ProfileLighting::Other { effect: 0x0b }
    );
    profile
        .set_lighting(0, ProfileLighting::Other { effect: 0x0b })
        .unwrap();
    assert_eq!(profile.encode(), bytes);
}

#[test]
fn an_unnamed_profile_has_no_name() {
    let mut bytes = profile_bytes();
    bytes[0xa0..0xd0].fill(0xff);
    let profile = ProfileSector::decode_factory(ProfileFormat::G303, bytes).unwrap();
    assert_eq!(profile.name(), None);
}

#[test]
fn the_directory_round_trips() {
    let directory = ProfileDirectory::new(vec![
        DirectoryEntry {
            sector: SectorAddress::user_profile(1),
            enabled: true,
        },
        DirectoryEntry {
            sector: SectorAddress::user_profile(2),
            enabled: false,
        },
    ]);
    let bytes = directory.encode(SECTOR_SIZE);
    assert_eq!(bytes.len(), SECTOR_SIZE);
    assert_eq!(&bytes[..12], &[0, 1, 1, 0, 0, 2, 0, 0, 0xff, 0xff, 0, 0]);
    assert_eq!(ProfileDirectory::decode(&bytes), Some(directory));
}

#[test]
fn an_unwritten_directory_holds_no_profiles() {
    assert_eq!(ProfileDirectory::decode(&[0xff; SECTOR_SIZE]), None);
    assert_eq!(
        ProfileDirectory::decode(&ProfileDirectory::new(Vec::new()).encode(SECTOR_SIZE)),
        None
    );
}

#[test]
fn sector_addresses_name_their_bank() {
    assert!(!SectorAddress::user_profile(1).is_rom());
    assert!(SectorAddress::new(0x0101).is_rom());
    assert_eq!(SectorAddress::new(0x0103).profile_number(), 3);
}

#[test]
fn the_mode_is_one_of_two() {
    futures::executor::block_on(async {
        let (feature, handle) = feature().await;
        handle.queue_response(Message::Short(header(2), [2, 0, 0]).into());
        assert_eq!(feature.get_mode().await.unwrap(), OnboardMode::Host);
        handle.queue_response(Message::Short(header(2), [0, 0, 0]).into());
        assert_matches!(
            feature.get_mode().await,
            Err(Hidpp20Error::UnsupportedResponse)
        );
    });
}

#[test]
fn a_sector_is_read_in_overlapping_chunks() {
    futures::executor::block_on(async {
        let (feature, handle) = feature().await;
        let sector: Vec<u8> = (0..SECTOR_SIZE)
            .map(|i| u8::try_from(i % 251).unwrap())
            .collect();
        let mut offsets = Vec::new();
        let mut offset = 0;
        while offset < SECTOR_SIZE {
            let start = offset.min(SECTOR_SIZE - 16);
            offsets.push(start);
            let mut reply = [0; 16];
            reply.copy_from_slice(&sector[start..start + 16]);
            handle.queue_response(Message::Long(header(5), reply).into());
            offset += 16;
        }
        let read = feature
            .read_sector(SectorAddress::user_profile(1), 255)
            .await
            .unwrap();
        assert_eq!(read, sector);

        let written = handle.written_reports();
        assert_eq!(written.len(), 16);
        assert_eq!(*offsets.last().unwrap(), 239);
        for (report, start) in written.iter().zip(offsets) {
            assert_eq!(&report[4..6], &[0x00, 0x01]);
            assert_eq!(
                u16::from_be_bytes([report[6], report[7]]),
                u16::try_from(start).unwrap()
            );
        }
    });
}

#[test]
fn a_sector_write_is_one_framed_transaction() {
    futures::executor::block_on(async {
        let (feature, handle) = feature().await;
        let data = profile_bytes();
        handle.queue_response(Message::Long(header(6), [0; 16]).into());
        for _ in 0..16 {
            handle.queue_response(Message::Long(header(7), [0; 16]).into());
        }
        handle.queue_response(Message::Short(header(8), [0; 3]).into());
        feature
            .write_sector(SectorAddress::user_profile(1), &data)
            .await
            .unwrap();

        let written = handle.written_reports();
        assert_eq!(written.len(), 18);
        // memoryAddrWrite: sector 1, offset 0, 255 bytes.
        assert_eq!(&written[0][4..10], &[0, 1, 0, 0, 0, 255]);
        let sent: Vec<u8> = written[1..17]
            .iter()
            .flat_map(|report| report[4..20].to_vec())
            .collect();
        assert_eq!(&sent[..SECTOR_SIZE], data.as_slice());
        assert_eq!(sent[SECTOR_SIZE], 0xff);
        // memoryWriteEnd is a short request to function 8.
        assert_eq!(written[17][0], 0x10);
        assert_eq!(written[17][3] >> 4, 8);
    });
}
