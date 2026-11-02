//! The enumerator's open channels and their retirement: a node's channel
//! serves until it is retired, then drains until nothing else holds it, and
//! only a later pass may open the node again.

use std::{
    collections::{HashMap, HashSet, hash_map::Entry},
    hash::Hash,
};

use tracing::warn;

/// One channel's place in its lifecycle: serving, or retired and draining
/// toward quiescence so its node may reopen.
enum ChannelState<Channel> {
    Active(Channel),
    Retiring(Channel),
}

/// Per-node channel lifecycle, generic so ownership transitions can be tested
/// without constructing a platform HID node. One map on purpose: a node
/// holding an active *and* a retiring channel — two live opens of one OS
/// node, the state the macOS dead-delivery hazard rides on — used to be
/// representable across two maps and guarded only by a release-silent
/// `debug_assert`; keyed by node in a single map, it cannot exist.
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
        match self.channels.get(node)? {
            ChannelState::Active(channel) => Some(channel),
            ChannelState::Retiring(_) => None,
        }
    }

    /// The active channels, for callers that sweep what is currently serving.
    pub(super) fn active_iter(&self) -> impl Iterator<Item = (&Node, &Channel)> {
        self.channels
            .iter()
            .filter_map(|(node, state)| match state {
                ChannelState::Active(channel) => Some((node, channel)),
                ChannelState::Retiring(_) => None,
            })
    }

    pub(super) fn insert(&mut self, node: Node, channel: Channel) {
        match self.channels.entry(node) {
            Entry::Occupied(mut entry) => match entry.get_mut() {
                state @ ChannelState::Active(_) => *state = ChannelState::Active(channel),
                // A retiring channel is still draining toward quiescence, and
                // `prepare_open` refuses the node until it has — so this arm
                // is unreachable through current callers. Keeping the
                // draining channel (and dropping the newcomer, which closes
                // its OS handle promptly) preserves the one-channel-per-node
                // invariant either way.
                ChannelState::Retiring(_) => {
                    warn!("refusing to replace a retiring channel — dropping the fresh open");
                    drop(channel);
                }
            },
            Entry::Vacant(entry) => {
                entry.insert(ChannelState::Active(channel));
            }
        }
    }

    /// Move an active channel into the retiring state. `false` when the node
    /// is unknown or already retiring (the original retirement keeps its
    /// place — and its drain clock).
    pub(super) fn retire_node(&mut self, node: &Node) -> bool {
        let Some(state) = self.channels.get_mut(node) else {
            return false;
        };
        if matches!(state, ChannelState::Retiring(_)) {
            return false;
        }
        let Some(ChannelState::Active(channel) | ChannelState::Retiring(channel)) =
            self.channels.remove(node)
        else {
            return false;
        };
        self.channels
            .insert(node.clone(), ChannelState::Retiring(channel));
        true
    }

    /// Whether this node may be opened during the current tick. A quiescent
    /// retirement is dropped here, but opening remains deferred to a later tick.
    pub(super) fn prepare_open(
        &mut self,
        node: &Node,
        is_quiescent: impl FnOnce(&Channel) -> bool,
    ) -> bool {
        let Some(ChannelState::Retiring(channel)) = self.channels.get(node) else {
            return true;
        };
        if is_quiescent(channel) {
            self.channels.remove(node);
        }
        false
    }

    /// Retire every active node not in `seen`; returns how many retired.
    pub(super) fn retire_absent(&mut self, seen: &HashSet<Node>) -> usize {
        let absent = self
            .active_iter()
            .map(|(node, _)| node)
            .filter(|node| !seen.contains(*node))
            .cloned()
            .collect::<Vec<_>>();
        absent
            .into_iter()
            .filter(|node| self.retire_node(node))
            .count()
    }

    pub(super) fn reap_absent(
        &mut self,
        seen: &HashSet<Node>,
        is_quiescent: impl Fn(&Channel) -> bool,
    ) {
        self.channels.retain(|node, state| match state {
            ChannelState::Active(_) => true,
            ChannelState::Retiring(channel) => seen.contains(node) || !is_quiescent(channel),
        });
    }

    #[cfg(test)]
    pub(super) fn is_retiring(&self, node: &Node) -> bool {
        matches!(self.channels.get(node), Some(ChannelState::Retiring(_)))
    }
}
