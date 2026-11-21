//! Which node connect/disconnect events concern a Logitech node.

use std::collections::HashSet;
use std::hash::Hash;

use crate::LOGITECH_VENDOR_ID;

/// The Logitech nodes whose removal has not been seen yet.
///
/// A removed node can no longer be queried, so this is what tells its removal
/// apart from another vendor's. It is not the handle cache: an enumeration
/// replaces that cache, and a removal can still be queued behind a later
/// enumeration that no longer lists the node.
pub(super) struct LogitechNodes<Id> {
    nodes: HashSet<Id>,
}

impl<Id> Default for LogitechNodes<Id> {
    fn default() -> Self {
        Self {
            nodes: HashSet::new(),
        }
    }
}

impl<Id: Eq + Hash> LogitechNodes<Id> {
    /// Record the Logitech nodes of an enumeration, given as `(id, vendor_id)`
    /// pairs. A node it no longer lists stays until its removal is read.
    pub(super) fn enumerated(&mut self, nodes: impl IntoIterator<Item = (Id, u16)>) {
        self.nodes.extend(
            nodes
                .into_iter()
                .filter(|&(_, vendor_id)| vendor_id == LOGITECH_VENDOR_ID)
                .map(|(id, _)| id),
        );
    }

    /// Whether a node's arrival concerns Logitech, given the vendor ids of the
    /// devices behind it, or `None` when the node could not be queried. An
    /// unqueried node counts, since hiding a Logitech arrival costs more than
    /// one extra rescan, and the rescan it causes records the node if it is one.
    pub(super) fn arrived(
        &mut self,
        id: Id,
        vendor_ids: Option<impl IntoIterator<Item = u16>>,
    ) -> bool {
        let Some(vendor_ids) = vendor_ids else {
            return true;
        };
        let logitech = vendor_ids
            .into_iter()
            .any(|vendor_id| vendor_id == LOGITECH_VENDOR_ID);
        if logitech {
            self.nodes.insert(id);
        }
        logitech
    }

    /// Whether a node's removal concerns Logitech.
    pub(super) fn departed(&mut self, id: &Id) -> bool {
        self.nodes.remove(id)
    }
}

#[cfg(test)]
mod tests {
    use super::LogitechNodes;
    use crate::LOGITECH_VENDOR_ID;

    const APPLE: u16 = 0x05ac;

    #[test]
    fn another_vendors_node_raises_nothing_either_way() {
        let mut nodes = LogitechNodes::default();
        nodes.enumerated([(1, APPLE)]);
        assert!(!nodes.departed(&1), "an enumerated node of another vendor");
        assert!(!nodes.arrived(2, Some([APPLE, APPLE])));
        assert!(!nodes.departed(&2), "an arrived node of another vendor");
    }

    #[test]
    fn a_logitech_arrival_and_its_removal_each_raise_once() {
        let mut nodes = LogitechNodes::default();
        assert!(nodes.arrived(1, Some([LOGITECH_VENDOR_ID])));
        assert!(nodes.departed(&1));
        assert!(
            !nodes.departed(&1),
            "a repeated removal is not a new change"
        );
    }

    #[test]
    fn an_unqueried_arrival_raises_but_is_not_recorded() {
        let mut nodes = LogitechNodes::default();
        assert!(nodes.arrived(1, None::<[u16; 0]>));
        assert!(
            !nodes.departed(&1),
            "only the rescan it caused may record it"
        );
    }

    #[test]
    fn an_enumerated_logitech_nodes_removal_raises() {
        let mut nodes = LogitechNodes::default();
        nodes.enumerated([(1, LOGITECH_VENDOR_ID), (2, APPLE)]);
        assert!(nodes.departed(&1));
    }

    #[test]
    fn a_removal_queued_behind_a_later_enumeration_still_raises() {
        let mut nodes = LogitechNodes::default();
        nodes.enumerated([(1, LOGITECH_VENDOR_ID)]);
        // The node left, and an enumeration ran before its removal was read.
        nodes.enumerated([]);
        assert!(nodes.departed(&1));
    }
}
