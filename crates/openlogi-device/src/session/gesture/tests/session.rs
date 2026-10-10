//! The capture session lifecycle: arming, restore through a replacement channel, rollback, and teardown order.

use super::*;

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
            vec![ArmedReporting {
                cid: reprog_controls::GESTURE_BUTTON_CID,
                original: reporting(false, None),
            }],
        ),
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
            vec![ArmedReporting {
                cid: reprog_controls::GESTURE_BUTTON_CID,
                original: reporting(false, None),
            }],
        ),
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
            vec![ArmedReporting {
                cid: reprog_controls::GESTURE_BUTTON_CID,
                original: reporting(false, None),
            }],
        ),
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
