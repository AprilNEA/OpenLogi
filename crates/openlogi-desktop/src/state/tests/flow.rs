//! `[flow]` edits the Flow window makes, and how ready Flow reads to it.

use std::collections::BTreeMap;

use openlogi_core::config::{
    FlowConfig, FlowConfigParts, FlowDevice, FlowEdge, FlowHostSlot, FlowLayout, FlowPeer,
    FlowPeerName, FlowPublicKey,
};
use openlogi_core::device_order::PhysicalDeviceKey;
use openlogi_ipc::FlowLinkState;

use crate::state::flow::{forgotten, placed, readiness, with_channel};
use crate::state::{FlowHealth, FlowHost, FlowIssue};

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
    let device_name = |key: &PhysicalDeviceKey| match key.as_str() {
        "unit:0000beef" => "MX Keys".to_owned(),
        other => other.to_owned(),
    };

    let ready = readiness(&flow, link, device_name);
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
        |key| key.as_str().to_owned(),
    );
    assert_eq!(blocked.health, FlowHealth::Blocked);
    assert!(blocked.routes.is_empty());

    // A placed peer no device follows is named before its link.
    assert_eq!(
        readiness(&config(Vec::new()), link, device_name).issues,
        [
            FlowIssue::NoDevice(name("work")),
            FlowIssue::NoDevice(name("lab")),
            FlowIssue::Unplaced(name("den")),
        ]
    );

    let mut off = flow.into_parts();
    off.enabled = false;
    let off = FlowConfig::try_from(off).unwrap();
    assert_eq!(readiness(&off, link, device_name).health, FlowHealth::Off);
}
