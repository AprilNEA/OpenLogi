//! Runtime inventory evidence used to attribute receiver-open failures.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, RwLock};

use openlogi_core::device::ReceiverInfo;

use crate::backend::NodeId;

/// Receiver identities learned by this backend's inventory enumerator.
///
/// Share this between an enumerator and pairing on the same backend. OS node
/// identities are runtime hints, never persisted or sent over IPC. They only
/// attribute an open failure; a channel that opens must still prove its UID
/// before any pairing or unpairing write.
#[derive(Clone, Default)]
pub struct ReceiverIdentityCache {
    entries: Arc<RwLock<HashMap<NodeId, (u16, String)>>>,
}

impl ReceiverIdentityCache {
    pub(crate) fn observe(&self, node: &NodeId, receiver: &ReceiverInfo) {
        let Ok(mut entries) = self.entries.write() else {
            return;
        };
        if crate::find_receiver(receiver.vendor_id, receiver.product_id).is_some()
            && let Some(uid) = receiver.unique_id.as_ref().filter(|uid| !uid.is_empty())
        {
            entries.insert(node.clone(), (receiver.product_id, uid.clone()));
        } else {
            entries.remove(node);
        }
    }

    pub(crate) fn retain_nodes(&self, nodes: &HashSet<NodeId>) {
        if let Ok(mut entries) = self.entries.write() {
            entries.retain(|node, _| nodes.contains(node));
        }
    }

    pub(crate) fn identity(&self, node: &NodeId) -> Option<(u16, String)> {
        self.entries.read().ok()?.get(node).cloned()
    }
}
