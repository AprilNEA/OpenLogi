use super::*;
use hidpp::feature::extended_dpi::{DpiRange, Lod, SensorCapabilities};
use hidpp::feature::per_key_lighting::FramePersistence;
use hidpp::feature::smartshift::WheelMode;

use crate::SharedChannel;
use crate::channel::scripted::{ScriptedRawHidChannel, feature_error, scripted_channel};
use crate::write::diagnostics::dump_firmware_entities_on_channel;
use crate::write::dpi::expand_dpi_ranges;
use crate::write::lighting::{collect_present_zones, per_key_commit, per_key_frames};
use crate::write::smartshift::{
    is_missing_enhanced, is_transient_smartshift_error, smartshift_to_wheel,
    status_matches_desired, wheel_mode_to_smartshift,
};
use crate::write::{HidppFeatureErrorKind, HidppOperation};
use crate::{
    BacklightControls, BacklightMode, BacklightState, BacklightStatus, FnLockState,
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
    let frames = per_key_frames(0x03, 0x27, 0x11, 0x22, 0x33);

    assert_eq!(frames.len(), 17);
    assert!(frames.iter().all(|report| report.len() == 64));
    assert!(frames.iter().all(|report| report[0] == 0x12));
    assert!(frames.iter().all(|report| report[1] == 0x03));
    assert!(frames.iter().all(|report| report[2] == 0x27));
    assert!(frames.iter().all(|report| report[3] == 0x3a));
    assert!(frames.iter().all(|report| report[5] == 0x01));
    // Bytes 6..8 are the big-endian count of entries the frame carries: 14
    // per full frame, and 9 in the last one (233 keys = 16 × 14 + 9).
    let (tail, full) = frames.split_last().expect("per-key lighting emits frames");
    assert!(full.iter().all(|report| report[6..8] == [0, 0x0e]));
    assert_eq!(tail[6..8], [0, 0x09]);
    assert!(
        tail[8 + 9 * 4..].iter().all(|byte| *byte == 0),
        "nothing follows the nine declared entries"
    );
    let declared: usize = frames.iter().map(|report| usize::from(report[7])).sum();
    assert_eq!(declared, 0xe9);

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

    // The commit is function 5 with an empty payload, sent as a 20-byte
    // request so its reply can be awaited.
    let commit = per_key_commit(0x03, 0x27);
    assert!(matches!(commit, hidpp::protocol::v20::Message::Long(..)));
    let header = commit.header();
    assert_eq!((header.device_index, header.feature_index), (0x03, 0x27));
    assert_eq!(
        (header.function_id.to_lo(), header.software_id.to_lo()),
        (0x5, 0xa)
    );
    assert!(commit.extend_payload().iter().all(|byte| *byte == 0));
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

    let backlight = get_backlight_on(&shared).await?;
    assert_eq!(
        backlight,
        BacklightState {
            enabled: true,
            mode: BacklightMode::Automatic,
            status: BacklightStatus::AlsAutomatic,
            current_level: 4,
            nb_levels: 8,
            power_save: false,
            controls: BacklightControls::default(),
        }
    );

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
    let dpi_x = std::sync::Mutex::new(800);
    let (raw, handle) = ScriptedRawHidChannel::with_dynamic_responder(move |request| {
        extended_dpi_scripted_response(request, &dpi_x)
    });
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
    // The sensor does not advertise an independent Y axis, so the spec has
    // the host send 0.
    assert_eq!(u16::from_be_bytes([write[7], write[8]]), 0);
    // It does advertise lift-off control, so the LOD is read back and
    // rewritten unchanged — the packet has no "leave alone" encoding.
    assert_eq!(write[9], u8::from(Lod::Medium));
    Ok(())
}

#[tokio::test]
async fn a_dropped_dpi_write_is_retried_once() -> Result<(), WriteError> {
    // Firmware still drowsy from a link re-establishment can ACK a
    // `setSensorDpi` yet keep the old value. The read-back catches the
    // mismatch and a single re-write lands the requested DPI.
    let state = std::sync::Arc::new(std::sync::Mutex::new(DrowsyDpiState {
        current: 1000,
        set_writes: 0,
    }));
    let responder_state = std::sync::Arc::clone(&state);
    let (raw, _handle) = ScriptedRawHidChannel::with_dynamic_responder(move |request| {
        adjustable_dpi_drowsy_response(request, &responder_state)
    });
    let channel = scripted_channel(raw).await;
    let shared = SharedChannel::new(
        channel,
        DeviceRoute::Direct {
            vendor_id: 0x046d,
            product_id: 0xb034,
        },
    );

    set_dpi_on(&shared, Dpi::new(650)).await?;

    let state = state.lock().unwrap();
    assert_eq!(
        state.set_writes, 2,
        "the dropped write must be retried exactly once"
    );
    assert_eq!(state.current, 650);
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
                0x1982 => 0x08,
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
        // Backlight config and info. The config mode occupies bits 3..=4 of
        // the little-endian options field; both replies carry long payloads.
        (0x08, 0x00) => {
            payload[0] = 1;
            payload[1] = u8::from(hidpp::feature::backlight::BacklightMode::Automatic) << 3;
            payload[5] = 4;
            true
        }
        (0x08, 0x02) => {
            payload[..4].copy_from_slice(&[
                8,
                4,
                u8::from(hidpp::feature::backlight::BacklightStatus::AlsAutomatic),
                u8::from(hidpp::feature::backlight::BacklightEffect::Static),
            ]);
            true
        }
        // 0x8080 frameEnd is acknowledged like any HID++2.0 request.
        (0x07, 0x05) => true,
        // Anything else gets no reply. The 0x12 per-key frames never reach
        // here: the filter above admits only 0x10/0x11 requests.
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
/// the DPI panel up for but the write path could not drive. `dpi_x` is the
/// sensor's live X-axis DPI, which a write changes.
fn extended_dpi_scripted_response(
    request: &[u8],
    dpi_x: &std::sync::Mutex<u16>,
) -> Option<Vec<u8>> {
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
        // getSensorCapabilities: echo the sensor, no DPI levels, lift-off
        // distance control but no independent Y axis.
        (0x05, 0x01) => {
            payload[0] = request[4];
            payload[2] = SensorCapabilities::LOD.bits();
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
        // getSensorDpiParameters: the live DPI, 800 by default, no
        // independent Y axis (dpiY reads 0), lift-off distance MEDIUM.
        (0x05, 0x05) => {
            payload[1..3].copy_from_slice(&dpi_x.lock().unwrap().to_be_bytes());
            payload[3..5].copy_from_slice(&800u16.to_be_bytes());
            payload[9] = u8::from(Lod::Medium);
            true
        }
        // setSensorDpiParameters: apply dpiX and echo the request back.
        (0x05, 0x06) => {
            *dpi_x.lock().unwrap() = u16::from_be_bytes([request[5], request[6]]);
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

/// Live sensor state for [`adjustable_dpi_drowsy_response`].
struct DrowsyDpiState {
    current: u16,
    set_writes: u8,
}

/// A `0x2201`-only mouse whose first `setSensorDpi` is ACKed but dropped —
/// firmware drowsy from a link re-establishment — while the second is honored.
fn adjustable_dpi_drowsy_response(
    request: &[u8],
    state: &std::sync::Mutex<DrowsyDpiState>,
) -> Option<Vec<u8>> {
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
        // Root feature lookup: only 0x2201 present.
        (0x00, 0x00) => {
            let feature_id = u16::from_be_bytes([request[4], request[5]]);
            payload[0] = u8::from(feature_id == 0x2201) * 0x04;
            false
        }
        // getSensorDpiList: the echoed sensor index, then 650 and 1000.
        (0x04, 0x01) => {
            payload[..7].copy_from_slice(&[request[4], 0x02, 0x8a, 0x03, 0xe8, 0x00, 0x00]);
            true
        }
        // getSensorDpi: the echoed sensor index, then the live value.
        (0x04, 0x02) => {
            payload[0] = request[4];
            payload[1..3].copy_from_slice(&state.lock().unwrap().current.to_be_bytes());
            false
        }
        // setSensorDpi: always ACKed, but the first write changes nothing.
        (0x04, 0x03) => {
            let mut state = state.lock().unwrap();
            state.set_writes += 1;
            if state.set_writes > 1 {
                state.current = u16::from_be_bytes([request[5], request[6]]);
            }
            payload[..3].copy_from_slice(&request[4..7]);
            false
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
const ENTITY_COUNT: u8 = 5;

/// A wired G502 LIGHTSPEED, plus two invented entities.
///
/// Entities 0..=2 are what the real mouse reports: a bootloader, the running
/// main application, and a Softdevice radio stack whose build field is binary
/// rather than BCD. Entities 3 and 4 cover the two entity-local failures: a
/// record that will not parse (a prefix that is not UTF-8), and firmware that
/// refuses the read outright.
fn g502_plus_two_failing_entities(request: &[u8]) -> Option<Vec<u8>> {
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
                // Softdevice whose build reads 0x00a9 — binary, not packed
                // BCD. It decodes as build 169, as Options+ reads it.
                2 => payload[..11].copy_from_slice(&[
                    0x05, b'R', b'Q', b'M', 0x00, 0x00, 0x00, 0xa9, 0x00, 0x00, 0x00,
                ]),
                // A prefix that is not UTF-8: get_fw_info answers
                // UnsupportedResponse.
                3 => payload[..11].copy_from_slice(&[
                    0x05, 0xff, 0xfe, 0xfd, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00,
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
    // Both entity-local shapes: a record that will not decode (entity 3) and a
    // read the firmware refuses (entity 4). Neither says anything about the
    // route, so the remaining entities are still read and the declared count
    // still matches what the device claimed.
    let (raw, handle) = ScriptedRawHidChannel::with_responder(g502_plus_two_failing_entities);
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

    // The Softdevice's binary build is read as a number, not refused.
    let FirmwareEntity::Readable { info, .. } = &entities[2] else {
        panic!("entity 2 parses: {:?}", entities[2]);
    };
    assert_eq!(info.kind, DeviceEntityType::Softdevice);
    assert_eq!(info.prefix, "RQM");
    assert_eq!((info.number, info.revision, info.build), (0, 0, 169));

    assert!(
        matches!(
            entities[3],
            FirmwareEntity::Unreadable {
                index: 3,
                error: WriteError::UnsupportedResponse { .. }
            }
        ),
        "a record that will not decode is reported, not dropped: {:?}",
        entities[3]
    );
    assert!(
        matches!(
            entities[4],
            FirmwareEntity::Unreadable {
                index: 4,
                error: WriteError::HidppFeature {
                    kind: HidppFeatureErrorKind::Busy,
                    ..
                }
            }
        ),
        "a refused read is reported, not dropped: {:?}",
        entities[4]
    );

    let written = handle.written_reports();
    assert!(
        written.iter().any(|report| is_fw_info_for(report, 4)),
        "every declared entity is asked about"
    );
}

#[tokio::test]
async fn a_channel_failure_aborts_the_dump_instead_of_blaming_the_firmware() {
    // The node goes away while entity 1 is being read. Continuing would spend
    // a timeout per remaining entity and then print malformed-firmware rows
    // for what is really a disconnect, so the whole dump fails instead.
    let (raw, handle) =
        ScriptedRawHidChannel::with_failing_writes(g502_plus_two_failing_entities, |request| {
            is_fw_info_for(request, 1)
        });
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

/// A multi-host keyboard: `0x40a3` at index 0x09 and `0x1815 HostsInfo` at
/// index 0x0a reporting host slot 2 as current. Both fn-inversion echoes
/// report whatever state the last `set` wrote (shared across the test through
/// the request the responder sees, since scripted responders are stateless).
fn multi_host_keyboard_response(request: &[u8]) -> Option<Vec<u8>> {
    if request.len() < 7 || !matches!(request[0], 0x10 | 0x11) {
        return None;
    }
    let feature_index = request[2];
    let function = request[3] >> 4;
    let mut payload = [0u8; 16];
    match (feature_index, function) {
        (0x00, 0x01) => payload[0] = 4,
        (0x00, 0x00) => {
            let feature_id = u16::from_be_bytes([request[4], request[5]]);
            payload[0] = match feature_id {
                0x40a3 => 0x09,
                0x1815 => 0x0a,
                _ => 0x00,
            };
        }
        // getFeatureInfo: capabilities, descriptor caps, 3 hosts, current = 2.
        (0x0a, 0x00) => payload[..4].copy_from_slice(&[0x07, 0x00, 3, 2]),
        // getGlobalFnInversion(host): inversion ON (media keys first).
        (0x09, 0x00) => payload[..4].copy_from_slice(&[request[4], 1, 1, 1]),
        // setGlobalFnInversion(host, state): echo host and the written state.
        (0x09, 0x01) => payload[..4].copy_from_slice(&[request[4], request[5], 1, 1]),
        _ => return None,
    }
    // Long replies: `getFeatureInfo` and the inversion echoes carry four
    // payload bytes, one more than a short report holds.
    let mut response = vec![0u8; 20];
    response[0] = 0x11;
    response[1..4].copy_from_slice(&request[1..4]);
    response[4..].copy_from_slice(&payload);
    Some(response)
}

#[tokio::test]
async fn fn_lock_addresses_the_current_host_slot_read_from_hosts_info() -> Result<(), WriteError> {
    let (raw, handle) = ScriptedRawHidChannel::with_responder(multi_host_keyboard_response);
    let channel = scripted_channel(raw).await;
    let shared = SharedChannel::new(
        channel,
        DeviceRoute::Direct {
            vendor_id: 0x046d,
            product_id: 0xb378,
        },
    );

    // Inversion ON in firmware reads back as Fn-lock off.
    let state = get_fn_lock_on(&shared).await?;
    assert_eq!(
        state,
        FnLockState {
            fn_lock: false,
            default_fn_lock: Some(false),
        }
    );
    let get = handle
        .written_reports()
        .into_iter()
        .find(|report| report[2] == 0x09 && report[3] >> 4 == 0x00)
        .expect("a getGlobalFnInversion read");
    // A read addresses the spec's 0xff "current host": every firmware answers
    // it, and it must not depend on 0x1815 being readable.
    assert_eq!(get[4], 0xff);
    assert!(
        !handle
            .written_reports()
            .into_iter()
            .any(|report| report[2] == 0x0a),
        "a read never asks 0x1815 which slot is current"
    );

    let written = set_fn_lock_on(&shared, true).await?;
    assert!(written.fn_lock);

    let set = handle
        .written_reports()
        .into_iter()
        .find(|report| report[2] == 0x09 && report[3] >> 4 == 0x01)
        .expect("a setGlobalFnInversion write");
    // Host slot 2 from 0x1815, not the 0xff "current host" the MX Keys S
    // firmware ignores; Fn-lock on is inversion OFF (0).
    assert_eq!(&set[4..6], &[2, 0]);
    Ok(())
}

/// The multi-host keyboard whose `0x1815 getFeatureInfo` answers with a HID++
/// error (`Busy`): the feature is there, the slot is not readable right now.
fn multi_host_keyboard_with_unreadable_hosts_info(request: &[u8]) -> Option<Vec<u8>> {
    if request.len() >= 4 && request[2] == 0x0a && request[3] >> 4 == 0x00 {
        // HID++ 2.0 error frame: feature index 0xff, then the failing
        // feature index, the function/software id nibble pair, and the code.
        let mut response = vec![0u8; 20];
        response[0] = 0x11;
        response[1] = request[1];
        response[2] = 0xff;
        response[3] = request[2];
        response[4] = request[3];
        response[5] = 8; // Busy
        return Some(response);
    }
    multi_host_keyboard_response(request)
}

#[tokio::test]
async fn fn_lock_write_does_not_guess_the_host_when_hosts_info_is_unreadable() {
    let (raw, handle) =
        ScriptedRawHidChannel::with_responder(multi_host_keyboard_with_unreadable_hosts_info);
    let channel = scripted_channel(raw).await;
    let shared = SharedChannel::new(
        channel,
        DeviceRoute::Direct {
            vendor_id: 0x046d,
            product_id: 0xb378,
        },
    );

    let error = set_fn_lock_on(&shared, true)
        .await
        .expect_err("a keyboard with 0x1815 that cannot say which slot is current fails the write");
    assert_eq!(
        error,
        WriteError::HidppFeature {
            operation: HidppOperation::WriteFnLock,
            feature_hex: 0x1815,
            kind: HidppFeatureErrorKind::Busy,
        }
    );
    // Nothing was written to 0x40a3: a 0xff-addressed write is the one the
    // MX Keys S firmware drops, so it is never sent as a guess.
    assert!(
        !handle
            .written_reports()
            .into_iter()
            .any(|report| report[2] == 0x09 && report[3] >> 4 == 0x01),
        "no setGlobalFnInversion without a known host slot"
    );

    // The read does not need the slot: 0xff reads work on the MX Keys S, so
    // the GUI still learns the keyboard's own state while 0x1815 is busy.
    let state = get_fn_lock_on(&shared)
        .await
        .expect("a read addresses the current host directly");
    assert!(!state.fn_lock);
}

/// A keyboard with only the original `0x40a0` (at index 0x0c), whose state
/// reads as inversion OFF. Its `set` answers no state, as Options+ expects.
fn legacy_keyboard_response(request: &[u8]) -> Option<Vec<u8>> {
    if request.len() < 7 || !matches!(request[0], 0x10 | 0x11) {
        return None;
    }
    let feature_index = request[2];
    let function = request[3] >> 4;
    let mut payload = [0u8; 3];
    match (feature_index, function) {
        (0x00, 0x01) => payload[0] = 4,
        (0x00, 0x00) => {
            let feature_id = u16::from_be_bytes([request[4], request[5]]);
            payload[0] = u8::from(feature_id == 0x40a0) * 0x0c;
        }
        // getGlobalFnInversion: inversion OFF in the low nibble; the high
        // nibble is not the state.
        (0x0c, 0x00) => payload[0] = 0x50,
        (0x0c, 0x01) => {}
        _ => return None,
    }
    let mut response = vec![0u8; 7];
    response[0] = 0x10;
    response[1..4].copy_from_slice(&request[1..4]);
    response[4..].copy_from_slice(&payload);
    Some(response)
}

#[tokio::test]
async fn fn_lock_on_a_legacy_keyboard_uses_0x40a0_and_reads_back_its_writes() {
    let (raw, handle) = ScriptedRawHidChannel::with_responder(legacy_keyboard_response);
    let channel = scripted_channel(raw).await;
    let shared = SharedChannel::new(
        channel,
        DeviceRoute::Direct {
            vendor_id: 0x046d,
            product_id: 0xc31c,
        },
    );
    let calls = |function: u8| {
        handle
            .written_reports()
            .into_iter()
            .filter(|report| report[2] == 0x0c && report[3] >> 4 == function)
            .count()
    };

    // Inversion OFF reads as Fn-lock on; 0x40a0 has no default to report.
    let state = get_fn_lock_on(&shared)
        .await
        .expect("0x40a0 is the last fallback");
    assert_eq!(
        state,
        FnLockState {
            fn_lock: true,
            default_fn_lock: None,
        }
    );
    assert_eq!((calls(0), calls(1)), (1, 0));

    // Its reply carries no state, so the write is confirmed by a read.
    let written = set_fn_lock_on(&shared, true)
        .await
        .expect("the keyboard reads back what was written");
    assert_eq!(written, state);
    assert_eq!((calls(0), calls(1)), (2, 1));
    let set = handle
        .written_reports()
        .into_iter()
        .find(|report| report[2] == 0x0c && report[3] >> 4 == 0x01)
        .expect("a setGlobalFnInversion write");
    // Fn-lock on is inversion OFF (0), the only argument byte.
    assert_eq!(&set[4..7], &[0, 0, 0]);

    // A keyboard that reads back the old state did not take the write.
    let error = set_fn_lock_on(&shared, false)
        .await
        .expect_err("a read-back that disagrees with the write is not a success");
    assert_eq!(
        error,
        WriteError::UnsupportedResponse {
            operation: HidppOperation::WriteFnLock,
            feature_hex: 0x40a0,
        }
    );
    let sets: Vec<_> = handle
        .written_reports()
        .into_iter()
        .filter(|report| report[2] == 0x0c && report[3] >> 4 == 0x01)
        .collect();
    assert_eq!(sets.len(), 2);
    assert_eq!(sets[1][4], 1, "Fn-lock off is inversion ON");
    assert_eq!(calls(0), 3);
}

/// A single-host keyboard (`0x40a2` only) whose firmware echoes inversion ON
/// no matter what is written — the keyboard did not take the write.
fn stubborn_single_host_keyboard_response(request: &[u8]) -> Option<Vec<u8>> {
    if request.len() < 7 || !matches!(request[0], 0x10 | 0x11) {
        return None;
    }
    let feature_index = request[2];
    let function = request[3] >> 4;
    let mut payload = [0u8; 16];
    match (feature_index, function) {
        (0x00, 0x01) => payload[0] = 4,
        (0x00, 0x00) => {
            let feature_id = u16::from_be_bytes([request[4], request[5]]);
            payload[0] = u8::from(feature_id == 0x40a2) * 0x0b;
        }
        (0x0b, 0x00 | 0x01) => payload[..2].copy_from_slice(&[1, 1]),
        _ => return None,
    }
    let mut response = vec![0u8; 7];
    response[0] = 0x10;
    response[1..4].copy_from_slice(&request[1..4]);
    response[4..].copy_from_slice(&payload[..3]);
    Some(response)
}

#[tokio::test]
async fn fn_lock_write_that_the_keyboard_does_not_echo_fails() {
    let (raw, handle) =
        ScriptedRawHidChannel::with_responder(stubborn_single_host_keyboard_response);
    let channel = scripted_channel(raw).await;
    let shared = SharedChannel::new(
        channel,
        DeviceRoute::Direct {
            vendor_id: 0x046d,
            product_id: 0xb342,
        },
    );

    let error = set_fn_lock_on(&shared, true)
        .await
        .expect_err("an echo that disagrees with the write is not a success");
    assert_eq!(
        error,
        WriteError::UnsupportedResponse {
            operation: HidppOperation::WriteFnLock,
            feature_hex: 0x40a2,
        }
    );
    // The single-host fallback wrote inversion OFF once; no 0x1815 lookup
    // is attempted on a keyboard without 0x40a3.
    let sets: Vec<_> = handle
        .written_reports()
        .into_iter()
        .filter(|report| report[2] == 0x0b && report[3] >> 4 == 0x01)
        .collect();
    assert_eq!(sets.len(), 1);
    assert_eq!(sets[0][4], 0);
}

/// Zones a scripted `0x8070` keyboard reports, in index order: each zone's raw
/// location and the raw effect ids of its own effect list.
type ScriptedZones = &'static [(u16, &'static [u16])];

const COLOR_LED_EFFECTS_INDEX: u8 = 0x09;

/// A `0x8070` keyboard answering `getInfo`, `getZoneInfo`,
/// `getZoneEffectInfo` and `setZoneEffect` from `zones`. A request for a zone
/// or effect it does not have gets no reply.
fn color_led_effects_device(
    zones: ScriptedZones,
) -> impl Fn(&[u8]) -> Option<Vec<u8>> + Send + Sync + 'static {
    move |request| {
        if request.len() < 7 || !matches!(request[0], 0x10 | 0x11) {
            return None;
        }
        let mut payload = [0u8; 16];
        let long = match (request[2], request[3] >> 4) {
            (0x00, 0x01) => {
                payload[0] = 4;
                false
            }
            (0x00, 0x00) => {
                if u16::from_be_bytes([request[4], request[5]]) == 0x8070 {
                    payload[0] = COLOR_LED_EFFECTS_INDEX;
                }
                false
            }
            (COLOR_LED_EFFECTS_INDEX, 0x00) => {
                payload[0] = u8::try_from(zones.len()).expect("scripted zone count fits a byte");
                true
            }
            (COLOR_LED_EFFECTS_INDEX, 0x01) => {
                let (location, effects) = zones.get(usize::from(request[4]))?;
                payload[0] = request[4];
                payload[1..3].copy_from_slice(&location.to_be_bytes());
                payload[3] =
                    u8::try_from(effects.len()).expect("scripted effect count fits a byte");
                true
            }
            (COLOR_LED_EFFECTS_INDEX, 0x02) => {
                let (_, effects) = zones.get(usize::from(request[4]))?;
                let effect_id = effects.get(usize::from(request[5]))?;
                payload[0] = request[4];
                payload[1] = request[5];
                payload[2..4].copy_from_slice(&effect_id.to_be_bytes());
                true
            }
            (COLOR_LED_EFFECTS_INDEX, 0x03) => true,
            _ => return None,
        };
        let mut response = vec![0u8; if long { 20 } else { 7 }];
        response[0] = if long { 0x11 } else { 0x10 };
        response[1..4].copy_from_slice(&request[1..4]);
        let payload_len = response.len() - 4;
        response[4..].copy_from_slice(&payload[..payload_len]);
        Some(response)
    }
}

/// Every `setZoneEffect` the keyboard received, as `(zone, effect index)`.
fn zone_effect_writes(handle: &crate::replay::ReplayChannelHandle) -> Vec<(u8, u8)> {
    handle
        .written_reports()
        .into_iter()
        .filter(|report| {
            report.len() == 20 && report[2] == COLOR_LED_EFFECTS_INDEX && report[3] >> 4 == 0x03
        })
        .map(|report| {
            assert_eq!(
                report[6..9],
                [0x11, 0x22, 0x33],
                "fixed colour params are RGB"
            );
            assert_eq!(report[16], 0, "the colour is applied volatilely");
            (report[4], report[5])
        })
        .collect()
}

async fn color_led_effects_shared(
    zones: ScriptedZones,
) -> (SharedChannel, crate::replay::ReplayChannelHandle) {
    let (raw, handle) =
        ScriptedRawHidChannel::with_dynamic_responder(color_led_effects_device(zones));
    let shared = SharedChannel::new(
        scripted_channel(raw).await,
        DeviceRoute::Direct {
            vendor_id: 0x046d,
            product_id: 0xc33f,
        },
    );
    (shared, handle)
}

#[tokio::test]
async fn a_fixed_colour_targets_each_zones_own_fixed_effect() -> Result<(), WriteError> {
    // Zone 0 lists the fixed colour third; zone 1 after an effect id this
    // crate does not model; zone 2 has none, so it is left alone; zone 3 has a
    // location the parser rejects, so it keeps the legacy index 1.
    const ZONES: ScriptedZones = &[
        (0x0001, &[0x0000, 0x0004, 0x0001]),
        (0x0006, &[0x0063, 0x0004, 0x0001]),
        (0x0002, &[0x0003]),
        (0x0042, &[0x0001, 0x0004]),
    ];
    let (shared, handle) = color_led_effects_shared(ZONES).await;

    set_keyboard_color_with_on(&shared, LightingMethod::Effects, 0x11, 0x22, 0x33).await?;

    assert_eq!(zone_effect_writes(&handle), [(0, 2), (1, 2), (3, 1)]);
    Ok(())
}

#[tokio::test]
async fn a_keyboard_whose_zones_offer_no_fixed_colour_is_not_written() {
    const ZONES: ScriptedZones = &[(0x0001, &[0x0003, 0x0004]), (0x0002, &[0x0000])];
    let (shared, handle) = color_led_effects_shared(ZONES).await;

    let result =
        set_keyboard_color_with_on(&shared, LightingMethod::Effects, 0x11, 0x22, 0x33).await;

    assert_eq!(
        result,
        Err(WriteError::FeatureUnsupported {
            feature_hex: 0x8070
        }),
        "Auto falls through to the next path on this error"
    );
    assert!(zone_effect_writes(&handle).is_empty());
}

/// `true` for the 20-byte `0x8080` frameEnd sent to the scripted keyboard,
/// whose `0x8080` sits at feature index `0x07`.
fn is_per_key_commit(report: &[u8]) -> bool {
    report.len() == 20 && report[0] == 0x11 && report[2] == 0x07 && report[3] >> 4 == 0x05
}

/// The first key id of the third `0x12` frame; each frame carries 14 keys.
const THIRD_PER_KEY_FRAME_FIRST_KEY: u8 = 2 * 14;

/// [`scripted_response`], with the third `0x12` per-key frame rejected as an
/// invalid argument.
fn per_key_frame_rejected_response(request: &[u8]) -> Option<Vec<u8>> {
    if request.first() == Some(&0x12) && request.get(8) == Some(&THIRD_PER_KEY_FRAME_FIRST_KEY) {
        return Some(feature_error(request, 0x02));
    }
    scripted_response(request)
}

/// [`scripted_response`], with the `0x8080` frameEnd answered busy.
fn per_key_commit_busy_response(request: &[u8]) -> Option<Vec<u8>> {
    if is_per_key_commit(request) {
        return Some(feature_error(request, 0x08));
    }
    scripted_response(request)
}

#[tokio::test]
async fn a_rejected_per_key_frame_fails_the_write_before_the_commit() {
    // The 0x12 frames were written fire-and-forget, so a frame the keyboard
    // rejected was committed anyway and the write reported success.
    let (raw, handle) = ScriptedRawHidChannel::with_responder(per_key_frame_rejected_response);
    let shared = SharedChannel::new(
        scripted_channel(raw).await,
        DeviceRoute::Direct {
            vendor_id: 0x046d,
            product_id: 0xc339,
        },
    );

    let result =
        set_keyboard_color_with_on(&shared, LightingMethod::PerKey, 0x11, 0x22, 0x33).await;

    assert_eq!(
        result,
        Err(WriteError::HidppFeature {
            operation: HidppOperation::Lighting,
            feature_hex: 0x8080,
            kind: HidppFeatureErrorKind::InvalidArgument,
        })
    );
    assert!(
        !handle
            .written_reports()
            .iter()
            .any(|report| is_per_key_commit(report)),
        "a rejected frame must not be committed"
    );
}

#[tokio::test]
async fn a_rejected_per_key_commit_fails_the_write() {
    // The frameEnd was written raw too, so its reply was never read.
    let (raw, _handle) = ScriptedRawHidChannel::with_responder(per_key_commit_busy_response);
    let shared = SharedChannel::new(
        scripted_channel(raw).await,
        DeviceRoute::Direct {
            vendor_id: 0x046d,
            product_id: 0xc339,
        },
    );

    let result =
        set_keyboard_color_with_on(&shared, LightingMethod::PerKey, 0x11, 0x22, 0x33).await;

    assert_eq!(
        result,
        Err(WriteError::HidppFeature {
            operation: HidppOperation::Lighting,
            feature_hex: 0x8080,
            kind: HidppFeatureErrorKind::Busy,
        })
    );
}

#[tokio::test]
async fn per_key_frames_are_fenced_before_their_one_commit() -> Result<(), WriteError> {
    // Errors for the 0x12 frames arrive asynchronously. A round trip after the
    // last frame proves the device has answered every earlier frame, so the
    // commit is only sent once no frame can still report an error.
    let (raw, handle) = ScriptedRawHidChannel::with_responder(scripted_response);
    let shared = SharedChannel::new(
        scripted_channel(raw).await,
        DeviceRoute::Direct {
            vendor_id: 0x046d,
            product_id: 0xc339,
        },
    );

    set_keyboard_color_with_on(&shared, LightingMethod::PerKey, 0x11, 0x22, 0x33).await?;

    let written = handle.written_reports();
    let last_frame = written
        .iter()
        .rposition(|report| report.first() == Some(&0x12))
        .expect("the frames were streamed");
    let is_root_ping = |report: &Vec<u8>| {
        matches!(report.first(), Some(0x10 | 0x11)) && report[2] == 0x00 && report[3] >> 4 == 0x01
    };
    let fence = written[last_frame..]
        .iter()
        .position(is_root_ping)
        .map(|offset| last_frame + offset)
        .expect("a root ping follows the last frame");
    let commits: Vec<usize> = written
        .iter()
        .enumerate()
        .filter(|(_, report)| is_per_key_commit(report))
        .map(|(position, _)| position)
        .collect();
    assert_eq!(commits.len(), 1, "exactly one frameEnd");
    assert!(commits[0] > fence, "the frameEnd follows the fence");
    Ok(())
}
