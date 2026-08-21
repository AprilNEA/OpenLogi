use super::*;
use hidpp::feature::extended_dpi::{DpiRange, Lod};
use hidpp::feature::per_key_lighting::FramePersistence;
use hidpp::feature::smartshift::WheelMode;

use crate::SharedChannel;
use crate::channel::scripted::{ScriptedRawHidChannel, feature_error, scripted_channel};
use crate::write::diagnostics::dump_firmware_entities_on_channel;
use crate::write::dpi::expand_dpi_ranges;
use crate::write::lighting::{collect_present_zones, per_key_reports};
use crate::write::smartshift::{
    is_missing_enhanced, is_transient_smartshift_error, smartshift_to_wheel,
    status_matches_desired, wheel_mode_to_smartshift,
};
use crate::write::{HidppFeatureErrorKind, HidppOperation};
use crate::{
    SmartShiftAutoDisengage, SmartShiftMode, SmartShiftStatus, SmartShiftThreshold, TunableTorque,
};
use hidpp::feature::device_information::DeviceEntityType;

const TEST_THRESHOLD: SmartShiftThreshold = match SmartShiftThreshold::try_new(10) {
    Ok(value) => value,
    Err(_) => panic!("valid test SmartShift threshold"),
};
const TEST_TORQUE: TunableTorque = match TunableTorque::try_new(33) {
    Ok(value) => value,
    Err(_) => panic!("valid test SmartShift torque"),
};

#[test]
fn smartshift_and_wheel_mode_byte_encodings_match() {
    // The whole design relies on 0x2110 WheelMode and 0x2111
    // SmartShiftMode sharing one wire encoding (Free/Freespin = 1,
    // Ratchet = 2). If the fork ever renumbers WheelMode this fails loudly.
    assert_eq!(
        u8::from(SmartShiftMode::Free),
        u8::from(WheelMode::Freespin)
    );
    assert_eq!(
        u8::from(SmartShiftMode::Ratchet),
        u8::from(WheelMode::Ratchet)
    );
}

#[test]
fn wheel_mode_maps_to_smartshift_mode() {
    assert_eq!(
        wheel_mode_to_smartshift(WheelMode::Freespin),
        SmartShiftMode::Free
    );
    assert_eq!(
        wheel_mode_to_smartshift(WheelMode::Ratchet),
        SmartShiftMode::Ratchet
    );
}

#[test]
fn smartshift_to_wheel_round_trips() {
    // smartshift_to_wheel is the inverse of wheel_mode_to_smartshift.
    for mode in [SmartShiftMode::Free, SmartShiftMode::Ratchet] {
        assert_eq!(wheel_mode_to_smartshift(smartshift_to_wheel(mode)), mode);
    }
}

#[test]
fn missing_enhanced_triggers_fallback() {
    assert!(is_missing_enhanced(&WriteError::FeatureUnsupported {
        feature_hex: 0x2111,
    }));
}

#[test]
fn missing_legacy_does_not_trigger_fallback() {
    // A device missing 0x2110 must NOT loop back — it genuinely has no
    // SmartShift.
    assert!(!is_missing_enhanced(&WriteError::FeatureUnsupported {
        feature_hex: 0x2110,
    }));
}

#[test]
fn transport_errors_do_not_trigger_fallback() {
    // Real failures must propagate, not be masked by a fallback attempt.
    assert!(!is_missing_enhanced(&WriteError::DeviceUnreachable {
        index: 0xff,
    }));
    assert!(!is_missing_enhanced(&WriteError::Hidpp("boom".into())));
}

#[test]
fn transient_smartshift_errors_are_retryable() {
    assert!(is_transient_smartshift_error(&WriteError::HidppFeature {
        operation: HidppOperation::WriteSmartShift,
        feature_hex: 0x2111,
        kind: HidppFeatureErrorKind::InvalidArgument,
    }));
    assert!(is_transient_smartshift_error(&WriteError::HidppFeature {
        operation: HidppOperation::WriteSmartShift,
        feature_hex: 0x2110,
        kind: HidppFeatureErrorKind::Busy,
    }));
    assert!(is_transient_smartshift_error(
        &WriteError::UnsupportedResponse {
            operation: HidppOperation::ReadSmartShift,
            feature_hex: 0x2110,
        }
    ));
}

#[test]
fn permanent_smartshift_errors_are_not_retryable() {
    assert!(!is_transient_smartshift_error(
        &WriteError::FeatureUnsupported {
            feature_hex: 0x2111,
        }
    ));
    assert!(!is_transient_smartshift_error(&WriteError::HidppFeature {
        operation: HidppOperation::WriteSmartShift,
        feature_hex: 0x2111,
        kind: HidppFeatureErrorKind::InvalidFunctionId,
    }));
}

#[test]
fn status_match_preserves_absent_torque() {
    let current = SmartShiftStatus {
        mode: SmartShiftMode::Ratchet,
        auto_disengage: SmartShiftAutoDisengage::Threshold(TEST_THRESHOLD),
        tunable_torque: Some(TEST_TORQUE),
    };
    let desired = SmartShiftStatus {
        mode: SmartShiftMode::Ratchet,
        auto_disengage: SmartShiftAutoDisengage::Threshold(TEST_THRESHOLD),
        tunable_torque: None,
    };
    assert!(status_matches_desired(current, desired));
    assert!(!status_matches_desired(
        current,
        SmartShiftStatus {
            mode: SmartShiftMode::Free,
            ..desired
        }
    ));
}

#[test]
fn per_key_lighting_builds_only_very_long_frames_then_one_long_commit() {
    let reports = per_key_reports(0x03, 0x27, 0x11, 0x22, 0x33);
    let (commit, frames) = reports
        .split_last()
        .expect("per-key lighting must emit a commit");

    assert_eq!(frames.len(), 17);
    assert!(frames.iter().all(|report| report.len() == 64));
    assert!(frames.iter().all(|report| report[0] == 0x12));
    assert!(frames.iter().all(|report| report[1] == 0x03));
    assert!(frames.iter().all(|report| report[2] == 0x27));
    assert!(frames.iter().all(|report| report[3] == 0x3a));
    assert!(frames.iter().all(|report| report[5] == 0x01));
    assert!(frames.iter().all(|report| report[7] == 0x0e));

    let entries: Vec<_> = frames
        .iter()
        .flat_map(|report| report[8..64].as_chunks::<4>().0)
        .take(0xe9)
        .map(|&[a, b, c, d]| (a, b, c, d))
        .collect();
    assert_eq!(entries.len(), 0xe9);
    for (key, entry) in (0x00u8..=0xe8).zip(entries) {
        assert_eq!(entry, (key, 0x11, 0x22, 0x33));
    }

    assert_eq!(commit.len(), 20);
    assert_eq!(&commit[..4], &[0x11, 0x03, 0x27, 0x5a]);
    assert!(commit[4..].iter().all(|byte| *byte == 0));
}

#[tokio::test]
async fn shared_read_and_lighting_apis_use_the_supplied_channel() -> Result<(), WriteError> {
    let (raw, handle) = ScriptedRawHidChannel::with_responder(scripted_response);
    let channel = scripted_channel(raw).await;
    let shared = SharedChannel::new(
        channel,
        DeviceRoute::Direct {
            vendor_id: 0x046d,
            product_id: 0xb35b,
        },
    );

    let dpi = get_dpi_info_on(&shared).await?;
    assert_eq!(dpi.current, Dpi::new(800));
    assert_eq!(
        dpi.capabilities.values(),
        [Dpi::new(400), Dpi::new(800), Dpi::new(1600)]
    );

    let smartshift = get_smartshift_status_on(&shared).await?;
    assert_eq!(smartshift.mode, SmartShiftMode::Ratchet);
    assert_eq!(
        smartshift.auto_disengage,
        SmartShiftAutoDisengage::Threshold(TEST_THRESHOLD)
    );
    assert_eq!(smartshift.tunable_torque, Some(TEST_TORQUE));

    // The scripted device reports no 0x8070 effect engine, so Auto must fall
    // back to 0x8080 without opening a second transport.
    set_keyboard_color_on(&shared, 0x11, 0x22, 0x33).await?;

    let written = handle.written_reports();
    let very_long: Vec<_> = written
        .iter()
        .filter(|report| report.first() == Some(&0x12))
        .collect();
    assert_eq!(very_long.len(), 17);
    assert!(very_long.iter().all(|report| report.len() == 64));
    assert!(written.iter().any(|report| {
        report.len() == 20
            && report[0] == 0x11
            && report[1] == 0xff
            && report[2] == 0x07
            && report[3] >> 4 == 0x05
    }));
    Ok(())
}

#[test]
fn stepped_dpi_ranges_expand_onto_their_step_grid() {
    assert_eq!(
        expand_dpi_ranges(&[DpiRange::Stepped {
            from: 400,
            to: 800,
            step: 100,
        }]),
        [400, 500, 600, 700, 800]
    );
}

#[test]
fn a_stepped_range_always_offers_its_high_endpoint() {
    // 1000 is not an exact multiple of 300 from 100, but the spec makes the
    // high endpoint selectable regardless — dropping it would put a device's
    // maximum DPI out of reach.
    assert_eq!(
        expand_dpi_ranges(&[DpiRange::Stepped {
            from: 100,
            to: 1000,
            step: 300,
        }]),
        [100, 400, 700, 1000]
    );
}

#[test]
fn fixed_and_stepped_ranges_mix_in_one_description() {
    assert_eq!(
        expand_dpi_ranges(&[
            DpiRange::Fixed(200),
            DpiRange::Stepped {
                from: 400,
                to: 600,
                step: 100,
            },
            DpiRange::Fixed(1600),
        ]),
        [200, 400, 500, 600, 1600]
    );
}

#[test]
fn adjacent_ranges_may_share_an_endpoint() {
    // The device reports one range's high value as the next one's low value;
    // `DpiCapabilities::new` is what deduplicates, so the raw expansion is
    // allowed to repeat it.
    let values = expand_dpi_ranges(&[
        DpiRange::Stepped {
            from: 100,
            to: 300,
            step: 100,
        },
        DpiRange::Stepped {
            from: 300,
            to: 500,
            step: 100,
        },
    ]);
    assert_eq!(values, [100, 200, 300, 300, 400, 500]);
    assert_eq!(
        DpiCapabilities::new(values).expect("non-empty").values(),
        [
            Dpi::new(100),
            Dpi::new(200),
            Dpi::new(300),
            Dpi::new(400),
            Dpi::new(500)
        ]
    );
}

#[test]
fn a_single_value_range_yields_just_that_value() {
    assert_eq!(
        expand_dpi_ranges(&[DpiRange::Stepped {
            from: 800,
            to: 800,
            step: 50,
        }]),
        [800]
    );
}

#[tokio::test]
async fn dpi_reads_and_writes_work_on_a_device_with_only_extended_dpi() -> Result<(), WriteError> {
    // `Capabilities::from_feature_ids` turns the DPI panel on for 0x2201 *or*
    // 0x2202, so a mouse that only speaks 0x2202 has to be drivable — it used
    // to get a panel that failed every read and write.
    let (raw, handle) = ScriptedRawHidChannel::with_responder(extended_dpi_scripted_response);
    let channel = scripted_channel(raw).await;
    let shared = SharedChannel::new(
        channel,
        DeviceRoute::Direct {
            vendor_id: 0x046d,
            product_id: 0xb35b,
        },
    );

    let dpi = get_dpi_info_on(&shared).await?;
    assert_eq!(dpi.current, Dpi::new(800));
    // The stepped 400..800 range expands onto its step grid and the trailing
    // fixed value survives.
    assert_eq!(
        dpi.capabilities.values(),
        [
            Dpi::new(400),
            Dpi::new(500),
            Dpi::new(600),
            Dpi::new(700),
            Dpi::new(800),
            Dpi::new(1200)
        ]
    );

    set_dpi_on(&shared, Dpi::new(1200)).await?;

    // setSensorDpiParameters is a long request on function 6 of feature index
    // 0x05: [dpiX, dpiY, lod] after the echoed sensor index.
    let write = handle
        .written_reports()
        .into_iter()
        .find(|report| report.len() == 20 && report[2] == 0x05 && report[3] >> 4 == 0x06)
        .expect("a DPI write must reach the device");
    assert_eq!(u16::from_be_bytes([write[5], write[6]]), 1200);
    // No independent Y axis on this sensor, so the spec has the host send 0.
    assert_eq!(u16::from_be_bytes([write[7], write[8]]), 0);
    // Lift-off distance is read back and rewritten unchanged — the packet has
    // no "leave alone" encoding, so writing a bare 0 would retune the sensor.
    assert_eq!(write[9], u8::from(Lod::Medium));
    Ok(())
}

#[test]
fn zone_presence_bits_decode_lsb_first_from_the_page_base() {
    let mut bitfield = [0u8; 14];
    bitfield[0] = 0b0000_0110; // zones 1 and 2
    bitfield[1] = 0b1000_0000; // zone 15
    let mut zones = Vec::new();

    collect_present_zones(0, &bitfield, &mut zones);

    assert_eq!(zones, [1, 2, 15]);
}

#[test]
fn zone_presence_pages_are_offset_by_their_base() {
    let mut bitfield = [0u8; 14];
    bitfield[0] = 0b0000_0001;
    let mut zones = Vec::new();

    collect_present_zones(112, &bitfield, &mut zones);

    assert_eq!(zones, [112]);
}

#[test]
fn zone_presence_skips_sentinels_and_padding_past_255() {
    // The last page covers only 224..=255, so the rest of its 112 bits are
    // padding — decoding them would wrap back onto low zone ids. Ids 0 and
    // 0xff are the feature's own end-of-list sentinels.
    let mut zones = Vec::new();
    collect_present_zones(0, &[0b0000_0001; 14], &mut zones);
    assert!(!zones.contains(&0), "zone 0 is an end-of-list sentinel");

    let mut last_page = [0u8; 14];
    last_page[3] = 0b1000_0000; // id 255 — the other sentinel
    last_page[5] = 0b0000_0001; // id 264 — past the addressable range
    let mut zones = Vec::new();
    collect_present_zones(224, &last_page, &mut zones);
    assert!(zones.is_empty(), "got {zones:?}");
}

#[tokio::test]
async fn a_keyboard_with_only_per_key_v2_can_be_coloured() -> Result<(), WriteError> {
    // 0x8081 supersedes 0x8080 but nothing had ever driven it, so a keyboard
    // exposing only 0x8081 could not be coloured at all.
    let (raw, handle) = ScriptedRawHidChannel::with_responder(per_key_v2_scripted_response);
    let channel = scripted_channel(raw).await;
    let shared = SharedChannel::new(
        channel,
        DeviceRoute::Direct {
            vendor_id: 0x046d,
            product_id: 0xc339,
        },
    );

    set_keyboard_color_on(&shared, 0x11, 0x22, 0x33).await?;

    let written = handle.written_reports();
    let long_on = |function: u8| {
        written
            .iter()
            .filter(move |report| {
                report.len() == 20 && report[2] == 0x07 && report[3] >> 4 == function
            })
            .collect::<Vec<_>>()
    };

    // One setRgbZonesSingleValue carrying the colour and every present zone —
    // four zones fit in a single request.
    let paints = long_on(0x06);
    assert_eq!(paints.len(), 1);
    assert_eq!(&paints[0][4..7], &[0x11, 0x22, 0x33]);
    assert_eq!(&paints[0][7..11], &[1, 2, 3, 4]);

    // Then exactly one frameEnd, volatile so the colour does not burn flash on
    // every pick.
    let commits = long_on(0x07);
    assert_eq!(commits.len(), 1);
    assert_eq!(commits[0][4], u8::from(FramePersistence::Volatile));

    // The raw 0x8080 stream must not have run — this device has no 0x8080.
    assert!(written.iter().all(|report| report.first() != Some(&0x12)));
    Ok(())
}

fn scripted_response(request: &[u8]) -> Option<Vec<u8>> {
    if request.len() < 7 || !matches!(request[0], 0x10 | 0x11) {
        return None;
    }
    let feature_index = request[2];
    let function = request[3] >> 4;
    let mut payload = [0u8; 16];
    let long = match (feature_index, function) {
        // Root ping used by Device::new.
        (0x00, 0x01) => {
            payload[0] = 4;
            false
        }
        // Root feature lookup.
        (0x00, 0x00) => {
            let feature_id = u16::from_be_bytes([request[4], request[5]]);
            payload[0] = match feature_id {
                0x2201 => 0x05,
                0x2111 => 0x06,
                0x8080 => 0x07,
                _ => 0x00,
            };
            false
        }
        // AdjustableDpi sensor count/current/list.
        (0x05, 0x00) => {
            payload[0] = 1;
            false
        }
        (0x05, 0x02) => {
            payload[1..3].copy_from_slice(&800u16.to_be_bytes());
            false
        }
        (0x05, 0x01) => {
            payload[..8].copy_from_slice(&[0, 0x01, 0x90, 0x03, 0x20, 0x06, 0x40, 0]);
            true
        }
        // Enhanced SmartShift status.
        (0x06, 0x01) => {
            payload[..3].copy_from_slice(&[u8::from(WheelMode::Ratchet), 10, 33]);
            false
        }
        // Raw per-key frame commit expects no reply.
        _ => return None,
    };

    let mut response = vec![0u8; if long { 20 } else { 7 }];
    response[0] = if long { 0x11 } else { 0x10 };
    response[1..4].copy_from_slice(&request[1..4]);
    let payload_len = response.len() - 4;
    response[4..].copy_from_slice(&payload[..payload_len]);
    Some(response)
}

/// A mouse that exposes `0x2202 ExtendedAdjustableDpi` and **no**
/// `0x2201 AdjustableDpi` — the shape `Capabilities::from_feature_ids` lights
/// the DPI panel up for but the write path could not drive.
fn extended_dpi_scripted_response(request: &[u8]) -> Option<Vec<u8>> {
    if request.len() < 7 || !matches!(request[0], 0x10 | 0x11) {
        return None;
    }
    let feature_index = request[2];
    let function = request[3] >> 4;
    let mut payload = [0u8; 16];
    let long = match (feature_index, function) {
        // Root ping used by Device::new.
        (0x00, 0x01) => {
            payload[0] = 4;
            false
        }
        // Root feature lookup. 0x2201 is deliberately absent.
        (0x00, 0x00) => {
            let feature_id = u16::from_be_bytes([request[4], request[5]]);
            payload[0] = u8::from(feature_id == 0x2202) * 0x05;
            false
        }
        // getSensorCount.
        (0x05, 0x00) => {
            payload[0] = 1;
            false
        }
        // getSensorDpiRanges: echo (sensorIdx, direction, page), then 400 with
        // a step-100 hyphen up to 800, a fixed 1200, and the end-of-list word.
        (0x05, 0x02) => {
            payload[..3].copy_from_slice(&request[4..7]);
            payload[3..13]
                .copy_from_slice(&[0x01, 0x90, 0xe0, 0x64, 0x03, 0x20, 0x04, 0xb0, 0x00, 0x00]);
            true
        }
        // getSensorDpiParameters: 800 DPI now and by default, no independent Y
        // axis (dpiY reads 0), lift-off distance MEDIUM.
        (0x05, 0x05) => {
            payload[1..3].copy_from_slice(&800u16.to_be_bytes());
            payload[3..5].copy_from_slice(&800u16.to_be_bytes());
            payload[9] = u8::from(Lod::Medium);
            true
        }
        // setSensorDpiParameters: echo the request back.
        (0x05, 0x06) => {
            payload[..6].copy_from_slice(&request[4..10]);
            true
        }
        _ => return None,
    };

    let mut response = vec![0u8; if long { 20 } else { 7 }];
    response[0] = if long { 0x11 } else { 0x10 };
    response[1..4].copy_from_slice(&request[1..4]);
    let payload_len = response.len() - 4;
    response[4..].copy_from_slice(&payload[..payload_len]);
    Some(response)
}

/// A keyboard that exposes `0x8081 PerKeyLighting2` and neither `0x8070`
/// ColorLedEffects nor `0x8080` PerKeyLighting — the shape that had no way to
/// set a colour at all, since nothing ever drove 0x8081.
fn per_key_v2_scripted_response(request: &[u8]) -> Option<Vec<u8>> {
    if request.len() < 7 || !matches!(request[0], 0x10 | 0x11) {
        return None;
    }
    let feature_index = request[2];
    let function = request[3] >> 4;
    let mut payload = [0u8; 16];
    let long = match (feature_index, function) {
        // Root ping used by Device::new.
        (0x00, 0x01) => {
            payload[0] = 4;
            false
        }
        // Root feature lookup. Only 0x8081 is present.
        (0x00, 0x00) => {
            let feature_id = u16::from_be_bytes([request[4], request[5]]);
            payload[0] = u8::from(feature_id == 0x8081) * 0x07;
            false
        }
        // getRgbZonePresence: echo (typeOfInfo, page) then the 14-byte
        // bitfield. Page 0 reports zones 1..=4 present; the others are empty.
        (0x07, 0x00) => {
            payload[..2].copy_from_slice(&request[4..6]);
            if request[5] == 0 {
                payload[2] = 0b0001_1110;
            }
            true
        }
        // setRgbZonesSingleValue and frameEnd: echo the request back.
        (0x07, 0x06 | 0x07) => {
            payload[..12].copy_from_slice(&request[4..16]);
            true
        }
        _ => return None,
    };

    let mut response = vec![0u8; if long { 20 } else { 7 }];
    response[0] = if long { 0x11 } else { 0x10 };
    response[1..4].copy_from_slice(&request[1..4]);
    let payload_len = response.len() - 4;
    response[4..].copy_from_slice(&payload[..payload_len]);
    Some(response)
}

/// Feature index the scripted mouse reports for `0x0003 DeviceInformation`.
const DEVICE_INFO_INDEX: u8 = 0x03;
/// `ErrorType::Busy`, the refusal the scripted mouse answers entity 3 with.
const BUSY: u8 = 0x08;
/// How many firmware entities the scripted mouse declares.
const ENTITY_COUNT: u8 = 4;

/// A wired G502 LIGHTSPEED, plus one invented entity.
///
/// Entities 0..=2 are what the real mouse reports: a bootloader, the running
/// main application, and a Softdevice radio stack whose build field is not
/// valid BCD, so this parser cannot decode it. Entity 3 is added to cover the
/// other entity-local failure — firmware that refuses the read outright rather
/// than answering with a record that will not parse.
fn mouse_with_an_unparseable_radio_stack(request: &[u8]) -> Option<Vec<u8>> {
    if request.len() < 7 || !matches!(request[0], 0x10 | 0x11) {
        return None;
    }
    let mut payload = [0u8; 16];
    let long = match (request[2], request[3] >> 4) {
        // Root ping used by Device::new.
        (0x00, 0x01) => {
            payload[0] = 4;
            false
        }
        // Root feature lookup.
        (0x00, 0x00) => {
            let feature_id = u16::from_be_bytes([request[4], request[5]]);
            payload[0] = u8::from(feature_id == 0x0003) * DEVICE_INFO_INDEX;
            false
        }
        // getDeviceInfo: the declared entity count, everything else zero.
        (DEVICE_INFO_INDEX, 0x00) => {
            payload[0] = ENTITY_COUNT;
            false
        }
        // getFwInfo, one entity per index.
        (DEVICE_INFO_INDEX, 0x01) => {
            match request[4] {
                // Bootloader BOT92.00_B0008, dormant, DFU-mode PID.
                0 => payload[..11].copy_from_slice(&[
                    0x01, b'B', b'O', b'T', 0x92, 0x00, 0x00, 0x08, 0x00, 0xaa, 0xef,
                ]),
                // MainApplication MPM17.00_B0008, the running image.
                1 => payload[..11].copy_from_slice(&[
                    0x00, b'M', b'P', b'M', 0x17, 0x00, 0x00, 0x08, 0x01, 0xc0, 0x8d,
                ]),
                // Softdevice whose build reads 0x00a9 — not packed BCD, so
                // get_fw_info answers UnsupportedResponse.
                2 => payload[..11].copy_from_slice(&[
                    0x05, b'R', b'Q', b'M', 0x00, 0x00, 0x00, 0xa9, 0x00, 0x00, 0x00,
                ]),
                _ => return Some(feature_error(request, BUSY)),
            }
            true
        }
        _ => return None,
    };

    let mut response = vec![0u8; if long { 20 } else { 7 }];
    response[0] = if long { 0x11 } else { 0x10 };
    response[1..4].copy_from_slice(&request[1..4]);
    let payload_len = response.len() - 4;
    response[4..].copy_from_slice(&payload[..payload_len]);
    Some(response)
}

/// True for the `getFwInfo` request addressing `entity`.
fn is_fw_info_for(request: &[u8], entity: u8) -> bool {
    request.len() >= 7
        && request[2] == DEVICE_INFO_INDEX
        && request[3] >> 4 == 1
        && request[4] == entity
}

async fn scripted_device_info_channel(channel: ScriptedRawHidChannel) -> Arc<HidppChannel> {
    scripted_channel(channel).await
}

#[tokio::test]
async fn an_entity_the_device_cannot_describe_does_not_stop_the_dump() {
    // Both entity-local shapes: a record that will not decode (entity 2) and a
    // read the firmware refuses (entity 3). Neither says anything about the
    // route, so the remaining entities are still read and the declared count
    // still matches what the device claimed.
    let (raw, handle) =
        ScriptedRawHidChannel::with_responder(mouse_with_an_unparseable_radio_stack);
    let channel = scripted_device_info_channel(raw).await;

    let entities = dump_firmware_entities_on_channel(&channel, 1)
        .await
        .expect("an entity the device cannot describe is a row, not a failed dump");

    assert_eq!(entities.len(), usize::from(ENTITY_COUNT));
    let FirmwareEntity::Readable { info, .. } = &entities[1] else {
        panic!("entity 1 parses: {:?}", entities[1]);
    };
    assert_eq!(info.kind, DeviceEntityType::MainApplication);
    assert_eq!(info.prefix, "MPM");
    assert_eq!((info.number, info.revision, info.build), (17, 0, 8));
    assert_eq!(info.transport_pid, 0xc08d);
    assert!(info.active);

    assert!(
        matches!(
            entities[2],
            FirmwareEntity::Unreadable {
                index: 2,
                error: WriteError::UnsupportedResponse { .. }
            }
        ),
        "a record that will not decode is reported, not dropped: {:?}",
        entities[2]
    );
    assert!(
        matches!(
            entities[3],
            FirmwareEntity::Unreadable {
                index: 3,
                error: WriteError::HidppFeature {
                    kind: HidppFeatureErrorKind::Busy,
                    ..
                }
            }
        ),
        "a refused read is reported, not dropped: {:?}",
        entities[3]
    );

    let written = handle.written_reports();
    assert!(
        written.iter().any(|report| is_fw_info_for(report, 3)),
        "every declared entity is asked about"
    );
}

#[tokio::test]
async fn a_channel_failure_aborts_the_dump_instead_of_blaming_the_firmware() {
    // The node goes away while entity 1 is being read. Continuing would spend
    // a timeout per remaining entity and then print malformed-firmware rows
    // for what is really a disconnect, so the whole dump fails instead.
    let (raw, handle) = ScriptedRawHidChannel::with_failing_writes(
        mouse_with_an_unparseable_radio_stack,
        |request| is_fw_info_for(request, 1),
    );
    let channel = scripted_device_info_channel(raw).await;

    let error = dump_firmware_entities_on_channel(&channel, 1)
        .await
        .expect_err("a dead route is not an entity-local failure");
    assert!(matches!(error, WriteError::Hidpp(_)), "got {error:?}");

    let written = handle.written_reports();
    assert!(
        !written.iter().any(|report| is_fw_info_for(report, 2)),
        "the dump stops at the failure rather than timing out per entity"
    );
}
