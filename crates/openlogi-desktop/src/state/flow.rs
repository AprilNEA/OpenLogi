//! Flow: the `[flow]` section of `config.toml` as the Flow window edits it,
//! and the link state the agent reports for each peer.
//!
//! The agent owns every peer session; this module only reads their states
//! from the snapshot. Each edit is built on [`FlowConfigParts`] and checked by
//! `FlowConfig::try_from`, so a change that would break one of `[flow]`'s
//! invariants is refused before anything is written.

use std::collections::BTreeMap;

use openlogi_core::config::{
    FlowConfig, FlowConfigError, FlowDevice, FlowEdge, FlowHostSlot, FlowLayout, FlowPeerName,
};
use openlogi_core::device::DeviceKind;
use openlogi_core::device_order::PhysicalDeviceKey;
use openlogi_ipc::{FlowLinkState, FlowStatus};
use tracing::warn;

use super::events::StateEvents;
use super::{AppState, Load, StateEvent};

/// A computer one of a device's Easy-Switch channels leads to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum FlowHost {
    /// This computer.
    This,
    /// A paired peer.
    Peer(FlowPeerName),
}

/// A device the Flow window lists: one this computer knows that can switch
/// computers with Easy-Switch, or one `[flow.devices]` names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct FlowDeviceRow {
    pub(crate) key: PhysicalDeviceKey,
    pub(crate) name: String,
    /// `None` for a configured device this computer has never seen.
    pub(crate) kind: Option<DeviceKind>,
    /// The channel the device is on now, when a read has said so.
    pub(crate) current: Option<FlowHostSlot>,
    /// Its `[flow.devices]` entry, when it takes part in Flow.
    pub(crate) config: Option<FlowDevice>,
}

impl FlowDeviceRow {
    /// The channel this device uses to reach `host`.
    pub(crate) fn slot(&self, host: &FlowHost) -> Option<FlowHostSlot> {
        let config = self.config.as_ref()?;
        match host {
            FlowHost::This => Some(config.self_slot()),
            FlowHost::Peer(peer) => config.peer_slot(peer.as_str()),
        }
    }

    /// Whether another computer than `host` already uses `slot`.
    pub(crate) fn slot_taken(&self, host: &FlowHost, slot: FlowHostSlot) -> bool {
        let Some(config) = &self.config else {
            return false;
        };
        let by_this = *host != FlowHost::This && config.self_slot() == slot;
        let by_peer = config.peer_slots().iter().any(|(peer, used)| {
            *used == slot && !matches!(host, FlowHost::Peer(own) if own == peer)
        });
        by_this || by_peer
    }
}

/// How far Flow is from switching.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FlowHealth {
    /// `[flow] enabled` is off.
    Off,
    /// No peer is paired.
    NoPeers,
    /// At least one edge leads to a peer the pointer can cross to now.
    Ready,
    /// Peers are paired, but none can be reached.
    Blocked,
}

/// Something that keeps one peer from working.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum FlowIssue {
    /// No edge of this computer leads to the peer.
    Unplaced(FlowPeerName),
    /// No device has a channel for the peer.
    NoDevice(FlowPeerName),
    /// The agent has no working session with the peer; `None` when the agent
    /// is not connected or has not reported the peer yet.
    Unreachable {
        peer: FlowPeerName,
        link: Option<FlowLinkState>,
    },
    /// A device that takes part in Flow stays behind when the pointer moves
    /// to the peer.
    NotFollowing { device: String, peer: FlowPeerName },
}

/// What the Flow window's summary and the main window's Flow button show.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct FlowReadiness {
    pub(crate) health: FlowHealth,
    /// Edges that lead to a reachable peer, in layout order.
    pub(crate) routes: Vec<FlowLayout>,
    pub(crate) issues: Vec<FlowIssue>,
}

/// Decide how ready `config` is. The agent arms an edge only for a connected
/// peer and hands over only the devices with a channel for it, so that is
/// what counts as reachable.
pub(crate) fn readiness(
    config: &FlowConfig,
    link: impl Fn(&str) -> Option<FlowLinkState>,
    device_name: impl Fn(&PhysicalDeviceKey) -> String,
) -> FlowReadiness {
    let quiet = |health| FlowReadiness {
        health,
        routes: Vec::new(),
        issues: Vec::new(),
    };
    if !config.enabled {
        return quiet(FlowHealth::Off);
    }
    if config.peers().is_empty() {
        return quiet(FlowHealth::NoPeers);
    }
    let mut routes = Vec::new();
    let mut issues = Vec::new();
    for peer in config.peers() {
        let name = &peer.name;
        let edges: Vec<&FlowLayout> = config
            .layout()
            .iter()
            .filter(|layout| &layout.peer == name)
            .collect();
        let (following, staying): (Vec<&FlowDevice>, Vec<&FlowDevice>) = config
            .devices()
            .iter()
            .partition(|device| device.peer_slot(name.as_str()).is_some());
        if edges.is_empty() {
            issues.push(FlowIssue::Unplaced(name.clone()));
            continue;
        }
        if following.is_empty() {
            issues.push(FlowIssue::NoDevice(name.clone()));
            continue;
        }
        match link(name.as_str()) {
            Some(FlowLinkState::Connected) => routes.extend(edges.into_iter().cloned()),
            state => issues.push(FlowIssue::Unreachable {
                peer: name.clone(),
                link: state,
            }),
        }
        issues.extend(staying.into_iter().map(|device| FlowIssue::NotFollowing {
            device: device_name(device.key()),
            peer: name.clone(),
        }));
    }
    FlowReadiness {
        health: if routes.is_empty() {
            FlowHealth::Blocked
        } else {
            FlowHealth::Ready
        },
        routes,
        issues,
    }
}

/// `config` with `peer` on `edge`. A peer already there takes `peer`'s old
/// edge, or leaves the arrangement when `peer` had none.
pub(crate) fn placed(
    config: &FlowConfig,
    peer: &FlowPeerName,
    edge: FlowEdge,
) -> Result<FlowConfig, FlowConfigError> {
    let mut parts = config.clone().into_parts();
    let previous = parts
        .layout
        .iter()
        .find(|layout| &layout.peer == peer)
        .map(|layout| layout.edge);
    parts.layout.retain(|layout| &layout.peer != peer);
    match previous {
        Some(old) => {
            if let Some(occupant) = parts.layout.iter_mut().find(|layout| layout.edge == edge) {
                occupant.edge = old;
            }
        }
        None => parts.layout.retain(|layout| layout.edge != edge),
    }
    parts.layout.push(FlowLayout {
        edge,
        peer: peer.clone(),
    });
    FlowConfig::try_from(parts)
}

/// `config` with `device` on `slot` for `host`, or no longer reaching `host`
/// when `slot` is `None`. A device that stops reaching this computer leaves
/// Flow. A device joining Flow for a peer reaches this computer on `current`,
/// the channel it is on now, or else on the lowest channel the peer leaves
/// free; the window shows that channel so a wrong guess can be corrected.
pub(crate) fn with_channel(
    config: &FlowConfig,
    device: &PhysicalDeviceKey,
    host: &FlowHost,
    slot: Option<FlowHostSlot>,
    current: Option<FlowHostSlot>,
) -> Result<FlowConfig, FlowConfigError> {
    let mut parts = config.clone().into_parts();
    let index = parts.devices.iter().position(|entry| entry.key() == device);
    let existing = index.and_then(|index| parts.devices.get(index));
    let entry = match (host, slot) {
        (FlowHost::This, None) => None,
        (FlowHost::This, Some(slot)) => Some(FlowDevice::new(
            device.clone(),
            slot,
            existing
                .map(|entry| entry.peer_slots().clone())
                .unwrap_or_default(),
        )?),
        (FlowHost::Peer(peer), slot) => {
            let (self_slot, mut peers) = match (existing, slot) {
                (Some(entry), _) => (entry.self_slot(), entry.peer_slots().clone()),
                (None, Some(slot)) => (joining_self_slot(current, slot)?, BTreeMap::new()),
                (None, None) => return Ok(config.clone()),
            };
            match slot {
                Some(slot) => peers.insert(peer.clone(), slot),
                None => peers.remove(peer),
            };
            Some(FlowDevice::new(device.clone(), self_slot, peers)?)
        }
    };
    match (index, entry) {
        (Some(index), Some(entry)) => {
            if let Some(slot) = parts.devices.get_mut(index) {
                *slot = entry;
            }
        }
        (Some(index), None) => {
            parts.devices.remove(index);
        }
        (None, Some(entry)) => parts.devices.push(entry),
        (None, None) => {}
    }
    FlowConfig::try_from(parts)
}

fn joining_self_slot(
    current: Option<FlowHostSlot>,
    peer: FlowHostSlot,
) -> Result<FlowHostSlot, FlowConfigError> {
    if let Some(current) = current.filter(|current| *current != peer) {
        return Ok(current);
    }
    let free = (0..FlowHostSlot::MAX_HOSTS)
        .find(|slot| *slot != peer.get())
        .unwrap_or_default();
    FlowHostSlot::new(free)
}

/// `config` without `peer`: no edge leads to it and no device keeps a
/// channel for it.
pub(crate) fn forgotten(
    config: &FlowConfig,
    peer: &FlowPeerName,
) -> Result<FlowConfig, FlowConfigError> {
    let mut parts = config.clone().into_parts();
    parts.peers.retain(|entry| &entry.name != peer);
    parts.layout.retain(|layout| &layout.peer != peer);
    parts.devices = parts
        .devices
        .into_iter()
        .map(|device| {
            let mut peers = device.peer_slots().clone();
            peers.remove(peer);
            FlowDevice::new(device.key().clone(), device.self_slot(), peers)
        })
        .collect::<Result<_, _>>()?;
    FlowConfig::try_from(parts)
}

/// Device kinds that move the pointer or type, the ones a Flow handoff is for.
fn takes_part_in_flow(kind: DeviceKind) -> bool {
    kind.is_keyboard_family()
        || matches!(
            kind,
            DeviceKind::Mouse | DeviceKind::Trackball | DeviceKind::Touchpad
        )
}

impl AppState {
    /// The `[flow]` section as last loaded or saved.
    #[must_use]
    pub(crate) fn flow_config(&self) -> &FlowConfig {
        &self.config.flow
    }

    /// The agent's link state with `peer`, or `None` while no agent is
    /// connected or it has not reported the peer.
    #[must_use]
    pub(crate) fn flow_link(&self, peer: &str) -> Option<FlowLinkState> {
        self.agent_status()?;
        self.agent
            .flow
            .peers
            .iter()
            .find(|status| status.name.as_str() == peer)
            .map(|status| status.state)
    }

    /// How ready Flow is to switch.
    #[must_use]
    pub(crate) fn flow_readiness(&self) -> FlowReadiness {
        let devices = self.flow_devices();
        readiness(
            &self.config.flow,
            |peer| self.flow_link(peer),
            |key| {
                devices
                    .iter()
                    .find(|row| &row.key == key)
                    .map_or_else(|| key.as_str().to_owned(), |row| row.name.clone())
            },
        )
    }

    /// The devices the Flow window lists, in the device list's order, then
    /// configured devices this computer has never seen.
    #[must_use]
    pub(crate) fn flow_devices(&self) -> Vec<FlowDeviceRow> {
        let configured = self.config.flow.devices();
        let mut rows: Vec<FlowDeviceRow> = Vec::new();
        for record in self.devices() {
            if !record.is_persistent() || !takes_part_in_flow(record.kind) {
                continue;
            }
            let Some(key) =
                PhysicalDeviceKey::parse(&record.config_key).and_then(|key| key.to_identity())
            else {
                continue;
            };
            if rows.iter().any(|row| row.key == key) {
                continue;
            }
            let current = match self.pointer.reads.easy_switch_load(&record.device_key()) {
                Some(Load::Ready(hosts)) => {
                    hosts.current.and_then(|slot| FlowHostSlot::new(slot).ok())
                }
                _ => None,
            };
            rows.push(FlowDeviceRow {
                config: configured
                    .iter()
                    .find(|device| device.key() == &key)
                    .cloned(),
                key,
                name: record.display_name.clone(),
                kind: Some(record.kind),
                current,
            });
        }
        for device in configured {
            if !rows.iter().any(|row| &row.key == device.key()) {
                rows.push(FlowDeviceRow {
                    key: device.key().clone(),
                    name: device.key().as_str().to_owned(),
                    kind: None,
                    current: None,
                    config: Some(device.clone()),
                });
            }
        }
        rows
    }

    /// Turn Flow on or off.
    pub(crate) fn commit_flow_enabled(&mut self, enabled: bool) -> StateEvents {
        self.commit_flow("Flow switch", |flow| {
            let mut flow = flow.clone();
            flow.enabled = enabled;
            Ok(flow)
        })
    }

    /// Require Control to be held while the pointer crosses an edge, or not.
    pub(crate) fn commit_flow_require_modifier(&mut self, required: bool) -> StateEvents {
        self.commit_flow("Flow modifier", |flow| {
            let mut flow = flow.clone();
            flow.require_modifier = required;
            Ok(flow)
        })
    }

    /// Put `peer` beyond `edge` of this computer's screen.
    pub(crate) fn commit_flow_edge(&mut self, peer: &FlowPeerName, edge: FlowEdge) -> StateEvents {
        self.commit_flow("Flow arrangement", |flow| placed(flow, peer, edge))
    }

    /// Set the Easy-Switch channel `device` uses for `host`; see
    /// [`with_channel`].
    pub(crate) fn commit_flow_channel(
        &mut self,
        device: &PhysicalDeviceKey,
        host: &FlowHost,
        slot: Option<FlowHostSlot>,
    ) -> StateEvents {
        let current = self
            .flow_devices()
            .into_iter()
            .find(|row| &row.key == device)
            .and_then(|row| row.current);
        self.commit_flow("Flow channel", |flow| {
            with_channel(flow, device, host, slot, current)
        })
    }

    /// Stop trusting `peer` and remove every reference to it.
    pub(crate) fn commit_flow_forget(&mut self, peer: &FlowPeerName) -> StateEvents {
        self.commit_flow("Flow peer removal", |flow| forgotten(flow, peer))
    }

    fn commit_flow(
        &mut self,
        what: &str,
        edit: impl FnOnce(&FlowConfig) -> Result<FlowConfig, FlowConfigError>,
    ) -> StateEvents {
        let events = StateEvents::from(StateEvent::FlowChanged);
        let flow = match edit(&self.config.flow) {
            Ok(flow) => flow,
            Err(error) => {
                warn!(%error, what, "refused a Flow edit that breaks [flow]");
                return events;
            }
        };
        if flow == self.config.flow {
            return events;
        }
        self.config.edit(|config| config.flow = flow);
        if self.persist_and_reload(what) {
            events
        } else {
            // The write failed and the rollback restored `[flow]`; the root
            // view shows the persistence error.
            events.and(StateEvent::SettingsChanged)
        }
    }

    /// Adopt the agent's Flow report.
    pub(super) fn set_flow_status(&mut self, status: &FlowStatus) -> StateEvents {
        if self.agent.flow == *status {
            return StateEvents::none();
        }
        self.agent.flow = status.clone();
        StateEvent::FlowChanged.into()
    }
}
