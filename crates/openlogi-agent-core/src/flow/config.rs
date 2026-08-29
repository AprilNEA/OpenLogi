use std::collections::{BTreeMap, HashMap, HashSet};

use openlogi_core::config::{FlowConfig, FlowEdge};
use openlogi_flow::sas::PublicKey;
use openlogi_hook::edge::EdgeSide;
use openlogi_ipc::{FlowLinkState, FlowPeerStatus, FlowStatus};
use thiserror::Error;

#[derive(Clone, Debug)]
pub(super) struct CompiledFlowConfig {
    pub(super) enabled: bool,
    pub(super) peers: Vec<CompiledPeer>,
    pub(super) layout: HashMap<EdgeSide, usize>,
    pub(super) devices: BTreeMap<String, BTreeMap<String, u8>>,
}

#[derive(Clone, Debug)]
pub(super) struct CompiledPeer {
    pub(super) name: String,
    pub(super) public_key: PublicKey,
    pub(super) canonical_key: String,
    pub(super) addresses: Vec<String>,
}

impl CompiledFlowConfig {
    pub(super) fn compile(config: &FlowConfig) -> Result<Self, FlowConfigError> {
        let mut peer_names = HashSet::new();
        let mut peer_keys = HashSet::new();
        let mut peers = Vec::with_capacity(config.peers.len());
        for peer in &config.peers {
            if peer.name.is_empty() {
                return Err(FlowConfigError::EmptyPeerName);
            }
            if !peer_names.insert(peer.name.clone()) {
                return Err(FlowConfigError::DuplicatePeerName(peer.name.clone()));
            }
            let public_key = parse_public_key(&peer.public_key)?;
            if !peer_keys.insert(public_key) {
                return Err(FlowConfigError::DuplicatePeerKey(peer.public_key.clone()));
            }
            peers.push(CompiledPeer {
                name: peer.name.clone(),
                public_key,
                canonical_key: format_public_key(public_key),
                addresses: peer.addresses.clone(),
            });
        }

        let by_name: HashMap<_, _> = peers
            .iter()
            .enumerate()
            .map(|(index, peer)| (peer.name.as_str(), index))
            .collect();
        let mut layout = HashMap::new();
        for placement in &config.layout {
            let side = edge_side(placement.edge);
            let peer = by_name
                .get(placement.peer.as_str())
                .copied()
                .ok_or_else(|| FlowConfigError::UnknownLayoutPeer(placement.peer.clone()))?;
            if layout.insert(side, peer).is_some() {
                return Err(FlowConfigError::DuplicateLayoutEdge(placement.edge));
            }
        }

        let mut devices = BTreeMap::new();
        for device in &config.devices {
            if device.key.is_empty() {
                return Err(FlowConfigError::EmptyDeviceKey);
            }
            if !device.peer_channels.contains_key("self") {
                return Err(FlowConfigError::MissingSelfChannel(device.key.clone()));
            }
            for peer in device.peer_channels.keys().filter(|peer| *peer != "self") {
                if !by_name.contains_key(peer.as_str()) {
                    return Err(FlowConfigError::UnknownDevicePeer {
                        device: device.key.clone(),
                        peer: peer.clone(),
                    });
                }
            }
            if devices
                .insert(device.key.clone(), device.peer_channels.clone())
                .is_some()
            {
                return Err(FlowConfigError::DuplicateDevice(device.key.clone()));
            }
        }

        Ok(Self {
            enabled: config.enabled,
            peers,
            layout,
            devices,
        })
    }

    pub(super) fn status(&self) -> FlowStatus {
        FlowStatus {
            enabled: self.enabled,
            peers: self
                .peers
                .iter()
                .map(|peer| FlowPeerStatus {
                    name: peer.name.clone(),
                    public_key: peer.canonical_key.clone(),
                    state: FlowLinkState::Lost,
                })
                .collect(),
        }
    }
}

pub(super) const fn edge_side(edge: FlowEdge) -> EdgeSide {
    match edge {
        FlowEdge::Left => EdgeSide::Left,
        FlowEdge::Right => EdgeSide::Right,
        FlowEdge::Top => EdgeSide::Top,
        FlowEdge::Bottom => EdgeSide::Bottom,
    }
}

pub(super) fn parse_public_key(value: &str) -> Result<PublicKey, FlowConfigError> {
    let Some(hex) = value.strip_prefix("ed25519:") else {
        return Err(FlowConfigError::InvalidPublicKey(value.to_owned()));
    };
    if hex.len() != 64
        || !hex
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(FlowConfigError::InvalidPublicKey(value.to_owned()));
    }
    let mut bytes = [0; 32];
    for (index, pair) in hex.as_bytes().as_chunks::<2>().0.iter().enumerate() {
        let pair = std::str::from_utf8(pair)
            .map_err(|_| FlowConfigError::InvalidPublicKey(value.to_owned()))?;
        bytes[index] = u8::from_str_radix(pair, 16)
            .map_err(|_| FlowConfigError::InvalidPublicKey(value.to_owned()))?;
    }
    Ok(PublicKey::new(bytes))
}

pub(super) fn format_public_key(key: PublicKey) -> String {
    let mut value = String::with_capacity(72);
    value.push_str("ed25519:");
    for byte in key.as_bytes() {
        use std::fmt::Write as _;
        write!(value, "{byte:02x}").unwrap_or_else(|_| unreachable!("writing to String succeeds"));
    }
    value
}

#[derive(Debug, Error, PartialEq, Eq)]
pub(super) enum FlowConfigError {
    #[error("Flow peer names must not be empty")]
    EmptyPeerName,
    #[error("duplicate Flow peer name {0:?}")]
    DuplicatePeerName(String),
    #[error("duplicate Flow peer public key {0:?}")]
    DuplicatePeerKey(String),
    #[error("invalid Flow public key {0:?}; expected ed25519: plus 64 lowercase hex digits")]
    InvalidPublicKey(String),
    #[error("Flow layout references unknown peer {0:?}")]
    UnknownLayoutPeer(String),
    #[error("Flow edge {0:?} is configured more than once")]
    DuplicateLayoutEdge(FlowEdge),
    #[error("Flow device keys must not be empty")]
    EmptyDeviceKey,
    #[error("duplicate Flow device {0:?}")]
    DuplicateDevice(String),
    #[error("Flow device {0:?} has no self channel")]
    MissingSelfChannel(String),
    #[error("Flow device {device:?} references unknown peer {peer:?}")]
    UnknownDevicePeer { device: String, peer: String },
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use openlogi_core::config::{FlowDevice, FlowLayout, FlowPeer};

    use super::*;

    fn key(byte: &str) -> String {
        format!("ed25519:{}", byte.repeat(32))
    }

    #[test]
    fn compiles_peer_layout_and_device_channels() {
        let config = FlowConfig {
            enabled: true,
            require_modifier: true,
            peers: vec![FlowPeer {
                name: "desk".into(),
                public_key: key("ab"),
                addresses: vec!["desk.local".into()],
            }],
            layout: vec![FlowLayout {
                edge: FlowEdge::Right,
                peer: "desk".into(),
            }],
            devices: vec![FlowDevice {
                key: "unit:01020304".into(),
                peer_channels: BTreeMap::from([("self".into(), 0), ("desk".into(), 1)]),
            }],
        };

        let compiled = CompiledFlowConfig::compile(&config).unwrap();
        assert!(compiled.enabled);
        assert!(config.require_modifier);
        assert_eq!(compiled.layout[&EdgeSide::Right], 0);
        assert_eq!(compiled.devices["unit:01020304"]["desk"], 1);
        assert_eq!(compiled.status().peers[0].public_key, key("ab"));
    }

    #[test]
    fn rejects_noncanonical_keys_and_dangling_references() {
        let mut config = FlowConfig {
            peers: vec![FlowPeer {
                name: "desk".into(),
                public_key: key("AB"),
                addresses: Vec::new(),
            }],
            ..FlowConfig::default()
        };
        assert!(matches!(
            CompiledFlowConfig::compile(&config),
            Err(FlowConfigError::InvalidPublicKey(_))
        ));

        config.peers[0].public_key = key("ab");
        config.layout.push(FlowLayout {
            edge: FlowEdge::Left,
            peer: "missing".into(),
        });
        assert_eq!(
            CompiledFlowConfig::compile(&config).unwrap_err(),
            FlowConfigError::UnknownLayoutPeer("missing".into())
        );
    }
}
