//! The capture session lifecycle: arming, restore through a replacement channel, rollback, and teardown order.

use super::*;
use crate::channel::registry::PublicationState;
use crate::channel::scripted::feature_error;

#[tokio::test]
async fn alternate_gesture_control_arming_respects_mode_and_capabilities() {
    for cid in [0x00d0u16, 0x00c3, 0x00d7] {
        for flags in [0x0120u16, 0x0020, 0x0100, 0] {
            for gestures in [false, true] {
                let (raw, handle) = ScriptedRawHidChannel::with_dynamic_responder(move |request| {
                    let mut response = vec![0; 20];
                    response[..4].copy_from_slice(&request[..4]);
                    response[0] = 0x11;
                    match (request[2], request[3] >> 4) {
                        (0, 1) => response[4] = 4,
                        (0, 0) => response[4] = 2,
                        (2, 0) => response[4] = 1,
                        (2, 1) => {
                            response[4..6].copy_from_slice(&cid.to_be_bytes());
                            let [low, high] = flags.to_le_bytes();
                            response[8] = low;
                            response[12] = high;
                        }
                        (2, 2) => response[4..6].copy_from_slice(&cid.to_be_bytes()),
                        (2, 3) => return Some(request.to_vec()),
                        _ => panic!("unexpected capture request: {request:02x?}"),
                    }
                    Some(response)
                });
                let channel = scripted_channel(raw).await;
                let device = Device::new(channel.clone(), 0xff).await.unwrap();
                let sources: Vec<_> = GESTURE_SOURCE_BUTTONS
                    .into_iter()
                    .filter(|(_, button)| *button == ButtonId::GestureButton)
                    .collect();
                let spec = if gestures {
                    CaptureSpec {
                        divert_gesture_sources: sources.iter().map(|&(cid, _)| cid).collect(),
                        ..CaptureSpec::default()
                    }
                } else {
                    CaptureSpec {
                        divert_buttons: sources,
                        ..CaptureSpec::default()
                    }
                };
                let mut armed = ArmedControls::default();
                arm_controls_into(&device, &channel, 0xff, &spec, &mut armed)
                    .await
                    .unwrap();
                // Gesture mode diverts with raw XY where the control offers
                // it, and plainly (click only) where it does not.
                let expected = cid != 0x00d7 && flags & 0x0020 != 0;
                let raw_xy = gestures && flags & 0x0100 != 0;
                let writes: Vec<_> = handle
                    .written_reports()
                    .into_iter()
                    .filter(|report| report[2] == 2 && report[3] >> 4 == 3)
                    .map(|report| report[4..7].to_vec())
                    .collect();
                let [hi, lo] = cid.to_be_bytes();
                assert_eq!(
                    writes,
                    if expected {
                        vec![vec![hi, lo, if raw_xy { 0x33 } else { 0x23 }]]
                    } else {
                        vec![]
                    },
                    "CID {cid:04x}, flags {flags:04x}, gestures {gestures}"
                );
                assert_eq!(
                    armed.gesture_cids,
                    if expected && gestures {
                        vec![cid]
                    } else {
                        vec![]
                    }
                );
                assert_eq!(
                    armed.button_cids,
                    if expected && !gestures {
                        vec![(cid, ButtonId::GestureButton)]
                    } else {
                        vec![]
                    }
                );
            }
        }
    }
}

#[tokio::test]
async fn dpi_gesture_arming_never_overwrites_raw_xy_with_plain_diversion() {
    for cid in [0x00c4u16, 0x00ed, 0x00fd] {
        for (gestures, raw_xy, expected_flags) in [
            (true, true, vec![0x33]),
            (true, false, vec![]),
            (false, true, vec![0x23]),
            (false, false, vec![0x23]),
        ] {
            let (raw, handle) = ScriptedRawHidChannel::with_dynamic_responder(move |request| {
                let mut response = vec![0; 20];
                response[..4].copy_from_slice(&request[..4]);
                response[0] = 0x11;
                match (request[2], request[3] >> 4) {
                    (0, 1) => response[4] = 4,
                    (0, 0) => response[4] = 2,
                    (2, 0) => response[4] = 1,
                    (2, 1) => {
                        response[4..6].copy_from_slice(&cid.to_be_bytes());
                        response[8] = 0x20;
                        response[12] = u8::from(raw_xy);
                    }
                    (2, 2) => response[4..6].copy_from_slice(&cid.to_be_bytes()),
                    (2, 3) => return Some(request.to_vec()),
                    _ => panic!("unexpected capture request: {request:02x?}"),
                }
                Some(response)
            });
            let channel = scripted_channel(raw).await;
            let device = Device::new(channel.clone(), 0xff).await.unwrap();
            let spec = CaptureSpec {
                divert_gesture_buttons: if gestures {
                    vec![(cid, ButtonId::DpiToggle)]
                } else {
                    vec![]
                },
                ..CaptureSpec::default()
            };
            let mut armed = ArmedControls::default();
            arm_controls_into(&device, &channel, 0xff, &spec, &mut armed)
                .await
                .unwrap();
            let writes: Vec<_> = handle
                .written_reports()
                .into_iter()
                .filter(|report| report[2] == 2 && report[3] >> 4 == 3)
                .map(|report| report[6])
                .collect();
            assert_eq!(
                writes, expected_flags,
                "CID {cid:04x}, gestures {gestures}, raw XY {raw_xy}"
            );
            assert_eq!(
                armed.gesture_button_cids,
                if gestures && raw_xy {
                    vec![(cid, ButtonId::DpiToggle)]
                } else {
                    vec![]
                }
            );
            assert_eq!(armed.dpi_cids, if gestures { vec![] } else { vec![cid] });
        }
    }
}

/// The M720 Triathlon's thumb button reports
/// [`reprog_controls::MULTIPLATFORM_GESTURE_BUTTON_CID`] rather than the MX
/// gesture CID, so arming must find it in the device's own control table:
/// raw-XY diversion in gesture mode, plain diversion for a single binding, and
/// plain diversion as a click-only gesture source when a gesture was asked of
/// a control without raw XY.
#[tokio::test]
async fn m720_gesture_button_arms_from_its_reported_control_table() {
    let cid = reprog_controls::MULTIPLATFORM_GESTURE_BUTTON_CID;
    for (gestures, raw_xy, expected_flags) in [
        (true, true, vec![0x33]),
        (true, false, vec![0x23]),
        (false, true, vec![0x23]),
        (false, false, vec![0x23]),
    ] {
        let (raw, handle) = ScriptedRawHidChannel::with_dynamic_responder(move |request| {
            let mut response = vec![0; 20];
            response[..4].copy_from_slice(&request[..4]);
            response[0] = 0x11;
            match (request[2], request[3] >> 4) {
                (0, 1) => response[4] = 4,
                (0, 0) => response[4] = 2,
                (2, 0) => response[4] = 1,
                (2, 1) => {
                    response[4..6].copy_from_slice(&cid.to_be_bytes());
                    // The M720's `getCidInfo` task for this control.
                    response[6..8].copy_from_slice(&0x00adu16.to_be_bytes());
                    response[8] = 0x20;
                    response[12] = u8::from(raw_xy);
                }
                (2, 2) => response[4..6].copy_from_slice(&cid.to_be_bytes()),
                (2, 3) => return Some(request.to_vec()),
                _ => panic!("unexpected capture request: {request:02x?}"),
            }
            Some(response)
        });
        let channel = scripted_channel(raw).await;
        let device = Device::new(channel.clone(), 0xff).await.unwrap();
        let spec = if gestures {
            CaptureSpec {
                divert_gesture_sources: vec![cid],
                ..CaptureSpec::default()
            }
        } else {
            CaptureSpec {
                divert_buttons: vec![(cid, ButtonId::GestureButton)],
                ..CaptureSpec::default()
            }
        };
        let mut armed = ArmedControls::default();
        arm_controls_into(&device, &channel, 0xff, &spec, &mut armed)
            .await
            .unwrap();
        let writes: Vec<_> = handle
            .written_reports()
            .into_iter()
            .filter(|report| report[2] == 2 && report[3] >> 4 == 3)
            .map(|report| {
                assert_eq!(u16::from_be_bytes([report[4], report[5]]), cid);
                report[6]
            })
            .collect();
        assert_eq!(
            writes, expected_flags,
            "gestures {gestures}, raw XY {raw_xy}"
        );
        assert_eq!(
            armed.gesture_cids,
            if gestures { vec![cid] } else { vec![] }
        );
        assert_eq!(
            armed.button_cids,
            if gestures {
                vec![]
            } else {
                vec![(cid, ButtonId::GestureButton)]
            }
        );
    }
}

/// The first-generation MX Master's gesture button is divertable but reports
/// no raw XY (`mse, reprogrammable, divertable`). Left native in gesture
/// mode, its firmware ran its own action in place of the configured click
/// (#1311, #2167). Arming diverts it plainly as a gesture source, so a press
/// and release reach the map's `Click`, with the button edges around it.
#[tokio::test]
async fn a_gesture_source_without_raw_xy_still_delivers_its_click() {
    let cid = reprog_controls::GESTURE_BUTTON_CID;
    let (raw, handle) = ScriptedRawHidChannel::with_dynamic_responder(move |request| {
        let mut response = vec![0; 20];
        response[..4].copy_from_slice(&request[..4]);
        response[0] = 0x11;
        match (request[2], request[3] >> 4) {
            (0, 1) => response[4] = 4,
            (0, 0) => response[4] = 2,
            (2, 0) => response[4] = 1,
            (2, 1) => {
                response[4..6].copy_from_slice(&cid.to_be_bytes());
                // mse | reprogrammable | divertable; no raw-XY bit.
                response[8] = 0x31;
                response[12] = 0;
            }
            (2, 2) => response[4..6].copy_from_slice(&cid.to_be_bytes()),
            (2, 3) => return Some(request.to_vec()),
            _ => panic!("unexpected capture request: {request:02x?}"),
        }
        Some(response)
    });
    let channel = scripted_channel(raw).await;
    let device = Device::new(channel.clone(), 0xff).await.unwrap();
    let spec = CaptureSpec {
        divert_gesture_sources: vec![cid],
        ..CaptureSpec::default()
    };
    let mut armed = ArmedControls::default();
    arm_controls_into(&device, &channel, 0xff, &spec, &mut armed)
        .await
        .unwrap();
    let writes: Vec<_> = handle
        .written_reports()
        .into_iter()
        .filter(|report| report[2] == 2 && report[3] >> 4 == 3)
        .map(|report| report[4..7].to_vec())
        .collect();
    let [hi, lo] = cid.to_be_bytes();
    assert_eq!(writes, vec![vec![hi, lo, 0x23]], "plain divert, raw XY off");
    assert_eq!(armed.gesture_cids, vec![cid]);
    assert!(armed.button_cids.is_empty());

    let (tx, mut rx) = mpsc::unbounded_channel();
    let mut acc = CaptureAccum::default();
    for event in [
        RawControlEvent::DivertedButtons([cid, 0, 0, 0]),
        RawControlEvent::DivertedButtons([0; 4]),
    ] {
        handle_reprog(
            &mut acc,
            event,
            &armed.gesture_cids,
            &armed.dpi_cids,
            &armed.button_cids,
            &tx,
        );
    }
    assert_eq!(
        rx.try_recv(),
        Ok(CapturedInput::ButtonDown(ButtonId::GestureButton))
    );
    assert_eq!(
        rx.try_recv(),
        Ok(CapturedInput::Gesture(
            ButtonId::GestureButton,
            GestureDirection::Click
        ))
    );
    assert_eq!(
        rx.try_recv(),
        Ok(CapturedInput::ButtonUp(ButtonId::GestureButton))
    );
    assert!(rx.try_recv().is_err(), "one press is one click");
}

/// The buttons a session reports as diverted are the ones arming wrote, not
/// the ones the plan asked for: a Forward the device cannot divert still
/// reaches the OS, so the OS hook must keep handling it (#2158).
#[tokio::test]
async fn diverted_buttons_are_what_arming_wrote_not_what_was_requested() {
    const BACK_CID: u16 = 0x0053;
    const FORWARD_CID: u16 = 0x0056;
    let (raw, _handle) = ScriptedRawHidChannel::with_dynamic_responder(|request| {
        let mut response = vec![0; 20];
        response[..4].copy_from_slice(&request[..4]);
        response[0] = 0x11;
        match (request[2], request[3] >> 4) {
            (0, 1) => response[4] = 4,
            // Every feature sits at index 2, and `0x1b04` lists two controls.
            (0 | 2, 0) => response[4] = 2,
            (2, 1) => {
                // Index 0 is a divertable Back, index 1 a Forward that is not.
                let (cid, flags) = if request[4] == 0 {
                    (BACK_CID, 0x20)
                } else {
                    (FORWARD_CID, 0x00)
                };
                response[4..6].copy_from_slice(&cid.to_be_bytes());
                response[8] = flags;
            }
            (2, 2) => response[4..6].copy_from_slice(&request[4..6]),
            (2, 3) => return Some(request.to_vec()),
            _ => panic!("unexpected capture request: {request:02x?}"),
        }
        Some(response)
    });
    let channel = scripted_channel(raw).await;
    let device = Device::new(channel.clone(), 0xff).await.unwrap();
    let spec = CaptureSpec {
        divert_buttons: vec![(BACK_CID, ButtonId::Back), (FORWARD_CID, ButtonId::Forward)],
        ..CaptureSpec::default()
    };
    let mut armed = ArmedControls::default();
    arm_controls_into(&device, &channel, 0xff, &spec, &mut armed)
        .await
        .unwrap();
    assert_eq!(
        armed.diverted_buttons(),
        std::collections::BTreeSet::from([ButtonId::Back])
    );
}

#[test]
fn diverted_buttons_cover_every_button_diversion_once() {
    let mut armed = ArmedControls::default();
    armed.button_cids.push((0x0053, ButtonId::Back));
    armed.wheel_button_cids.push((0x0056, ButtonId::Forward));
    // A second Back CID on the same mouse is still one button.
    armed.gesture_button_cids.push((0x00ba, ButtonId::Back));
    // DPI/ModeShift plain diversion is not a button the OS ever sees.
    armed.dpi_cids.push(0x00c4);
    assert_eq!(
        armed.diverted_buttons(),
        std::collections::BTreeSet::from([ButtonId::Back, ButtonId::Forward])
    );
}

#[tokio::test]
async fn pending_restore_waits_for_a_replacement_then_undiverts_through_it() {
    let route = DeviceRoute::Direct {
        vendor_id: 0x046d,
        product_id: 0xb35b,
    };
    let (retired_raw, retired_handle) = ScriptedRawHidChannel::with_responder(|_| None);
    let retired_channel = scripted_channel(retired_raw).await;
    let retired = SharedChannel::new(retired_channel.clone(), route.clone());
    let registry = ChannelRegistry::default();
    let node = NodeId::from("mouse-node".to_owned());
    registry.replace_node(node.clone(), [route.clone()], retired_channel);
    let pending = PendingCaptureRestore::new(
        &retired,
        ReprogRestore::new(
            0x22,
            vec![ArmedReporting::pointer(
                reprog_controls::GESTURE_BUTTON_CID,
                reporting(false, None),
                PointerDivert::RawXy,
            )],
        ),
        None,
        None,
        None,
    )
    .expect("one diverted control should require restoration");

    let pending = match pending.retry(&registry).await {
        CaptureSessionOutcome::RestorePending(pending) => pending,
        CaptureSessionOutcome::Restored => {
            panic!("the retired transport must never restore underneath a replacement")
        }
    };
    assert!(retired_handle.written_reports().is_empty());

    let (replacement_raw, replacement_handle) =
        ScriptedRawHidChannel::with_responder(|request| Some(request.to_vec()));
    let replacement = scripted_channel(replacement_raw).await;
    registry.replace_node(node, [route.clone()], replacement);

    assert!(matches!(
        pending.retry(&registry).await,
        CaptureSessionOutcome::Restored
    ));
    let reports = replacement_handle.written_reports();
    assert_eq!(reports.len(), 1);
    let restore = &reports[0];
    assert_eq!(restore[2], 0x22, "restore must address ReprogControlsV4");
    assert_eq!(restore[3] >> 4, 3, "restore must call setCidReporting");
    assert_eq!(
        &restore[4..7],
        &[0x00, 0xc3, 0x22],
        "restore must clear diversion and raw-XY using their valid bits"
    );
}

fn diverted_gesture(
    route: &DeviceRoute,
    channel: &std::sync::Arc<hidpp::channel::HidppChannel>,
) -> PendingCaptureRestore {
    let retired = SharedChannel::new(std::sync::Arc::clone(channel), route.clone());
    PendingCaptureRestore::new(
        &retired,
        ReprogRestore::new(
            0x22,
            vec![ArmedReporting::pointer(
                reprog_controls::GESTURE_BUTTON_CID,
                reporting(false, None),
                PointerDivert::RawXy,
            )],
        ),
        None,
        None,
        None,
    )
    .expect("one diverted control should require restoration")
}

#[tokio::test]
async fn pending_restore_follows_receiver_and_direct_transport_handoffs() {
    let bolt = DeviceRoute::Bolt {
        receiver_uid: "receiver-a".to_owned(),
        slot: 4,
    };
    let other_bolt = DeviceRoute::Bolt {
        receiver_uid: "receiver-b".to_owned(),
        slot: 2,
    };
    let direct = DeviceRoute::Direct {
        vendor_id: 0x046d,
        product_id: 0xb042,
    };
    for (retired_route, successor_route) in [
        (bolt.clone(), other_bolt),
        (bolt.clone(), direct.clone()),
        (direct, bolt),
    ] {
        let (retired_raw, retired_handle) = ScriptedRawHidChannel::with_responder(|_| None);
        let retired_channel = scripted_channel(retired_raw).await;
        let pending = diverted_gesture(&retired_route, &retired_channel);
        let registry = ChannelRegistry::default();
        registry.replace_node(
            NodeId::from("receiver-a-node".to_owned()),
            [retired_route],
            retired_channel,
        );
        let (successor_raw, successor_handle) =
            ScriptedRawHidChannel::with_responder(|request| Some(request.to_vec()));
        registry.replace_node(
            NodeId::from("receiver-b-node".to_owned()),
            [successor_route.clone()],
            scripted_channel(successor_raw).await,
        );

        assert!(matches!(
            restore_after_stop(
                CaptureStop::Handoff(successor_route),
                Some(pending),
                &registry,
            )
            .await,
            CaptureSessionOutcome::Restored
        ));
        assert!(
            retired_handle.written_reports().is_empty(),
            "handoff must not time out against the receiver the device left"
        );
        assert_eq!(
            successor_handle.written_reports().len(),
            1,
            "native reporting must be restored before capture arms on the successor route"
        );
    }
}

#[tokio::test]
async fn pending_restore_remembers_a_successor_route_that_appears_later() {
    let retired_route = DeviceRoute::Bolt {
        receiver_uid: "receiver-a".to_owned(),
        slot: 4,
    };
    let successor_route = DeviceRoute::Bolt {
        receiver_uid: "receiver-b".to_owned(),
        slot: 2,
    };
    let (retired_raw, _) = ScriptedRawHidChannel::with_responder(|_| None);
    let retired_channel = scripted_channel(retired_raw).await;
    let pending = diverted_gesture(&retired_route, &retired_channel);
    let registry = ChannelRegistry::default();

    let pending = match pending.retry_via(successor_route.clone(), &registry).await {
        CaptureSessionOutcome::RestorePending(pending) => pending,
        CaptureSessionOutcome::Restored => {
            panic!("handoff must remain pending until the successor route is published")
        }
    };
    let (successor_raw, successor_handle) =
        ScriptedRawHidChannel::with_responder(|request| Some(request.to_vec()));
    registry.replace_node(
        NodeId::from("receiver-b-node".to_owned()),
        [successor_route],
        scripted_channel(successor_raw).await,
    );

    assert!(matches!(
        pending.retry(&registry).await,
        CaptureSessionOutcome::Restored
    ));
    assert_eq!(
        successor_handle.written_reports().len(),
        1,
        "later retries must keep following the elected successor route"
    );
}

#[tokio::test]
async fn pending_handoff_can_follow_the_mouse_back_to_its_original_route() {
    let retired_route = DeviceRoute::Bolt {
        receiver_uid: "receiver-a".to_owned(),
        slot: 4,
    };
    let absent_successor_route = DeviceRoute::Bolt {
        receiver_uid: "receiver-b".to_owned(),
        slot: 2,
    };
    let (retired_raw, retired_handle) =
        ScriptedRawHidChannel::with_responder(|request| Some(request.to_vec()));
    let retired_channel = scripted_channel(retired_raw).await;
    let pending = diverted_gesture(&retired_route, &retired_channel);
    let registry = ChannelRegistry::default();
    registry.replace_node(
        NodeId::from("receiver-a-node".to_owned()),
        [retired_route.clone()],
        retired_channel,
    );

    let pending = match pending.retry_via(absent_successor_route, &registry).await {
        CaptureSessionOutcome::RestorePending(pending) => pending,
        CaptureSessionOutcome::Restored => {
            panic!("handoff must remain pending while the elected route is absent")
        }
    };
    assert!(matches!(
        pending.retry_via(retired_route, &registry).await,
        CaptureSessionOutcome::Restored
    ));
    assert_eq!(
        retired_handle.written_reports().len(),
        1,
        "a rapid route switch back must restore through the latest live route"
    );
}

#[tokio::test]
async fn pending_handoff_replaces_a_stale_route_with_the_latest_publication() {
    let origin = DeviceRoute::Bolt {
        receiver_uid: "receiver-a".to_owned(),
        slot: 4,
    };
    let stale = DeviceRoute::Bolt {
        receiver_uid: "receiver-b".to_owned(),
        slot: 2,
    };
    let latest = DeviceRoute::Bolt {
        receiver_uid: "receiver-c".to_owned(),
        slot: 1,
    };
    let (origin_raw, origin_handle) = ScriptedRawHidChannel::with_responder(|_| None);
    let origin_channel = scripted_channel(origin_raw).await;
    let pending = diverted_gesture(&origin, &origin_channel);
    let registry = ChannelRegistry::default();

    let pending = match pending.retry_via(stale, &registry).await {
        CaptureSessionOutcome::RestorePending(pending) => pending,
        CaptureSessionOutcome::Restored => panic!("the intermediate route is not published"),
    };
    let (latest_raw, latest_handle) =
        ScriptedRawHidChannel::with_responder(|request| Some(request.to_vec()));
    registry.replace_node(
        NodeId::from("receiver-c-node".to_owned()),
        [latest.clone()],
        scripted_channel(latest_raw).await,
    );

    assert!(matches!(
        pending.retry_via(latest, &registry).await,
        CaptureSessionOutcome::Restored
    ));
    assert!(origin_handle.written_reports().is_empty());
    assert_eq!(
        latest_handle.written_reports().len(),
        1,
        "a later publication must replace the route stored by the first handoff"
    );
}

#[tokio::test]
async fn restore_retries_when_inventory_changes_during_an_awaited_write() {
    let route = DeviceRoute::Direct {
        vendor_id: 0x046d,
        product_id: 0xb35b,
    };
    let registry = ChannelRegistry::default();
    let node = NodeId::from("mouse-node".to_owned());
    let (retired_raw, _) = ScriptedRawHidChannel::with_responder(|_| None);
    let retired_channel = scripted_channel(retired_raw).await;
    let retired = SharedChannel::new(retired_channel, route.clone());
    let pending = PendingCaptureRestore::new(
        &retired,
        ReprogRestore::new(
            0x22,
            vec![ArmedReporting::pointer(
                reprog_controls::GESTURE_BUTTON_CID,
                reporting(false, None),
                PointerDivert::RawXy,
            )],
        ),
        None,
        None,
        None,
    )
    .expect("one diverted control should require restoration");

    let (winner_raw, winner_handle) =
        ScriptedRawHidChannel::with_responder(|request| Some(request.to_vec()));
    let winner = scripted_channel(winner_raw).await;
    let replacement_registry = registry.clone();
    let replacement_node = node.clone();
    let replacement_route = route.clone();
    let replacement_winner = winner.clone();
    let (superseded_raw, superseded_handle) =
        ScriptedRawHidChannel::with_dynamic_responder(move |request| {
            replacement_registry.replace_node(
                replacement_node.clone(),
                [replacement_route.clone()],
                replacement_winner.clone(),
            );
            Some(request.to_vec())
        });
    let superseded = scripted_channel(superseded_raw).await;
    registry.replace_node(node, [route], superseded);

    let pending = match pending.retry(&registry).await {
        CaptureSessionOutcome::RestorePending(pending) => pending,
        CaptureSessionOutcome::Restored => {
            panic!("a write to a publication replaced during await is not final")
        }
    };
    assert_eq!(superseded_handle.written_reports().len(), 1);
    assert!(matches!(
        pending.retry(&registry).await,
        CaptureSessionOutcome::Restored
    ));
    assert_eq!(winner_handle.written_reports().len(), 1);
}

#[tokio::test]
async fn failed_setup_rollback_returns_its_restore_capability() {
    let route = DeviceRoute::Direct {
        vendor_id: 0x046d,
        product_id: 0xb35b,
    };
    let (raw, handle) =
        ScriptedRawHidChannel::with_failing_writes(|request| Some(request.to_vec()), |_| true);
    let channel = scripted_channel(raw).await;
    let registry = ChannelRegistry::default();
    registry.replace_node(
        NodeId::from("mouse-node".to_owned()),
        [route.clone()],
        channel.clone(),
    );
    let shared = SharedChannel::new(channel, route.clone());
    let pending = PendingCaptureRestore::new(
        &shared,
        ReprogRestore::new(
            0x22,
            vec![ArmedReporting::pointer(
                reprog_controls::GESTURE_BUTTON_CID,
                reporting(false, None),
                PointerDivert::RawXy,
            )],
        ),
        None,
        None,
        None,
    );

    let failure = rollback_start(
        CaptureError::Hidpp("diversion failed".into()),
        pending,
        &registry,
    )
    .await;
    let (_, pending) = failure.into_parts();

    assert!(
        !handle.written_reports().is_empty(),
        "rollback must attempt the compensating write on the current publication"
    );
    assert!(
        pending.is_some(),
        "a failed compensating write must not discard firmware ownership"
    );
}

#[tokio::test]
async fn an_offline_receiver_slot_restores_on_return_to_the_same_usb_channel() {
    let route = DeviceRoute::Bolt {
        receiver_uid: "receiver-a".into(),
        slot: 2,
    };
    let sibling = DeviceRoute::Bolt {
        receiver_uid: "receiver-a".into(),
        slot: 3,
    };
    let (raw, handle) =
        ScriptedRawHidChannel::with_dynamic_responder(|request| Some(request.to_vec()));
    let channel = scripted_channel(raw).await;
    let registry = ChannelRegistry::default();
    let node = NodeId::from("receiver-node".to_owned());
    registry.replace_node(
        node.clone(),
        [route.clone(), sibling.clone()],
        channel.clone(),
    );
    let shared = SharedChannel::new(channel.clone(), route.clone());
    let pending = PendingCaptureRestore::new(
        &shared,
        ReprogRestore::new(
            0x22,
            vec![ArmedReporting::pointer(
                reprog_controls::GESTURE_BUTTON_CID,
                reporting(false, None),
                PointerDivert::RawXy,
            )],
        ),
        None,
        None,
        None,
    );
    registry.replace_node(node.clone(), [sibling.clone()], channel.clone());
    let before = handle.written_reports().len();
    let pending = match restore_after_stop(
        stop_for_current_publication(&registry, &shared),
        pending,
        &registry,
    )
    .await
    {
        CaptureSessionOutcome::RestorePending(pending) => pending,
        CaptureSessionOutcome::Restored => {
            panic!("the inactive slot still owes native restoration")
        }
    };
    assert_eq!(
        handle.written_reports().len(),
        before,
        "no restoration writes on an inactive slot"
    );
    registry.replace_node(node.clone(), [], channel.clone());
    assert_eq!(
        registry.publication(&shared),
        PublicationState::SlotOffline,
        "an all-offline receiver still owns its USB channel"
    );
    let pending = match pending.retry(&registry).await {
        CaptureSessionOutcome::RestorePending(pending) => pending,
        CaptureSessionOutcome::Restored => panic!("an offline slot cannot finish its restore"),
    };
    assert_eq!(handle.written_reports().len(), before);
    registry.replace_node(node, [route, sibling], channel);
    assert!(
        matches!(
            pending.retry(&registry).await,
            CaptureSessionOutcome::Restored
        ),
        "the receiver never unplugged; returning to its original USB channel must release the old debt"
    );
    assert_eq!(handle.written_reports().len(), before + 1);
}

#[tokio::test]
async fn capture_listener_outlives_native_reporting_restore() {
    struct DropProbe(Arc<std::sync::atomic::AtomicBool>);

    impl Drop for DropProbe {
        fn drop(&mut self) {
            self.0.store(true, std::sync::atomic::Ordering::Relaxed);
        }
    }

    let dropped = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let (restored_tx, restored_rx) = oneshot::channel();
    let task = tokio::spawn(drop_listener_after(
        DropProbe(Arc::clone(&dropped)),
        async move {
            let _ = restored_rx.await;
        },
    ));

    tokio::task::yield_now().await;
    assert!(
        !dropped.load(std::sync::atomic::Ordering::Relaxed),
        "the listener must remain installed while native reporting is still diverted"
    );

    restored_tx.send(()).expect("restore signal should be open");
    task.await.expect("listener-retirement task should finish");
    assert!(
        dropped.load(std::sync::atomic::Ordering::Relaxed),
        "the listener may be removed after native reporting is restored"
    );
}

#[tokio::test(start_paused = true)]
async fn channel_change_takes_teardown_precedence_over_ready_shutdown() {
    let route = DeviceRoute::Direct {
        vendor_id: 0x046d,
        product_id: 0xb35b,
    };
    let registry = ChannelRegistry::default();
    let node = NodeId::from("mouse-node".to_owned());
    let (retired_raw, _) = ScriptedRawHidChannel::with_responder(|_| None);
    let retired_channel = scripted_channel(retired_raw).await;
    registry.replace_node(node.clone(), [route.clone()], retired_channel);
    let retired = registry
        .lookup(&route)
        .expect("the capture publication should be current");

    let (replacement_raw, _) = ScriptedRawHidChannel::with_responder(|_| None);
    let replacement = scripted_channel(replacement_raw).await;
    registry.replace_node(node, [route], replacement);

    // These are the two monitor branches that can become ready together. The
    // explicit shutdown branch re-checks the publication, so both preserve the
    // typed replacement teardown rather than restoring through `retired`.
    assert!(matches!(
        stop_for_current_publication(&registry, &retired),
        CaptureStop::ChannelChanged
    ));
    assert!(matches!(
        wait_for_channel_change(&registry, &retired).await,
        CaptureStop::ChannelChanged
    ));
}

/// A mouse with only `0x2150` (index 3) whose `getThumbwheelStatus` reports
/// `status`; `setThumbwheelReporting` writes are echoed.
/// A liveness restart reports its channel before it stops. Inventory retires
/// a reported channel on its next pass, so the restore waits for the
/// replacement instead of timing out against the silent channel (#2020).
#[tokio::test]
async fn a_dead_channel_stop_reports_the_channel_and_restores_on_the_replacement() {
    let route = DeviceRoute::Direct {
        vendor_id: 0x046d,
        product_id: 0xb35b,
    };
    let registry = ChannelRegistry::default();
    let node = NodeId::from("mouse-node".to_owned());
    let (raw, handle) = ScriptedRawHidChannel::with_responder(|_| None);
    let channel = scripted_channel(raw).await;
    let pending = diverted_gesture(&route, &channel);
    registry.replace_node(node.clone(), [route.clone()], channel);
    let dead = registry
        .lookup(&route)
        .expect("the capture publication should be current");

    let stop = stop_for_dead_channel(&registry, &dead);
    assert!(
        matches!(stop, CaptureStop::ChannelChanged),
        "an accepted report hands the restore to the replacement"
    );
    assert!(registry.take_dead_reports().contains(dead.channel()));
    let pending = match restore_after_stop(stop, Some(pending), &registry).await {
        CaptureSessionOutcome::RestorePending(pending) => pending,
        CaptureSessionOutcome::Restored => panic!("the restore is owed to the replacement"),
    };
    assert!(
        handle.written_reports().is_empty(),
        "no restore write goes to the reported channel"
    );

    let (replacement_raw, replacement_handle) =
        ScriptedRawHidChannel::with_responder(|request| Some(request.to_vec()));
    registry.replace_node(node, [route], scripted_channel(replacement_raw).await);
    assert!(matches!(
        pending.retry(&registry).await,
        CaptureSessionOutcome::Restored
    ));
    assert_eq!(replacement_handle.written_reports().len(), 1);
}

/// A receiver's channel carries every paired slot, so its report is ignored
/// and the stop restores through the receiver as any other stop does.
#[tokio::test]
async fn a_dead_receiver_slot_stop_restores_through_the_receiver() {
    let route = DeviceRoute::Bolt {
        receiver_uid: "receiver-a".to_owned(),
        slot: 1,
    };
    let registry = ChannelRegistry::default();
    let (raw, _) = ScriptedRawHidChannel::with_responder(|_| None);
    registry.replace_node(
        NodeId::from("receiver-node".to_owned()),
        [route.clone()],
        scripted_channel(raw).await,
    );
    let slot = registry
        .lookup(&route)
        .expect("the capture publication should be current");

    assert!(matches!(
        stop_for_dead_channel(&registry, &slot),
        CaptureStop::Shutdown
    ));
    assert!(registry.take_dead_reports().is_empty());
}

pub(super) fn thumbwheel_device(
    status: [u8; 2],
) -> (ScriptedRawHidChannel, crate::replay::ReplayChannelHandle) {
    ScriptedRawHidChannel::with_dynamic_responder(thumbwheel_responder(status, true))
}

/// [`thumbwheel_device`]'s responder. Without `answers_info`, the wheel
/// refuses `getThumbwheelInfo` with `InvalidArgument`.
fn thumbwheel_responder(
    status: [u8; 2],
    answers_info: bool,
) -> impl Fn(&[u8]) -> Option<Vec<u8>> + Send + Sync + 'static {
    move |request| {
        let mut response = vec![0; 20];
        response[..4].copy_from_slice(&request[..4]);
        response[0] = 0x11;
        match (request[2], request[3] >> 4) {
            (0, 1) => response[4] = 4,
            (0, 0) => {
                if u16::from_be_bytes([request[4], request[5]]) == crate::thumbwheel::FEATURE_ID {
                    response[4] = 3;
                }
            }
            (3, 0) if !answers_info => return Some(feature_error(request, 0x02)),
            // getThumbwheelInfo: native 20, diverted 120, default_dir 1.
            (3, 0) => response[4..9].copy_from_slice(&[0, 20, 0, 120, 1]),
            (3, 1) => response[4..6].copy_from_slice(&status),
            (3, 2) => return Some(request.to_vec()),
            _ => panic!("unexpected capture request: {request:02x?}"),
        }
        Some(response)
    }
}

/// Every `setThumbwheelReporting` payload `handle` saw, in order.
fn reporting_writes(handle: &crate::replay::ReplayChannelHandle) -> Vec<[u8; 2]> {
    handle
        .written_reports()
        .into_iter()
        .filter(|report| report[2] == 3 && report[3] >> 4 == 2)
        .map(|report| [report[4], report[5]])
        .collect()
}

fn thumbwheel_spec() -> CaptureSpec {
    CaptureSpec {
        capture_thumbwheel: true,
        ..CaptureSpec::default()
    }
}

/// Arm the thumb wheel, then hand it back through the capture restore path,
/// returning every `setThumbwheelReporting` payload in order.
async fn arm_then_restore_thumbwheel(status: [u8; 2]) -> Vec<[u8; 2]> {
    let route = DeviceRoute::Direct {
        vendor_id: 0x046d,
        product_id: 0xb034,
    };
    let (raw, handle) = thumbwheel_device(status);
    let channel = scripted_channel(raw).await;
    let registry = ChannelRegistry::default();
    registry.replace_node(
        NodeId::from("mouse-node".to_owned()),
        [route.clone()],
        channel.clone(),
    );
    let shared = SharedChannel::new(channel.clone(), route);
    let device = Device::new(channel.clone(), 0xff).await.unwrap();
    let mut armed = ArmedControls::default();
    arm_controls_into(&device, &channel, 0xff, &thumbwheel_spec(), &mut armed)
        .await
        .unwrap();
    let pending = armed
        .into_pending(&shared)
        .expect("a diverted thumb wheel must be owed a restore");
    assert!(matches!(
        pending.allow_current_channel().retry(&registry).await,
        CaptureSessionOutcome::Restored
    ));
    reporting_writes(&handle)
}

#[tokio::test]
async fn thumbwheel_restore_keeps_the_native_inversion_found_before_capture() {
    // Native + inverted before capture: divert (default_dir 1 → no sign flip),
    // then hand back native reporting with the user's inversion intact.
    assert_eq!(
        arm_then_restore_thumbwheel([0x00, 0x01]).await,
        vec![[0x01, 0x00], [0x00, 0x01]]
    );
    assert_eq!(
        arm_then_restore_thumbwheel([0x00, 0x00]).await,
        vec![[0x01, 0x00], [0x00, 0x00]]
    );
}

#[tokio::test]
async fn thumbwheel_found_diverted_is_handed_back_diverted() {
    // Diverted before capture, e.g. by another HID++ client: teardown returns
    // that mode and that sign rather than switching the wheel to native.
    assert_eq!(
        arm_then_restore_thumbwheel([0x01, 0x01]).await,
        vec![[0x01, 0x00], [0x01, 0x01]]
    );
    assert_eq!(
        arm_then_restore_thumbwheel([0x01, 0x00]).await,
        vec![[0x01, 0x00], [0x01, 0x00]]
    );
}

/// Without `getThumbwheelInfo` the wheel's diverted reports have no known
/// sign or scale, so a wheel that refuses the info read stays native: arming
/// goes on without it, owes it nothing, and never writes its reporting.
#[tokio::test]
async fn a_thumbwheel_refusing_its_info_read_is_left_native() {
    let (raw, handle) =
        ScriptedRawHidChannel::with_dynamic_responder(thumbwheel_responder([0x00, 0x00], false));
    let channel = scripted_channel(raw).await;
    let device = Device::new(channel.clone(), 0xff).await.unwrap();
    let mut armed = ArmedControls::default();
    arm_controls_into(&device, &channel, 0xff, &thumbwheel_spec(), &mut armed)
        .await
        .unwrap();
    assert!(armed.thumb.is_none(), "the wheel must not be owned");
    assert_eq!(reporting_writes(&handle), Vec::<[u8; 2]>::new());
}

/// Whether `report` is a `getThumbwheelInfo` request.
fn is_thumbwheel_info_read(report: &[u8]) -> bool {
    report[2] == 3 && report[3] >> 4 == 0
}

/// A transport failure on the info read fails arming before any divert, so
/// the session retries instead of diverting the wheel blind.
#[tokio::test]
async fn a_transport_failure_reading_thumbwheel_info_fails_arming_before_any_divert() {
    let (raw, handle) = ScriptedRawHidChannel::with_failing_writes(
        thumbwheel_responder([0x00, 0x00], true),
        is_thumbwheel_info_read,
    );
    let channel = scripted_channel(raw).await;
    let device = Device::new(channel.clone(), 0xff).await.unwrap();
    let mut armed = ArmedControls::default();
    arm_controls_into(&device, &channel, 0xff, &thumbwheel_spec(), &mut armed)
        .await
        .unwrap_err();
    assert!(armed.thumb.is_none(), "the wheel must not be owned");
    assert_eq!(reporting_writes(&handle), Vec::<[u8; 2]>::new());
}

/// A wake that mutes the captured buttons but cannot start the spy leaves
/// them dead, so the session has to end and restore the mouse.
#[tokio::test]
async fn host_rearm_reports_a_spy_that_did_not_start() {
    const ONBOARD: u8 = 9;
    const SPY: u8 = 10;
    const START_SPY: u8 = 1;
    let spy_fails = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let (raw, _handle) = ScriptedRawHidChannel::with_dynamic_responder({
        let spy_fails = Arc::clone(&spy_fails);
        move |request| {
            let mut response = vec![0; 20];
            response[..4].copy_from_slice(&request[..4]);
            response[0] = 0x11;
            match (request[2], request[3] >> 4) {
                (0, 1) => response[4] = 4,
                (0, 0) => {
                    response[4] = match u16::from_be_bytes([request[4], request[5]]) {
                        0x8100 => ONBOARD,
                        0x8110 => SPY,
                        _ => 0,
                    };
                }
                (SPY, START_SPY) if spy_fails.load(std::sync::atomic::Ordering::Relaxed) => {
                    response[2] = 0xff;
                    response[3] = SPY;
                    response[4] = request[3];
                    response[5] = 2;
                }
                (ONBOARD | SPY, _) => {}
                _ => panic!("unexpected capture request: {request:02x?}"),
            }
            Some(response)
        }
    });
    let channel = scripted_channel(raw).await;
    let device = Device::new(channel.clone(), 0xff).await.unwrap();
    let spec = CaptureSpec {
        onboard: Some(OnboardTarget::Host(HostMode {
            slots: vec![(ButtonId::G9, SlotRoute::Captured)],
            report_rate: None,
        })),
        ..CaptureSpec::default()
    };
    let mut armed = ArmedControls::default();
    arm_controls_into(&device, &channel, 0xff, &spec, &mut armed)
        .await
        .unwrap();

    assert!(armed.rearm().await);
    spy_fails.store(true, std::sync::atomic::Ordering::Relaxed);
    assert!(!armed.rearm().await);
}

/// A session that puts the mouse in host mode says so, so the agent writes
/// the saved DPI that leaving onboard mode may have replaced.
#[tokio::test]
async fn a_host_mode_session_reports_entering_host_mode() {
    const ONBOARD: u8 = 9;
    const SPY: u8 = 10;
    let (raw, _handle) = ScriptedRawHidChannel::with_dynamic_responder(move |request| {
        let mut response = vec![0; 20];
        response[..4].copy_from_slice(&request[..4]);
        response[0] = 0x11;
        match (request[2], request[3] >> 4) {
            (0, 1) => response[4] = 4,
            (0, 0) => {
                response[4] = match u16::from_be_bytes([request[4], request[5]]) {
                    0x8100 => ONBOARD,
                    0x8110 => SPY,
                    _ => 0,
                };
            }
            (ONBOARD | SPY, _) => {}
            _ => panic!("unexpected capture request: {request:02x?}"),
        }
        Some(response)
    });
    let channel = scripted_channel(raw).await;
    let (sink, mut inputs) = mpsc::unbounded_channel();
    let (shutdown, shutdown_rx) = oneshot::channel();
    let session = tokio::spawn(async move {
        let route = DeviceRoute::Direct {
            vendor_id: 0x046d,
            product_id: 0xc539,
        };
        let registry = ChannelRegistry::default();
        registry.replace_node(
            NodeId::from("mouse-node".to_owned()),
            [route.clone()],
            channel,
        );
        let (_signal, device_io) = crate::device_io_channel();
        let spec = CaptureSpec {
            onboard: Some(OnboardTarget::Host(HostMode {
                slots: vec![(ButtonId::G9, SlotRoute::Captured)],
                report_rate: None,
            })),
            ..CaptureSpec::default()
        };
        let host = CaptureHost {
            sink,
            shutdown: shutdown_rx,
            channel_slot: Arc::new(std::sync::RwLock::new(None)),
            registry: &registry,
            device_io,
        };
        run_capture_session(route, spec, host).await
    });

    let input = tokio::time::timeout(std::time::Duration::from_secs(2), inputs.recv())
        .await
        .expect("the session reports in time")
        .expect("the session is running");
    assert_eq!(input, CapturedInput::HostModeEntered);
    shutdown.send(CaptureSessionStop::Shutdown).unwrap();
    let outcome = tokio::time::timeout(std::time::Duration::from_secs(2), session)
        .await
        .expect("the session stops")
        .expect("the session task does not panic");
    assert!(matches!(outcome, Ok(CaptureSessionOutcome::Restored)));
}
