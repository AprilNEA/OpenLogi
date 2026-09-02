//! Flow config for cross-machine device handoff.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// Cross-machine Flow settings.
///
/// This is persisted configuration only. Discovery, transport, and device
/// handoff live outside `openlogi-core`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FlowConfig {
    /// Whether Flow handoff is enabled.
    #[serde(default)]
    pub enabled: bool,
    /// Whether the Control modifier must be held while crossing a configured
    /// screen edge.
    #[serde(default)]
    pub require_modifier: bool,
    /// Trusted peer machines.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub peers: Vec<FlowPeer>,
    /// Mappings from this machine's screen edges to peers.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub layout: Vec<FlowLayout>,
    /// Devices that participate in Flow and their per-machine host channels.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub devices: Vec<FlowDevice>,
}

impl FlowConfig {
    /// Whether this value is exactly the implicit default and can be omitted
    /// from `config.toml`.
    #[must_use]
    pub fn is_default(&self) -> bool {
        self == &Self::default()
    }
}

/// One trusted Flow peer and its optional manual address hints.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FlowPeer {
    /// User-visible peer name, also referenced by layout and channel mappings.
    pub name: String,
    /// Peer's pinned Ed25519 public key, encoded as `"ed25519:"` followed by
    /// 64 lowercase hexadecimal digits.
    pub public_key: String,
    /// Manual hostnames or IP addresses used when mDNS cannot reach this peer.
    /// Hostnames are kept intact for resolution by the networking layer.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub addresses: Vec<String>,
}

/// One screen-edge mapping from this machine to a Flow peer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FlowLayout {
    /// Edge of this machine's screen that leads to the peer.
    pub edge: FlowEdge,
    /// Name of the peer reached through this edge.
    pub peer: String,
}

/// Screen edge that can trigger a Flow handoff.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FlowEdge {
    /// Left edge of the screen.
    Left,
    /// Right edge of the screen.
    Right,
    /// Top edge of the screen.
    Top,
    /// Bottom edge of the screen.
    Bottom,
}

/// One device participating in Flow.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FlowDevice {
    /// Existing stable physical-device config key, such as `"unit:0f1e2d3c"`.
    pub key: String,
    /// Easy-Switch host channel for this machine (`"self"`) and each peer,
    /// keyed by peer name.
    pub peer_channels: BTreeMap<String, u8>,
}
