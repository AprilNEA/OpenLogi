//! The `[flow]` section: round-trips, defaults, and rejected shapes.

use std::collections::BTreeMap;

use super::*;

#[test]
fn full_flow_section_roundtrips() {
    let source = r#"
schema_version = 6

[flow]
enabled = true
require_modifier = false
clipboard = false

[[flow.peers]]
name = "work-laptop"
public_key = "ed25519:00112233445566778899aabbccddeeff00112233445566778899aabbccddeeff"
addresses = ["work-laptop.tailnet.example", "10.0.0.7"]

[[flow.layout]]
edge = "right"
peer = "work-laptop"

[[flow.devices]]
key = "unit:0f1e2d3c"
peer_channels = { self = 0, "work-laptop" = 1 }
"#;

    let config: Config = toml::from_str(source).expect("full Flow config must parse");
    let flow = FlowConfig {
        enabled: true,
        require_modifier: false,
        clipboard: false,
        peers: vec![FlowPeer {
            name: "work-laptop".into(),
            public_key: "ed25519:00112233445566778899aabbccddeeff00112233445566778899aabbccddeeff"
                .into(),
            addresses: vec!["work-laptop.tailnet.example".into(), "10.0.0.7".into()],
        }],
        layout: vec![FlowLayout {
            edge: FlowEdge::Right,
            peer: "work-laptop".into(),
        }],
        devices: vec![FlowDevice {
            key: "unit:0f1e2d3c".into(),
            peer_channels: BTreeMap::from([("self".into(), 0), ("work-laptop".into(), 1)]),
        }],
    };
    assert_eq!(config.flow, flow);

    let reparsed = write_and_read(&config);
    assert_eq!(reparsed.flow, flow);
}

#[test]
fn empty_flow_section_uses_defaults() {
    for source in ["schema_version = 6\n", "schema_version = 6\n\n[flow]\n"] {
        let config: Config = toml::from_str(source).expect("empty Flow config must parse");
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
fn clipboard_sync_defaults_on_and_is_written_only_when_off() {
    let config: Config = toml::from_str("schema_version = 6\n\n[flow]\nenabled = true\n")
        .expect("Flow config without the clipboard key must parse");
    assert!(config.flow.clipboard, "clipboard sync must default to on");
    assert!(
        !toml::to_string_pretty(&config)
            .expect("serialize Flow config")
            .contains("clipboard"),
        "the default clipboard setting should be omitted"
    );

    let mut config = config;
    config.flow.clipboard = false;
    assert!(
        toml::to_string_pretty(&config)
            .expect("serialize Flow config")
            .contains("clipboard = false"),
        "a disabled clipboard setting must be written"
    );
    assert!(!write_and_read(&config).flow.clipboard);
}

#[test]
fn malformed_flow_entries_are_rejected() {
    for source in [
        "schema_version = 6\n[flow]\nunknown = true\n",
        "schema_version = 6\n[flow]\nclipboard = \"text\"\n",
        "schema_version = 6\n[[flow.peers]]\nname = \"peer\"\n",
        "schema_version = 6\n[[flow.peers]]\nname = \"peer\"\npublic_key = \"ed25519:00112233445566778899aabbccddeeff00112233445566778899aabbccddeeff\"\naddresses = \"peer.example\"\n",
        "schema_version = 6\n[[flow.layout]]\nedge = \"diagonal\"\npeer = \"peer\"\n",
        "schema_version = 6\n[[flow.devices]]\nkey = \"unit:0f1e2d3c\"\npeer_channels = { self = \"zero\" }\n",
    ] {
        assert!(
            toml::from_str::<Config>(source).is_err(),
            "accepted malformed Flow config: {source}"
        );
    }
}
