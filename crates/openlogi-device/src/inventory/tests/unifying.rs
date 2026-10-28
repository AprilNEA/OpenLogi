//! The Unifying receiver: arrival broadcasts, the arrival trigger and its retries, and codename reads.

use super::*;

use openlogi_fixture::{
    CassetteExchange, FIXTURE_SCHEMA_VERSION, HidCassette, ReportSupport, RequestMatch,
};

use crate::replay::{
    ChannelConnection, NodePresence, OpenOutcome, RawWriterAvailability, ReplayBackend,
    ReplayChannel, ReplayNode, ReplayTopology,
};

// Synthetic protocol expectations, not a capture or a claim about c545 firmware.
fn c545_backend() -> Arc<ReplayBackend> {
    let mut info = scripted_node_info("c545-two-device-discovery");
    info.product_id = 0xc545;
    let mut identity = vec![0x11, 0xff, 0x83, 0xb5, 0x03, 0xa1, 0xb2, 0xc3, 0xd4, 0, 2];
    identity.resize(20, 0);
    let exchanges = [
        (
            vec![0x10, 0xff, 0x81, 0x02, 0, 0, 0],
            vec![0x10, 0xff, 0x81, 0x02, 0, 2, 0],
        ),
        (vec![0x10, 0xff, 0x83, 0xb5, 0x03, 0, 0], identity),
        (
            vec![0x10, 0xff, 0x80, 0x02, 2, 0, 0],
            vec![0x10, 0xff, 0x80, 0x02, 0, 0, 0],
        ),
        (
            vec![0x10, 0xff, 0x81, 0x00, 0, 0, 0],
            vec![0x10, 0xff, 0x81, 0x00, 0, 1, 0],
        ),
    ]
    .into_iter()
    .map(|(request, response)| CassetteExchange {
        request_match: RequestMatch::Exact,
        request,
        response: Some(response),
        required: true,
    })
    .collect();
    Arc::new(
        ReplayBackend::new(
            ReplayTopology {
                nodes: vec![ReplayNode {
                    info,
                    presence: NodePresence::Present,
                    open_outcome: OpenOutcome::Hidpp,
                    channel: Some("c545".to_string()),
                    raw_writer: RawWriterAvailability::Unavailable,
                    receiver_slots: vec![],
                }],
                channels: vec![ReplayChannel {
                    id: "c545".to_string(),
                    connection: ChannelConnection::Connected,
                    report_support: ReportSupport::ShortAndLong,
                }],
            },
            vec![HidCassette {
                schema_version: FIXTURE_SCHEMA_VERSION,
                name: "synthetic-c545-discovery".to_string(),
                channel: "c545".to_string(),
                report_support: ReportSupport::ShortAndLong,
                exchanges,
            }],
        )
        .expect("valid synthetic receiver"),
    )
}

#[tokio::test]
async fn c545_discovers_two_paired_devices_on_one_receiver() {
    let backend = c545_backend();
    let trigger = backend
        .hold_next_response(
            "c545",
            RequestMatch::Exact,
            &[0x10, 0xff, 0x80, 0x02, 2, 0, 0],
        )
        .expect("known receiver channel");
    let mut enumerator = Enumerator::with_backend(backend.clone());
    let discovery = async {
        tokio::join!(enumerator.enumerate(), async {
            trigger.request_written().await;
            // Offline mouse and keyboard: both must surface without feature probes.
            // Reverse order also checks stable slot ordering in the inventory.
            for report in [
                [0x10, 2, 0x41, 0x04, 0x61, 0x02, 0x40],
                [0x10, 1, 0x41, 0x04, 0x62, 0x01, 0x40],
            ] {
                assert_eq!(backend.emit_channel_report("c545", &report).unwrap(), 1);
            }
            trigger.release();
        })
    };
    let (inventories, ()) = tokio::time::timeout(Duration::from_secs(5), discovery)
        .await
        .expect("discovery must reach and complete the receiver path");
    let inventories = inventories.expect("synthetic discovery succeeds");
    assert_eq!(inventories.len(), 1);
    let inventory = &inventories[0];
    assert_eq!(inventory.receiver.product_id, 0xc545);
    assert_eq!(inventory.receiver.name, "Lightspeed Receiver");
    assert_eq!(inventory.receiver.unique_id.as_deref(), Some("A1B2C3D4"));
    assert_eq!(inventory.paired.len(), 2);
    for (device, (slot, wpid, kind)) in inventory.paired.iter().zip([
        (1, 0x4001, DeviceKind::Mouse),
        (2, 0x4002, DeviceKind::Keyboard),
    ]) {
        assert_eq!(
            (device.slot, device.wpid, device.kind),
            (slot, Some(wpid), kind)
        );
        assert!(!device.online);
        assert_eq!(
            DeviceRoute::for_slot(inventory, slot),
            Some(DeviceRoute::Unifying {
                receiver_uid: "A1B2C3D4".to_string(),
                slot,
            })
        );
    }
    backend
        .require_complete()
        .expect("all expected requests consumed, no extra probes");
}

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
        0,
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
