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
    let payload = CidReportingChange::temporary_diversion(true, true).to_payload(ControlId(0x00c3));

    assert_eq!(&payload[0..=1], &0x00c3u16.to_be_bytes());
    assert_eq!(payload[2], 0x33);
    assert_eq!(payload[3], 0);
    assert_eq!(payload[4], 0);
    assert_eq!(payload[5], 0);
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
