//! Flow config for cross-machine device handoff.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// Cross-machine Flow settings.
///
/// This is persisted configuration only. Discovery, transport, and device
/// handoff live outside `openlogi-core`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FlowConfig {
    /// Whether Flow handoff is enabled.
    #[serde(default)]
    pub enabled: bool,
    /// Whether the Control modifier must be held while crossing a configured
    /// screen edge.
    #[serde(default)]
    pub require_modifier: bool,
    /// Whether the text or files copied on this machine follow the pointer to
    /// the peer it crosses to, and what arrives that way replaces this
    /// machine's clipboard. Both machines must have it on. Defaults to `true`
    /// and is only serialized when disabled.
    #[serde(default = "default_true", skip_serializing_if = "is_true")]
    pub clipboard: bool,
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

impl Default for FlowConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            require_modifier: false,
            clipboard: true,
            peers: Vec::new(),
            layout: Vec::new(),
            devices: Vec::new(),
        }
    }
}

impl FlowConfig {
    /// Whether this value is exactly the implicit default and can be omitted
    /// from `config.toml`.
    #[must_use]
    pub fn is_default(&self) -> bool {
        self == &Self::default()
    }
}

/// serde default for [`FlowConfig::clipboard`], so a `[flow]` section written
/// before the field existed keeps clipboard sync on.
const fn default_true() -> bool {
    true
}

#[expect(
    clippy::trivially_copy_pass_by_ref,
    reason = "serde's skip_serializing_if requires a fn(&T) -> bool signature"
)]
const fn is_true(value: &bool) -> bool {
    *value
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
