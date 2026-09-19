//! The Unifying receiver: arrival broadcasts, the arrival trigger and its retries, and codename reads.

use super::*;

#[tokio::test]
async fn offline_arrival_rebroadcasts_surface_without_probing_the_device() {
    // The exact wire bytes once misread as proof that the online bit is
    // stuck: `04 62 69 40` is an encrypted MX Master 2S (wpid 0x4069) slot
    // re-broadcast with bit 6 *set* — link not established, device offline.
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

    let (raw, handle) = ScriptedRawHidChannel::with_responder(|_| None);
    let channel = scripted_channel(raw).await;
    let writes_before = handle.written_reports().len();

    let cache = HashMap::new();
    let pass = PassContext {
        cache: &cache,
        now: Instant::now(),
        subscriptions: None,
        timeouts: &ProbeTimeouts::DEFAULT,
    };
    let (device, _) = probe_unifying_slot(&channel, &event, "SERIAL", pass)
        .await
        .expect("an offline slot still surfaces from its re-broadcast");

    assert!(!device.online);
    assert_eq!(device.wpid, Some(0x4069));
    assert_eq!(
        handle.written_reports().len(),
        writes_before,
        "an offline slot must not be probed for features, battery, or codename"
    );
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
    );
    assert!(device.online);
    assert_eq!(device.wpid, Some(0x40b8));
    assert_eq!(device.kind, DeviceKind::Mouse);
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
