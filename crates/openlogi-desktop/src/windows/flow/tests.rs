use std::collections::BTreeMap;

use gpui::{AppContext as _, Modifiers, MouseButton, TestAppContext, VisualTestContext, point, px};
use gpui_kit::component::Root;
use openlogi_core::config::{
    Config, FlowConfigParts, FlowDevice, FlowHostSlot, FlowLayout, FlowPeer, FlowPublicKey,
};
use openlogi_core::device_order::PhysicalDeviceKey;
use openlogi_ipc::{
    AgentSnapshot, AgentStatus, FlowPairTarget, FlowPairingPeer, FlowPairingPhase, FlowPeerStatus,
    FlowPlatform, FlowStatus, ForegroundApps, InventoryHealth, PROTOCOL_VERSION,
};
use tokio::sync::mpsc::UnboundedReceiver;

use super::*;
use crate::services::assets::AssetResolver;
use crate::services::i18n::LOCALE_LOCK;
use crate::services::ipc::{Command, PairFlowPeer};
use crate::state::Sources;
use crate::state::tests::direct_inventory;

const MOUSE: [u8; 4] = [0xa3, 0x93, 0xca, 0xe0];

fn name(name: &str) -> FlowPeerName {
    FlowPeerName::new(name).unwrap()
}

fn slot(index: u8) -> FlowHostSlot {
    FlowHostSlot::new(index).unwrap()
}

fn public_key(byte: u8) -> FlowPublicKey {
    FlowPublicKey::from_bytes([byte; 32])
}

/// Flow on, with `Work PC` to the right and `Lab` paired but not placed.
fn config() -> Config {
    let mut config = Config::ephemeral();
    config.flow = FlowConfig::try_from(FlowConfigParts {
        enabled: true,
        require_modifier: false,
        peers: vec![
            FlowPeer {
                name: name("Work PC"),
                public_key: public_key(1),
                addresses: Vec::new(),
            },
            FlowPeer {
                name: name("Lab"),
                public_key: public_key(2),
                addresses: Vec::new(),
            },
        ],
        layout: vec![FlowLayout {
            edge: FlowEdge::Right,
            peer: name("Work PC"),
        }],
        devices: Vec::new(),
    })
    .unwrap();
    config
}

/// A connected agent that sees one mouse, with `Work PC` reachable and `Lab`
/// offline.
fn snapshot() -> AgentSnapshot {
    AgentSnapshot {
        status: AgentStatus {
            accessibility_granted: true,
            hook_installed: true,
            launch_at_login: false,
            inventory: InventoryHealth::Ready,
            protocol_version: PROTOCOL_VERSION,
            agent_version: "flow-test".into(),
            input_monitoring_granted: true,
            hid_open_failures: false,
            secure_input: openlogi_ipc::SecureInput::Off,
            virtual_keyboard: openlogi_ipc::VirtualKeyboardStatus::Unsupported,
            battery_widget: openlogi_core::device::BatteryWidgetStatus::Disabled,
        },
        inventory: vec![direct_inventory(MOUSE)],
        standalone: vec![],
        camera_active: false,
        pairing: None,
        foreground: ForegroundApps::default(),
        lights: Vec::new(),
        flow: FlowStatus {
            enabled: true,
            peers: vec![
                FlowPeerStatus {
                    name: name("Work PC"),
                    public_key: public_key(1),
                    state: FlowLinkState::Connected,
                },
                FlowPeerStatus {
                    name: name("Lab"),
                    public_key: public_key(2),
                    state: FlowLinkState::Lost,
                },
            ],
            pairing: None,
        },
    }
}

fn open_window(cx: &mut TestAppContext) -> &mut VisualTestContext {
    open_window_to_agent(cx).0
}

/// The window, and the commands it sends the agent. Callers hold
/// `LOCALE_LOCK`: a press refreshes the window, and the redraw before the
/// release re-lays it out in whatever locale a parallel test has set, so the
/// release misses the element the press hit.
fn open_window_to_agent(
    cx: &mut TestAppContext,
) -> (&mut VisualTestContext, UnboundedReceiver<Command>) {
    cx.update(gpui_kit::init);
    cx.update(theme::register_builtin_themes);
    cx.update(crate::app::menu::install);
    let (sender, receiver) = tokio::sync::mpsc::unbounded_channel();
    cx.update(|cx| {
        let resolver = AssetResolver::new();
        let state = cx.new(|_| AppState::new(Sources::in_memory(config(), &resolver, sender)));
        AppState::set_global(state, cx);
        AppState::update(cx, |state, cx| {
            state
                .apply_agent_snapshot(&snapshot(), &resolver, &[])
                .events
                .emit(cx);
        });
    });
    let (_, visual) = cx.add_window_view(|window, cx| {
        let view = cx.new(|cx| FlowView::new(window, cx));
        Root::new(view, window, cx)
    });
    draw(visual);
    (visual, receiver)
}

fn draw(visual: &mut VisualTestContext) {
    visual.update(|window, cx| window.draw(cx).clear(cx));
}

fn click(visual: &mut VisualTestContext, selector: &'static str) {
    let bounds = visual
        .debug_bounds(selector)
        .unwrap_or_else(|| panic!("`{selector}` is drawn"));
    visual.simulate_click(bounds.center(), Modifiers::none());
    draw(visual);
}

fn flow(visual: &mut VisualTestContext) -> FlowConfig {
    visual.update(|_, cx| AppState::try_read(cx).unwrap().flow_config().clone())
}

fn edge_of(visual: &mut VisualTestContext, peer: &str) -> Option<FlowEdge> {
    flow(visual)
        .layout()
        .iter()
        .find(|layout| layout.peer.as_str() == peer)
        .map(|layout| layout.edge)
}

fn health(visual: &mut VisualTestContext) -> FlowHealth {
    visual.update(|_, cx| AppState::try_read(cx).unwrap().flow_readiness().health)
}

/// The path a user takes after pairing: pick the peer, send the mouse to it
/// on channel 2, then move it with the keyboard. The mouse reaches this
/// computer on the lowest channel the peer left free, and Flow is ready only
/// once a device follows.
#[gpui::test]
fn a_peer_gets_a_device_and_moves_with_the_arrow_keys(cx: &mut TestAppContext) {
    let _locale = LOCALE_LOCK.lock().unwrap();
    let visual = open_window(cx);
    assert_eq!(health(visual), FlowHealth::Blocked, "no device follows yet");
    assert!(
        visual.debug_bounds("flow-inspector-name").is_none(),
        "the window opens on this computer"
    );

    click(visual, "flow-tile-Work PC");
    assert!(visual.debug_bounds("flow-inspector-name").is_some());
    click(visual, "flow-peer-unit:a393cae0-2");
    assert_eq!(
        flow(visual).devices(),
        [FlowDevice::new(
            PhysicalDeviceKey::parse("unit:a393cae0").unwrap(),
            slot(0),
            BTreeMap::from([(name("Work PC"), slot(1))]),
        )
        .unwrap()]
    );
    assert_eq!(health(visual), FlowHealth::Ready);
    assert!(
        visual.debug_bounds("flow-self-unit:a393cae0-1").is_none(),
        "the peer's inspector edits only the peer's channel"
    );

    // Arrow keys move the selected tile once the arrangement has focus.
    click(visual, "flow-tile-Work PC");
    visual.simulate_keystrokes("up");
    draw(visual);
    assert_eq!(edge_of(visual, "Work PC"), Some(FlowEdge::Top));

    // This computer's channel 2 is the peer's: it cannot be picked here.
    click(visual, "flow-tile-self");
    click(visual, "flow-self-unit:a393cae0-2");
    assert_eq!(flow(visual).devices()[0].self_slot(), slot(0));
    click(visual, "flow-self-unit:a393cae0-3");
    assert_eq!(flow(visual).devices()[0].self_slot(), slot(2));
}

/// An unplaced peer gets an edge from the inspector, then moves by drag; the
/// tray empties once it is placed.
#[gpui::test]
fn an_unplaced_peer_is_placed_from_the_inspector_and_dragged(cx: &mut TestAppContext) {
    let _locale = LOCALE_LOCK.lock().unwrap();
    let visual = open_window(cx);
    assert!(visual.debug_bounds("flow-tile-Lab").is_none());

    click(visual, "flow-tray-Lab");
    click(visual, "flow-edge-Left");
    assert_eq!(edge_of(visual, "Lab"), Some(FlowEdge::Left));
    assert!(visual.debug_bounds("flow-tray-Lab").is_none());
    assert_eq!(edge_of(visual, "Work PC"), Some(FlowEdge::Right));

    let from = visual.debug_bounds("flow-tile-Lab").unwrap().center();
    visual.simulate_mouse_down(from, MouseButton::Left, Modifiers::none());
    visual.simulate_mouse_move(
        from + point(px(12.), px(12.)),
        MouseButton::Left,
        Modifiers::none(),
    );
    draw(visual);
    let to = visual
        .debug_bounds("flow-slot-Bottom")
        .expect("a drag shows where the tile can land")
        .center();
    visual.simulate_mouse_move(to, MouseButton::Left, Modifiers::none());
    visual.simulate_mouse_up(to, MouseButton::Left, Modifiers::none());
    draw(visual);
    assert_eq!(edge_of(visual, "Lab"), Some(FlowEdge::Bottom));
    assert!(visual.debug_bounds("flow-slot-Bottom").is_none());
}

/// The toolbar switch and the trigger choice write `[flow]`, and removing a
/// peer asks first, then forgets its key and edge.
#[gpui::test]
fn the_switch_trigger_and_removal_write_the_flow_table(cx: &mut TestAppContext) {
    let _locale = LOCALE_LOCK.lock().unwrap();
    let visual = open_window(cx);

    click(visual, "flow-trigger-modifier");
    assert!(flow(visual).require_modifier);
    click(visual, "flow-enabled");
    assert!(!flow(visual).enabled);
    assert_eq!(health(visual), FlowHealth::Off);

    click(visual, "flow-tile-Work PC");
    click(visual, "flow-remove");
    assert!(
        flow(visual).peer("Work PC").is_some(),
        "removing waits for confirmation"
    );
    visual.simulate_keystrokes("enter");
    draw(visual);
    let flow = flow(visual);
    assert!(flow.peer("Work PC").is_none());
    assert!(flow.layout().is_empty());
    assert!(
        visual.debug_bounds("flow-inspector-name").is_none(),
        "the inspector falls back to this computer"
    );
}

/// The agent's snapshot, with pairing at `phase`.
fn show_pairing(visual: &mut VisualTestContext, phase: FlowPairingPhase) {
    let mut snapshot = snapshot();
    snapshot.flow.pairing = Some(phase);
    visual.update(|_, cx| {
        AppState::update(cx, |state, cx| {
            state
                .apply_agent_snapshot(&snapshot, &AssetResolver::new(), &[])
                .events
                .emit(cx);
        });
    });
    draw(visual);
}

fn nearby(name: &str, key: u8) -> FlowPairingPeer {
    FlowPairingPeer {
        name: name.into(),
        public_key: public_key(key),
        platform: FlowPlatform::Linux,
    }
}

fn row_of(key: u8) -> &'static str {
    format!("flow-pair-peer-{}", public_key(key)).leak()
}

/// Add Computer opens pairing, and Pair pairs with the computer picked in
/// the list. A computer already added is listed but cannot be picked.
#[gpui::test]
fn adding_a_computer_pairs_with_the_one_picked(cx: &mut TestAppContext) {
    let _locale = LOCALE_LOCK.lock().unwrap();
    let (visual, mut commands) = open_window_to_agent(cx);
    while commands.try_recv().is_ok() {}
    click(visual, "flow-add");
    assert!(matches!(
        commands.try_recv(),
        Ok(Command::StartFlowPairing(_))
    ));

    show_pairing(
        visual,
        FlowPairingPhase::Found(vec![nearby("devbox", 9), nearby("Work PC", 1)]),
    );
    click(visual, row_of(1));
    click(visual, "flow-pair-continue");
    assert!(commands.try_recv().is_err(), "Work PC is already added");

    click(visual, row_of(9));
    click(visual, "flow-pair-continue");
    let Ok(Command::PairFlowPeer(PairFlowPeer { target })) = commands.try_recv() else {
        panic!("Pair pairs with the computer picked");
    };
    assert_eq!(target, FlowPairTarget::Nearby(public_key(9)));
}

/// Once the agent has saved the computer, the dialog closes, pairing stops,
/// and the inspector shows the computer so it can get a side.
#[gpui::test]
fn a_saved_computer_closes_the_dialog_and_is_selected(cx: &mut TestAppContext) {
    let _locale = LOCALE_LOCK.lock().unwrap();
    let (visual, mut commands) = open_window_to_agent(cx);
    click(visual, "flow-add");
    show_pairing(
        visual,
        FlowPairingPhase::WaitingForPeer {
            peer: nearby("Lab", 2),
            code: "418672".into(),
        },
    );
    assert!(visual.debug_bounds("flow-pair-code").is_some());
    while commands.try_recv().is_ok() {}

    show_pairing(
        visual,
        FlowPairingPhase::Paired {
            peer: nearby("Lab", 2),
            name: name("Lab"),
        },
    );
    assert!(visual.debug_bounds("flow-pair").is_none());
    assert!(visual.debug_bounds("flow-tray-Lab").is_some());
    assert!(visual.debug_bounds("flow-inspector-name").is_some());
    let sent: Vec<_> = std::iter::from_fn(|| commands.try_recv().ok()).collect();
    assert!(
        sent.iter()
            .any(|command| matches!(command, Command::StopFlowPairing(_))),
        "closing the dialog stops pairing"
    );
}

/// Escape closes the dialog, and pairing with it, even once the row that had
/// focus is gone with the step it was on. Enter alone closes nothing.
#[gpui::test]
fn closing_the_dialog_stops_pairing(cx: &mut TestAppContext) {
    let _locale = LOCALE_LOCK.lock().unwrap();
    let (visual, mut commands) = open_window_to_agent(cx);
    click(visual, "flow-add");
    show_pairing(visual, FlowPairingPhase::Found(vec![nearby("devbox", 9)]));
    visual.simulate_keystrokes("enter");
    draw(visual);
    assert!(visual.debug_bounds("flow-pair").is_some());

    click(visual, row_of(9));
    click(visual, "flow-pair-continue");
    show_pairing(
        visual,
        FlowPairingPhase::Connecting {
            peer: Some(nearby("devbox", 9)),
        },
    );
    while commands.try_recv().is_ok() {}

    visual.simulate_keystrokes("escape");
    draw(visual);
    assert!(visual.debug_bounds("flow-pair").is_none());
    assert!(matches!(
        commands.try_recv(),
        Ok(Command::StopFlowPairing(_))
    ));
}

#[gpui::test]
fn window_commands_remain_available_while_pairing(cx: &mut TestAppContext) {
    use crate::app::menu::{CloseWindow, Minimize, Zoom};

    let _locale = LOCALE_LOCK.lock().unwrap();
    let other = cx.open_window(gpui::size(px(400.), px(300.)), |_, _| gpui::Empty);
    let (visual, mut commands) = open_window_to_agent(cx);
    visual.update(|window, _| window.activate_window());
    click(visual, "flow-add");
    show_pairing(visual, FlowPairingPhase::Found(vec![nearby("devbox", 9)]));
    visual.update(|window, cx| {
        assert!(window.is_action_available(&Minimize, cx));
        assert!(window.is_action_available(&Zoom, cx));
        assert!(window.is_action_available(&CloseWindow, cx));
    });
    while commands.try_recv().is_ok() {}

    visual.simulate_keystrokes(if cfg!(target_os = "macos") {
        "cmd-w"
    } else {
        "ctrl-w"
    });
    cx.update(|cx| assert_eq!(cx.windows(), vec![other.into()]));
    assert!(matches!(
        commands.try_recv(),
        Ok(Command::StopFlowPairing(_))
    ));
}
