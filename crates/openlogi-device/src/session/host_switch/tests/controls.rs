use super::*;

/// A reporting snapshot with unrelated bits deliberately set, so the
/// cleanup tests prove that only the bits they vary are restored.
fn noisy_reporting() -> CidReporting {
    CidReporting {
        cid: ControlId(0x00d3),
        diverted: false,
        persistently_diverted: true,
        force_raw_xy: true,
        raw_xy: false,
        remap: Some(ControlId(0x1234)),
        analytics_key_events: false,
        raw_wheel: true,
    }
}

pub(super) fn analytics_request(host: u8) -> HostSwitchRequest {
    let cid = 0x00d1 + u16::from(host);
    let controls = [ArmedControl {
        cid,
        host,
        mode: ReportingMode::Analytics,
        original: noisy_reporting(),
    }];
    let mut events = [AnalyticsKeyEvent::default(); 5];
    events[0] = AnalyticsKeyEvent {
        cid: ControlId(cid),
        event: 1,
    };
    host_control_request(&controls, ReprogControlsEvent::AnalyticsKeyEvents(events))
        .expect("analytics host press")
        .request
}

async fn scripted_reprog_controls(
    responder: crate::channel::scripted::Responder,
) -> (ReprogControlsV4, ScriptedRawHidHandle) {
    let (raw, handle) = ScriptedRawHidChannel::with_responder(responder);
    let channel = crate::channel::scripted::scripted_channel(raw).await;
    (ReprogControlsV4::new(channel, 1, 0x09), handle)
}

fn echo_reprog_reporting_write(request: &[u8]) -> Option<Vec<u8>> {
    (request.len() >= 20 && request[2] == 0x09 && request[3] >> 4 == 3).then(|| request.to_vec())
}

fn ignore_reprog_reporting_write(_request: &[u8]) -> Option<Vec<u8>> {
    None
}

#[test]
fn receiver_slots_share_one_channel() {
    let keyboard = DeviceRoute::Bolt {
        receiver_uid: "AABB".into(),
        slot: 1,
    };
    let mouse = DeviceRoute::Bolt {
        receiver_uid: "aabb".into(),
        slot: 2,
    };
    assert!(shares_channel(&keyboard, &mouse));
}

#[test]
fn direct_devices_do_not_share_channels() {
    let route = DeviceRoute::Direct {
        vendor_id: 0x046d,
        product_id: 0xb025,
    };
    assert!(!shares_channel(&route, &route));
}

#[test]
fn host_controls_are_recognized_by_task_when_cid_varies() {
    let info = CtrlIdInfo {
        cid: 0x1234,
        task_id: 0x00af,
        flags: 0,
    };
    assert_eq!(reprog_controls::host_switch_channel(info), Some(1));
}

#[test]
fn analytics_event_marks_a_native_keyboard_transition() {
    let controls = [ArmedControl {
        cid: 0x00d3,
        host: 2,
        mode: ReportingMode::Analytics,
        original: noisy_reporting(),
    }];
    let mut events = [AnalyticsKeyEvent::default(); 5];
    events[0] = AnalyticsKeyEvent {
        cid: ControlId(0x00d3),
        event: 1,
    };
    let captured = host_control_request(&controls, ReprogControlsEvent::AnalyticsKeyEvents(events))
        .expect("analytics host press");

    assert_eq!(captured.request.host, 2);
    assert_eq!(
        captured.request.keyboard_transition,
        KeyboardHostTransition::AnalyticsEvent {
            host_slot: ReportedHostSlot::Unknown,
        },
        "an undiverted analytics control may begin its native transition immediately"
    );
    assert_eq!(captured.cleanup, ControlCleanup::Prompt);
}

#[tokio::test]
async fn analytics_event_samples_pairing_at_the_event_boundary() {
    let channel = scripted_channel(keyboard_with_an_empty_third_slot).await;
    let capture = resolve_change_host_capture(&channel, 1)
        .await
        .expect("ChangeHost feature");
    let (controls, _handle) = scripted_reprog_controls(echo_reprog_reporting_write).await;
    let armed = ArmedControl {
        cid: 0x00d3,
        host: 2,
        mode: ReportingMode::Analytics,
        original: noisy_reporting(),
    };
    let mut events = [AnalyticsKeyEvent::default(); 5];
    events[0] = AnalyticsKeyEvent {
        cid: ControlId(armed.cid),
        event: 1,
    };
    let captured = host_control_request(&[armed], ReprogControlsEvent::AnalyticsKeyEvents(events))
        .expect("analytics host press");

    let request = finish_captured_control(captured, &controls, vec![armed], Some(&capture)).await;

    assert_eq!(
        request.keyboard_transition,
        KeyboardHostTransition::AnalyticsEvent {
            host_slot: ReportedHostSlot::Empty,
        }
    );
}

#[test]
fn diverted_event_requires_a_commanded_transition() {
    let controls = [ArmedControl {
        cid: 0x00d3,
        host: 2,
        mode: ReportingMode::Diverted,
        original: noisy_reporting(),
    }];
    let mut cids = [ControlId::default(); 4];
    cids[0] = ControlId(0x00d3);
    let captured = host_control_request(&controls, ReprogControlsEvent::DivertedButtons(cids))
        .expect("diverted host press");

    assert_eq!(captured.request.host, 2);
    assert_eq!(
        captured.request.keyboard_transition,
        KeyboardHostTransition::CommandRequired,
    );
    assert_eq!(captured.cleanup, ControlCleanup::Full);
}

#[tokio::test]
async fn prompt_analytics_cleanup_restores_an_answering_keyboard() {
    let (controls, handle) = scripted_reprog_controls(echo_reprog_reporting_write).await;
    let armed = ArmedControl {
        cid: 0x00d3,
        host: 2,
        mode: ReportingMode::Analytics,
        original: noisy_reporting(),
    };

    restore_host_controls_promptly(&controls, vec![armed]).await;

    let reports = handle.written_reports();
    let restore = reports
        .iter()
        .find(|report| report.len() >= 20 && report[2] == 0x09 && report[3] >> 4 == 3)
        .expect("analytics cleanup must write the original reporting state");
    assert_eq!(&restore[4..6], &armed.cid.to_be_bytes());
    assert_eq!(
        restore[9] & 0b11,
        0b10,
        "analytics reporting must be explicitly restored to its original false value"
    );
}

#[tokio::test]
async fn prompt_analytics_cleanup_does_not_wait_for_a_departing_keyboard() {
    let (controls, handle) = scripted_reprog_controls(ignore_reprog_reporting_write).await;
    let armed = ArmedControl {
        cid: 0x00d3,
        host: 2,
        mode: ReportingMode::Analytics,
        original: noisy_reporting(),
    };

    tokio::time::timeout(
        ANALYTICS_CLEANUP_TIMEOUT + Duration::from_millis(250),
        restore_host_controls_promptly(&controls, vec![armed]),
    )
    .await
    .expect("prompt cleanup exceeded its bounded latency");
    assert_eq!(
        handle.written_reports().len(),
        1,
        "prompt cleanup must not retry a keyboard that is already departing"
    );
}

#[test]
fn diverted_cleanup_restores_only_the_original_temporary_bits() {
    let change = restoration_change(ArmedControl {
        cid: 0x00d3,
        host: 2,
        mode: ReportingMode::Diverted,
        original: CidReporting {
            diverted: true,
            raw_xy: true,
            ..noisy_reporting()
        },
    });

    assert_eq!(change.diverted, Some(true));
    assert_eq!(change.raw_xy, Some(true));
    assert_eq!(change.analytics_key_events, None);
    assert_eq!(change.persistently_diverted, None);
    assert_eq!(change.remap, None);
}

#[test]
fn analytics_cleanup_restores_the_original_analytics_bit() {
    let change = restoration_change(ArmedControl {
        cid: 0x00d3,
        host: 2,
        mode: ReportingMode::Analytics,
        original: CidReporting {
            analytics_key_events: true,
            ..noisy_reporting()
        },
    });

    assert_eq!(change.analytics_key_events, Some(true));
    assert_eq!(change.diverted, None);
    assert_eq!(change.raw_xy, None);
}
