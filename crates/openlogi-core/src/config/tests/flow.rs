//! The `[flow]` section: parsing, defaults, and rejection of malformed entries.

use super::*;

#[test]
fn full_flow_section_roundtrips() {
    let source = r#"
schema_version = 6

[flow]
enabled = true
require_modifier = false

[[flow.peers]]
name = "work-laptop"
public_key = "ed25519:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"
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
        peers: vec![FlowPeer {
            name: "work-laptop".into(),
            public_key: "ed25519:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"
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

    let written = toml::to_string_pretty(&config).expect("serialize Flow config");
    let reparsed: Config = toml::from_str(&written).expect("reparse Flow config");
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
fn malformed_flow_entries_are_rejected() {
    for source in [
        "schema_version = 6\n[flow]\nunknown = true\n",
        "schema_version = 6\n[[flow.peers]]\nname = \"peer\"\n",
        "schema_version = 6\n[[flow.peers]]\nname = \"peer\"\npublic_key = \"ed25519:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef\"\naddresses = \"peer.example\"\n",
        "schema_version = 6\n[[flow.layout]]\nedge = \"diagonal\"\npeer = \"peer\"\n",
        "schema_version = 6\n[[flow.devices]]\nkey = \"unit:0f1e2d3c\"\npeer_channels = { self = \"zero\" }\n",
    ] {
        assert!(
            toml::from_str::<Config>(source).is_err(),
            "accepted malformed Flow config: {source}"
        );
    }
}
