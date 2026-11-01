//! Flow config for cross-machine device handoff.
//!
//! `[flow]` deserializes through [`FlowConfigParts`] and `TryFrom`, so every
//! [`FlowConfig`] value satisfies the invariants documented on it. Consumers
//! such as the agent compile host-specific state from it and never
//! re-validate names, keys, references, or Easy-Switch slots.

use std::{
    borrow::Borrow,
    collections::{BTreeMap, BTreeSet},
    fmt,
    str::FromStr,
};

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::device_order::PhysicalDeviceKey;

/// Key in `peer_channels` that names this machine's own Easy-Switch slot.
pub const SELF_CHANNEL: &str = "self";

const PUBLIC_KEY_PREFIX: &str = "ed25519:";

/// An invalid `[flow]` value or cross-reference.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum FlowConfigError {
    /// A peer key is not `ed25519:` followed by 64 lowercase hex digits.
    #[error("invalid Flow public key {0:?}: expected \"ed25519:\" and 64 lowercase hex digits")]
    InvalidPublicKey(String),
    /// A peer name is empty, padded, contains control characters, or is `self`.
    #[error(
        "invalid Flow peer name {0:?}: use a non-empty name without surrounding \
         whitespace or control characters, other than \"self\""
    )]
    InvalidPeerName(String),
    /// An Easy-Switch slot is outside the hardware range.
    #[error(
        "Easy-Switch slot {0} is out of range: slots are 0-based and devices have at most {max} hosts",
        max = FlowHostSlot::MAX_HOSTS
    )]
    SlotOutOfRange(u8),
    /// A device key does not name the device by its own identity.
    #[error(
        "Flow device key {0:?} is not a device identity key: use the device's \"serial:\" or \"unit:\" key"
    )]
    InvalidDeviceKey(String),
    /// Two peers share a name.
    #[error("Flow peer name {0} is used twice")]
    DuplicatePeerName(FlowPeerName),
    /// Two peers share a public key.
    #[error("Flow public key {0} is used by two peers")]
    DuplicatePeerKey(FlowPublicKey),
    /// A layout entry names a peer that is not configured.
    #[error("Flow layout references unknown peer {0}")]
    UnknownLayoutPeer(FlowPeerName),
    /// Two layout entries use the same screen edge.
    #[error("Flow layout maps the {0:?} edge twice")]
    DuplicateLayoutEdge(FlowEdge),
    /// Two device entries share a key.
    #[error("Flow device {0} is configured twice")]
    DuplicateDevice(String),
    /// A device has no slot for this machine.
    #[error("Flow device {0} has no \"self\" Easy-Switch slot")]
    MissingSelfSlot(String),
    /// A device maps a peer that is not configured.
    #[error("Flow device {device} maps unknown peer {peer}")]
    UnknownDevicePeer {
        /// Device config key.
        device: String,
        /// Unknown peer name.
        peer: FlowPeerName,
    },
    /// A device maps two hosts to one Easy-Switch slot.
    #[error("Flow device {device} maps two hosts to Easy-Switch slot {slot}")]
    DuplicateSlot {
        /// Device config key.
        device: String,
        /// The slot used twice.
        slot: u8,
    },
}

/// Pinned Ed25519 identity of a Flow peer.
///
/// The persisted form is canonical: `ed25519:` and 64 lowercase hex digits.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct FlowPublicKey([u8; 32]);

impl FlowPublicKey {
    /// Wraps raw key bytes.
    #[must_use]
    pub const fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    /// Returns the raw key bytes.
    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

impl FromStr for FlowPublicKey {
    type Err = FlowConfigError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let invalid = || FlowConfigError::InvalidPublicKey(value.to_owned());
        let hex = value.strip_prefix(PUBLIC_KEY_PREFIX).ok_or_else(invalid)?;
        if hex.len() != 64 {
            return Err(invalid());
        }
        let mut bytes = [0; 32];
        // The length check above leaves no remainder.
        let (pairs, _) = hex.as_bytes().as_chunks::<2>();
        for (byte, &[high, low]) in bytes.iter_mut().zip(pairs) {
            let (Some(high), Some(low)) = (lower_hex_digit(high), lower_hex_digit(low)) else {
                return Err(invalid());
            };
            *byte = high << 4 | low;
        }
        Ok(Self(bytes))
    }
}

const fn lower_hex_digit(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        _ => None,
    }
}

impl TryFrom<String> for FlowPublicKey {
    type Error = FlowConfigError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        value.parse()
    }
}

impl From<FlowPublicKey> for String {
    fn from(key: FlowPublicKey) -> Self {
        key.to_string()
    }
}

impl fmt::Display for FlowPublicKey {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(PUBLIC_KEY_PREFIX)?;
        self.0
            .iter()
            .try_for_each(|byte| write!(formatter, "{byte:02x}"))
    }
}

/// User-visible name of a Flow peer.
///
/// Non-empty, without surrounding whitespace or control characters, and never
/// `self`, which `peer_channels` reserves for this machine.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct FlowPeerName(String);

impl FlowPeerName {
    /// Validates a peer name.
    pub fn new(name: impl Into<String>) -> Result<Self, FlowConfigError> {
        let name = name.into();
        if name.is_empty()
            || name.trim() != name
            || name.chars().any(char::is_control)
            || name == SELF_CHANNEL
        {
            return Err(FlowConfigError::InvalidPeerName(name));
        }
        Ok(Self(name))
    }

    /// Borrows the name.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for FlowPeerName {
    type Error = FlowConfigError;

    fn try_from(name: String) -> Result<Self, Self::Error> {
        Self::new(name)
    }
}

impl From<FlowPeerName> for String {
    fn from(name: FlowPeerName) -> Self {
        name.0
    }
}

impl Borrow<str> for FlowPeerName {
    fn borrow(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for FlowPeerName {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// A 0-based Easy-Switch host slot, as in HID++ `0x1814 ChangeHost`.
///
/// The config bound is the hardware maximum. The agent still checks a slot
/// against the probed host count of the connected device before it switches.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(try_from = "u8", into = "u8")]
pub struct FlowHostSlot(u8);

impl FlowHostSlot {
    /// Most host slots a Logitech Easy-Switch device exposes.
    pub const MAX_HOSTS: u8 = 3;

    /// Validates a 0-based slot.
    pub const fn new(slot: u8) -> Result<Self, FlowConfigError> {
        if slot < Self::MAX_HOSTS {
            Ok(Self(slot))
        } else {
            Err(FlowConfigError::SlotOutOfRange(slot))
        }
    }

    /// Returns the 0-based slot index.
    #[must_use]
    pub const fn get(self) -> u8 {
        self.0
    }
}

impl TryFrom<u8> for FlowHostSlot {
    type Error = FlowConfigError;

    fn try_from(slot: u8) -> Result<Self, Self::Error> {
        Self::new(slot)
    }
}

impl From<FlowHostSlot> for u8 {
    fn from(slot: FlowHostSlot) -> Self {
        slot.0
    }
}

/// Cross-machine Flow settings.
///
/// This is persisted configuration only. Discovery, transport, and device
/// handoff live outside `openlogi-core`. Every value upholds:
///
/// - peer names and public keys are unique;
/// - each layout entry names a configured peer, and each edge appears once;
/// - device keys are unique physical-device keys;
/// - each device has a `self` slot, maps only configured peers, and uses each
///   slot at most once.
///
/// Edit a value through [`Self::into_parts`] and `FlowConfig::try_from`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "FlowConfigParts", into = "FlowConfigParts")]
pub struct FlowConfig {
    /// Whether Flow handoff is enabled. Carries no invariant.
    pub enabled: bool,
    /// Whether the Control modifier must be held while crossing a configured
    /// screen edge. Carries no invariant.
    pub require_modifier: bool,
    peers: Vec<FlowPeer>,
    layout: Vec<FlowLayout>,
    devices: Vec<FlowDevice>,
}

impl FlowConfig {
    /// Whether this value is exactly the implicit default and can be omitted
    /// from `config.toml`.
    #[must_use]
    pub fn is_default(&self) -> bool {
        self == &Self::default()
    }

    /// Trusted peer machines.
    #[must_use]
    pub fn peers(&self) -> &[FlowPeer] {
        &self.peers
    }

    /// Looks up a peer by name.
    #[must_use]
    pub fn peer(&self, name: &str) -> Option<&FlowPeer> {
        self.peers.iter().find(|peer| peer.name.as_str() == name)
    }

    /// Mappings from this machine's screen edges to peers.
    #[must_use]
    pub fn layout(&self) -> &[FlowLayout] {
        &self.layout
    }

    /// Devices that participate in Flow and their Easy-Switch slots.
    #[must_use]
    pub fn devices(&self) -> &[FlowDevice] {
        &self.devices
    }

    /// Returns the unvalidated parts for editing.
    #[must_use]
    pub fn into_parts(self) -> FlowConfigParts {
        FlowConfigParts {
            enabled: self.enabled,
            require_modifier: self.require_modifier,
            peers: self.peers,
            layout: self.layout,
            devices: self.devices,
        }
    }
}

impl TryFrom<FlowConfigParts> for FlowConfig {
    type Error = FlowConfigError;

    fn try_from(parts: FlowConfigParts) -> Result<Self, Self::Error> {
        validate(&parts)?;
        Ok(Self {
            enabled: parts.enabled,
            require_modifier: parts.require_modifier,
            peers: parts.peers,
            layout: parts.layout,
            devices: parts.devices,
        })
    }
}

impl From<FlowConfig> for FlowConfigParts {
    fn from(config: FlowConfig) -> Self {
        config.into_parts()
    }
}

fn validate(parts: &FlowConfigParts) -> Result<(), FlowConfigError> {
    let mut names = BTreeSet::new();
    let mut keys = BTreeSet::new();
    for peer in &parts.peers {
        if !names.insert(peer.name.as_str()) {
            return Err(FlowConfigError::DuplicatePeerName(peer.name.clone()));
        }
        if !keys.insert(peer.public_key) {
            return Err(FlowConfigError::DuplicatePeerKey(peer.public_key));
        }
    }
    let mut edges = BTreeSet::new();
    for placement in &parts.layout {
        if !names.contains(placement.peer.as_str()) {
            return Err(FlowConfigError::UnknownLayoutPeer(placement.peer.clone()));
        }
        if !edges.insert(placement.edge) {
            return Err(FlowConfigError::DuplicateLayoutEdge(placement.edge));
        }
    }
    let mut devices = BTreeSet::new();
    for device in &parts.devices {
        if !devices.insert(device.key.as_str()) {
            return Err(FlowConfigError::DuplicateDevice(
                device.key.as_str().to_owned(),
            ));
        }
        if let Some(peer) = device
            .peer_slots
            .keys()
            .find(|peer| !names.contains(peer.as_str()))
        {
            return Err(FlowConfigError::UnknownDevicePeer {
                device: device.key.as_str().to_owned(),
                peer: peer.clone(),
            });
        }
    }
    Ok(())
}

/// Unvalidated `[flow]` contents: the persisted shape and the editing form.
///
/// Convert with `FlowConfig::try_from` to check every invariant.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FlowConfigParts {
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
    /// Devices that participate in Flow and their Easy-Switch slots.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub devices: Vec<FlowDevice>,
}

/// One trusted Flow peer and its optional manual address hints.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FlowPeer {
    /// User-visible peer name, also referenced by layout and device slots.
    pub name: FlowPeerName,
    /// Peer's pinned Ed25519 public key.
    pub public_key: FlowPublicKey,
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
    pub peer: FlowPeerName,
}

/// Screen edge that can trigger a Flow handoff.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
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
///
/// Persisted as `key` plus `peer_channels`, a map from `self` and peer names
/// to slots. Every value has a `self` slot and uses each slot at most once.
/// The key is the device's identity key, `serial:` or `unit:`: peers name a
/// device by its identity, and a route key such as `receiver:<uid>:slot:<n>`
/// cannot be matched against one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "FlowDeviceWire", into = "FlowDeviceWire")]
pub struct FlowDevice {
    key: PhysicalDeviceKey,
    self_slot: FlowHostSlot,
    peer_slots: BTreeMap<FlowPeerName, FlowHostSlot>,
}

impl FlowDevice {
    /// Validates that `key` is an identity key and that this machine and each
    /// peer use distinct slots. The key is stored in the inventory's
    /// lowercase form, so it matches the device in any case it was written.
    pub fn new(
        key: PhysicalDeviceKey,
        self_slot: FlowHostSlot,
        peer_slots: BTreeMap<FlowPeerName, FlowHostSlot>,
    ) -> Result<Self, FlowConfigError> {
        let Some(key) = key.to_identity() else {
            return Err(FlowConfigError::InvalidDeviceKey(key.into_string()));
        };
        let mut used = BTreeSet::from([self_slot]);
        if let Some(slot) = peer_slots.values().find(|slot| !used.insert(**slot)) {
            return Err(FlowConfigError::DuplicateSlot {
                device: key.as_str().to_owned(),
                slot: slot.get(),
            });
        }
        Ok(Self {
            key,
            self_slot,
            peer_slots,
        })
    }

    /// The device's identity key, such as `"unit:0f1e2d3c"`.
    #[must_use]
    pub const fn key(&self) -> &PhysicalDeviceKey {
        &self.key
    }

    /// Slot the device uses to reach this machine.
    #[must_use]
    pub const fn self_slot(&self) -> FlowHostSlot {
        self.self_slot
    }

    /// Slot the device uses to reach `peer`, if mapped.
    #[must_use]
    pub fn peer_slot(&self, peer: &str) -> Option<FlowHostSlot> {
        self.peer_slots.get(peer).copied()
    }

    /// Slots for every mapped peer.
    #[must_use]
    pub const fn peer_slots(&self) -> &BTreeMap<FlowPeerName, FlowHostSlot> {
        &self.peer_slots
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct FlowDeviceWire {
    key: String,
    peer_channels: BTreeMap<String, u8>,
}

impl TryFrom<FlowDeviceWire> for FlowDevice {
    type Error = FlowConfigError;

    fn try_from(wire: FlowDeviceWire) -> Result<Self, Self::Error> {
        let key = PhysicalDeviceKey::parse(&wire.key)
            .ok_or_else(|| FlowConfigError::InvalidDeviceKey(wire.key.clone()))?;
        let mut self_slot = None;
        let mut peer_slots = BTreeMap::new();
        for (name, slot) in wire.peer_channels {
            let slot = FlowHostSlot::new(slot)?;
            if name == SELF_CHANNEL {
                self_slot = Some(slot);
            } else {
                peer_slots.insert(FlowPeerName::new(name)?, slot);
            }
        }
        let self_slot = self_slot.ok_or(FlowConfigError::MissingSelfSlot(wire.key))?;
        Self::new(key, self_slot, peer_slots)
    }
}

impl From<FlowDevice> for FlowDeviceWire {
    fn from(device: FlowDevice) -> Self {
        let mut peer_channels = BTreeMap::from([(SELF_CHANNEL.to_owned(), device.self_slot.get())]);
        peer_channels.extend(
            device
                .peer_slots
                .into_iter()
                .map(|(name, slot)| (String::from(name), slot.get())),
        );
        Self {
            key: device.key.into_string(),
            peer_channels,
        }
    }
}
