//! Registry of HID++ channels owned by the persistent inventory enumerator.

use std::collections::HashSet;
use std::hash::Hash;
use std::sync::{Arc, PoisonError, RwLock};

use crate::backend::NodeId;
use hidpp::channel::HidppChannel;
use tokio::sync::watch;

use crate::{DeviceRoute, SharedChannel};

struct Publication<Node, Channel> {
    node: Node,
    sequence: u64,
    routes: Vec<DeviceRoute>,
    channel: Channel,
}

struct NodeRegistry<Node, Channel> {
    publications: Vec<Publication<Node, Channel>>,
    next_sequence: u64,
}

impl<Node, Channel> Default for NodeRegistry<Node, Channel> {
    fn default() -> Self {
        Self {
            publications: Vec::new(),
            next_sequence: 0,
        }
    }
}

impl<Node: Eq, Channel> NodeRegistry<Node, Channel> {
    fn replace_node(
        &mut self,
        node: Node,
        routes: impl IntoIterator<Item = DeviceRoute>,
        channel: Channel,
        same_channel: fn(&Channel, &Channel) -> bool,
    ) -> bool {
        let routes: Vec<_> = routes.into_iter().collect();
        if let Some(publication) = self
            .publications
            .iter_mut()
            .find(|publication| publication.node == node)
        {
            let same_routes = publication
                .routes
                .iter()
                .all(|route| routes.contains(route))
                && routes
                    .iter()
                    .all(|route| publication.routes.contains(route));
            if same_routes && same_channel(&publication.channel, &channel) {
                return false;
            }
            publication.routes = routes;
            publication.channel = channel;
            return true;
        }

        let sequence = self.next_sequence;
        self.next_sequence = self.next_sequence.wrapping_add(1);
        self.publications.push(Publication {
            node,
            sequence,
            routes,
            channel,
        });
        true
    }

    fn remove_node(&mut self, node: &Node) -> bool {
        let previous_len = self.publications.len();
        self.publications
            .retain(|publication| publication.node != *node);
        self.publications.len() != previous_len
    }

    fn lookup(&self, route: &DeviceRoute) -> Option<&Channel> {
        self.publications
            .iter()
            .filter(|publication| publication.routes.contains(route))
            .min_by_key(|publication| publication.sequence)
            .map(|publication| &publication.channel)
    }

    fn any_current(&self, mut predicate: impl FnMut(&DeviceRoute, &Channel) -> bool) -> bool {
        self.publications.iter().any(|publication| {
            publication.routes.iter().any(|route| {
                predicate(route, &publication.channel)
                    && self
                        .lookup(route)
                        .is_some_and(|winner| std::ptr::eq(winner, &raw const publication.channel))
            })
        })
    }
}

impl<Node: Eq + Hash, Channel> NodeRegistry<Node, Channel> {
    fn retain_nodes(&mut self, nodes: &HashSet<Node>) -> bool {
        let previous_len = self.publications.len();
        self.publications
            .retain(|publication| nodes.contains(&publication.node));
        self.publications.len() != previous_len
    }
}

struct Registry<Node, Channel> {
    state: Arc<RwLock<NodeRegistry<Node, Channel>>>,
    changes: watch::Sender<()>,
    same_channel: fn(&Channel, &Channel) -> bool,
}

impl<Node, Channel> Clone for Registry<Node, Channel> {
    fn clone(&self) -> Self {
        Self {
            state: Arc::clone(&self.state),
            changes: self.changes.clone(),
            same_channel: self.same_channel,
        }
    }
}

impl<Node, Channel: PartialEq> Default for Registry<Node, Channel> {
    fn default() -> Self {
        Self::new(PartialEq::eq)
    }
}

impl<Node, Channel> Registry<Node, Channel> {
    fn new(same_channel: fn(&Channel, &Channel) -> bool) -> Self {
        let (changes, _) = watch::channel(());
        Self {
            state: Arc::new(RwLock::new(NodeRegistry::default())),
            changes,
            same_channel,
        }
    }

    fn subscribe(&self) -> watch::Receiver<()> {
        self.changes.subscribe()
    }

    fn notify_change(&self) {
        // `watch` versions each send internally; the unit payload deliberately
        // carries no registry state and means only "read the SSOT again".
        self.changes.send_modify(|()| {});
    }
}

impl<Node: Eq, Channel> Registry<Node, Channel> {
    fn replace_node(
        &self,
        node: Node,
        routes: impl IntoIterator<Item = DeviceRoute>,
        channel: Channel,
    ) {
        let changed = self
            .state
            .write()
            .unwrap_or_else(PoisonError::into_inner)
            .replace_node(node, routes, channel, self.same_channel);
        if changed {
            self.notify_change();
        }
    }

    fn remove_node(&self, node: &Node) {
        let changed = self
            .state
            .write()
            .unwrap_or_else(PoisonError::into_inner)
            .remove_node(node);
        if changed {
            self.notify_change();
        }
    }
}

impl<Node: Eq + Hash, Channel> Registry<Node, Channel> {
    fn retain_nodes(&self, nodes: &HashSet<Node>) {
        let changed = self
            .state
            .write()
            .unwrap_or_else(PoisonError::into_inner)
            .retain_nodes(nodes);
        if changed {
            self.notify_change();
        }
    }
}

impl<Node: Eq, Channel: Clone> Registry<Node, Channel> {
    fn lookup(&self, route: &DeviceRoute) -> Option<Channel> {
        self.state.read().ok()?.lookup(route).cloned()
    }
}

impl<Node: Eq, Channel> Registry<Node, Channel> {
    fn any_current(&self, predicate: impl FnMut(&DeviceRoute, &Channel) -> bool) -> bool {
        self.state
            .read()
            .is_ok_and(|state| state.any_current(predicate))
    }
}

/// Channels already opened and owned by the persistent inventory enumerator.
///
/// Publications are keyed by OS HID node internally and selected by exact
/// [`DeviceRoute`]. When identical direct devices publish the same route, the
/// oldest live node wins until it is removed.
#[derive(Clone)]
pub struct ChannelRegistry {
    inner: Registry<NodeId, Arc<HidppChannel>>,
}

impl Default for ChannelRegistry {
    fn default() -> Self {
        Self {
            inner: Registry::new(Arc::ptr_eq),
        }
    }
}

impl ChannelRegistry {
    /// Replace every route published by `node`, preserving that node's original
    /// collision priority when it was already present.
    pub(crate) fn replace_node(
        &self,
        node: NodeId,
        routes: impl IntoIterator<Item = DeviceRoute>,
        channel: Arc<HidppChannel>,
    ) {
        self.inner.replace_node(node, routes, channel);
    }

    /// Remove every route and channel reference owned by `node`.
    pub(crate) fn remove_node(&self, node: &NodeId) {
        self.inner.remove_node(node);
    }

    /// Remove publications for nodes absent from the current OS enumeration.
    pub(crate) fn retain_nodes(&self, nodes: &HashSet<NodeId>) {
        self.inner.retain_nodes(nodes);
    }

    /// Subscribe to semantic publication changes.
    ///
    /// The receiver coalesces changes and carries no route data: after it
    /// changes, read this registry again with [`Self::lookup`] or
    /// [`Self::is_current`]. Re-publishing the same node, route set, and channel
    /// does not wake subscribers.
    #[must_use]
    pub fn subscribe(&self) -> watch::Receiver<()> {
        self.inner.subscribe()
    }

    /// Clone the current exact-route winner.
    #[must_use]
    pub fn lookup(&self, route: &DeviceRoute) -> Option<SharedChannel> {
        self.inner
            .lookup(route)
            .map(|channel| SharedChannel::new(channel, route.clone()))
    }

    /// Whether `shared` is still the winning publication for its exact route
    /// and points to the same underlying connection.
    #[must_use]
    pub fn is_current(&self, shared: &SharedChannel) -> bool {
        self.inner.any_current(|route, channel| {
            shared.matches(route) && Arc::ptr_eq(channel, shared.channel())
        })
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;
    use std::panic::{AssertUnwindSafe, catch_unwind};
    use std::sync::Arc;
    use std::time::Duration;

    use crate::DeviceRoute;

    use super::{PoisonError, Registry};

    impl<Node, Channel> Registry<Node, Channel> {
        fn poison_for_test(&self) {
            let _ = catch_unwind(AssertUnwindSafe(|| {
                let _guard = self.state.write().unwrap_or_else(PoisonError::into_inner);
                panic!("poison registry for test");
            }));
        }
    }

    impl<Node: Eq, Channel: Clone> Registry<Node, Channel> {
        fn publisher_lookup_for_test(&self, route: &DeviceRoute) -> Option<Channel> {
            self.state
                .read()
                .unwrap_or_else(PoisonError::into_inner)
                .lookup(route)
                .cloned()
        }
    }

    fn direct(product_id: u16) -> DeviceRoute {
        DeviceRoute::Direct {
            vendor_id: 0x046d,
            product_id,
        }
    }

    fn bolt(uid: &str, slot: u8) -> DeviceRoute {
        DeviceRoute::Bolt {
            receiver_uid: uid.into(),
            slot,
        }
    }

    #[test]
    fn lookup_rejects_every_non_exact_route_field() {
        let registry = Registry::<u8, &'static str>::default();
        registry.replace_node(1, [bolt("AABB", 2)], "channel-a");

        assert_eq!(registry.lookup(&bolt("AABB", 2)), Some("channel-a"));
        assert_eq!(registry.lookup(&bolt("AABB", 3)), None);
        assert_eq!(registry.lookup(&bolt("CCDD", 2)), None);
        assert_eq!(registry.lookup(&direct(0xb35b)), None);
    }

    #[test]
    fn one_node_can_publish_multiple_receiver_slots() {
        let registry = Registry::<u8, &'static str>::default();
        registry.replace_node(1, [bolt("AABB", 1), bolt("AABB", 4)], "receiver-channel");

        assert_eq!(registry.lookup(&bolt("AABB", 1)), Some("receiver-channel"));
        assert_eq!(registry.lookup(&bolt("AABB", 4)), Some("receiver-channel"));
    }

    #[test]
    fn current_check_uses_only_the_exact_winning_publication() {
        let route = direct(0xb35b);
        let registry = Registry::<u8, &'static str>::default();
        registry.replace_node(1, [route.clone()], "a");
        registry.replace_node(2, [route.clone()], "b");

        assert!(
            registry.any_current(|candidate, channel| { candidate == &route && *channel == "a" })
        );
        assert!(
            !registry.any_current(|candidate, channel| { candidate == &route && *channel == "b" })
        );
    }

    #[test]
    fn same_route_with_a_different_arc_is_not_current() {
        let route = direct(0xb35b);
        let published = Arc::new(());
        let stale = Arc::new(());
        let registry = Registry::<u8, Arc<()>>::default();
        registry.replace_node(1, [route.clone()], Arc::clone(&published));

        assert!(registry.any_current(|candidate, channel| {
            candidate == &route && Arc::ptr_eq(channel, &published)
        }));
        assert!(!registry.any_current(|candidate, channel| {
            candidate == &route && Arc::ptr_eq(channel, &stale)
        }));
    }

    #[test]
    fn replacing_winner_preserves_priority_then_removal_promotes_next_owner() {
        let route = direct(0xb35b);
        let registry = Registry::<u8, &'static str>::default();
        registry.replace_node(1, [route.clone()], "a-v1");
        registry.replace_node(2, [route.clone()], "b");

        assert_eq!(registry.lookup(&route), Some("a-v1"));

        registry.replace_node(1, [route.clone()], "a-v2");
        assert_eq!(registry.lookup(&route), Some("a-v2"));

        registry.remove_node(&1);
        assert_eq!(registry.lookup(&route), Some("b"));
    }

    #[test]
    fn replacement_and_removal_wake_every_subscriber() {
        let route = direct(0xb35b);
        let first_channel = Arc::new(());
        let replacement = Arc::new(());
        let registry = Registry::<u8, Arc<()>>::new(Arc::ptr_eq);
        registry.replace_node(1, [route.clone()], Arc::clone(&first_channel));
        let mut first = registry.subscribe();
        let mut second = registry.subscribe();

        registry.replace_node(1, [route.clone()], Arc::clone(&replacement));

        assert!(first.has_changed().expect("registry sender remains alive"));
        assert!(second.has_changed().expect("registry sender remains alive"));
        first.borrow_and_update();
        second.borrow_and_update();
        assert!(!registry.any_current(|candidate, channel| {
            candidate == &route && Arc::ptr_eq(channel, &first_channel)
        }));
        assert!(registry.any_current(|candidate, channel| {
            candidate == &route && Arc::ptr_eq(channel, &replacement)
        }));

        registry.remove_node(&1);

        assert!(first.has_changed().expect("registry sender remains alive"));
        assert!(second.has_changed().expect("registry sender remains alive"));
        assert_eq!(registry.lookup(&route), None);
    }

    #[test]
    fn no_op_publications_do_not_wake_subscribers() {
        let first_route = bolt("AABB", 1);
        let second_route = bolt("AABB", 2);
        let channel = Arc::new(());
        let registry = Registry::<u8, Arc<()>>::new(Arc::ptr_eq);
        registry.replace_node(
            1,
            [first_route.clone(), second_route.clone()],
            Arc::clone(&channel),
        );
        let changes = registry.subscribe();

        registry.replace_node(1, [second_route, first_route], Arc::clone(&channel));
        registry.remove_node(&2);
        registry.retain_nodes(&HashSet::from([1]));

        assert!(
            !changes
                .has_changed()
                .expect("registry sender remains alive")
        );
    }

    #[tokio::test]
    async fn change_between_current_check_and_wait_is_not_missed() {
        let route = direct(0xb35b);
        let first_channel = Arc::new(());
        let replacement = Arc::new(());
        let registry = Registry::<u8, Arc<()>>::new(Arc::ptr_eq);
        registry.replace_node(1, [route.clone()], Arc::clone(&first_channel));

        // This is the ordering used by capture restore: subscribe, check the
        // SSOT, then wait. Publishing in that final gap must remain visible.
        let mut changes = registry.subscribe();
        assert!(registry.any_current(|candidate, channel| {
            candidate == &route && Arc::ptr_eq(channel, &first_channel)
        }));
        registry.replace_node(1, [route.clone()], Arc::clone(&replacement));

        tokio::time::timeout(Duration::from_millis(100), changes.changed())
            .await
            .expect("a pre-wait publication must be observed")
            .expect("registry sender remains alive");
        assert!(registry.any_current(|candidate, channel| {
            candidate == &route && Arc::ptr_eq(channel, &replacement)
        }));
    }

    #[test]
    fn replacing_one_node_is_atomic_and_does_not_touch_another() {
        let registry = Registry::<u8, &'static str>::default();
        registry.replace_node(1, [bolt("A", 1), bolt("A", 2)], "a");
        registry.replace_node(2, [bolt("B", 1)], "b");

        registry.replace_node(1, [bolt("A", 3)], "a-new");

        assert_eq!(registry.lookup(&bolt("A", 1)), None);
        assert_eq!(registry.lookup(&bolt("A", 2)), None);
        assert_eq!(registry.lookup(&bolt("A", 3)), Some("a-new"));
        assert_eq!(registry.lookup(&bolt("B", 1)), Some("b"));
    }

    #[test]
    fn retaining_nodes_removes_only_absent_owners() {
        let registry = Registry::<u8, &'static str>::default();
        registry.replace_node(1, [direct(0xb35b)], "a");
        registry.replace_node(2, [direct(0xb36b)], "b");

        registry.retain_nodes(&HashSet::from([2]));

        assert_eq!(registry.lookup(&direct(0xb35b)), None);
        assert_eq!(registry.lookup(&direct(0xb36b)), Some("b"));
    }

    #[test]
    fn poisoned_read_fails_closed_but_publishers_can_clean_up() {
        let registry = Registry::<u8, &'static str>::default();
        registry.replace_node(1, [direct(0xb35b)], "a");
        registry.poison_for_test();

        assert_eq!(registry.lookup(&direct(0xb35b)), None);
        assert!(!registry.any_current(|_, _| true));

        registry.remove_node(&1);
        registry.replace_node(2, [direct(0xb36b)], "b");
        registry.retain_nodes(&HashSet::from([2]));

        assert_eq!(registry.publisher_lookup_for_test(&direct(0xb35b)), None);
        assert_eq!(
            registry.publisher_lookup_for_test(&direct(0xb36b)),
            Some("b")
        );
    }
}
