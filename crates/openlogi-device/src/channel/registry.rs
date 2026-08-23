//! Registry of HID++ channels owned by the persistent inventory enumerator.

use std::collections::HashSet;
use std::hash::Hash;
use std::sync::{Arc, PoisonError, RwLock};

use crate::backend::NodeId;
use hidpp::channel::HidppChannel;
use openlogi_core::device::PairedDevice;

use crate::channel::DeviceCacheIdentity;
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
    ) {
        let routes = routes.into_iter().collect();
        if let Some(publication) = self
            .publications
            .iter_mut()
            .find(|publication| publication.node == node)
        {
            publication.routes = routes;
            publication.channel = channel;
            return;
        }

        let sequence = self.next_sequence;
        self.next_sequence = self.next_sequence.wrapping_add(1);
        self.publications.push(Publication {
            node,
            sequence,
            routes,
            channel,
        });
    }

    fn remove_node(&mut self, node: &Node) {
        self.publications
            .retain(|publication| publication.node != *node);
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
    fn retain_nodes(&mut self, nodes: &HashSet<Node>) {
        self.publications
            .retain(|publication| nodes.contains(&publication.node));
    }
}

struct Registry<Node, Channel> {
    state: Arc<RwLock<NodeRegistry<Node, Channel>>>,
}

impl<Node, Channel> Clone for Registry<Node, Channel> {
    fn clone(&self) -> Self {
        Self {
            state: Arc::clone(&self.state),
        }
    }
}

impl<Node, Channel> Default for Registry<Node, Channel> {
    fn default() -> Self {
        Self {
            state: Arc::new(RwLock::new(NodeRegistry::default())),
        }
    }
}

impl<Node: Eq, Channel> Registry<Node, Channel> {
    fn replace_node(
        &self,
        node: Node,
        routes: impl IntoIterator<Item = DeviceRoute>,
        channel: Channel,
    ) {
        self.state
            .write()
            .unwrap_or_else(PoisonError::into_inner)
            .replace_node(node, routes, channel);
    }

    fn remove_node(&self, node: &Node) {
        self.state
            .write()
            .unwrap_or_else(PoisonError::into_inner)
            .remove_node(node);
    }
}

impl<Node: Eq + Hash, Channel> Registry<Node, Channel> {
    fn retain_nodes(&self, nodes: &HashSet<Node>) {
        self.state
            .write()
            .unwrap_or_else(PoisonError::into_inner)
            .retain_nodes(nodes);
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

/// One route discovered during inventory, plus enough identity to decide
/// whether immutable feature metadata can safely survive the next refresh.
#[derive(Clone, Debug)]
pub(crate) struct PublishedRoute {
    route: DeviceRoute,
    identity: Option<DeviceCacheIdentity>,
}

impl PublishedRoute {
    pub(crate) fn for_device(
        route: DeviceRoute,
        paired: &PairedDevice,
        identity_is_current: bool,
    ) -> Self {
        let identity = if identity_is_current {
            match route {
                DeviceRoute::Direct { .. } => Some(DeviceCacheIdentity::Direct),
                DeviceRoute::Bolt { .. } => paired.model_info.as_ref().and_then(|model| {
                    let unit_id = (model.unit_id != [0; 4]).then_some(model.unit_id);
                    let serial_number = model
                        .serial_number
                        .as_deref()
                        .filter(|serial| !serial.is_empty())
                        .map(str::to_owned);
                    (unit_id.is_some() || serial_number.is_some()).then_some(
                        DeviceCacheIdentity::Physical {
                            unit_id,
                            serial_number,
                        },
                    )
                }),
                // Unifying arrival events identify only receiver + slot +
                // model-level WPID. Its inventory probe cache is slot-keyed,
                // so model_info may still describe the former occupant just
                // after re-pairing. Without a live per-unit discriminator, a
                // same-model replacement cannot be told apart safely. Raw HID
                // routes do not carry HID++ feature metadata at all.
                DeviceRoute::Unifying { .. } | DeviceRoute::RawHid { .. } => None,
            }
        } else {
            None
        };
        Self { route, identity }
    }

    #[cfg(test)]
    pub(crate) fn route(&self) -> &DeviceRoute {
        &self.route
    }

    #[cfg(test)]
    pub(crate) fn has_cache_identity(&self) -> bool {
        self.identity.is_some()
    }

    #[cfg(test)]
    pub(crate) fn cache_identity(&self) -> Option<&DeviceCacheIdentity> {
        self.identity.as_ref()
    }
}

#[derive(Clone)]
struct RegisteredChannel {
    channel: Arc<HidppChannel>,
    routes: Vec<PublishedRoute>,
}

/// Channels already opened and owned by the persistent inventory enumerator.
///
/// Publications are keyed by OS HID node internally and selected by exact
/// [`DeviceRoute`]. When identical direct devices publish the same route, the
/// oldest live node wins until it is removed.
#[derive(Clone, Default)]
pub struct ChannelRegistry {
    inner: Registry<NodeId, RegisteredChannel>,
}

impl ChannelRegistry {
    /// Replace every route published by `node`, preserving that node's original
    /// collision priority when it was already present.
    pub(crate) fn replace_node(
        &self,
        node: NodeId,
        routes: impl IntoIterator<Item = PublishedRoute>,
        channel: Arc<HidppChannel>,
    ) {
        let routes = routes.into_iter().collect::<Vec<_>>();
        let plain_routes = routes
            .iter()
            .map(|published| published.route.clone())
            .collect::<Vec<_>>();
        self.inner
            .replace_node(node, plain_routes, RegisteredChannel { channel, routes });
    }

    /// Remove every route and channel reference owned by `node`.
    pub(crate) fn remove_node(&self, node: &NodeId) {
        self.inner.remove_node(node);
    }

    /// Remove publications for nodes absent from the current OS enumeration.
    pub(crate) fn retain_nodes(&self, nodes: &HashSet<NodeId>) {
        self.inner.retain_nodes(nodes);
    }

    /// Clone the current exact-route winner.
    #[must_use]
    pub fn lookup(&self, route: &DeviceRoute) -> Option<SharedChannel> {
        self.inner.lookup(route).map(|registered| {
            let cache_identity = registered
                .routes
                .iter()
                .find(|published| published.route == *route)
                .and_then(|published| published.identity.clone());
            SharedChannel::with_cache_identity(registered.channel, route.clone(), cache_identity)
        })
    }

    /// Whether `shared` is still the winning publication for its exact route,
    /// connection, and physical-device identity.
    #[must_use]
    pub fn is_current(&self, shared: &SharedChannel) -> bool {
        self.inner.any_current(|route, registered| {
            let cache_identity = registered
                .routes
                .iter()
                .find(|published| published.route == *route)
                .and_then(|published| published.identity.as_ref());
            shared.matches(route)
                && Arc::ptr_eq(&registered.channel, shared.channel())
                && shared.cache_identity_matches(cache_identity)
        })
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;
    use std::panic::{AssertUnwindSafe, catch_unwind};
    use std::sync::Arc;

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
