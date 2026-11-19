//! The enumerator's open channels and their retirement: a node's channel
//! serves until it is retired, then drains until nothing else holds it, and
//! only a later pass may open the node again. A channel retired because it
//! failed keeps that cause through its drain, so the node's next open is
//! known to replace it.

use std::{
    collections::{HashMap, HashSet, hash_map::Entry},
    hash::Hash,
};

use tracing::warn;

/// One node's place in the channel lifecycle.
enum ChannelState<Channel> {
    /// Serving.
    Active(Channel),
    /// Serving the channel the latest pass opened in place of a failed one
    /// while the node was still replayed online: a reconnect no published
    /// snapshot showed. The next pass serves it as [`Self::Active`].
    Replaced(Channel),
    /// Retired, draining toward quiescence so the node may reopen.
    Retiring(Channel, Retirement),
    /// A failed channel drained while the OS kept listing its node; the
    /// node's next open replaces it.
    AwaitingReopen,
}

/// Why a channel was retired, which decides what reopening its node is.
#[derive(Clone, Copy)]
enum Retirement {
    /// The node left the listing. It comes back as a new arrival, which the
    /// agent's offline→online transition already reconfigures.
    Absent,
    /// Its probes kept failing, or capture reported it dead, while the OS
    /// kept listing the node.
    Failed,
}

impl<Channel> ChannelState<Channel> {
    fn serving(&self) -> Option<&Channel> {
        match self {
            Self::Active(channel) | Self::Replaced(channel) => Some(channel),
            Self::Retiring(..) | Self::AwaitingReopen => None,
        }
    }

    /// Move to the state `step` makes of this one.
    fn advance(&mut self, step: impl FnOnce(Self) -> Self) {
        *self = step(std::mem::replace(self, Self::AwaitingReopen));
    }

    /// A replacement serves on as an ordinary active channel.
    fn settle_replacement(self) -> Self {
        match self {
            Self::Replaced(channel) => Self::Active(channel),
            state => state,
        }
    }
}

/// Per-node channel lifecycle, generic so ownership transitions can be tested
/// without constructing a platform HID node. One map on purpose: a node
/// holding an active *and* a retiring channel — two live opens of one OS
/// node, the state the macOS dead-delivery hazard rides on — used to be
/// representable across two maps and guarded only by a release-silent
/// `debug_assert`; keyed by node in a single map, it cannot exist. The
/// failed-channel retirement and the replacement it leads to live here for
/// the same reason: kept as node sets beside the map, they could mark a node
/// whose channel still served, or one both evicted and just replaced.
pub(super) struct ChannelCache<Node, Channel> {
    channels: HashMap<Node, ChannelState<Channel>>,
}

impl<Node, Channel> Default for ChannelCache<Node, Channel> {
    fn default() -> Self {
        Self {
            channels: HashMap::new(),
        }
    }
}

impl<Node: Eq + Hash + Clone, Channel> ChannelCache<Node, Channel> {
    pub(super) fn get(&self, node: &Node) -> Option<&Channel> {
        self.channels.get(node)?.serving()
    }

    /// The serving channels, for callers that sweep what is currently open.
    pub(super) fn active_iter(&self) -> impl Iterator<Item = (&Node, &Channel)> {
        self.channels
            .iter()
            .filter_map(|(node, state)| Some((node, state.serving()?)))
    }

    /// Start a pass: the previous pass's replacements serve on as ordinary
    /// active channels.
    pub(super) fn begin_pass(&mut self) {
        for state in self.channels.values_mut() {
            state.advance(ChannelState::settle_replacement);
        }
    }

    /// Serve a freshly opened channel for `node`. Reopening a node whose
    /// failed channel drained replaces that channel, unobserved only while
    /// `replayed_online` — the ledger still replays the node's last-good
    /// inventory, so no published snapshot showed it offline.
    pub(super) fn open(
        &mut self,
        node: Node,
        channel: Channel,
        replayed_online: impl FnOnce() -> bool,
    ) {
        let state = match self.channels.entry(node) {
            Entry::Vacant(entry) => {
                entry.insert(ChannelState::Active(channel));
                return;
            }
            Entry::Occupied(entry) => entry.into_mut(),
        };
        match state {
            // A retiring channel is still draining toward quiescence, and
            // `prepare_open` refuses the node until it has — so this arm is
            // unreachable through current callers. Keeping the draining
            // channel (and dropping the newcomer, which closes its OS handle
            // promptly) preserves the one-channel-per-node invariant either
            // way.
            ChannelState::Retiring(..) => {
                warn!("refusing to replace a retiring channel — dropping the fresh open");
                drop(channel);
            }
            ChannelState::AwaitingReopen if replayed_online() => {
                *state = ChannelState::Replaced(channel);
            }
            ChannelState::Active(_) | ChannelState::Replaced(_) | ChannelState::AwaitingReopen => {
                *state = ChannelState::Active(channel);
            }
        }
    }

    /// Retire `node`'s channel because its probes kept failing or capture
    /// reported it dead, so the node's next open replaces it. `false` when
    /// no channel was serving: an earlier retirement keeps its place and its
    /// drain clock, and a node without a channel still awaits the
    /// replacement.
    pub(super) fn evict(&mut self, node: &Node) -> bool {
        let state = self
            .channels
            .entry(node.clone())
            .or_insert(ChannelState::AwaitingReopen);
        let retired = state.serving().is_some();
        state.advance(|state| match state {
            ChannelState::Active(channel)
            | ChannelState::Replaced(channel)
            | ChannelState::Retiring(channel, _) => {
                ChannelState::Retiring(channel, Retirement::Failed)
            }
            ChannelState::AwaitingReopen => ChannelState::AwaitingReopen,
        });
        retired
    }

    /// The pass's snapshot shows `node` offline, so the agent observes its
    /// return: a replacement this pass opened for it is an ordinary
    /// reconnect.
    pub(super) fn surfaced_offline(&mut self, node: &Node) {
        if let Some(state) = self.channels.get_mut(node) {
            state.advance(ChannelState::settle_replacement);
        }
    }

    /// Whether the latest pass opened a replacement no published snapshot
    /// observed.
    pub(super) fn has_replacement(&self) -> bool {
        self.channels
            .values()
            .any(|state| matches!(state, ChannelState::Replaced(_)))
    }

    /// Whether this node may be opened during the current pass. A quiescent
    /// retirement is dropped here — a failed one leaves the node awaiting
    /// its replacement — but opening remains deferred to a later pass.
    pub(super) fn prepare_open(
        &mut self,
        node: &Node,
        is_quiescent: impl FnOnce(&Channel) -> bool,
    ) -> bool {
        let Some(state) = self.channels.get_mut(node) else {
            return true;
        };
        let ChannelState::Retiring(channel, retirement) = state else {
            return true;
        };
        if is_quiescent(channel) {
            match *retirement {
                Retirement::Failed => *state = ChannelState::AwaitingReopen,
                Retirement::Absent => {
                    self.channels.remove(node);
                }
            }
        }
        false
    }

    /// Retire every serving node not in `seen`; returns how many retired.
    pub(super) fn retire_absent(&mut self, seen: &HashSet<Node>) -> usize {
        let mut retired = 0;
        for (node, state) in &mut self.channels {
            if seen.contains(node) || state.serving().is_none() {
                continue;
            }
            retired += 1;
            state.advance(|state| match state {
                ChannelState::Active(channel) | ChannelState::Replaced(channel) => {
                    ChannelState::Retiring(channel, Retirement::Absent)
                }
                state => state,
            });
        }
        retired
    }

    /// Forget what the listing no longer shows: a quiescent retirement is
    /// dropped, and a node that left keeps no failed-channel cause.
    pub(super) fn reap_absent(
        &mut self,
        seen: &HashSet<Node>,
        is_quiescent: impl Fn(&Channel) -> bool,
    ) {
        self.channels.retain(|node, state| {
            if seen.contains(node) {
                return true;
            }
            match state {
                ChannelState::Active(_) | ChannelState::Replaced(_) => true,
                ChannelState::Retiring(channel, retirement) => {
                    *retirement = Retirement::Absent;
                    !is_quiescent(channel)
                }
                ChannelState::AwaitingReopen => false,
            }
        });
    }

    #[cfg(test)]
    pub(super) fn is_retiring(&self, node: &Node) -> bool {
        matches!(self.channels.get(node), Some(ChannelState::Retiring(..)))
    }
}
