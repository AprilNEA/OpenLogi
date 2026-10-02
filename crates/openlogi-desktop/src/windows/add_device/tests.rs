use gpui::{AppContext as _, Modifiers, TestAppContext};
use openlogi_core::config::Config;
use openlogi_core::device::DeviceInventory;
use openlogi_ipc::{AgentSnapshot, AgentStatus, ForegroundApps, InventoryHealth, PROTOCOL_VERSION};
use tokio::sync::mpsc;

use super::*;
use crate::services::assets::AssetResolver;
use crate::state::Sources;

fn install_state(cx: &mut App) -> mpsc::UnboundedReceiver<Command> {
    let (sender, receiver) = mpsc::unbounded_channel();
    let resolver = AssetResolver::new();
    let state =
        cx.new(|_| AppState::new(Sources::in_memory(Config::ephemeral(), &resolver, sender)));
    AppState::set_global(state, cx);
    receiver
}

fn snapshot(receivers: Vec<ReceiverInfo>) -> AgentSnapshot {
    AgentSnapshot {
        status: AgentStatus {
            accessibility_granted: true,
            hook_installed: true,
            launch_at_login: false,
            inventory: InventoryHealth::Ready,
            protocol_version: PROTOCOL_VERSION,
            agent_version: "pairing-test".into(),
            input_monitoring_granted: true,
            hid_open_failures: false,
        },
        inventory: receivers
            .into_iter()
            .map(|receiver| DeviceInventory {
                receiver,
                paired: vec![],
            })
            .collect(),
        standalone: vec![],
        camera_active: false,
        pairing: None,
        foreground: ForegroundApps::default(),
    }
}

fn receiver(name: &str, product_id: u16, uid: Option<&str>) -> ReceiverInfo {
    ReceiverInfo {
        name: name.into(),
        vendor_id: 0x046d,
        product_id,
        unique_id: uid.map(str::to_string),
    }
}

#[gpui::test]
fn reopening_keeps_the_selected_receiver_for_retry(cx: &mut TestAppContext) {
    cx.update(gpui_component::init);
    cx.update(theme::register_builtin_themes);
    let mut commands = cx.update(install_state);
    let bolt = receiver("Bolt", 0xc548, Some("00000000AAAABBBB"));
    cx.update(|cx| {
        AppState::update(cx, |state, cx| {
            state
                .apply_agent_snapshot(&snapshot(vec![bolt.clone()]), &AssetResolver::new(), &[])
                .events
                .emit(cx);
        });
    });
    {
        let (_, visual) = cx.add_window_view(AddDeviceView::new);
        visual.update(|window, cx| window.draw(cx).clear(cx));
        let bounds = visual
            .debug_bounds("pairing-receiver-c548-00000000AAAABBBB")
            .unwrap();
        visual.simulate_click(bounds.center(), Modifiers::default());
        assert!(matches!(commands.try_recv(), Ok(Command::StartPairing(_))));
        visual.update(|window, _| window.remove_window());
    }
    cx.update(|cx| apply_state(cx, Some(PairingPhase::Failed(PairingFailure::Timeout))));
    let (_, visual) = cx.add_window_view(AddDeviceView::new);
    visual.update(|window, cx| window.draw(cx).clear(cx));
    let retry = visual
        .debug_bounds("pairing-retry")
        .expect("reopening must retain retry");
    visual.simulate_click(retry.center(), Modifiers::default());
    let Command::StartPairing(request) = commands.try_recv().unwrap() else {
        panic!("expected retry");
    };
    assert_eq!(request.selector, receiver_selector(&bolt).unwrap());
}

#[gpui::test]
fn changing_receiver_waits_for_cancellation_acknowledgement(cx: &mut TestAppContext) {
    cx.update(gpui_component::init);
    cx.update(theme::register_builtin_themes);
    let mut commands = cx.update(install_state);
    cx.update(|cx| apply_undeliverable(cx, PairingFailure::AlreadyActive));
    let (_, visual) = cx.add_window_view(AddDeviceView::new);
    visual.update(|window, cx| window.draw(cx).clear(cx));
    let change = visual.debug_bounds("pairing-change-receiver").unwrap();
    visual.simulate_click(change.center(), Modifiers::default());
    assert!(matches!(commands.try_recv(), Ok(Command::CancelPairing(_))));
    visual.update(|_, cx| {
        assert!(
            !matches!(cx.global::<PairingUi>(), PairingUi::Idle),
            "picker must wait for cleanup"
        );
        apply_state(cx, None);
        assert!(matches!(cx.global::<PairingUi>(), PairingUi::Cancelling));
        apply_cancelled(cx);
        assert!(matches!(cx.global::<PairingUi>(), PairingUi::Idle));
    });
}

#[gpui::test]
fn a_recovered_receiver_identity_enables_the_same_open_picker(cx: &mut TestAppContext) {
    cx.update(gpui_component::init);
    cx.update(theme::register_builtin_themes);
    let mut commands = cx.update(install_state);
    let (_, visual) = cx.add_window_view(AddDeviceView::new);
    for uid in [None, Some("00000000AAAABBBB")] {
        visual.update(|_, cx| {
            AppState::update(cx, |state, cx| {
                state
                    .apply_agent_snapshot(
                        &snapshot(vec![receiver("Bolt", 0xc548, uid)]),
                        &AssetResolver::new(),
                        &[],
                    )
                    .events
                    .emit(cx);
            });
        });
        visual.update(|window, cx| window.draw(cx).clear(cx));
        let choice = visual.debug_bounds("pairing-receiver-c548-00000000AAAABBBB");
        assert_eq!(choice.is_some(), uid.is_some());
        if let Some(bounds) = choice {
            visual.simulate_click(bounds.center(), Modifiers::default());
        } else {
            assert!(
                commands.try_recv().is_err(),
                "missing identity must never pair by guessing"
            );
        }
    }
    let Command::StartPairing(request) = commands.try_recv().unwrap() else {
        panic!("expected selection");
    };
    assert_eq!(
        request.selector,
        ReceiverSelector::ReceiverUid {
            product_id: 0xc548,
            uid: "00000000AAAABBBB".into()
        }
    );
    visual.update(|window, cx| window.draw(cx).clear(cx));
    assert!(
        visual
            .debug_bounds("pairing-receiver-c548-00000000AAAABBBB")
            .is_none(),
        "a submitted choice must not remain clickable"
    );
}

#[gpui::test]
fn choosing_bolt_and_retrying_keep_the_explicit_target(cx: &mut TestAppContext) {
    cx.update(gpui_component::init);
    cx.update(theme::register_builtin_themes);
    let mut commands = cx.update(install_state);
    let unifying = receiver("Unifying Receiver", 0xc52b, Some("11223344"));
    let bolt = receiver("Logi Bolt Receiver", 0xc548, Some("00000000AAAABBBB"));
    cx.update(|cx| {
        AppState::update(cx, |state, cx| {
            state
                .apply_agent_snapshot(
                    &snapshot(vec![unifying.clone(), bolt.clone()]),
                    &AssetResolver::new(),
                    &[],
                )
                .events
                .emit(cx);
        });
    });
    let (_, cx) = cx.add_window_view(AddDeviceView::new);
    cx.update(|window, cx| window.draw(cx).clear(cx));
    let choice = cx
        .debug_bounds("pairing-receiver-c548-00000000AAAABBBB")
        .expect("Bolt choice rendered beside Unifying");
    cx.simulate_click(choice.center(), Modifiers::default());
    let Command::StartPairing(request) = commands
        .try_recv()
        .expect("selection sends a pairing command")
    else {
        panic!("expected StartPairing");
    };
    assert_eq!(request.selector, receiver_selector(&bolt).unwrap());

    // Inventory order/liveness may change while a pairing attempt is active.
    // Retry must retain the chosen identity, never silently use Unifying.
    cx.update(|_, cx| {
        AppState::update(cx, |state, cx| {
            state
                .apply_agent_snapshot(&snapshot(vec![unifying]), &AssetResolver::new(), &[])
                .events
                .emit(cx);
        });
        apply_state(
            cx,
            Some(PairingPhase::Failed(PairingFailure::ReceiverNotFound)),
        );
    });
    cx.update(|window, cx| window.draw(cx).clear(cx));
    let retry = cx
        .debug_bounds("pairing-retry")
        .expect("failure offers retry");
    cx.simulate_click(retry.center(), Modifiers::default());
    let Command::StartPairing(request) =
        commands.try_recv().expect("retry sends a pairing command")
    else {
        panic!("expected StartPairing");
    };
    assert_eq!(request.selector, receiver_selector(&bolt).unwrap());
    cx.update(|_, cx| {
        assert_eq!(cx.global::<PairingSelection>().0.as_ref(), Some(&bolt));
    });
}

#[test]
fn only_identified_receivers_can_be_selected() {
    assert!(receiver_selector(&receiver("Direct mouse", 0xb023, Some("mouse"))).is_none());
    assert!(receiver_selector(&receiver("Unifying", 0xc52b, None)).is_none());
    assert!(receiver_selector(&receiver("Bolt", 0xc548, Some(""))).is_none());
    assert!(matches!(
        receiver_selector(&receiver("Unifying", 0xc52b, Some("11223344"))),
        Some(ReceiverSelector::ReceiverUid {
            product_id: 0xc52b,
            ..
        })
    ));
}

#[gpui::test]
fn empty_receiver_hotplug_updates_the_picker(cx: &mut TestAppContext) {
    cx.update(gpui_component::init);
    cx.update(theme::register_builtin_themes);
    let mut commands = cx.update(install_state);
    let (_, cx) = cx.add_window_view(AddDeviceView::new);
    let bolt = receiver("Logi Bolt Receiver", 0xc548, Some("00000000AAAABBBB"));
    cx.update(|window, cx| window.draw(cx).clear(cx));
    assert!(
        cx.debug_bounds("pairing-receiver-c548-00000000AAAABBBB")
            .is_none()
    );
    cx.update(|_, cx| {
        AppState::update(cx, |state, cx| {
            let snapshot = snapshot(vec![bolt.clone()]);
            let changes = state.apply_agent_snapshot(&snapshot, &AssetResolver::new(), &[]);
            assert!(changes.events.contains(&StateEvent::InventoryChanged));
            changes.events.emit(cx);
            assert!(
                state
                    .apply_agent_snapshot(&snapshot, &AssetResolver::new(), &[])
                    .events
                    .is_empty()
            );
        });
    });
    cx.update(|window, cx| window.draw(cx).clear(cx));
    assert!(
        cx.debug_bounds("pairing-receiver-c548-00000000AAAABBBB")
            .is_some()
    );
    assert!(
        commands.try_recv().is_err(),
        "hotplug must not begin pairing without a choice"
    );
}

#[gpui::test]
fn opening_waits_for_a_receiver_choice(cx: &mut TestAppContext) {
    cx.update(gpui_component::init);
    cx.update(theme::register_builtin_themes);
    let mut commands = cx.update(install_state);
    cx.update(open);
    assert!(
        commands.try_recv().is_err(),
        "opening Add Device must not pair on the first enumerated receiver"
    );
}

#[gpui::test]
fn change_receiver_clears_a_failure_without_an_agent_session(cx: &mut TestAppContext) {
    cx.update(gpui_component::init);
    cx.update(theme::register_builtin_themes);
    let mut commands = cx.update(install_state);
    cx.update(|cx| apply_undeliverable(cx, PairingFailure::WatcherUnavailable));
    let (_, cx) = cx.add_window_view(AddDeviceView::new);
    cx.update(|window, cx| window.draw(cx).clear(cx));
    let change = cx
        .debug_bounds("pairing-change-receiver")
        .expect("failure offers a receiver change");
    cx.simulate_click(change.center(), Modifiers::default());
    cx.update(|_, cx| {
        assert!(matches!(cx.global::<PairingUi>(), PairingUi::Cancelling));
        apply_cancelled(cx);
        assert!(matches!(cx.global::<PairingUi>(), PairingUi::Idle));
    });
    assert!(matches!(commands.try_recv(), Ok(Command::CancelPairing(_))));
}
