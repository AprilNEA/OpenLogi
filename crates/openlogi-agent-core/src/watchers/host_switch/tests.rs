use std::{
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use openlogi_hid::{
    DeviceRoute, HostSwitchCaptureMode, HostSwitchError, HostSwitchRequest, KeyboardHostTransition,
    ReportedHostSlot,
};
use tokio::sync::{oneshot, watch};

use super::{
    HostSwitchInventory, HostSwitchLink, HostSwitchLinks, HostSwitchManagerState,
    HostTransitionExecutor, RETRY_DELAY, RunningSession, SessionCompletion, TransitionTimeouts,
    accept_completion, capture_mode_for, keyboard_departed, remember_announcement_keyboard,
    run_transition_with, wait_for_departure,
};
use crate::receiver_access::{ExclusiveAccessReason, ReceiverAccess, ReceiverRequestState};

#[derive(Default)]
struct DepartureExecutor {
    calls: AtomicUsize,
    target_writes: AtomicUsize,
}

impl HostTransitionExecutor for DepartureExecutor {
    async fn switch(
        &self,
        _keyboard: &DeviceRoute,
        targets: &[DeviceRoute],
        host: u8,
        keyboard_transition: KeyboardHostTransition,
        _channel_pool: &openlogi_hid::ChannelPool,
    ) -> Result<bool, HostSwitchError> {
        self.calls.fetch_add(1, Ordering::Relaxed);
        let outcome = match keyboard_transition {
            KeyboardHostTransition::AlreadyDeparting {
                host_slot: ReportedHostSlot::Empty,
            } => Err(HostSwitchError::HostSlotEmpty { host }),
            KeyboardHostTransition::AlreadyDeparting { .. } => {
                self.target_writes
                    .fetch_add(targets.len(), Ordering::Relaxed);
                Ok(true)
            }
            transition @ (KeyboardHostTransition::AnalyticsEvent { .. }
            | KeyboardHostTransition::CommandRequired) => {
                panic!("unexpected scripted transition: {transition:?}")
            }
        };
        std::future::ready(outcome).await
    }
}

async fn wait_for_host_transition_request(receiver_access: &ReceiverAccess) {
    tokio::time::timeout(Duration::from_secs(1), async {
        while !receiver_access.requested(ExclusiveAccessReason::HostTransition) {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("host transition never requested exclusive access");
}

fn test_link() -> HostSwitchLink {
    HostSwitchLink {
        keyboard_key: "keyboard-a".into(),
        keyboard: DeviceRoute::Direct {
            vendor_id: 0x046d,
            product_id: 0xb37c,
        },
        targets: Vec::new(),
    }
}

#[test]
fn suspended_device_io_disables_retry_deadlines() {
    let retry_at = tokio::time::Instant::now() + RETRY_DELAY;
    let mut state = HostSwitchManagerState::new();
    state.restart_after.push((test_link(), retry_at));

    assert_eq!(
        state.deadline(ReceiverRequestState::default(), true),
        Some(retry_at),
    );
    assert_eq!(
        state.deadline(ReceiverRequestState::default(), false),
        None,
        "host-switch retries must stay dormant until visible resume",
    );
}

#[test]
fn observed_announcement_enables_lightweight_reconnect_capture() {
    let mut keyboards = Vec::new();

    assert_eq!(
        capture_mode_for(&keyboards, "keyboard-a"),
        HostSwitchCaptureMode::Full
    );
    remember_announcement_keyboard(&mut keyboards, "keyboard-a".into());
    assert_eq!(
        capture_mode_for(&keyboards, "keyboard-a"),
        HostSwitchCaptureMode::ChangeHostAnnouncement
    );
    assert_eq!(
        capture_mode_for(&keyboards, "replacement-keyboard"),
        HostSwitchCaptureMode::Full,
        "a device reusing the same route must not inherit another keyboard's capture mode"
    );
}

#[test]
fn config_link_removal_does_not_confirm_a_departure() {
    let link = test_link();
    let keyboard = link.keyboard.clone();
    let (links_tx, _links): (_, HostSwitchLinks) = watch::channel(Arc::new(vec![link]));
    let (inventory_tx, mut inventory): (_, HostSwitchInventory) =
        watch::channel(Arc::new(vec![keyboard.clone()]));

    assert!(!keyboard_departed(&mut inventory, &keyboard));
    links_tx.send_replace(Arc::new(Vec::new()));
    assert!(
        !keyboard_departed(&mut inventory, &keyboard),
        "configuration state must not stand in for physical inventory"
    );
    inventory_tx.send_replace(Arc::new(Vec::new()));
    assert!(keyboard_departed(&mut inventory, &keyboard));
}

#[tokio::test(start_paused = true)]
async fn departure_publication_finishes_wait_without_advancing_time() {
    let keyboard = test_link().keyboard;
    let (inventory_tx, mut inventory): (_, HostSwitchInventory) =
        watch::channel(Arc::new(vec![keyboard.clone()]));
    let started = tokio::time::Instant::now();
    let waiting = tokio::spawn(async move {
        assert!(
            wait_for_departure(&mut inventory, &keyboard, Duration::from_secs(10)).await,
            "published physical departure should complete the wait"
        );
        tokio::time::Instant::now()
    });
    tokio::task::yield_now().await;

    inventory_tx.send_replace(Arc::new(Vec::new()));
    tokio::task::yield_now().await;

    assert_eq!(
        waiting.await.expect("departure waiter should finish"),
        started,
        "inventory publication should reconcile departure immediately"
    );
}

#[tokio::test]
async fn paired_announcement_moves_targets_after_exclusive_wait() {
    let mut link = test_link();
    let keyboard = link.keyboard.clone();
    link.targets.push(DeviceRoute::Direct {
        vendor_id: 0x046d,
        product_id: 0xb025,
    });
    let (_inventory_tx, inventory): (_, HostSwitchInventory) =
        watch::channel(Arc::new(vec![keyboard]));
    let channel_pool = openlogi_hid::host::channel_pool();
    let receiver_access = ReceiverAccess::default();
    let session_lease = receiver_access
        .try_acquire_for_session()
        .expect("the test session should hold shared receiver access");
    let executor = Arc::new(DepartureExecutor::default());
    let request = HostSwitchRequest {
        host: 2,
        keyboard_transition: KeyboardHostTransition::AlreadyDeparting {
            host_slot: ReportedHostSlot::Paired,
        },
    };
    let no_wait = TransitionTimeouts {
        commanded_departure: Duration::ZERO,
    };
    let transition = tokio::spawn({
        let mut inventory = inventory.clone();
        let channel_pool = channel_pool.clone();
        let receiver_access = receiver_access.clone();
        let executor = Arc::clone(&executor);
        async move {
            run_transition_with(
                &mut inventory,
                &channel_pool,
                &receiver_access,
                link,
                request,
                executor.as_ref(),
                no_wait,
            )
            .await;
        }
    });

    wait_for_host_transition_request(&receiver_access).await;
    drop(session_lease);
    transition.await.expect("host transition task panicked");

    assert_eq!(executor.calls.load(Ordering::Relaxed), 1);
    assert_eq!(
        executor.target_writes.load(Ordering::Relaxed),
        1,
        "the firmware departure announcement should move the target after access is acquired"
    );
}

#[tokio::test]
async fn physical_departure_completes_one_target_switch_without_a_retry() {
    let mut link = test_link();
    let keyboard = link.keyboard.clone();
    link.targets.push(DeviceRoute::Direct {
        vendor_id: 0x046d,
        product_id: 0xb025,
    });
    let (inventory_tx, inventory): (_, HostSwitchInventory) =
        watch::channel(Arc::new(vec![keyboard]));
    let channel_pool = openlogi_hid::host::channel_pool();
    let receiver_access = ReceiverAccess::default();
    let session_lease = receiver_access
        .try_acquire_for_session()
        .expect("the test session should hold shared receiver access");
    let executor = Arc::new(DepartureExecutor::default());
    let request = HostSwitchRequest {
        host: 2,
        keyboard_transition: KeyboardHostTransition::AlreadyDeparting {
            host_slot: ReportedHostSlot::Unknown,
        },
    };
    let no_wait = TransitionTimeouts {
        commanded_departure: Duration::ZERO,
    };
    let transition = tokio::spawn({
        let mut inventory = inventory.clone();
        let channel_pool = channel_pool.clone();
        let receiver_access = receiver_access.clone();
        let executor = Arc::clone(&executor);
        async move {
            run_transition_with(
                &mut inventory,
                &channel_pool,
                &receiver_access,
                link,
                request,
                executor.as_ref(),
                no_wait,
            )
            .await;
        }
    });

    wait_for_host_transition_request(&receiver_access).await;
    inventory_tx.send_replace(Arc::new(Vec::new()));
    drop(session_lease);
    transition.await.expect("host transition task panicked");

    assert_eq!(
        executor.calls.load(Ordering::Relaxed),
        1,
        "physical absence must not trigger a second target switch"
    );
    assert_eq!(
        executor.target_writes.load(Ordering::Relaxed),
        1,
        "the announced departure should switch the target exactly once"
    );
}

#[tokio::test]
async fn stale_completion_cannot_remove_or_command_the_current_session() {
    let (stop, _stop_rx) = oneshot::channel();
    let task = tokio::spawn(async {});
    let mut sessions = vec![RunningSession {
        link: test_link(),
        generation: 2,
        stop,
        task,
    }];
    let stale = SessionCompletion {
        generation: 1,
        request: Some((
            test_link(),
            HostSwitchRequest {
                host: 2,
                keyboard_transition: KeyboardHostTransition::AlreadyDeparting {
                    host_slot: ReportedHostSlot::Paired,
                },
            },
        )),
    };

    assert!(accept_completion(&mut sessions, stale).is_none());
    assert_eq!(sessions.len(), 1, "the replacement session must stay armed");

    let current = sessions.pop().expect("replacement session remains live");
    current
        .task
        .await
        .expect("test session should finish cleanly");
}
