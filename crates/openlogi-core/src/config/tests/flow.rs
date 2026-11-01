//! The `[flow]` section: parsing, defaults, editing through parts, and
//! rejection of malformed or inconsistent entries.

use super::*;
use crate::device_order::PhysicalDeviceKey;

const KEY: &str = "ed25519:0f1e2d3c4b5a69780f1e2d3c4b5a69780f1e2d3c4b5a69780f1e2d3c4b5a6978";
const OTHER_KEY: &str = "ed25519:fedcba9876543210fedcba9876543210fedcba9876543210fedcba9876543210";

/// A valid `[flow]` section. Each rejection case changes one part of it.
const VALID: &str = r#"
[flow]
enabled = true
require_modifier = false

[[flow.peers]]
name = "work-laptop"
public_key = "ed25519:0f1e2d3c4b5a69780f1e2d3c4b5a69780f1e2d3c4b5a69780f1e2d3c4b5a6978"
addresses = ["work-laptop.tailnet.example", "10.0.0.7"]

[[flow.layout]]
edge = "right"
peer = "work-laptop"

[[flow.devices]]
key = "unit:0f1e2d3c"
peer_channels = { self = 0, "work-laptop" = 1 }
"#;

fn parse(flow: &str) -> Result<Config, toml::de::Error> {
    toml::from_str(&format!("schema_version = {SCHEMA_VERSION}\n{flow}"))
}

/// Inserts `block` right before the first line that starts with `anchor`.
fn insert_before(anchor: &str, block: &str) -> String {
    VALID.replacen(anchor, &format!("{block}\n{anchor}"), 1)
}

#[test]
fn full_flow_section_roundtrips() {
    let config = parse(VALID).expect("full Flow config must parse");
    let flow = &config.flow;
    assert!(flow.enabled);
    assert!(!flow.require_modifier);

    let [peer] = flow.peers() else {
        panic!("expected one peer, got {:?}", flow.peers());
    };
    assert_eq!(peer.name.as_str(), "work-laptop");
    assert_eq!(peer.public_key.to_string(), KEY);
    assert_eq!(peer.addresses, ["work-laptop.tailnet.example", "10.0.0.7"]);
    assert_eq!(flow.peer("work-laptop"), Some(peer));

    assert_eq!(
        flow.layout(),
        [FlowLayout {
            edge: FlowEdge::Right,
            peer: peer.name.clone(),
        }]
    );

    let [device] = flow.devices() else {
        panic!("expected one device, got {:?}", flow.devices());
    };
    assert_eq!(device.key().as_str(), "unit:0f1e2d3c");
    assert_eq!(device.self_slot().get(), 0);
    assert_eq!(
        device.peer_slot("work-laptop").map(FlowHostSlot::get),
        Some(1)
    );

    let reparsed = write_and_read(&config);
    assert_eq!(&reparsed.flow, flow);
}

/// The inventory keys a device in lowercase, so a device key in another case
/// is stored the way the inventory writes it.
#[test]
fn device_keys_are_stored_in_lowercase() {
    for (written, stored) in [
        ("unit:0F1E2D3C", "unit:0f1e2d3c"),
        ("serial:AB12cd", "serial:ab12cd"),
    ] {
        let config = parse(&VALID.replacen("unit:0f1e2d3c", written, 1))
            .unwrap_or_else(|error| panic!("{written} must load: {error}"));
        assert_eq!(config.flow.devices()[0].key().as_str(), stored);
    }
}

#[test]
fn empty_flow_section_uses_defaults() {
    for flow in ["", "\n[flow]\n"] {
        let config = parse(flow).expect("empty Flow config must parse");
        assert_eq!(config.flow, FlowConfig::default());
        assert!(
            !toml::to_string_pretty(&config)
                .expect("serialize default Flow config")
                .contains("[flow]"),
            "default Flow config should be omitted"
        );
    }
}

#[test]
fn parts_build_a_config_and_refuse_a_broken_edit() {
    let name = FlowPeerName::new("desk").unwrap();
    let peer = FlowPeer {
        name: name.clone(),
        public_key: KEY.parse().unwrap(),
        addresses: Vec::new(),
    };
    let device = FlowDevice::new(
        PhysicalDeviceKey::parse("unit:0f1e2d3c").unwrap(),
        FlowHostSlot::new(0).unwrap(),
        BTreeMap::from([(name.clone(), FlowHostSlot::new(2).unwrap())]),
    )
    .unwrap();
    let flow = FlowConfig::try_from(FlowConfigParts {
        enabled: true,
        peers: vec![peer],
        devices: vec![device],
        ..FlowConfigParts::default()
    })
    .unwrap();
    assert_eq!(
        flow.devices()[0].peer_slot("desk").map(FlowHostSlot::get),
        Some(2)
    );

    // Removing the peer would leave the device pointing at nothing.
    let mut parts = flow.into_parts();
    parts.peers.clear();
    assert_eq!(
        FlowConfig::try_from(parts),
        Err(FlowConfigError::UnknownDevicePeer {
            device: "unit:0f1e2d3c".into(),
            peer: name,
        })
    );
}

#[test]
fn value_constructors_reject_invalid_input() {
    for name in ["", "self", " padded", "padded ", "line\nbreak"] {
        assert_eq!(
            FlowPeerName::new(name),
            Err(FlowConfigError::InvalidPeerName(name.into()))
        );
    }
    for key in [
        String::new(),
        KEY.trim_start_matches("ed25519:").to_owned(),
        "ed25519:0f1e".to_owned(),
        KEY.replacen("4b5a", "4B5A", 1),
        KEY.replacen("78", "7g", 1),
    ] {
        assert_eq!(
            key.parse::<FlowPublicKey>(),
            Err(FlowConfigError::InvalidPublicKey(key.clone()))
        );
    }
    assert_eq!(FlowHostSlot::new(2).map(FlowHostSlot::get), Ok(2));
    for slot in [3, 255] {
        assert_eq!(
            FlowHostSlot::new(slot),
            Err(FlowConfigError::SlotOutOfRange(slot))
        );
    }
}

/// Parses each `(case, flow, expected)` and checks the error names the
/// expected failure, so a case cannot pass by failing for another reason.
fn assert_rejected(cases: Vec<(&str, String, &str)>) {
    for (case, flow, expected) in cases {
        let error = parse(&flow).expect_err(case).to_string();
        assert!(
            error.contains(expected),
            "{case}: expected {expected:?} in error:\n{error}"
        );
    }
}

/// Shape and type errors from serde itself.
#[test]
fn malformed_flow_entries_are_rejected() {
    assert_rejected(vec![
        (
            "unknown field",
            VALID.replacen("enabled", "unknown", 1),
            "unknown field",
        ),
        (
            "missing key",
            VALID.replacen(&format!("public_key = \"{KEY}\""), "", 1),
            "missing field",
        ),
        (
            "addresses not a list",
            VALID.replacen(
                "addresses = [\"work-laptop.tailnet.example\", \"10.0.0.7\"]",
                "addresses = \"peer.example\"",
                1,
            ),
            "invalid type",
        ),
        (
            "unknown edge",
            VALID.replacen("\"right\"", "\"diagonal\"", 1),
            "unknown variant",
        ),
        (
            "slot not a number",
            VALID.replacen("self = 0", "self = \"zero\"", 1),
            "invalid type",
        ),
    ]);
}

/// Keys, names, slots, and device keys outside their domain.
#[test]
fn invalid_flow_values_are_rejected() {
    assert_rejected(vec![
        (
            "uppercase key",
            VALID.replacen("4b5a", "4B5A", 1),
            "invalid Flow public key",
        ),
        (
            "key without prefix",
            VALID.replacen("\"ed25519:", "\"", 1),
            "invalid Flow public key",
        ),
        (
            "empty name",
            VALID.replacen("name = \"work-laptop\"", "name = \"\"", 1),
            "invalid Flow peer name",
        ),
        (
            "reserved name",
            VALID.replacen("name = \"work-laptop\"", "name = \"self\"", 1),
            "invalid Flow peer name",
        ),
        (
            "padded name",
            VALID.replacen("name = \"work-laptop\"", "name = \" work-laptop\"", 1),
            "invalid Flow peer name",
        ),
        (
            "slot 3",
            VALID.replacen("\"work-laptop\" = 1", "\"work-laptop\" = 3", 1),
            "Easy-Switch slot 3 is out of range",
        ),
        (
            "slot 255",
            VALID.replacen("\"work-laptop\" = 1", "\"work-laptop\" = 255", 1),
            "Easy-Switch slot 255 is out of range",
        ),
        (
            "non-physical device key",
            VALID.replacen("key = \"unit:0f1e2d3c\"", "key = \"\"", 1),
            "is not a device identity key",
        ),
        (
            "receiver route key",
            VALID.replacen(
                "key = \"unit:0f1e2d3c\"",
                "key = \"receiver:aabbccdd:slot:1\"",
                1,
            ),
            "is not a device identity key",
        ),
    ]);
}

/// Duplicates and references to things that are not configured.
#[test]
fn inconsistent_flow_references_are_rejected() {
    let duplicate_peer = |name: &str, key: &str| {
        insert_before(
            "[[flow.layout]]",
            &format!("[[flow.peers]]\nname = \"{name}\"\npublic_key = \"{key}\"\n"),
        )
    };
    assert_rejected(vec![
        (
            "duplicate slot",
            VALID.replacen("\"work-laptop\" = 1", "\"work-laptop\" = 0", 1),
            "maps two hosts to Easy-Switch slot 0",
        ),
        (
            "missing self slot",
            VALID.replacen("self = 0, ", "", 1),
            "has no \"self\" Easy-Switch slot",
        ),
        (
            "unknown device peer",
            VALID.replacen("\"work-laptop\" = 1", "\"home-desktop\" = 1", 1),
            "maps unknown peer home-desktop",
        ),
        (
            "unknown layout peer",
            VALID.replacen("peer = \"work-laptop\"", "peer = \"home-desktop\"", 1),
            "layout references unknown peer home-desktop",
        ),
        (
            "duplicate edge",
            insert_before(
                "[[flow.devices]]",
                "[[flow.layout]]\nedge = \"right\"\npeer = \"work-laptop\"\n",
            ),
            "maps the Right edge twice",
        ),
        (
            "duplicate peer name",
            duplicate_peer("work-laptop", OTHER_KEY),
            "peer name work-laptop is used twice",
        ),
        (
            "duplicate peer key",
            duplicate_peer("home-desktop", KEY),
            "is used by two peers",
        ),
        (
            "duplicate device",
            format!(
                "{VALID}\n[[flow.devices]]\nkey = \"unit:0f1e2d3c\"\npeer_channels = {{ self = 0 }}\n"
            ),
            "is configured twice",
        ),
        (
            "duplicate device in another case",
            format!(
                "{VALID}\n[[flow.devices]]\nkey = \"unit:0F1E2D3C\"\npeer_channels = {{ self = 0 }}\n"
            ),
            "is configured twice",
        ),
    ]);
}
