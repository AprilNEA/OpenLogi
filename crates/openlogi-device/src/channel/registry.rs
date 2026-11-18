//! Registry of HID++ channels owned by the persistent inventory enumerator.

use std::collections::HashSet;
use std::hash::Hash;
use std::sync::{Arc, Mutex, PoisonError, RwLock, Weak};

use crate::backend::NodeId;
use hidpp::channel::HidppChannel;
use tokio::sync::{Notify, watch};

use crate::{DeviceRoute, SharedChannel};
use openlogi_core::device::DeviceInventory;
use std::time::{Duration, Instant};

struct Publication<Node, Channel> {
    node: Node,
    sequence: u64,
    routes: Vec<DeviceRoute>,
    offline: Vec<DeviceRoute>,
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
        offline: Vec<DeviceRoute>,
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
            if same_routes
                && publication.offline == offline
                && same_channel(&publication.channel, &channel)
            {
                return false;
            }
            publication.routes = routes;
            publication.offline = offline;
            publication.channel = channel;
            return true;
        }

        let sequence = self.next_sequence;
        self.next_sequence = self.next_sequence.wrapping_add(1);
        self.publications.push(Publication {
            node,
            sequence,
            routes,
            offline,
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
        self.lookup_matching(|p| p.routes.contains(route))
            .map(|p| &p.channel)
    }

    fn lookup_matching(
        &self,
        accepts: impl Fn(&Publication<Node, Channel>) -> bool,
    ) -> Option<&Publication<Node, Channel>> {
        self.publications
            .iter()
            .filter(|p| accepts(p))
            .min_by_key(|publication| publication.sequence)
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

    fn contains_channel(&self, channel: &Channel) -> bool {
        self.state.read().is_ok_and(|state| {
            state
                .publications
                .iter()
                .any(|publication| (self.same_channel)(&publication.channel, channel))
        })
    }
}

impl<Node: Eq, Channel> Registry<Node, Channel> {
    fn replace_node(
        &self,
        node: Node,
        routes: impl IntoIterator<Item = DeviceRoute>,
        channel: Channel,
    ) {
        self.publish(node, routes, Vec::new(), channel);
    }

    fn publish(
        &self,
        node: Node,
        routes: impl IntoIterator<Item = DeviceRoute>,
        offline: Vec<DeviceRoute>,
        channel: Channel,
    ) {
        let changed = self
            .state
            .write()
            .unwrap_or_else(PoisonError::into_inner)
            .replace_node(node, routes, offline, channel, self.same_channel);
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
    dead: Arc<DeadChannelReports>,
    links: Arc<Mutex<Vec<ConfirmedLink>>>,
    link_changed: watch::Sender<()>,
}

impl Default for ChannelRegistry {
    fn default() -> Self {
        Self {
            inner: Registry::new(Arc::ptr_eq),
            dead: Arc::default(),
            links: Arc::default(),
            link_changed: watch::channel(()).0,
        }
    }
}

/// A local radio observation tied to one channel lifetime and physical unit.
struct ConfirmedLink {
    route: DeviceRoute,
    unit: [u8; 4],
    channel: Weak<HidppChannel>,
    online: bool,
    at: Instant,
    // An authoritative scan can supersede presence before the agent consumes
    // the independent instruction to reapply this return's volatile settings.
    overrides_scan: bool,
    needs_reapply: bool,
}

impl ConfirmedLink {
    fn paired_index(&self, inventory: &DeviceInventory) -> Option<usize> {
        inventory.paired.iter().position(|p| {
            p.model_info
                .as_ref()
                .is_some_and(|m| m.unit_id == self.unit)
                && DeviceRoute::for_slot(inventory, p.slot).as_ref() == Some(&self.route)
        })
    }

    fn apply(&self, inventory: &mut DeviceInventory) -> bool {
        let Some(index) = self.paired_index(inventory) else {
            return false;
        };
        if self.overrides_scan {
            inventory.paired[index].online = self.online;
        }
        inventory.paired[index].online
    }
}

/// Never queue behind a complete receiver enumeration. A register reply has
/// no software ID, so the task that submitted it keeps the host-wide phase
/// until the read settles, even if the handoff has already used radio proof.
async fn pairing_confirms(
    receiver: &hidpp::receiver::bolt::Receiver,
    node: &NodeId,
    slot: u8,
    wpid: u16,
    unit: [u8; 4],
) -> bool {
    let Some(phase) =
        crate::host_lock::lock_receiver_registers(node, Duration::from_millis(25)).await
    else {
        return false;
    };
    if !phase.coordinated() {
        return false;
    }
    let receiver = receiver.clone();
    let read = tokio::spawn(async move {
        let _phase = phase;
        receiver
            .get_device_pairing_information(slot)
            .await
            .is_ok_and(|p| p.online && p.wpid == wpid && p.unit_id == unit)
    });
    matches!(
        tokio::time::timeout(Duration::from_millis(80), read).await,
        Ok(Ok(true))
    )
}

/// Direct channels capture found dead while inventory still publishes them.
/// Held weakly so a report never keeps a retired connection from draining.
#[derive(Default)]
struct DeadChannelReports {
    channels: Mutex<Vec<Weak<HidppChannel>>>,
    reported: Notify,
}

/// The connections reported through [`ChannelRegistry::report_dead`] since
/// inventory last took them.
pub(crate) struct DeadChannels(Vec<Weak<HidppChannel>>);

impl DeadChannels {
    pub(crate) fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Whether `channel` is one of the reported connections. A weak reference
    /// keeps its allocation, so a new channel can never alias a reported one.
    pub(crate) fn contains(&self, channel: &Arc<HidppChannel>) -> bool {
        self.0
            .iter()
            .any(|dead| std::ptr::eq(dead.as_ptr(), Arc::as_ptr(channel)))
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

    /// Publish offline pairings alongside the live routes. Offline slots can
    /// be probed for identity but never supply a writable channel to clients.
    pub(crate) fn publish(
        &self,
        node: NodeId,
        online: Vec<DeviceRoute>,
        offline: Vec<DeviceRoute>,
        channel: Arc<HidppChannel>,
    ) {
        self.inner.publish(node, online, offline, channel);
    }

    /// Subscribe before the sender writes. A Bolt link-up is a cue to read
    /// the physical unit on the slot; a link-down withdraws only the matching
    /// previously known pairing. Dropping the future removes its listener.
    #[must_use]
    pub fn watch_bolt_link(
        &self,
        route: &DeviceRoute,
        expected: [u8; 4],
        online: bool,
        gate: crate::DeviceIoGate,
    ) -> Option<impl Future<Output = bool> + Send + 'static> {
        use hidpp::receiver::bolt::{Event, Receiver};
        let DeviceRoute::Bolt { slot, .. } = route else {
            return None;
        };
        if expected == [0; 4] {
            return None;
        }
        let slot = *slot;
        let (node, channel) = self.paired_transport(route)?;
        let receiver = Receiver::new(Arc::clone(&channel)).ok()?;
        let events = receiver.listen();
        let registry = self.clone();
        let shared = SharedChannel::new(channel, route.clone());
        Some(async move {
            // Some receivers deliver the first link-up late, or before the
            // radio will answer a device query. During this transfer only,
            // also try the same read-only identity proof on a bounded cadence.
            let retry = Duration::from_millis(150);
            let mut next_probe = tokio::time::Instant::now() + Duration::from_millis(400);
            let mut speculative = true;
            loop {
                let announced = tokio::select! {
                    biased;
                    event = events.recv() => match event {
                        Ok(Event::DeviceConnection(c)) if c.index == slot && c.online == online => Some(c.wpid),
                        Ok(_) => continue,
                        Err(_) => break,
                    },
                    () = tokio::time::sleep_until(next_probe), if online => None,
                };
                if !gate.allows_io() {
                    break;
                }
                if online {
                    // The link report and a fresh USB pairing record together
                    // identify an arrived unit without first waking its radio.
                    if let Some(wpid) = announced
                        && pairing_confirms(&receiver, &node, slot, wpid, expected).await
                    {
                        return gate.allows_io() && registry.record_link(&shared, expected, true);
                    }
                    // An unanswered read sent while the radio is away has a
                    // longer transport timeout than the entire handoff. Give
                    // the initial speculative read a short budget. A later
                    // read has time to clear the abandoned header's fence.
                    let budget = if speculative && announced.is_none() {
                        Duration::from_millis(350)
                    } else {
                        Duration::from_millis(1500)
                    };
                    speculative = false;
                    let read = crate::get_unit_id_on(&shared, crate::HidppOperation::ChangeHost);
                    match tokio::time::timeout(budget, read).await {
                        Ok(Ok(Some(unit))) if unit == expected => {}
                        Ok(Ok(Some(_))) => break,
                        _ => {
                            next_probe = tokio::time::Instant::now() + retry;
                            continue;
                        }
                    }
                }
                return gate.allows_io() && registry.record_link(&shared, expected, online);
            }
            false
        })
    }

    fn record_link(&self, shared: &SharedChannel, unit: [u8; 4], online: bool) -> bool {
        let mut links = self.links.lock().unwrap_or_else(PoisonError::into_inner);
        let mut state = self
            .inner
            .state
            .write()
            .unwrap_or_else(PoisonError::into_inner);
        let route = shared.route();
        let Some(publication) = state
            .publications
            .iter_mut()
            .filter(|p| p.routes.contains(route) || p.offline.contains(route))
            .min_by_key(|p| p.sequence)
            .filter(|p| Arc::ptr_eq(&p.channel, shared.channel()))
        else {
            return false;
        };
        let (add, remove) = if online {
            (&mut publication.routes, &mut publication.offline)
        } else {
            (&mut publication.offline, &mut publication.routes)
        };
        remove.retain(|r| r != route);
        if !add.contains(route) {
            add.push(route.clone());
        }
        drop(state);
        links.retain(|link| link.channel.strong_count() != 0 && link.route != *route);
        links.push(ConfirmedLink {
            route: route.clone(),
            unit,
            channel: Arc::downgrade(shared.channel()),
            online,
            at: Instant::now(),
            overrides_scan: true,
            needs_reapply: online,
        });
        self.inner.notify_change();
        self.link_changed.send_modify(|()| {});
        true
    }

    /// Wakes the orchestrator to apply the new physical presence independently
    /// of the full enumeration which may still be walking features.
    #[must_use]
    pub fn subscribe_links(&self) -> watch::Receiver<()> {
        self.link_changed.subscribe()
    }

    /// Merge current presence and consume each confirmed return exactly once.
    /// A return orders settings replay even when intervening snapshots never
    /// showed this device offline. The route and unit together identify it.
    pub fn reconcile_links(
        &self,
        inventories: &mut [DeviceInventory],
    ) -> Vec<(DeviceRoute, [u8; 4])> {
        let mut links = self.links.lock().unwrap_or_else(PoisonError::into_inner);
        let mut returned = Vec::new();
        for link in &mut *links {
            if let Some((_, channel)) = self.paired_transport(&link.route)
                && Weak::ptr_eq(&link.channel, &Arc::downgrade(&channel))
            {
                for inventory in &mut *inventories {
                    if link.apply(inventory) && std::mem::take(&mut link.needs_reapply) {
                        returned.push((link.route.clone(), link.unit));
                    }
                }
            }
        }
        returned
    }

    /// Commit a node scan and its routes together. A live observation made
    /// during the scan wins over its older pairing snapshot.
    pub(crate) fn publish_scan(
        &self,
        node: NodeId,
        inventory: &mut DeviceInventory,
        channel: Arc<HidppChannel>,
        started: Instant,
        healthy: bool,
    ) {
        let mut links = self.links.lock().unwrap_or_else(PoisonError::into_inner);
        links.retain_mut(|link| {
            if link.channel.strong_count() == 0 {
                return false;
            }
            if !Weak::ptr_eq(&link.channel, &Arc::downgrade(&channel)) {
                return true;
            }
            if healthy && started >= link.at {
                link.overrides_scan = false;
                return link.needs_reapply
                    && link
                        .paired_index(inventory)
                        .is_some_and(|i| inventory.paired[i].online);
            }
            link.apply(inventory);
            true
        });
        let inventories = std::slice::from_ref(inventory);
        self.publish(
            node,
            crate::inventory::routes_for_inventories(inventories, true),
            crate::inventory::routes_for_inventories(inventories, false),
            channel,
        );
    }

    fn paired_transport(&self, route: &DeviceRoute) -> Option<(NodeId, Arc<HidppChannel>)> {
        let state = self.inner.state.read().ok()?;
        let p = state.lookup_matching(|p| p.routes.contains(route) || p.offline.contains(route))?;
        Some((p.node.clone(), Arc::clone(&p.channel)))
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

    /// Tell inventory that capture stopped hearing from the device behind
    /// `shared` while inventory still publishes it.
    ///
    /// Inventory otherwise replaces a channel only after two probes in a row
    /// fail, and a silent channel can hold each probe for its whole timeout,
    /// so every re-armed capture would reuse the dead channel meanwhile
    /// (#2020). The next inventory pass retires a reported channel without
    /// probing it. Only direct routes are reported: a receiver's channel
    /// carries every paired slot, and one silent device behind it says
    /// nothing about the others.
    ///
    /// Returns whether the report was accepted, i.e. whether inventory will
    /// retire the channel.
    #[must_use]
    pub fn report_dead(&self, shared: &SharedChannel) -> bool {
        if !matches!(shared.route(), DeviceRoute::Direct { .. }) || !self.is_current(shared) {
            return false;
        }
        self.dead
            .channels
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(Arc::downgrade(shared.channel()));
        self.dead.reported.notify_one();
        true
    }

    /// Resolves once [`Self::report_dead`] has accepted a report, including
    /// one accepted before this was awaited.
    pub async fn dead_channel_reported(&self) {
        self.dead.reported.notified().await;
    }

    /// Take the reported channels for inventory to retire.
    pub(crate) fn take_dead_reports(&self) -> DeadChannels {
        DeadChannels(std::mem::take(
            &mut *self
                .dead
                .channels
                .lock()
                .unwrap_or_else(PoisonError::into_inner),
        ))
    }

    /// How inventory publishes the connection `shared` was taken from now.
    ///
    /// This is the one place that tells an offline receiver slot from a
    /// retired transport. Sessions map the verdict to their own stop reason
    /// instead of recombining route lookups and transport checks.
    #[must_use]
    pub(crate) fn publication(&self, shared: &SharedChannel) -> PublicationState {
        if self.is_current(shared) {
            PublicationState::Current
        } else if matches!(
            shared.route(),
            DeviceRoute::Bolt { .. } | DeviceRoute::Unifying { .. }
        ) && self.inner.lookup(shared.route()).is_none()
            && self.inner.contains_channel(shared.channel())
        {
            PublicationState::SlotOffline
        } else {
            PublicationState::Retired
        }
    }
}

/// How inventory publishes a channel a session took earlier; the verdict of
/// [`ChannelRegistry::publication`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PublicationState {
    /// The channel still wins its exact route on the same connection.
    Current,
    /// Inventory withdrew the receiver slot's route but still publishes the
    /// USB transport, so the slot may return on the same connection.
    SlotOffline,
    /// Inventory removed or replaced the connection, or another publication
    /// now wins the route. Do not write through it again.
    Retired,
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
