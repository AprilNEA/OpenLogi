use super::*;
use crate::nibble::U4;

#[test]
fn parses_cid_info_flags_and_metadata() {
    let mut payload = [0; 16];
    payload[0..=1].copy_from_slice(&0x00c3u16.to_be_bytes());
    payload[2..=3].copy_from_slice(&0x009cu16.to_be_bytes());
    payload[4] = 0b1111_0001;
    payload[5] = 7;
    payload[6] = 2;
    payload[7] = 0b0000_0011;
    payload[8] = 0b0000_1111;

    let info = CidInfo::from_payload(payload);

    assert_eq!(info.cid, ControlId(0x00c3));
    assert_eq!(info.task_id, TaskId(0x009c));
    assert_eq!(info.position, 7);
    assert_eq!(info.group, 2);
    assert_eq!(info.group_mask, GroupMask(0b0000_0011));
    assert!(info.flags.is_mouse());
    assert!(info.flags.contains(CidFlags::REPROGRAMMABLE));
    assert!(info.flags.is_divertable());
    assert!(info.flags.is_persistently_divertable());
    assert!(info.flags.is_virtual_control());
    assert!(info.flags.supports_raw_xy());
    assert!(info.flags.supports_force_raw_xy());
    assert!(info.flags.supports_analytics_key_events());
    assert!(info.flags.supports_raw_wheel());
    assert_eq!(info.flags.raw(), 0x0ff1);
}

#[test]
fn builds_temporary_diversion_payload() {
    let payload = CidReportingChange {
        diverted: Some(true),
        raw_xy: Some(true),
        ..Default::default()
    }
    .to_payload(ControlId(0x00c3));

    assert_eq!(&payload[0..=1], &0x00c3u16.to_be_bytes());
    assert_eq!(payload[2], 0x33);
    assert_eq!(payload[3], 0);
    assert_eq!(payload[4], 0);
    assert_eq!(payload[5], 0);
}

/// `setCidReporting` reads remap `0` as "keep the previously configured remap",
/// so a remap is cleared by remapping the control to itself, never by `None`.
#[test]
fn remap_none_keeps_and_the_own_cid_clears() {
    let cid = ControlId(0x00c3);
    let keep = CidReportingChange::default().to_payload(cid);
    assert_eq!(&keep[3..=4], &[0, 0]);
    let clear = CidReportingChange {
        remap: Some(cid),
        ..Default::default()
    }
    .to_payload(cid);
    assert_eq!(&clear[3..=4], &0x00c3u16.to_be_bytes());
}

#[test]
fn parses_reporting_state() {
    let mut payload = [0; 16];
    payload[0..=1].copy_from_slice(&0x00c3u16.to_be_bytes());
    payload[2] = (1 << 0) | (1 << 2) | (1 << 4) | (1 << 6);
    payload[3..=4].copy_from_slice(&0x00c4u16.to_be_bytes());
    payload[5] = (1 << 0) | (1 << 2);

    let reporting = CidReporting::from_payload(payload);

    assert!(reporting.diverted);
    assert!(reporting.persistently_diverted);
    assert!(reporting.raw_xy);
    assert!(reporting.force_raw_xy);
    assert_eq!(reporting.remap, Some(ControlId(0x00c4)));
    assert!(reporting.analytics_key_events);
    assert!(reporting.raw_wheel);
}

#[test]
fn decodes_events() {
    let mut payload = [0; 16];
    payload[0..=1].copy_from_slice(&0x00c3u16.to_be_bytes());
    payload[2..=3].copy_from_slice(&0x00c4u16.to_be_bytes());
    assert_eq!(
        event::decode_event_payload(0, &payload),
        Some(ReprogControlsEvent::DivertedButtons([
            ControlId(0x00c3),
            ControlId(0x00c4),
            ControlId(0),
            ControlId(0),
        ]))
    );

    let mut payload = [0; 16];
    payload[0..=1].copy_from_slice(&(-5i16).to_be_bytes());
    payload[2..=3].copy_from_slice(&12i16.to_be_bytes());
    assert_eq!(
        event::decode_event_payload(1, &payload),
        Some(ReprogControlsEvent::DivertedRawMouseXy { dx: -5, dy: 12 })
    );

    let mut payload = [0; 16];
    payload[0] = 0b0001_0011;
    payload[1..=2].copy_from_slice(&123i16.to_be_bytes());
    assert_eq!(
        event::decode_event_payload(4, &payload),
        Some(ReprogControlsEvent::DivertedRawWheel {
            resolution: RawWheelResolution::High,
            periods: U4::from_lo(3),
            delta_vertical: 123,
        })
    );
}

/// Another client's `setCidReporting` reaches this channel only as a response
/// that matches none of its requests. The event stream publishes it as a
/// reporting change. This channel's own response is never published, matched
/// or late, and no other unmatched response is either.
#[test]
fn event_stream_publishes_only_another_clients_set_cid_reporting_response() {
    use std::sync::Arc;

    use crate::channel::mock::{MockRawHidChannel, channel_with_reader};
    use crate::feature::{CreatableFeature as _, EmittingFeature as _};
    use crate::protocol::v20::{Message, MessageHeader};

    let header = |function, software| MessageHeader {
        device_index: 2,
        feature_index: 7,
        function_id: U4::from_lo(function),
        software_id: U4::from_lo(software),
    };
    let [cid_hi, cid_lo] = 0x00c3u16.to_be_bytes();
    // `diverted` valid, value 0: hand the control back.
    let mut undivert = [0; 16];
    undivert[..3].copy_from_slice(&[cid_hi, cid_lo, 0b10]);
    let mut divert = undivert;
    divert[2] = 0b11;

    futures::executor::block_on(async {
        let (raw, handle) = MockRawHidChannel::new();
        // The default policy: every request of this channel carries id 1.
        let channel = Arc::new(channel_with_reader(raw).await);
        let feature = ReprogControlsFeature::new(Arc::clone(&channel), 2, 7);
        let events = feature.listen();

        // This channel's own write: the matched echo answers the caller only.
        handle.queue_response(Message::Long(header(3, 1), divert).into());
        let echo = feature
            .set_cid_reporting(
                ControlId(0x00c3),
                CidReportingChange {
                    diverted: Some(true),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        assert_eq!(echo.diverted, Some(true));

        // Unmatched, but not another client's change: a late response with
        // this channel's id, another client's read, and a short report that
        // is too short to be the echo.
        handle
            .send_incoming(Message::Long(header(3, 1), undivert).into())
            .await;
        handle
            .send_incoming(Message::Long(header(2, 9), undivert).into())
            .await;
        handle
            .send_incoming(Message::Short(header(3, 9), [cid_hi, cid_lo, 0b10]).into())
            .await;
        // Another client's `setCidReporting` response.
        handle
            .send_incoming(Message::Long(header(3, 9), undivert).into())
            .await;
        // The read thread delivers in order, so this broadcast arrives last.
        handle
            .send_incoming(Message::Long(header(0, 0), [0; 16]).into())
            .await;

        assert_eq!(
            events.recv().await.unwrap(),
            ReprogControlsEvent::ReportingChangedElsewhere(CidReportingChangeEcho {
                cid: ControlId(0x00c3),
                diverted: Some(false),
                persistently_diverted: None,
                force_raw_xy: None,
                raw_xy: None,
                remap: None,
                analytics_key_events: None,
                raw_wheel: None,
            })
        );
        assert_eq!(
            events.recv().await.unwrap(),
            ReprogControlsEvent::DivertedButtons([ControlId(0); 4])
        );
        assert!(events.is_empty());
    });
}
