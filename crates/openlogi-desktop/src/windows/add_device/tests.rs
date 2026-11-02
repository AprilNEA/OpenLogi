use gpui::{
    AppContext as _, AssetSource, Bounds, Modifiers, ScrollDelta, ScrollEvent, TestAppContext,
    point, size,
};
use openlogi_core::config::Config;
use openlogi_core::device::DeviceInventory;
use openlogi_ipc::{AgentSnapshot, AgentStatus, ForegroundApps, InventoryHealth, PROTOCOL_VERSION};
use openlogi_ui::action_icons::ActionIcons;
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
            secure_input: openlogi_ipc::SecureInput::Off,
            virtual_keyboard: openlogi_ipc::VirtualKeyboardStatus::Unsupported,
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
        lights: Vec::new(),
        flow: openlogi_ipc::FlowStatus::default(),
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
    let _locale = crate::services::i18n::LOCALE_LOCK.lock().unwrap();
    cx.update(gpui_kit::init);
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
fn a_refused_start_keeps_its_error_and_target_after_empty_snapshots(cx: &mut TestAppContext) {
    let _locale = crate::services::i18n::LOCALE_LOCK.lock().unwrap();
    cx.update(gpui_kit::init);
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
    let (_, visual) = cx.add_window_view(AddDeviceView::new);
    visual.update(|window, cx| window.draw(cx).clear(cx));
    let choice = visual
        .debug_bounds("pairing-receiver-c548-00000000AAAABBBB")
        .expect("the receiver is selectable");
    visual.simulate_click(choice.center(), Modifiers::default());
    let Command::StartPairing(request) = commands.try_recv().unwrap() else {
        panic!("expected the selected receiver's pairing command");
    };
    assert_eq!(request.selector, receiver_selector(&bolt).unwrap());

    // A refused StartPairing creates no agent session. Its forced refresh,
    // and later inventory updates, therefore still carry pairing: None.
    visual.update(|_, cx| {
        apply_undeliverable(cx, PairingFailure::ReceiverBusy);
        apply_state(cx, None);
        apply_state(cx, None);
    });
    visual.update(|window, cx| window.draw(cx).clear(cx));
    let retry = visual
        .debug_bounds("pairing-retry")
        .expect("an unchanged snapshot must not dismiss the refusal and its retry action");
    assert!(
        visual
            .debug_bounds("pairing-receiver-c548-00000000AAAABBBB")
            .is_none(),
        "the refusal must not turn back into the picker"
    );
    visual.simulate_click(retry.center(), Modifiers::default());
    let Command::StartPairing(request) = commands.try_recv().unwrap() else {
        panic!("expected a retry");
    };
    assert_eq!(request.selector, receiver_selector(&bolt).unwrap());
    visual.update(|_, cx| {
        assert!(matches!(cx.global::<PairingUi>(), PairingUi::Searching));
        assert_eq!(cx.global::<PairingSelection>().0.as_ref(), Some(&bolt));
    });
}

#[gpui::test]
fn an_agent_session_supersedes_a_command_failure_and_can_be_dismissed(cx: &mut TestAppContext) {
    for phase in [
        PairingPhase::Searching,
        PairingPhase::Failed(PairingFailure::Timeout),
    ] {
        cx.update(|cx| {
            apply_undeliverable(cx, PairingFailure::ReceiverBusy);
            apply_state(cx, None);
            assert!(matches!(
                cx.global::<PairingUi>(),
                PairingUi::CommandFailed(PairingFailure::ReceiverBusy)
            ));
            apply_state(cx, Some(phase));
            assert!(matches!(
                cx.global::<PairingUi>(),
                PairingUi::Searching | PairingUi::Failed(PairingFailure::Timeout)
            ));
            apply_state(cx, None);
            assert!(matches!(cx.global::<PairingUi>(), PairingUi::Idle));
        });
    }
}

#[gpui::test]
fn changing_receiver_waits_for_cancellation_acknowledgement(cx: &mut TestAppContext) {
    let _locale = crate::services::i18n::LOCALE_LOCK.lock().unwrap();
    cx.update(gpui_kit::init);
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
    let _locale = crate::services::i18n::LOCALE_LOCK.lock().unwrap();
    cx.update(gpui_kit::init);
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
    let _locale = crate::services::i18n::LOCALE_LOCK.lock().unwrap();
    cx.update(gpui_kit::init);
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
    cx.update(gpui_kit::init);
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
    cx.update(gpui_kit::init);
    cx.update(theme::register_builtin_themes);
    let mut commands = cx.update(install_state);
    cx.update(open);
    assert!(
        commands.try_recv().is_err(),
        "opening Add Device must not pair on the first enumerated receiver"
    );
}

/// More discovered devices than the 520 × 400 window shows at once.
fn long_device_list() -> Vec<FoundDevice> {
    (0..12u8)
        .map(|n| FoundDevice {
            address: [0, 0, 0, 0, 0, n],
            name: format!("Keyboard {n}"),
        })
        .collect()
}

#[gpui::test]
fn cancel_stays_in_the_bottom_bar_below_a_long_device_list(cx: &mut TestAppContext) {
    let _locale = crate::services::i18n::LOCALE_LOCK.lock().unwrap();
    cx.update(gpui_kit::init);
    cx.update(theme::register_builtin_themes);
    let mut commands = cx.update(install_state);
    cx.update(|cx| apply_state(cx, Some(PairingPhase::Found(long_device_list()))));
    let (_, cx) = cx.add_window_view(AddDeviceView::new);
    let viewport = Bounds::new(point(px(0.), px(0.)), size(px(520.), px(400.)));
    cx.simulate_resize(viewport.size);
    cx.update(|window, cx| window.draw(cx).clear(cx));

    let cancel = cx
        .debug_bounds("pairing-cancel")
        .expect("a running search offers Cancel");
    assert!(
        viewport.contains(&cancel.origin) && viewport.contains(&cancel.bottom_right()),
        "the list must scroll above Cancel, not push it off {viewport:?}: {cancel:?}"
    );
    assert!(
        cancel.left() > viewport.center().x && cancel.size.width < viewport.size.width / 4.,
        "Cancel is a right-aligned secondary button, not a full-width bar: {cancel:?}"
    );
    cx.simulate_click(cancel.center(), Modifiers::default());
    assert!(matches!(commands.try_recv(), Ok(Command::CancelPairing(_))));
}

/// A step taller than the window scrolls under the app's scrollbar, or
/// nothing says it continues. The scroll owner spans the window, so the bar
/// sits on the window's right edge, not 24 px inside it, and the rows keep
/// the page inset inside the owner.
#[gpui::test]
fn a_long_device_list_scrolls_under_a_scrollbar_on_the_window_edge(cx: &mut TestAppContext) {
    let _locale = crate::services::i18n::LOCALE_LOCK.lock().unwrap();
    cx.update(gpui_kit::init);
    cx.update(theme::register_builtin_themes);
    let _commands = cx.update(install_state);
    cx.update(|cx| apply_state(cx, Some(PairingPhase::Found(long_device_list()))));
    let (_, cx) = cx.add_window_view(AddDeviceView::new);
    let viewport = Bounds::new(point(px(0.), px(0.)), size(px(520.), px(400.)));
    cx.simulate_resize(viewport.size);
    cx.update(|window, cx| window.draw(cx).clear(cx));

    let bar = cx
        .debug_bounds("scrollbar-overlay")
        .expect("the step content has a scrollbar");
    assert_eq!(
        (bar.left(), bar.right()),
        (viewport.left(), viewport.right()),
        "the scrollbar spans the window, so it sits on the window edge: {bar:?}"
    );
    let cancel = cx
        .debug_bounds("pairing-cancel")
        .expect("a running search offers Cancel");
    assert!(
        bar.bottom() <= cancel.top(),
        "the scroll region ends above the action bar: {bar:?}, {cancel:?}"
    );

    let first = cx
        .debug_bounds("pairing-device-000000000000")
        .expect("the first device is listed");
    assert!(
        bar.contains(&first.center()),
        "the first device starts in view: {bar:?}, {first:?}"
    );
    let inset = first.left() - bar.left();
    assert!(
        inset > px(0.) && bar.right() - first.right() == inset,
        "rows keep the same page inset on both sides inside the scroll owner: {bar:?}, {first:?}"
    );

    cx.simulate_event(ScrollEvent {
        position: bar.center(),
        delta: ScrollDelta::Pixels(point(px(0.), px(-60.))),
        ..Default::default()
    });
    cx.update(|window, cx| window.draw(cx).clear(cx));
    let scrolled = cx
        .debug_bounds("pairing-device-000000000000")
        .expect("the first device is still listed");
    assert!(
        scrolled.top() < first.top(),
        "the wheel scrolls the rows under the bar: {first:?} -> {scrolled:?}"
    );
}

#[gpui::test]
fn cancel_before_choosing_a_receiver_closes_the_window(cx: &mut TestAppContext) {
    let _locale = crate::services::i18n::LOCALE_LOCK.lock().unwrap();
    cx.update(gpui_kit::init);
    cx.update(theme::register_builtin_themes);
    let mut commands = cx.update(install_state);
    let (_, visual) = cx.add_window_view(AddDeviceView::new);
    visual.update(|window, cx| window.draw(cx).clear(cx));
    let cancel = visual
        .debug_bounds("pairing-cancel")
        .expect("the receiver picker offers Cancel");
    visual.simulate_click(cancel.center(), Modifiers::default());
    assert!(cx.windows().is_empty(), "Cancel closes the picker's window");
    assert!(
        commands.try_recv().is_err(),
        "with no session there is nothing for the agent to cancel"
    );
}

#[gpui::test]
fn change_receiver_clears_a_failure_without_an_agent_session(cx: &mut TestAppContext) {
    let _locale = crate::services::i18n::LOCALE_LOCK.lock().unwrap();
    cx.update(gpui_kit::init);
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

/// Clicking a receiver focuses its row, and the next step removes that row.
/// The window takes focus back, so Tab starts from the new step's first
/// control instead of staying on a control that is gone.
#[gpui::test]
fn a_new_step_takes_focus_back_from_the_clicked_receiver(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    cx.update(theme::register_builtin_themes);
    let mut commands = cx.update(install_state);
    let bolt = receiver("Bolt", 0xc548, Some("00000000AAAABBBB"));
    cx.update(|cx| {
        AppState::update(cx, |state, cx| {
            state
                .apply_agent_snapshot(&snapshot(vec![bolt]), &AssetResolver::new(), &[])
                .events
                .emit(cx);
        });
    });
    let (view, cx) = cx.add_window_view(AddDeviceView::new);
    cx.update(|window, cx| window.draw(cx).clear(cx));
    let choice = cx
        .debug_bounds("pairing-receiver-c548-00000000AAAABBBB")
        .expect("the receiver is selectable");
    cx.simulate_click(choice.center(), Modifiers::default());
    assert!(matches!(commands.try_recv(), Ok(Command::StartPairing(_))));
    cx.update(|window, cx| window.draw(cx).clear(cx));
    cx.update(|window, cx| {
        assert!(
            view.read(cx).focus_handle.is_focused(window),
            "focus must not stay on the receiver row the search replaced"
        );
    });
}

#[test]
fn mouse_pairing_click_icons_are_embedded() {
    for click in [Click::Left, Click::Right] {
        let path = click_icon(click);
        let loaded = ActionIcons.load(path);
        assert!(
            matches!(loaded, Ok(Some(_))),
            "missing embedded asset for {path}"
        );
        let bytes = loaded.unwrap().unwrap();
        let content = std::str::from_utf8(&bytes).expect("valid utf-8 svg");
        assert!(content.contains("<svg"), "asset {path} should be an SVG");
    }
}

#[test]
fn mouse_pairing_click_icon_resolves_text_color() {
    let text_primary = gpui::hsla(0.5, 0.5, 0.5, 1.0);
    let pal = Palette {
        page: gpui::hsla(0., 0., 0., 1.),
        panel: gpui::hsla(0., 0., 0., 1.),
        control: gpui::hsla(0., 0., 0., 1.),
        control_hover: gpui::hsla(0., 0., 0., 1.),
        muted: gpui::hsla(0., 0., 0., 1.),
        border: gpui::hsla(0., 0., 0., 1.),
        text_primary,
        text_muted: gpui::hsla(0., 0., 0., 1.),
        card_radius: gpui::px(8.),
        control_radius: gpui::px(4.),
        ring: gpui::hsla(0., 0., 0., 1.),
        text_danger: gpui::hsla(0., 0., 0., 1.),
    };
    for click in [Click::Left, Click::Right] {
        let mut icon = click_icon_svg(click, pal);
        assert_eq!(
            icon.style().text.color,
            Some(text_primary),
            "click icon SVG must resolve text color so GPUI paints its path"
        );
    }
}

#[test]
fn spoken_click_sequence_formats_correctly() {
    let _locale = crate::services::i18n::LOCALE_LOCK.lock().unwrap();
    rust_i18n::set_locale("en");
    let clicks = [Click::Left, Click::Right, Click::Left];
    let spoken = spoken_click_sequence(&clicks);
    assert_eq!(spoken, "1. Left Click, 2. Right Click, 3. Left Click");
}

struct PointerPasskeyHarness {
    method: PasskeyMethod,
    pal: Palette,
}

impl Render for PointerPasskeyHarness {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        passkey_panel(&self.method, self.pal)
    }
}

#[gpui::test]
fn pointer_passkey_panel_renders(cx: &mut TestAppContext) {
    let pal = cx.update(|cx| {
        gpui_kit::init(cx);
        theme::register_builtin_themes(cx);
        theme::palette(cx)
    });
    let method = PasskeyMethod::Pointer {
        passkey: "123".into(),
        clicks: vec![Click::Left, Click::Right, Click::Left],
    };
    let (_view, cx) = cx.add_window_view(|_, _| PointerPasskeyHarness { method, pal });
    cx.update(|window, cx| window.draw(cx).clear(cx));
    let bounds = cx
        .debug_bounds("passkey-sequence")
        .expect("passkey sequence must be laid out and painted");
    assert!(bounds.size.width > gpui::px(0.));
    assert!(bounds.size.height > gpui::px(0.));
}

#[gpui::test]
fn escape_cancels_a_search_and_closes_the_idle_picker(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    cx.update(theme::register_builtin_themes);
    cx.update(crate::app::menu::install);
    let mut commands = cx.update(install_state);
    cx.update(|cx| apply_state(cx, Some(PairingPhase::Searching)));
    cx.update(open);
    let handle = cx.update(|cx| cx.global::<windows::WindowRegistry>().add_device.unwrap());
    let mut visual = gpui::VisualTestContext::from_window(handle.into(), cx);
    visual.update(|window, cx| window.draw(cx).clear(cx));

    visual.simulate_keystrokes("escape");
    assert!(matches!(commands.try_recv(), Ok(Command::CancelPairing(_))));

    visual.update(|_, cx| {
        apply_state(cx, None);
        apply_cancelled(cx);
    });
    visual.simulate_keystrokes("escape");
    assert!(
        cx.update(|cx| cx.windows().is_empty()),
        "Escape on the receiver picker closes the window, as Cancel does"
    );
}
