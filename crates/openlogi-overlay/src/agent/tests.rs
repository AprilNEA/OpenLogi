use super::*;

#[test]
fn the_first_failed_attempt_only_arms_the_give_up_clock() {
    let mut state = InvocationPollState::<()>::default();
    let now = Instant::now();
    assert!(!state.connection_failed(now));
    assert!(matches!(
        state,
        InvocationPollState::Reconnecting {
            unreachable_since: Some(armed)
        } if armed == now
    ));
}

#[test]
fn an_agent_that_stays_away_past_the_deadline_ends_the_overlay() {
    let start = Instant::now();
    let mut state = InvocationPollState::<()>::default();
    assert!(!state.connection_failed(start));
    assert!(!state.connection_failed(start + GIVE_UP_AFTER / 2));
    assert!(state.connection_failed(start + GIVE_UP_AFTER));
}

#[test]
fn an_agent_that_keeps_coming_back_never_accumulates_its_way_to_an_exit() {
    let start = Instant::now();
    let mut state = InvocationPollState::default();
    // Each round the agent is gone for half the deadline, then answers —
    // which moves the clock out of the reconnecting phase. Five rounds is
    // two and a half deadlines in total, so a version that kept the clock
    // across transitions would have exited by the third.
    for round in 1..=5 {
        let gone = start + (GIVE_UP_AFTER / 2) * round;
        assert!(
            !state.connection_failed(gone),
            "a reachable agent must not inherit the previous outage's clock"
        );
        state.connected(());
        assert_eq!(
            state.observation().map(|observation| observation.1),
            Some(0)
        );
        state.disconnected();
    }
}

#[test]
fn a_replacement_agent_starts_with_its_own_generation_cursor() {
    let mut state = InvocationPollState::default();
    state.connected(());
    state.observed(17);
    assert_eq!(
        state.observation().map(|observation| observation.1),
        Some(17)
    );

    state.disconnected();
    state.connected(());

    assert_eq!(
        state.observation().map(|observation| observation.1),
        Some(0)
    );
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
