use super::*;

#[tokio::test]
async fn a_paired_event_sample_does_not_authorize_an_unreachable_destination() {
    const KEYBOARD_PID: u16 = 0xb369;
    const TARGET_PID: u16 = 0xb042;

    let mut keyboard_node = scripted_node_info("departed-keyboard");
    keyboard_node.product_id = KEYBOARD_PID;
    let (target_node, target, target_handle) =
        scripted_live_node("reachable-target", TARGET_PID, keyboard_without_hosts_info).await;
    let pool = ChannelPool::with_backend(ScriptedBackend::new(vec![
        (keyboard_node, ScriptedNode::OpenFails),
        (target_node, target),
    ]));
    let keyboard = DeviceRoute::Direct {
        vendor_id: crate::LOGITECH_VENDOR_ID,
        product_id: KEYBOARD_PID,
    };
    let target = DeviceRoute::Direct {
        vendor_id: crate::LOGITECH_VENDOR_ID,
        product_id: TARGET_PID,
    };

    let error = switch_linked_hosts(
        &keyboard,
        std::slice::from_ref(&target),
        1,
        KeyboardHostTransition::AnalyticsEvent {
            host_slot: ReportedHostSlot::Paired,
        },
        &pool,
    )
    .await
    .expect_err("an event-time pairing sample must not prove a later departure");
    assert!(matches!(
        error,
        HostSwitchError::HostSlotUnverified { host: 1 }
    ));
    assert!(
        !sent_host_change(&target_handle, 1),
        "physical absence must not substitute for destination validation"
    );
}

#[tokio::test]
async fn an_unknown_departure_announcement_moves_the_target_without_reopening_the_keyboard() {
    const KEYBOARD_PID: u16 = 0xb369;
    const TARGET_PID: u16 = 0xb042;

    let mut keyboard_node = scripted_node_info("departed-keyboard");
    keyboard_node.product_id = KEYBOARD_PID;
    let (target_node, target, target_handle) =
        scripted_live_node("reachable-target", TARGET_PID, keyboard_without_hosts_info).await;
    let pool = ChannelPool::with_backend(ScriptedBackend::new(vec![
        (keyboard_node, ScriptedNode::OpenFails),
        (target_node, target),
    ]));
    let keyboard = DeviceRoute::Direct {
        vendor_id: crate::LOGITECH_VENDOR_ID,
        product_id: KEYBOARD_PID,
    };
    let target = DeviceRoute::Direct {
        vendor_id: crate::LOGITECH_VENDOR_ID,
        product_id: TARGET_PID,
    };

    assert!(
        switch_linked_hosts(
            &keyboard,
            &[target],
            1,
            KeyboardHostTransition::AlreadyDeparting {
                host_slot: ReportedHostSlot::Unknown,
            },
            &pool,
        )
        .await
        .expect("the firmware departure announcement must authorize target-only forwarding")
    );
    assert!(
        sent_host_change(&target_handle, 1),
        "the target was delayed behind an impossible keyboard reopen"
    );
}

#[tokio::test]
async fn an_unreachable_commanded_keyboard_never_moves_a_target() {
    const KEYBOARD_PID: u16 = 0xb369;
    const TARGET_PID: u16 = 0xb042;

    let mut keyboard_node = scripted_node_info("departed-keyboard");
    keyboard_node.product_id = KEYBOARD_PID;
    let (target_node, target, target_handle) =
        scripted_live_node("reachable-target", TARGET_PID, keyboard_without_hosts_info).await;
    let pool = ChannelPool::with_backend(ScriptedBackend::new(vec![
        (keyboard_node, ScriptedNode::OpenFails),
        (target_node, target),
    ]));
    let keyboard = DeviceRoute::Direct {
        vendor_id: crate::LOGITECH_VENDOR_ID,
        product_id: KEYBOARD_PID,
    };
    let target = DeviceRoute::Direct {
        vendor_id: crate::LOGITECH_VENDOR_ID,
        product_id: TARGET_PID,
    };

    switch_linked_hosts(
        &keyboard,
        &[target],
        1,
        KeyboardHostTransition::CommandRequired,
        &pool,
    )
    .await
    .expect_err("a commanded transition must fail closed when its keyboard is unreachable");
    assert!(
        !sent_host_change(&target_handle, 1),
        "the target moved without validating the commanded keyboard transition"
    );
}

#[tokio::test]
async fn an_unverified_analytics_destination_never_moves_a_target() {
    const KEYBOARD_PID: u16 = 0xb369;
    const TARGET_PID: u16 = 0xb042;

    let mut keyboard_node = scripted_node_info("departed-keyboard");
    keyboard_node.product_id = KEYBOARD_PID;
    let (target_node, target, target_handle) =
        scripted_live_node("reachable-target", TARGET_PID, keyboard_without_hosts_info).await;
    let pool = ChannelPool::with_backend(ScriptedBackend::new(vec![
        (keyboard_node, ScriptedNode::OpenFails),
        (target_node, target),
    ]));
    let keyboard = DeviceRoute::Direct {
        vendor_id: crate::LOGITECH_VENDOR_ID,
        product_id: KEYBOARD_PID,
    };
    let target = DeviceRoute::Direct {
        vendor_id: crate::LOGITECH_VENDOR_ID,
        product_id: TARGET_PID,
    };

    let error = switch_linked_hosts(
        &keyboard,
        &[target],
        1,
        KeyboardHostTransition::AnalyticsEvent {
            host_slot: ReportedHostSlot::Unknown,
        },
        &pool,
    )
    .await
    .expect_err("unknown analytics destinations must fail closed");
    assert!(matches!(
        error,
        HostSwitchError::HostSlotUnverified { host: 1 }
    ));
    assert!(
        !sent_host_change(&target_handle, 1),
        "the target moved without fresh destination validation"
    );
}

#[tokio::test]
async fn an_empty_keyboard_slot_never_moves_a_target() {
    const KEYBOARD_PID: u16 = 0xb369;
    const TARGET_PID: u16 = 0xb042;

    let (keyboard_node, keyboard, _keyboard_handle) = scripted_live_node(
        "keyboard-with-empty-slot",
        KEYBOARD_PID,
        keyboard_with_an_empty_third_slot,
    )
    .await;
    let (target_node, target, target_handle) =
        scripted_live_node("reachable-target", TARGET_PID, keyboard_without_hosts_info).await;
    let pool = ChannelPool::with_backend(ScriptedBackend::new(vec![
        (keyboard_node, keyboard),
        (target_node, target),
    ]));
    let keyboard = DeviceRoute::Direct {
        vendor_id: crate::LOGITECH_VENDOR_ID,
        product_id: KEYBOARD_PID,
    };
    let target = DeviceRoute::Direct {
        vendor_id: crate::LOGITECH_VENDOR_ID,
        product_id: TARGET_PID,
    };

    let analytics_request = analytics_request(2);
    let error = switch_linked_hosts(
        &keyboard,
        &[target],
        2,
        analytics_request.keyboard_transition,
        &pool,
    )
    .await
    .expect_err("the keyboard has no pairing in host slot 2");
    assert!(matches!(error, HostSwitchError::HostSlotEmpty { host: 2 }));
    assert!(
        !sent_host_change(&target_handle, 2),
        "the target moved even though the keyboard rejected the host slot"
    );
}

#[tokio::test]
async fn a_paired_departure_announcement_does_not_use_later_slot_status() {
    const KEYBOARD_PID: u16 = 0xb369;
    const TARGET_PID: u16 = 0xb042;

    let (keyboard_node, keyboard, _keyboard_handle) = scripted_live_node(
        "keyboard-with-changed-slot",
        KEYBOARD_PID,
        keyboard_with_an_empty_third_slot,
    )
    .await;
    let (target_node, target, target_handle) =
        scripted_live_node("reachable-target", TARGET_PID, keyboard_without_hosts_info).await;
    let pool = ChannelPool::with_backend(ScriptedBackend::new(vec![
        (keyboard_node, keyboard),
        (target_node, target),
    ]));
    let keyboard = DeviceRoute::Direct {
        vendor_id: crate::LOGITECH_VENDOR_ID,
        product_id: KEYBOARD_PID,
    };
    let target = DeviceRoute::Direct {
        vendor_id: crate::LOGITECH_VENDOR_ID,
        product_id: TARGET_PID,
    };

    assert!(
        switch_linked_hosts(
            &keyboard,
            &[target],
            2,
            KeyboardHostTransition::AlreadyDeparting {
                host_slot: ReportedHostSlot::Paired,
            },
            &pool,
        )
        .await
        .expect("the firmware departure announcement is authoritative")
    );
    assert!(
        sent_host_change(&target_handle, 2),
        "the target was blocked by a later, stale slot sample"
    );
}

#[tokio::test]
async fn an_unknown_departure_announcement_does_not_command_the_keyboard_again() {
    const KEYBOARD_PID: u16 = 0xb369;
    const TARGET_PID: u16 = 0xb042;

    let (keyboard_node, keyboard, keyboard_handle) = scripted_live_node(
        "keyboard-with-fresh-paired-slot",
        KEYBOARD_PID,
        keyboard_with_all_slots_paired,
    )
    .await;
    let (target_node, target, target_handle) =
        scripted_live_node("reachable-target", TARGET_PID, keyboard_without_hosts_info).await;
    let pool = ChannelPool::with_backend(ScriptedBackend::new(vec![
        (keyboard_node, keyboard),
        (target_node, target),
    ]));
    let keyboard = DeviceRoute::Direct {
        vendor_id: crate::LOGITECH_VENDOR_ID,
        product_id: KEYBOARD_PID,
    };
    let target = DeviceRoute::Direct {
        vendor_id: crate::LOGITECH_VENDOR_ID,
        product_id: TARGET_PID,
    };

    assert!(
        switch_linked_hosts(
            &keyboard,
            &[target],
            2,
            KeyboardHostTransition::AlreadyDeparting {
                host_slot: ReportedHostSlot::Unknown,
            },
            &pool,
        )
        .await
        .expect("the firmware departure announcement should move the target directly")
    );
    assert!(sent_host_change(&target_handle, 2));
    assert!(
        !sent_host_change(&keyboard_handle, 2),
        "an already-departing keyboard was commanded a second time"
    );
}

#[tokio::test]
async fn an_empty_departure_announcement_is_not_overridden_by_later_pairing() {
    const KEYBOARD_PID: u16 = 0xb369;
    const TARGET_PID: u16 = 0xb042;

    let (keyboard_node, keyboard, keyboard_handle) = scripted_live_node(
        "keyboard-with-newly-paired-slot",
        KEYBOARD_PID,
        keyboard_with_all_slots_paired,
    )
    .await;
    let (target_node, target, target_handle) =
        scripted_live_node("reachable-target", TARGET_PID, keyboard_without_hosts_info).await;
    let pool = ChannelPool::with_backend(ScriptedBackend::new(vec![
        (keyboard_node, keyboard),
        (target_node, target),
    ]));
    let keyboard = DeviceRoute::Direct {
        vendor_id: crate::LOGITECH_VENDOR_ID,
        product_id: KEYBOARD_PID,
    };
    let target = DeviceRoute::Direct {
        vendor_id: crate::LOGITECH_VENDOR_ID,
        product_id: TARGET_PID,
    };

    let error = switch_linked_hosts(
        &keyboard,
        &[target],
        2,
        KeyboardHostTransition::AlreadyDeparting {
            host_slot: ReportedHostSlot::Empty,
        },
        &pool,
    )
    .await
    .expect_err("an explicitly empty announced slot must fail closed");
    assert!(matches!(error, HostSwitchError::HostSlotEmpty { host: 2 }));
    assert!(!sent_host_change(&target_handle, 2));
    assert!(!sent_host_change(&keyboard_handle, 2));
}

#[tokio::test]
async fn an_unknown_departure_announcement_does_not_use_later_slot_status() {
    const KEYBOARD_PID: u16 = 0xb369;
    const TARGET_PID: u16 = 0xb042;

    let (keyboard_node, keyboard, keyboard_handle) = scripted_live_node(
        "keyboard-with-fresh-empty-slot",
        KEYBOARD_PID,
        keyboard_with_an_empty_third_slot,
    )
    .await;
    let (target_node, target, target_handle) =
        scripted_live_node("reachable-target", TARGET_PID, keyboard_without_hosts_info).await;
    let pool = ChannelPool::with_backend(ScriptedBackend::new(vec![
        (keyboard_node, keyboard),
        (target_node, target),
    ]));
    let keyboard = DeviceRoute::Direct {
        vendor_id: crate::LOGITECH_VENDOR_ID,
        product_id: KEYBOARD_PID,
    };
    let target = DeviceRoute::Direct {
        vendor_id: crate::LOGITECH_VENDOR_ID,
        product_id: TARGET_PID,
    };

    assert!(
        switch_linked_hosts(
            &keyboard,
            &[target],
            2,
            KeyboardHostTransition::AlreadyDeparting {
                host_slot: ReportedHostSlot::Unknown,
            },
            &pool,
        )
        .await
        .expect("the firmware departure announcement should move the target directly")
    );
    assert!(sent_host_change(&target_handle, 2));
    assert!(
        !sent_host_change(&keyboard_handle, 2),
        "an already-departing keyboard was reopened for a stale slot sample"
    );
}

#[tokio::test]
async fn an_event_transition_requires_a_fresh_paired_slot() {
    const KEYBOARD_PID: u16 = 0xb369;
    const TARGET_PID: u16 = 0xb042;

    let responders: [(&str, crate::channel::scripted::Responder); 5] = [
        ("missing-hosts-info", keyboard_without_hosts_info),
        (
            "hosts-info-lookup-error",
            keyboard_erroring_on_hosts_info_lookup,
        ),
        ("slot-status-error", keyboard_erroring_on_slot_status),
        ("slot-status-timeout", keyboard_timing_out_on_slot_status),
        (
            "unrecognized-slot-status",
            keyboard_with_unrecognized_slot_status,
        ),
    ];
    let transition = KeyboardHostTransition::AnalyticsEvent {
        host_slot: ReportedHostSlot::Paired,
    };

    for (case, responder) in responders {
        let (keyboard_node, keyboard, keyboard_handle) =
            scripted_live_node(case, KEYBOARD_PID, responder).await;
        let (target_node, target, target_handle) =
            scripted_live_node("reachable-target", TARGET_PID, keyboard_without_hosts_info).await;
        let pool = ChannelPool::with_backend(ScriptedBackend::new(vec![
            (keyboard_node, keyboard),
            (target_node, target),
        ]));
        let keyboard = DeviceRoute::Direct {
            vendor_id: crate::LOGITECH_VENDOR_ID,
            product_id: KEYBOARD_PID,
        };
        let target = DeviceRoute::Direct {
            vendor_id: crate::LOGITECH_VENDOR_ID,
            product_id: TARGET_PID,
        };

        let error = switch_linked_hosts(&keyboard, &[target], 2, transition, &pool)
            .await
            .expect_err("inconclusive fresh status must fail closed");
        assert!(
            matches!(error, HostSwitchError::HostSlotUnverified { host: 2 }),
            "{case} returned {error:?}"
        );
        assert!(
            !sent_host_change(&target_handle, 2),
            "{case} moved the target"
        );
        assert!(
            !sent_host_change(&keyboard_handle, 2),
            "{case} moved the keyboard"
        );
    }
}

#[tokio::test]
async fn a_commanded_transition_keeps_unknown_slot_status_compatible() {
    const KEYBOARD_PID: u16 = 0xb369;
    const TARGET_PID: u16 = 0xb042;

    let (keyboard_node, keyboard, keyboard_handle) = scripted_live_node(
        "keyboard-without-hosts-info",
        KEYBOARD_PID,
        keyboard_without_hosts_info,
    )
    .await;
    let (target_node, target, target_handle) =
        scripted_live_node("reachable-target", TARGET_PID, keyboard_without_hosts_info).await;
    let pool = ChannelPool::with_backend(ScriptedBackend::new(vec![
        (keyboard_node, keyboard),
        (target_node, target),
    ]));
    let keyboard = DeviceRoute::Direct {
        vendor_id: crate::LOGITECH_VENDOR_ID,
        product_id: KEYBOARD_PID,
    };
    let target = DeviceRoute::Direct {
        vendor_id: crate::LOGITECH_VENDOR_ID,
        product_id: TARGET_PID,
    };

    assert!(
        switch_linked_hosts(
            &keyboard,
            &[target],
            2,
            KeyboardHostTransition::CommandRequired,
            &pool,
        )
        .await
        .expect("manual switching must remain compatible without HostsInfo")
    );
    assert!(sent_host_change(&target_handle, 2));
    assert!(sent_host_change(&keyboard_handle, 2));
}

#[tokio::test]
async fn a_departure_announcement_for_an_empty_slot_never_moves_a_target() {
    const KEYBOARD_PID: u16 = 0xb369;
    const TARGET_PID: u16 = 0xb042;

    let mut keyboard_node = scripted_node_info("departing-keyboard");
    keyboard_node.product_id = KEYBOARD_PID;
    let (target_node, target, target_handle) =
        scripted_live_node("reachable-target", TARGET_PID, keyboard_without_hosts_info).await;
    let pool = ChannelPool::with_backend(ScriptedBackend::new(vec![
        (keyboard_node, ScriptedNode::OpenFails),
        (target_node, target),
    ]));
    let keyboard = DeviceRoute::Direct {
        vendor_id: crate::LOGITECH_VENDOR_ID,
        product_id: KEYBOARD_PID,
    };
    let target = DeviceRoute::Direct {
        vendor_id: crate::LOGITECH_VENDOR_ID,
        product_id: TARGET_PID,
    };

    let error = switch_linked_hosts(
        &keyboard,
        &[target],
        2,
        KeyboardHostTransition::AlreadyDeparting {
            host_slot: ReportedHostSlot::Empty,
        },
        &pool,
    )
    .await
    .expect_err("an announcement does not prove that its destination is paired");
    assert!(matches!(error, HostSwitchError::HostSlotEmpty { host: 2 }));
    assert!(
        !sent_host_change(&target_handle, 2),
        "the target moved even though the keyboard reported an empty slot"
    );
}
