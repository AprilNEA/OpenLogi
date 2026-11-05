//! The Unifying receiver: arrival broadcasts, the arrival trigger and its retries, and codename reads.

use hidpp::channel::SEND_RESPONSE_TIMEOUT;
use hidpp::receiver::unifying::DeviceConnection;
use openlogi_core::device::BatteryLevel;

use super::*;
use crate::channel::scripted::register_error;

/// Slot 1's offline re-broadcast. The exact wire bytes once misread as proof
/// that the online bit is stuck: `04 62 69 40` is an encrypted MX Master 2S
/// (wpid 0x4069) slot re-broadcast with bit 6 *set* — link not established,
/// device offline.
fn offline_mx_master_2s_arrival() -> DeviceConnection {
    let message = Message::Short(
        MessageHeader {
            device_index: 1,
            sub_id: 0x41,
        },
        [0x04, 0x62, 0x69, 0x40],
    );
    let Some(UnifyingEvent::DeviceConnection(event)) = decode_notification(&message) else {
        panic!("expected a device-connection event");
    };
    assert!(!event.online, "bit 6 set must decode as offline");
    event
}

#[tokio::test]
async fn offline_slot_is_named_by_the_receiver_without_probing_the_device() {
    let event = offline_mx_master_2s_arrival();
    let (raw, handle) = ScriptedRawHidChannel::with_responder(|request| {
        match (request[1], request[2], request[3]) {
            // #447: the receiver keeps slot 1's name at sub-register 0x40,
            // so a stale pairing is named while its device is away.
            (0xff, 0x83, 0xb5) if request[4] == 0x40 => {
                let mut response = vec![0x11, 0xff, 0x83, 0xb5, 0x40, 0x0c];
                response.extend_from_slice(b"MX Master 2S");
                response.resize(20, 0);
                Some(response)
            }
            _ => panic!("an offline slot must not be probed: {request:02x?}"),
        }
    });
    let channel = scripted_channel(raw).await;
    let writes_before = handle.written_reports().len();

    let cache = HashMap::new();
    let pass = PassContext {
        cache: &cache,
        now: Instant::now(),
        subscriptions: None,
        timeouts: &ProbeTimeouts::DEFAULT,
    };
    let (device, _) = probe_unifying_slot(
        &channel,
        &event,
        "SERIAL",
        &host_lock::ReceiverRegisterPhase::uncontended(),
        pass,
    )
    .await
    .expect("an offline slot still surfaces from its re-broadcast");

    assert!(!device.online);
    assert_eq!(device.wpid, Some(0x4069));
    assert_eq!(device.codename.as_deref(), Some("MX Master 2S"));
    let writes = &handle.written_reports()[writes_before..];
    assert!(
        writes.iter().all(|report| report[1] == 0xff),
        "an offline slot must not be probed for features or battery: {writes:02x?}"
    );
}

#[tokio::test]
async fn offline_slot_name_read_is_bounded_when_the_receiver_drops_the_reply() {
    // The offline name read runs on every reconciliation under the register
    // phase, so a receiver that never answers it must cost the receiver's
    // short budget, not a full request timeout.
    let event = offline_mx_master_2s_arrival();
    let (raw, _) = ScriptedRawHidChannel::with_responder(|_| None);
    let channel = scripted_channel(raw).await;

    let cache = HashMap::new();
    let pass = PassContext {
        cache: &cache,
        now: Instant::now(),
        subscriptions: None,
        timeouts: &ProbeTimeouts::DEFAULT,
    };
    let started = Instant::now();
    let (device, _) = probe_unifying_slot(
        &channel,
        &event,
        "SERIAL",
        &host_lock::ReceiverRegisterPhase::uncontended(),
        pass,
    )
    .await
    .expect("an offline slot surfaces without its name");

    assert!(!device.online);
    assert_eq!(device.codename, None);
    let waited = started.elapsed();
    assert!(
        waited < SEND_RESPONSE_TIMEOUT,
        "the name read waited {waited:?}"
    );
}

#[tokio::test]
async fn hidpp10_slot_is_named_by_the_receiver_and_reports_its_battery() {
    // A Performance MX (wpid 0x101a, HID++ 1.0) re-broadcast online in slot 1.
    let message = Message::Short(
        MessageHeader {
            device_index: 1,
            sub_id: 0x41,
        },
        [0x04, 0x22, 0x1a, 0x10],
    );
    let Some(UnifyingEvent::DeviceConnection(event)) = decode_notification(&message) else {
        panic!("expected a device-connection event");
    };
    let (raw, _) = ScriptedRawHidChannel::with_responder(|request| {
        match (request[1], request[2], request[3]) {
            // HID++ 2.0's ping reaches a HID++ 1.0 device as sub id 0x00.
            (0x01, 0x00, _) => Some(register_error(request, 0x01)),
            (0x01, 0x81, 0x0d) => Some(register_error(request, 0x02)),
            (0x01, 0x81, 0x07) => Some(vec![0x10, 0x01, 0x81, 0x07, 0x07, 0x00, 0x00]),
            // The receiver keeps slot 1's name at sub-register 0x40.
            (0xff, 0x83, 0xb5) if request[4] == 0x40 => {
                let mut response = vec![0x11, 0xff, 0x83, 0xb5, 0x40, 14];
                response.extend_from_slice(b"Performance MX");
                Some(response)
            }
            _ => panic!("unexpected request: {request:02x?}"),
        }
    });
    let channel = scripted_channel(raw).await;

    let cache = HashMap::new();
    let pass = PassContext {
        cache: &cache,
        now: Instant::now(),
        subscriptions: None,
        timeouts: &ProbeTimeouts::DEFAULT,
    };
    let (device, outcome) = probe_unifying_slot(
        &channel,
        &event,
        "SERIAL",
        &host_lock::ReceiverRegisterPhase::uncontended(),
        pass,
    )
    .await
    .expect("an online slot surfaces");

    assert_eq!(device.codename.as_deref(), Some("Performance MX"));
    assert_eq!(device.kind, DeviceKind::Mouse);
    assert!(
        device.hidpp10,
        "the GUI presumes only host-side tabs for it"
    );
    assert_eq!(
        device.capabilities, None,
        "no feature table: the UI keeps its kind-based fallback"
    );
    let battery = device.battery.expect("register 0x07 reports the battery");
    assert_eq!(battery.level, BatteryLevel::Full);
    assert_eq!(
        battery.percentage, None,
        "register 0x07 reports only a level"
    );
    assert!(
        matches!(outcome, CacheOutcome::Fresh(..)),
        "a completed HID++ 1.0 probe is memoized"
    );
}

#[tokio::test]
async fn hidpp10_trackball_slot_takes_its_kind_from_the_arrival() {
    // #2202: an M570 (wpid 0x1028, HID++ 1.0) surfaced as Unknown. With no
    // feature table, the arrival nibble is its only kind source, and the GUI
    // picks the Buttons panel from it. `0x28` is the trackball nibble plus
    // the encryption bit, not a wire capture.
    let message = Message::Short(
        MessageHeader {
            device_index: 1,
            sub_id: 0x41,
        },
        [0x04, 0x28, 0x28, 0x10],
    );
    let Some(UnifyingEvent::DeviceConnection(event)) = decode_notification(&message) else {
        panic!("expected a device-connection event");
    };
    let (raw, _) = ScriptedRawHidChannel::with_responder(|request| {
        match (request[1], request[2], request[3]) {
            (0x01, 0x00, _) => Some(register_error(request, 0x01)),
            (0x01, 0x81, 0x0d | 0x07) => Some(register_error(request, 0x02)),
            (0xff, 0x83, 0xb5) if request[4] == 0x40 => {
                let mut response = vec![0x11, 0xff, 0x83, 0xb5, 0x40, 4];
                response.extend_from_slice(b"M570");
                // A long report is 20 bytes; a shorter one is dropped.
                response.resize(20, 0);
                Some(response)
            }
            _ => panic!("unexpected request: {request:02x?}"),
        }
    });
    let channel = scripted_channel(raw).await;

    let cache = HashMap::new();
    let pass = PassContext {
        cache: &cache,
        now: Instant::now(),
        subscriptions: None,
        timeouts: &ProbeTimeouts::DEFAULT,
    };
    let (device, _) = probe_unifying_slot(
        &channel,
        &event,
        "SERIAL",
        &host_lock::ReceiverRegisterPhase::uncontended(),
        pass,
    )
    .await
    .expect("an online slot surfaces");

    assert_eq!(device.codename.as_deref(), Some("M570"));
    assert_eq!(device.kind, DeviceKind::Trackball);
    assert_eq!(device.capabilities, None);
    assert!(device.hidpp10);
}

#[test]
fn unifying_arrival_liveness_survives_missing_feature_data() {
    let device = assemble_unifying_device(
        1,
        None,
        0x40b8,
        DeviceKind::Mouse,
        ProbedFeatures::default(),
        true,
        0,
    );
    assert!(device.online);
    assert_eq!(device.wpid, Some(0x40b8));
    assert_eq!(device.kind, DeviceKind::Mouse);
    assert!(
        !device.hidpp10,
        "a probe that did not complete says nothing of the protocol"
    );
}

#[tokio::test]
async fn unifying_arrival_trigger_retries_one_transient_failure() {
    let mut attempts = 0;

    let result = retry_arrival_trigger(
        || {
            attempts += 1;
            std::future::ready((attempts > 1).then_some(()).ok_or("transient"))
        },
        std::time::Duration::from_secs(1),
        std::time::Duration::ZERO,
    )
    .await;

    assert_eq!(result, Some(()));
    assert_eq!(attempts, 2);
}

#[tokio::test]
async fn unifying_arrival_trigger_surfaces_a_persistent_failure() {
    let mut attempts = 0;

    let result = retry_arrival_trigger(
        || {
            attempts += 1;
            std::future::ready(Err::<(), _>("persistent"))
        },
        std::time::Duration::from_secs(1),
        std::time::Duration::ZERO,
    )
    .await;

    assert_eq!(result, None);
    assert_eq!(attempts, 2);
}

#[tokio::test]
async fn unifying_arrival_trigger_bounds_two_stalled_attempts() {
    let attempt_timeout = std::time::Duration::from_millis(1);
    let retry_delay = std::time::Duration::from_millis(1);
    let mut attempts = 0;

    let result = tokio::time::timeout(
        std::time::Duration::from_millis(100),
        retry_arrival_trigger(
            || {
                attempts += 1;
                std::future::pending::<Result<(), &str>>()
            },
            attempt_timeout,
            retry_delay,
        ),
    )
    .await
    .expect("the trigger retry must finish inside its caller's budget");

    assert_eq!(result, None);
    assert_eq!(attempts, 2);
}

#[test]
fn codename_reads_len_prefixed_name() {
    // wire-verified MX Master 2S reply: `40 0c "MX Master 2S"` then padding.
    let mut buf = vec![0x40, 0x0c];
    buf.extend_from_slice(b"MX Master 2S");
    buf.extend_from_slice(&[0u8; 2]); // trailing bytes of the 16-byte register
    assert_eq!(parse_codename(&buf).as_deref(), Some("MX Master 2S"));
}

#[test]
fn codename_clamps_overlong_len() {
    // a bogus length byte must not over-read past the buffer.
    let buf = [0x40, 0xff, b'h', b'i'];
    assert_eq!(parse_codename(&buf).as_deref(), Some("hi"));
}

#[test]
fn codename_rejects_short_response() {
    assert_eq!(parse_codename(&[0x40]), None);
}
