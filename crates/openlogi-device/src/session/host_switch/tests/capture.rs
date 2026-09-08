use super::*;

#[tokio::test]
async fn announcement_capture_resolves_the_current_runtime_feature_index() {
    let channel = scripted_channel(keyboard_without_hosts_info).await;
    let capture = resolve_change_host_capture(&channel, 1)
        .await
        .expect("ChangeHost feature");

    assert_eq!(capture.feature_index, CHANGE_HOST_INDEX);
    let request = capture
        .departure_request(2)
        .await
        .expect("missing HostsInfo must remain compatible");
    assert_eq!(
        request.keyboard_transition,
        KeyboardHostTransition::AlreadyDeparting {
            host_slot: ReportedHostSlot::Unknown,
        }
    );
}

#[tokio::test]
async fn announcement_capture_validates_pairing_status_at_the_event_boundary() {
    let channel = scripted_channel(keyboard_with_an_empty_third_slot).await;
    let capture = resolve_change_host_capture(&channel, 1)
        .await
        .expect("ChangeHost feature");

    assert_eq!(
        capture
            .departure_request(1)
            .await
            .expect("paired host should be reportable")
            .keyboard_transition,
        KeyboardHostTransition::AlreadyDeparting {
            host_slot: ReportedHostSlot::Paired,
        }
    );
    assert_eq!(
        capture
            .departure_request(2)
            .await
            .expect("empty host should be reportable")
            .keyboard_transition,
        KeyboardHostTransition::AlreadyDeparting {
            host_slot: ReportedHostSlot::Empty,
        }
    );
}

#[tokio::test]
async fn announcement_session_reads_pairing_changes_at_the_event_boundary() {
    let all_paired = Arc::new(AtomicBool::new(false));
    let responder_state = Arc::clone(&all_paired);
    let (raw, handle) = ScriptedRawHidChannel::with_dynamic_responder(move |request| {
        let status = if responder_state.load(Ordering::Relaxed) {
            SlotStatus::AllPaired
        } else {
            SlotStatus::Reported
        };
        scripted_keyboard(request, status)
    });
    let channel = crate::channel::scripted::scripted_channel(raw).await;
    let capture = resolve_change_host_capture(&channel, 1)
        .await
        .expect("ChangeHost feature");
    assert_eq!(
        capture.slot_reader.read_one(2).await,
        ReportedHostSlot::Empty
    );

    let (_stop_tx, stop_rx) = tokio::sync::oneshot::channel();
    let session = tokio::spawn(run_change_host_announcement_session(
        DeviceRoute::Direct {
            vendor_id: crate::LOGITECH_VENDOR_ID,
            product_id: 0xb369,
        },
        1,
        capture,
        stop_rx,
        Arc::clone(&channel),
        crate::device_io_channel().1,
    ));
    tokio::task::yield_now().await;
    all_paired.store(true, Ordering::Relaxed);
    let mut announcement = vec![0_u8; 20];
    announcement[0] = 0x11;
    announcement[1] = 1;
    announcement[2] = CHANGE_HOST_INDEX;
    announcement[5] = 2;
    handle.emit_report(announcement);

    let request = tokio::time::timeout(Duration::from_secs(1), session)
        .await
        .expect("announcement session should finish")
        .expect("announcement session should not panic")
        .expect("announcement session should remain valid")
        .expect("announcement should produce a host-switch request");
    assert_eq!(request.host, 2);
    assert_eq!(
        request.keyboard_transition,
        KeyboardHostTransition::AlreadyDeparting {
            host_slot: ReportedHostSlot::Paired,
        }
    );
}

#[tokio::test]
async fn announcement_slot_read_error_is_reported_as_unknown() {
    let reads_fail = Arc::new(AtomicBool::new(false));
    let responder_state = Arc::clone(&reads_fail);
    let (raw, _handle) = ScriptedRawHidChannel::with_dynamic_responder(move |request| {
        let status = if responder_state.load(Ordering::Relaxed) {
            SlotStatus::ReadErrors
        } else {
            SlotStatus::Reported
        };
        scripted_keyboard(request, status)
    });
    let channel = crate::channel::scripted::scripted_channel(raw).await;
    let capture = resolve_change_host_capture(&channel, 1)
        .await
        .expect("ChangeHost feature");
    assert_eq!(
        capture.slot_reader.read_one(2).await,
        ReportedHostSlot::Empty
    );

    reads_fail.store(true, Ordering::Relaxed);
    let request = capture
        .departure_request(2)
        .await
        .expect("ambiguous status should remain unverified");
    assert_eq!(
        request.keyboard_transition,
        KeyboardHostTransition::AlreadyDeparting {
            host_slot: ReportedHostSlot::Unknown,
        }
    );
}

#[tokio::test]
async fn announcement_lookup_error_is_reported_as_unknown() {
    let channel = scripted_channel(keyboard_erroring_on_hosts_info_lookup).await;
    let capture = resolve_change_host_capture(&channel, 1)
        .await
        .expect("ChangeHost feature");

    let request = capture
        .departure_request(2)
        .await
        .expect("indeterminate support should remain unverified");

    assert_eq!(
        request.keyboard_transition,
        KeyboardHostTransition::AlreadyDeparting {
            host_slot: ReportedHostSlot::Unknown,
        }
    );
}

#[tokio::test]
async fn announcement_validation_timeout_is_bounded_and_preserves_the_departure() {
    let (raw, handle) = ScriptedRawHidChannel::with_responder(keyboard_timing_out_on_slot_status);
    let channel = crate::channel::scripted::scripted_channel(raw).await;
    let capture = resolve_change_host_capture(&channel, 1)
        .await
        .expect("ChangeHost feature");
    let (_stop_tx, stop_rx) = tokio::sync::oneshot::channel();
    let session = tokio::spawn(run_change_host_announcement_session(
        DeviceRoute::Direct {
            vendor_id: crate::LOGITECH_VENDOR_ID,
            product_id: 0xb369,
        },
        1,
        capture,
        stop_rx,
        Arc::clone(&channel),
        crate::device_io_channel().1,
    ));
    tokio::task::yield_now().await;
    let mut announcement = vec![0_u8; 20];
    announcement[0] = 0x11;
    announcement[1] = 1;
    announcement[2] = CHANGE_HOST_INDEX;
    announcement[5] = 2;
    handle.emit_report(announcement);

    let request = tokio::time::timeout(Duration::from_millis(500), session)
        .await
        .expect("announcement validation exceeded its bounded latency")
        .expect("announcement session should not panic")
        .expect("announcement session should remain valid")
        .expect("the departure must survive when the keyboard leaves before replying");

    assert_eq!(
        request.keyboard_transition,
        KeyboardHostTransition::AlreadyDeparting {
            host_slot: ReportedHostSlot::Unknown,
        }
    );
}

#[tokio::test]
async fn announcement_validation_observes_a_pairing_removed_after_capture_started() {
    let all_paired = Arc::new(AtomicBool::new(true));
    let responder_state = Arc::clone(&all_paired);
    let (raw, _handle) = ScriptedRawHidChannel::with_dynamic_responder(move |request| {
        let status = if responder_state.load(Ordering::Relaxed) {
            SlotStatus::AllPaired
        } else {
            SlotStatus::Reported
        };
        scripted_keyboard(request, status)
    });
    let channel = crate::channel::scripted::scripted_channel(raw).await;
    let capture = resolve_change_host_capture(&channel, 1)
        .await
        .expect("ChangeHost feature");
    assert_eq!(
        capture.slot_reader.read_one(2).await,
        ReportedHostSlot::Paired
    );

    all_paired.store(false, Ordering::Relaxed);
    let request = capture
        .departure_request(2)
        .await
        .expect("empty status should be reported at the event boundary");

    assert_eq!(
        request.keyboard_transition,
        KeyboardHostTransition::AlreadyDeparting {
            host_slot: ReportedHostSlot::Empty,
        }
    );
}

#[test]
fn change_host_announcement_selects_the_reported_destination() {
    let mut payload = [0; 16];
    payload[0] = 0;
    payload[1] = 1;
    let announcement = v20::Message::Long(
        v20::MessageHeader {
            device_index: 1,
            feature_index: 9,
            function_id: U4::from_lo(0),
            software_id: U4::from_lo(0),
        },
        payload,
    );

    assert_eq!(change_host_announcement(&announcement, 1, 9, 3), Some(1));
}

#[test]
fn change_host_announcement_rejects_foreign_or_malformed_messages() {
    let announcement = |device_index, feature_index, function_id, software_id, target_host| {
        let mut payload = [0; 16];
        payload[1] = target_host;
        v20::Message::Long(
            v20::MessageHeader {
                device_index,
                feature_index,
                function_id: U4::from_lo(function_id),
                software_id: U4::from_lo(software_id),
            },
            payload,
        )
    };

    for message in [
        announcement(2, 9, 0, 0, 1),
        announcement(1, 8, 0, 0, 1),
        announcement(1, 9, 1, 0, 1),
        announcement(1, 9, 0, 1, 1),
        announcement(1, 9, 0, 0, 3),
    ] {
        assert_eq!(change_host_announcement(&message, 1, 9, 3), None);
    }
}
