//! `[flow]` edits the Flow window makes, and how ready Flow reads to it.

use std::collections::BTreeMap;

use openlogi_core::config::{
    Config, ConfigFile, FlowConfig, FlowConfigParts, FlowDevice, FlowEdge, FlowHostSlot,
    FlowLayout, FlowPeer, FlowPeerName, FlowPublicKey,
};
use openlogi_core::device_order::PhysicalDeviceKey;
use openlogi_ipc::{FlowLinkState, FlowPairingPeer, FlowPairingPhase, FlowPlatform, FlowStatus};

use crate::services::assets::AssetResolver;
use crate::services::ipc::Command;
use crate::state::flow::{forgotten, placed, readiness, with_channel};
use crate::state::tests::direct_inventory;
use crate::state::{
    AppState, ConfigPersistence, FlowDeviceRow, FlowHealth, FlowHost, FlowIssue, Sources,
};

fn name(name: &str) -> FlowPeerName {
    FlowPeerName::new(name).unwrap()
}

fn slot(index: u8) -> FlowHostSlot {
    FlowHostSlot::new(index).unwrap()
}

fn key(key: &str) -> PhysicalDeviceKey {
    PhysicalDeviceKey::parse(key).unwrap()
}

fn device(device: &str, this: u8, peers: &[(&str, u8)]) -> FlowDevice {
    FlowDevice::new(
        key(device),
        slot(this),
        peers
            .iter()
            .map(|(peer, index)| (name(peer), slot(*index)))
            .collect::<BTreeMap<_, _>>(),
    )
    .unwrap()
}

/// Peers `work`, `lab` and `den`; `work` on the right and `lab` on top.
fn config(devices: Vec<FlowDevice>) -> FlowConfig {
    FlowConfig::try_from(FlowConfigParts {
        enabled: true,
        require_modifier: false,
        peers: ["work", "lab", "den"]
            .into_iter()
            .zip(1u8..)
            .map(|(peer, byte)| FlowPeer {
                name: name(peer),
                public_key: FlowPublicKey::from_bytes([byte; 32]),
                addresses: Vec::new(),
            })
            .collect(),
        layout: vec![
            FlowLayout {
                edge: FlowEdge::Right,
                peer: name("work"),
            },
            FlowLayout {
                edge: FlowEdge::Top,
                peer: name("lab"),
            },
        ],
        devices,
    })
    .unwrap()
}

/// How the window sees one device, for [`readiness`].
fn row(device: &str, name: &str, online: bool, switchable: bool) -> FlowDeviceRow {
    FlowDeviceRow {
        key: key(device),
        name: name.to_owned(),
        kind: None,
        current: None,
        config: None,
        online,
        switchable,
    }
}

fn edge_of(config: &FlowConfig, peer: &str) -> Option<FlowEdge> {
    config
        .layout()
        .iter()
        .find(|layout| layout.peer.as_str() == peer)
        .map(|layout| layout.edge)
}

#[test]
fn a_peer_moved_onto_a_taken_edge_swaps_with_its_occupant() {
    let moved = placed(&config(Vec::new()), &name("lab"), FlowEdge::Right).unwrap();
    assert_eq!(edge_of(&moved, "lab"), Some(FlowEdge::Right));
    assert_eq!(edge_of(&moved, "work"), Some(FlowEdge::Top));

    // An unplaced peer has no edge to give back: the occupant leaves.
    let moved = placed(&moved, &name("den"), FlowEdge::Top).unwrap();
    assert_eq!(edge_of(&moved, "den"), Some(FlowEdge::Top));
    assert_eq!(edge_of(&moved, "work"), None);
    assert_eq!(edge_of(&moved, "lab"), Some(FlowEdge::Right));
    assert_eq!(moved.layout().len(), 2);
}

#[test]
fn choosing_a_side_a_peer_is_on_keeps_its_other_sides() {
    // A hand-edited layout can give one peer two edges.
    let mut parts = config(Vec::new()).into_parts();
    parts.layout.push(FlowLayout {
        edge: FlowEdge::Left,
        peer: name("work"),
    });
    let two_sides = FlowConfig::try_from(parts).unwrap();

    assert_eq!(
        placed(&two_sides, &name("work"), FlowEdge::Left).unwrap(),
        two_sides
    );
    let one_side = placed(&two_sides, &name("work"), FlowEdge::Bottom).unwrap();
    let work: Vec<FlowEdge> = one_side
        .layout()
        .iter()
        .filter(|layout| layout.peer.as_str() == "work")
        .map(|layout| layout.edge)
        .collect();
    assert_eq!(work, [FlowEdge::Bottom]);
    assert_eq!(edge_of(&one_side, "lab"), Some(FlowEdge::Top));
}

#[test]
fn a_device_joining_for_a_peer_reaches_this_computer_on_its_current_channel() {
    let mouse = key("unit:a393cae0");
    let work = FlowHost::Peer(name("work"));

    let joined = with_channel(
        &config(Vec::new()),
        &mouse,
        &work,
        Some(slot(0)),
        Some(slot(2)),
    )
    .unwrap();
    assert_eq!(
        joined.devices(),
        [device("unit:a393cae0", 2, &[("work", 0)])]
    );

    // When the device's channel now is the one picked for the peer, this
    // computer keeps the lowest channel the peer left free.
    let joined = with_channel(
        &config(Vec::new()),
        &mouse,
        &work,
        Some(slot(0)),
        Some(slot(0)),
    )
    .unwrap();
    assert_eq!(
        joined.devices(),
        [device("unit:a393cae0", 1, &[("work", 0)])]
    );
}

#[test]
fn clearing_a_peer_keeps_the_device_and_clearing_this_computer_drops_it() {
    let mouse = key("unit:a393cae0");
    let both = config(vec![device("unit:a393cae0", 0, &[("work", 1), ("lab", 2)])]);

    let without_work =
        with_channel(&both, &mouse, &FlowHost::Peer(name("work")), None, None).unwrap();
    assert_eq!(
        without_work.devices(),
        [device("unit:a393cae0", 0, &[("lab", 2)])]
    );

    let left = with_channel(&both, &mouse, &FlowHost::This, None, None).unwrap();
    assert!(left.devices().is_empty());

    // A channel another computer holds is refused, and nothing changes.
    assert!(
        with_channel(
            &both,
            &mouse,
            &FlowHost::Peer(name("work")),
            Some(slot(2)),
            None
        )
        .is_err(),
        "lab already holds channel 3"
    );
}

#[test]
fn forgetting_a_peer_removes_its_edge_and_channels_and_keeps_the_rest() {
    let before = config(vec![device("unit:a393cae0", 0, &[("work", 1), ("lab", 2)])]);
    let after = forgotten(&before, &name("work")).unwrap();
    assert!(after.peer("work").is_none());
    assert_eq!(edge_of(&after, "work"), None);
    assert_eq!(edge_of(&after, "lab"), Some(FlowEdge::Top));
    assert_eq!(after.devices(), [device("unit:a393cae0", 0, &[("lab", 2)])]);
}

#[test]
fn readiness_names_what_keeps_each_peer_from_working() {
    // `work` (right) is connected and has the mouse; `lab` (top) has the
    // mouse but its link is degraded; `den` has no edge. The keyboard follows
    // only to `lab`.
    let flow = config(vec![
        device("unit:a393cae0", 0, &[("work", 1), ("lab", 2)]),
        device("unit:0000beef", 0, &[("lab", 1)]),
    ]);
    let link = |peer: &str| match peer {
        "work" => Some(FlowLinkState::Connected),
        "lab" => Some(FlowLinkState::Degraded),
        _ => None,
    };
    let devices = [
        row("unit:a393cae0", "MX Master 3S", true, true),
        row("unit:0000beef", "MX Keys", true, true),
    ];

    let ready = readiness(&flow, link, &devices);
    assert_eq!(ready.health, FlowHealth::Ready);
    assert_eq!(
        ready.routes,
        [FlowLayout {
            edge: FlowEdge::Right,
            peer: name("work"),
        }]
    );
    assert_eq!(
        ready.issues,
        [
            FlowIssue::NotFollowing {
                device: "MX Keys".to_owned(),
                peer: name("work"),
            },
            FlowIssue::Unreachable {
                peer: name("lab"),
                link: Some(FlowLinkState::Degraded),
            },
            FlowIssue::Unplaced(name("den")),
        ]
    );

    // With `work` lost, nothing is reachable: Flow is blocked, not ready.
    let blocked = readiness(
        &flow,
        |peer| (peer == "work").then_some(FlowLinkState::Lost),
        &devices,
    );
    assert_eq!(blocked.health, FlowHealth::Blocked);
    assert!(blocked.routes.is_empty());

    // A placed peer no device follows is named before its link.
    assert_eq!(
        readiness(&config(Vec::new()), link, &devices).issues,
        [
            FlowIssue::NoDevice(name("work")),
            FlowIssue::NoDevice(name("lab")),
            FlowIssue::Unplaced(name("den")),
        ]
    );

    let mut off = flow.into_parts();
    off.enabled = false;
    let off = FlowConfig::try_from(off).unwrap();
    assert_eq!(readiness(&off, link, &devices).health, FlowHealth::Off);
}

/// A crossing hands over only online devices, all or none: a peer whose
/// devices are all away does nothing, and an online device that cannot
/// switch keeps every device here.
#[test]
fn a_route_needs_an_online_device_and_none_that_cannot_switch() {
    let flow = config(vec![
        device("unit:a393cae0", 0, &[("work", 1)]),
        device("unit:0000beef", 0, &[("work", 1)]),
    ]);
    let connected = |_: &str| Some(FlowLinkState::Connected);
    let routes = |devices: &[FlowDeviceRow]| readiness(&flow, connected, devices).routes;
    let work_issues = |devices: &[FlowDeviceRow]| {
        readiness(&flow, connected, devices)
            .issues
            .into_iter()
            .filter(|issue| !matches!(issue, FlowIssue::Unplaced(_) | FlowIssue::NoDevice(_)))
            .collect::<Vec<_>>()
    };
    let work_route = [FlowLayout {
        edge: FlowEdge::Right,
        peer: name("work"),
    }];

    // One device asleep is enough to leave the other to hand over.
    let one_awake = [
        row("unit:a393cae0", "MX Master 3S", true, true),
        row("unit:0000beef", "MX Keys", false, true),
    ];
    assert_eq!(routes(&one_awake), work_route);

    let all_away = [
        row("unit:a393cae0", "MX Master 3S", false, true),
        row("unit:0000beef", "MX Keys", false, true),
    ];
    assert!(routes(&all_away).is_empty());
    assert_eq!(
        work_issues(&all_away),
        [FlowIssue::DevicesAway(name("work"))]
    );

    let keys_cannot_switch = [
        row("unit:a393cae0", "MX Master 3S", true, true),
        row("unit:0000beef", "MX Keys", true, false),
    ];
    assert!(routes(&keys_cannot_switch).is_empty());
    assert_eq!(
        work_issues(&keys_cannot_switch),
        [FlowIssue::CannotSwitch {
            device: "MX Keys".to_owned(),
            peer: name("work"),
        }]
    );

    // Asleep, it is left out of the handover, but still named.
    let asleep_cannot_switch = [
        row("unit:a393cae0", "MX Master 3S", true, true),
        row("unit:0000beef", "MX Keys", false, false),
    ];
    assert_eq!(routes(&asleep_cannot_switch), work_route);
    assert_eq!(
        work_issues(&asleep_cannot_switch),
        [FlowIssue::CannotSwitch {
            device: "MX Keys".to_owned(),
            peer: name("work"),
        }]
    );
}

/// The fixture mouse on its cable, answering as HID++ 1.0 or not, with or
/// without a serial, and `flow`. Like the MX Master 3, it has no hosts list
/// (`0x1815`).
fn mouse_state(hidpp10: bool, serial: Option<&str>, flow: FlowConfig) -> AppState {
    let mut inventory = direct_inventory([0xa3, 0x93, 0xca, 0xe0]);
    let mouse = &mut inventory.paired[0];
    mouse.hidpp10 = hidpp10;
    mouse
        .capabilities
        .as_mut()
        .expect("the fixture device has capabilities")
        .easy_switch = false;
    mouse
        .model_info
        .as_mut()
        .expect("the fixture device has model info")
        .serial_number = serial.map(str::to_owned);
    let mut config = openlogi_core::config::Config::ephemeral();
    config.flow = flow;
    let resolver = AssetResolver::new();
    let (commands, _) = tokio::sync::mpsc::unbounded_channel();
    AppState::new(Sources {
        inventories: &[inventory],
        ..Sources::in_memory(config, &resolver, commands)
    })
}

/// Switching needs host change (`0x1814`), which every HID++ 2.0 device
/// with Easy-Switch has; only a HID++ 1.0 device cannot switch.
#[test]
fn a_device_that_cannot_switch_is_offered_only_to_be_turned_off() {
    let rows = mouse_state(false, None, config(Vec::new())).flow_devices();
    assert_eq!(rows.len(), 1, "no hosts list does not hide the device");
    assert!(rows[0].switchable && rows[0].online);

    assert!(
        mouse_state(true, None, config(Vec::new()))
            .flow_devices()
            .is_empty()
    );

    let configured = mouse_state(
        true,
        None,
        config(vec![device("unit:a393cae0", 0, &[("work", 1)])]),
    );
    let rows = configured.flow_devices();
    assert_eq!(rows.len(), 1);
    assert!(!rows[0].switchable);
    assert!(
        configured
            .flow_readiness()
            .issues
            .contains(&FlowIssue::CannotSwitch {
                device: rows[0].name.clone(),
                peer: name("work"),
            })
    );
}

/// The inventory keys a device that reports a serial by that serial, but
/// `[flow.devices]` may name it by its unit id, as the agent accepts.
#[test]
fn a_device_named_by_its_unit_id_is_found_when_listed_by_its_serial() {
    let state = mouse_state(
        false,
        Some("2214LZ0ABCD8"),
        config(vec![device("unit:a393cae0", 0, &[("work", 1)])]),
    );
    let rows = state.flow_devices();
    assert_eq!(
        rows.len(),
        1,
        "one device, not a listed one and a configured one"
    );
    assert_eq!(rows[0].key, key("unit:a393cae0"));
    assert!(rows[0].online && rows[0].config.is_some());
    assert!(
        !state
            .flow_readiness()
            .issues
            .contains(&FlowIssue::DevicesAway(name("work")))
    );
}

/// The agent saves a paired computer into `config.toml` itself. Once its
/// snapshot says so, the window adopts that file: the computer shows, and the
/// next save from here does not find the file changed under it.
#[test]
fn a_computer_the_agent_paired_is_adopted_before_the_next_save() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join(openlogi_core::paths::CONFIG_FILE);
    let (config, file) = ConfigFile::load_from_path(&path).unwrap();
    let resolver = AssetResolver::new();
    let (commands, mut receiver) = tokio::sync::mpsc::unbounded_channel();
    let mut state = AppState::new(Sources {
        persistence: ConfigPersistence::UserFile(file),
        ..Sources::in_memory(config, &resolver, commands)
    });

    let (mut saved, mut agent_file) = ConfigFile::load_from_path(&path).unwrap();
    saved.flow = FlowConfig::try_from(FlowConfigParts {
        enabled: false,
        require_modifier: false,
        peers: vec![FlowPeer {
            name: name("devbox"),
            public_key: FlowPublicKey::from_bytes([9; 32]),
            addresses: Vec::new(),
        }],
        layout: Vec::new(),
        devices: Vec::new(),
    })
    .unwrap();
    agent_file.save(&saved).unwrap();
    while receiver.try_recv().is_ok() {}

    let _ = state.set_flow_status(&FlowStatus {
        enabled: false,
        peers: Vec::new(),
        pairing: Some(FlowPairingPhase::Paired {
            peer: FlowPairingPeer {
                name: "devbox".into(),
                public_key: FlowPublicKey::from_bytes([9; 32]),
                platform: FlowPlatform::Linux,
            },
            name: name("devbox"),
        }),
    });
    assert!(state.flow_config().peer("devbox").is_some());
    assert!(
        matches!(receiver.try_recv(), Ok(Command::ReloadConfig(_))),
        "the agent confirms the revision the window adopted"
    );

    let _ = state.commit_flow_edge(&name("devbox"), FlowEdge::Left);
    assert_eq!(state.config_issue(), None);
    assert_eq!(
        edge_of(&Config::load_from_path(&path).unwrap().flow, "devbox"),
        Some(FlowEdge::Left)
    );
}
