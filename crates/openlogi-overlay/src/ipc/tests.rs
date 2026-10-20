use openlogi_core::device::{BatteryInfo, BatteryLevel, BatteryStatus, DeviceKind, LockKey};
use openlogi_ipc::testing::in_memory_agent;
use openlogi_ipc::{
    AgentRequest, AgentResponse, LockKeyChange, Notice, NoticeKind, RingObservation,
};

use super::*;

/// A client nobody serves: all a phase transition needs to stand for "an agent
/// answered", because an observe call is only sent once it is polled.
fn unserved_client() -> AgentClient {
    let (transport, _server) = tarpc::transport::channel::unbounded();
    AgentClient::new(tarpc::client::Config::default(), transport).spawn()
}

#[test]
fn the_first_failed_attempt_only_arms_the_give_up_clock() {
    let mut link = ObservationLink::<RingObservation>::default();
    let now = Instant::now();
    assert!(!link.connect_failed(now));
    assert!(matches!(
        link,
        ObservationLink::Reconnecting {
            unreachable_since: Some(armed)
        } if armed == now
    ));
}

#[test]
fn an_agent_that_stays_away_past_the_deadline_ends_the_overlay() {
    let start = Instant::now();
    let mut link = ObservationLink::<RingObservation>::default();
    assert!(!link.connect_failed(start));
    assert!(!link.connect_failed(start + GIVE_UP_AFTER / 2));
    assert!(link.connect_failed(start + GIVE_UP_AFTER));
}

#[tokio::test]
async fn an_agent_that_keeps_coming_back_never_accumulates_its_way_to_an_exit() {
    let start = Instant::now();
    let mut link = ObservationLink::<RingObservation>::default();
    // Each round the agent is gone for half the deadline, then answers —
    // which moves the clock out of the reconnecting phase. Five rounds is
    // two and a half deadlines in total, so a version that kept the clock
    // across transitions would have exited by the third.
    for round in 1..=5 {
        let gone = start + (GIVE_UP_AFTER / 2) * round;
        assert!(
            !link.connect_failed(gone),
            "a reachable agent must not inherit the previous outage's clock"
        );
        link.connected(Observer::action_ring(unserved_client()));
        link.disconnected();
    }
}

#[test]
fn activation_takes_priority_over_queued_hover_updates() {
    let hover = OverlayCommand::Hover {
        session_id: 1,
        slot: ActionRingSlot::Top,
    };
    let activation = OverlayCommand::Activate {
        session_id: 1,
        slot: ActionRingSlot::Right,
    };
    assert!(matches!(
        coalesce_command(hover, activation),
        OverlayCommand::Activate {
            slot: ActionRingSlot::Right,
            ..
        }
    ));
    assert!(matches!(
        coalesce_command(activation, hover),
        OverlayCommand::Activate { .. }
    ));
}

/// Rings open back to back — dismiss one, open the next — and the view
/// only emits a hover when the hovered slot *changes*. Swallowing the new
/// ring's first hover therefore loses it entirely for as long as the
/// pointer stays put: no hover buzz, and the agent believing nothing is
/// hovered.
#[test]
fn a_new_sessions_hover_survives_the_previous_sessions_dismissal() {
    let closing = OverlayCommand::Cancel { session_id: 1 };
    let hover = OverlayCommand::Hover {
        session_id: 2,
        slot: ActionRingSlot::Top,
    };

    assert!(matches!(
        coalesce_command(closing, hover),
        OverlayCommand::Hover { session_id: 2, .. }
    ));
}

#[tokio::test]
async fn newer_activation_supersedes_a_stale_retry_immediately() {
    let stale = OverlayCommand::Cancel { session_id: 1 };
    let replacement = OverlayCommand::Activate {
        session_id: 2,
        slot: ActionRingSlot::Right,
    };
    let (tx, mut rx) = mpsc::unbounded_channel();
    tx.send(replacement).unwrap();

    let (pending, _) = tokio::time::timeout(
        Duration::from_millis(20),
        wait_for_retry(&mut rx, stale, Some(Instant::now() + DISPLAY_LIFETIME)),
    )
    .await
    .expect("queued replacement should interrupt the retry delay")
    .expect("replacement command should remain pending");

    assert_eq!(pending, replacement);
}

#[tokio::test]
async fn newer_activation_supersedes_a_stalled_terminal_request() {
    let stale = OverlayCommand::Cancel { session_id: 1 };
    let replacement = OverlayCommand::Activate {
        session_id: 2,
        slot: ActionRingSlot::Right,
    };
    let stale_started = std::sync::Arc::new(tokio::sync::Notify::new());
    let replacement_sent = std::sync::Arc::new(tokio::sync::Notify::new());
    let (tx, mut rx) = mpsc::unbounded_channel();
    let worker = tokio::spawn({
        let stale_started = std::sync::Arc::clone(&stale_started);
        let replacement_sent = std::sync::Arc::clone(&replacement_sent);
        async move {
            send_commands_with(
                &mut rx,
                || Box::pin(async { Some(()) }),
                move |(), command| {
                    let stale_started = std::sync::Arc::clone(&stale_started);
                    let replacement_sent = std::sync::Arc::clone(&replacement_sent);
                    Box::pin(async move {
                        if command == stale {
                            stale_started.notify_one();
                            std::future::pending().await
                        } else {
                            replacement_sent.notify_one();
                            true
                        }
                    })
                },
            )
            .await;
        }
    });

    tx.send(stale).unwrap();
    tokio::time::timeout(Duration::from_millis(100), stale_started.notified())
        .await
        .expect("stale request should start");
    tx.send(replacement).unwrap();
    tokio::time::timeout(Duration::from_millis(100), replacement_sent.notified())
        .await
        .expect("replacement should cancel the stalled request");
    drop(tx);
    tokio::time::timeout(Duration::from_millis(100), worker)
        .await
        .expect("command worker should stop")
        .expect("command worker should not panic");
}

#[tokio::test]
async fn stalled_hover_stops_when_the_command_channel_closes() {
    let hover = OverlayCommand::Hover {
        session_id: 1,
        slot: ActionRingSlot::Top,
    };
    let request_started = std::sync::Arc::new(tokio::sync::Notify::new());
    let (tx, mut rx) = mpsc::unbounded_channel();
    let worker = tokio::spawn({
        let request_started = std::sync::Arc::clone(&request_started);
        async move {
            send_commands_with(
                &mut rx,
                || Box::pin(async { Some(()) }),
                move |(), _| {
                    let request_started = std::sync::Arc::clone(&request_started);
                    Box::pin(async move {
                        request_started.notify_one();
                        std::future::pending().await
                    })
                },
            )
            .await;
        }
    });

    tx.send(hover).unwrap();
    tokio::time::timeout(Duration::from_millis(100), request_started.notified())
        .await
        .expect("hover request should start");
    drop(tx);
    tokio::time::timeout(Duration::from_millis(100), worker)
        .await
        .expect("closing the channel should stop the command worker")
        .expect("command worker should not panic");
}

#[test]
fn only_terminal_commands_are_retryable() {
    let hover = OverlayCommand::Hover {
        session_id: 1,
        slot: ActionRingSlot::Top,
    };
    let activation = OverlayCommand::Activate {
        session_id: 1,
        slot: ActionRingSlot::Top,
    };
    let cancellation = OverlayCommand::Cancel { session_id: 1 };
    assert!(!hover.is_terminal());
    assert!(activation.is_terminal());
    assert!(cancellation.is_terminal());
}

#[test]
fn terminal_retries_last_only_until_the_session_deadline() {
    assert!(retry_before(Some(Instant::now() + Duration::from_secs(1))));
    let past = Instant::now()
        .checked_sub(Duration::from_secs(1))
        .unwrap_or_else(Instant::now);
    assert!(!retry_before(Some(past)));
    assert!(!retry_before(None));
}

#[test]
fn losing_the_agent_hides_the_backlight_indicator() {
    let lost = BACKLIGHT
        .lost
        .expect("the agent hides the indicator, so losing the agent must too");
    assert!(!lost().visible);
}

/// A notice the agent is showing.
fn low_battery_notice() -> NoticeObservation {
    NoticeObservation {
        generation: 1,
        notices: vec![Notice {
            id: 1,
            kind: NoticeKind::LowBattery {
                name: None,
                kind: DeviceKind::Mouse,
                battery: BatteryInfo {
                    percentage: Some(15),
                    level: BatteryLevel::Low,
                    status: BatteryStatus::Discharging,
                },
            },
        }],
        language: None,
    }
}

#[tokio::test(start_paused = true)]
async fn losing_the_agent_clears_the_notices_it_was_showing() {
    // The agent ends its notices on its own timer, so once it is gone nothing
    // else would ever take its card off the screen.
    let (hang_up, agent_gone) = tokio::sync::oneshot::channel::<()>();
    let agent = in_memory_agent(
        |request| {
            Box::pin(async move {
                let AgentRequest::ObserveNotices { since } = request else {
                    panic!("the notice loop sent an unexpected request: {request:?}");
                };
                if since != 0 {
                    // Hold the poll open, as a quiet agent does.
                    return std::future::pending().await;
                }
                Ok(AgentResponse::ObserveNotices(low_battery_notice()))
            })
        },
        async move {
            let _ = agent_gone.await;
        },
    );
    // The first attempt reaches the agent; no replacement ever answers.
    let mut agents = Some(agent);
    let connect = move || {
        let agent = agents.take();
        async move {
            match agent {
                Some(agent) => Some(agent),
                None => std::future::pending().await,
            }
        }
    };
    let mut hang_up = Some(hang_up);
    let mut shown = Vec::new();
    let observing = observe(connect, NOTICES, |observed: NoticeObservation| {
        shown.push(observed.notices.len());
        // The agent goes away once its notice is on screen.
        if let Some(hang_up) = hang_up.take() {
            let _ = hang_up.send(());
        }
        !observed.notices.is_empty()
    });

    tokio::time::timeout(Duration::from_secs(5), observing)
        .await
        .expect("a lost agent's notices are cleared");
    assert_eq!(shown, [1, 0]);
}

#[tokio::test(start_paused = true)]
async fn losing_the_agent_hides_the_lock_key_indicator() {
    // The agent hides the indicator on its own timer, so once it is gone
    // nothing else would ever take "Caps Lock On" off the screen.
    let caps_lock_on = LockKeyChange {
        key: LockKey::CapsLock,
        engaged: true,
    };
    let (hang_up, agent_gone) = tokio::sync::oneshot::channel::<()>();
    let agent = in_memory_agent(
        move |request| {
            Box::pin(async move {
                let AgentRequest::ObserveLockKeys { since } = request else {
                    panic!("the lock-key loop sent an unexpected request: {request:?}");
                };
                if since != 0 {
                    // Hold the poll open, as a quiet agent does.
                    return std::future::pending().await;
                }
                Ok(AgentResponse::ObserveLockKeys(LockKeyObservation {
                    generation: 1,
                    shown: Some(caps_lock_on),
                    language: None,
                }))
            })
        },
        async move {
            let _ = agent_gone.await;
        },
    );
    // The first attempt reaches the agent; no replacement ever answers.
    let mut agents = Some(agent);
    let connect = move || {
        let agent = agents.take();
        async move {
            match agent {
                Some(agent) => Some(agent),
                None => std::future::pending().await,
            }
        }
    };
    let mut hang_up = Some(hang_up);
    let mut shown = Vec::new();
    let observing = observe(connect, LOCK_KEYS, |observed: LockKeyObservation| {
        shown.push(observed.shown);
        // The agent goes away once its indicator is on screen.
        if let Some(hang_up) = hang_up.take() {
            let _ = hang_up.send(());
        }
        observed.shown.is_some()
    });

    tokio::time::timeout(Duration::from_secs(5), observing)
        .await
        .expect("a lost agent's indicator is hidden");
    assert_eq!(shown, [Some(caps_lock_on), None]);
}
