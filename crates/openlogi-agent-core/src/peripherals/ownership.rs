//! Acknowledged handoff from the existing protocol managers to an external driver.
//!
//! Stop admitting settings first. Keep inventory channels available for firmware
//! restoration, then retire those channels before admitting the replacement.

use std::{
    collections::{BTreeMap, HashSet},
    sync::{Arc, Mutex, MutexGuard},
};

use openlogi_core::peripheral::{PeripheralError, SessionId};
use openlogi_hid::{DeviceRoute, NodeId};
use tokio::sync::watch;

/// Existing owners which must acknowledge a transport handoff.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Owner {
    Gesture,
    Keyboard,
    HostSwitch,
    Runtime,
    Inventory,
}

/// One immutable admission decision consumed by the existing managers.
#[derive(Clone, Default)]
pub(crate) struct Requests {
    revision: u64,
    active: bool,
    routes: Vec<DeviceRoute>,
    excluded: HashSet<NodeId>,
}

impl Requests {
    pub(crate) fn has_requests(&self) -> bool {
        self.active
    }

    pub(crate) fn allows(&self, route: &DeviceRoute) -> bool {
        !self.routes.iter().any(|blocked| overlaps(blocked, route))
    }

    pub(crate) fn excluded(&self) -> &HashSet<NodeId> {
        &self.excluded
    }
}

struct Reservation {
    nodes: HashSet<NodeId>,
    routes: Vec<DeviceRoute>,
    retiring: bool,
}

#[derive(Default)]
struct Acknowledgement {
    revision: u64,
    routes: Vec<DeviceRoute>,
    nodes: HashSet<NodeId>,
}

#[derive(Default)]
struct State {
    reservations: BTreeMap<SessionId, Reservation>,
    owners: BTreeMap<Owner, (u64, Option<Acknowledgement>)>,
    operations: BTreeMap<u64, OperationTarget>,
    sequence: u64,
}

enum OperationTarget {
    Route(DeviceRoute),
    Node(NodeId),
}

impl OperationTarget {
    fn overlaps(&self, reservation: &Reservation) -> bool {
        match self {
            Self::Route(route) => reservation
                .routes
                .iter()
                .any(|blocked| overlaps(blocked, route)),
            Self::Node(node) => reservation.nodes.contains(node),
        }
    }
}

/// Shared admission policy. This is independent of the host's sleep/TCC gate.
#[derive(Clone)]
pub struct Ownership {
    state: Arc<Mutex<State>>,
    requests: watch::Sender<Requests>,
    changes: watch::Sender<()>,
}

impl Default for Ownership {
    fn default() -> Self {
        Self {
            state: Arc::new(Mutex::new(State::default())),
            requests: watch::channel(Requests::default()).0,
            changes: watch::channel(()).0,
        }
    }
}

impl Ownership {
    #[expect(
        clippy::expect_used,
        reason = "a poisoned admission ledger must fail closed"
    )]
    fn lock(&self) -> MutexGuard<'_, State> {
        // A poisoned ownership ledger cannot authorize a second device owner.
        self.state
            .lock()
            .expect("peripheral ownership ledger poisoned")
    }

    pub(crate) fn subscribe(&self) -> watch::Receiver<Requests> {
        self.requests.subscribe()
    }

    pub(crate) fn changes(&self) -> watch::Receiver<()> {
        self.changes.subscribe()
    }

    pub(crate) fn reconcile(&self) {
        self.changed();
    }

    pub(crate) fn requests(&self) -> Requests {
        self.requests.borrow().clone()
    }

    pub(crate) fn owner(&self, owner: Owner) -> Reporter {
        let mut state = self.lock();
        state.sequence += 1;
        let epoch = state.sequence;
        state.owners.insert(owner, (epoch, None));
        drop(state);
        self.changed();
        Reporter {
            ownership: self.clone(),
            owner,
            epoch,
        }
    }

    pub(crate) fn reserve(
        &self,
        session: &SessionId,
        nodes: HashSet<NodeId>,
        routes: Vec<DeviceRoute>,
    ) -> Result<(), PeripheralError> {
        let mut state = self.lock();
        if let Some(reservation) = state.reservations.get(session)
            && reservation.nodes == nodes
            && routes
                .iter()
                .all(|route| reservation.routes.contains(route))
        {
            return Ok(());
        }
        if state
            .reservations
            .iter()
            .any(|(owner, r)| owner != session && !r.nodes.is_disjoint(&nodes))
        {
            return Err(PeripheralError::DriverConflict(
                "another driver is acquiring this endpoint group".into(),
            ));
        }
        state.reservations.insert(
            session.clone(),
            Reservation {
                nodes,
                routes,
                retiring: false,
            },
        );
        self.publish(&state);
        Ok(())
    }

    /// Advance only after all existing session managers have observed this request.
    pub(crate) fn ready(&self, session: &SessionId) -> bool {
        let mut state = self.lock();
        let Some(reservation) = state.reservations.get(session) else {
            return false;
        };
        let revision = self.requests.borrow().revision;
        let restored = [
            Owner::Gesture,
            Owner::Keyboard,
            Owner::HostSwitch,
            Owner::Runtime,
        ]
        .iter()
        .all(|owner| {
            state.owners.get(owner).is_some_and(|(_, ack)| {
                ack.as_ref().is_some_and(|ack| {
                    ack.revision == revision
                        && ack.routes.iter().all(|r| {
                            !reservation
                                .routes
                                .iter()
                                .any(|blocked| overlaps(blocked, r))
                        })
                })
            })
        });
        let operations_finished = state
            .operations
            .values()
            .all(|target| !target.overlaps(reservation));
        if !restored || !operations_finished {
            return false;
        }
        if !reservation.retiring {
            if let Some(reservation) = state.reservations.get_mut(session) {
                reservation.retiring = true;
            }
            self.publish(&state);
            return false;
        }
        state.owners.get(&Owner::Inventory).is_some_and(|(_, ack)| {
            ack.as_ref().is_some_and(|ack| {
                ack.revision == revision && ack.nodes.is_disjoint(&reservation.nodes)
            })
        })
    }

    pub(crate) fn release(&self, session: &SessionId) {
        let mut state = self.lock();
        if state.reservations.remove(session).is_some() {
            self.publish(&state);
        }
    }

    /// Admit a bounded operation, retaining the admission until the native work ends.
    pub(crate) fn operation(&self, route: &DeviceRoute) -> Result<Operation, PeripheralError> {
        self.admit(OperationTarget::Route(route.clone()))
    }

    /// Pairing admits the discovered node before probing its receiver identity.
    pub(crate) fn node_operation(&self, node: &NodeId) -> Option<Operation> {
        self.admit(OperationTarget::Node(node.clone())).ok()
    }

    fn admit(&self, target: OperationTarget) -> Result<Operation, PeripheralError> {
        let mut state = self.lock();
        if state
            .reservations
            .values()
            .any(|reservation| target.overlaps(reservation))
        {
            return Err(PeripheralError::DriverConflict(
                "the selected transport is reserved by another driver".into(),
            ));
        }
        state.sequence += 1;
        let id = state.sequence;
        state.operations.insert(id, target);
        Ok(Operation {
            ownership: self.clone(),
            id,
        })
    }

    fn publish(&self, state: &State) {
        self.requests.send_modify(|requests| {
            requests.revision += 1;
            requests.active = !state.reservations.is_empty();
            requests.routes = state
                .reservations
                .values()
                .flat_map(|r| r.routes.clone())
                .collect();
            requests.excluded = state
                .reservations
                .values()
                .filter(|r| r.retiring)
                .flat_map(|r| r.nodes.iter().cloned())
                .collect();
        });
        self.changed();
    }

    fn changed(&self) {
        self.changes.send_modify(|()| {});
    }
}

fn overlaps(left: &DeviceRoute, right: &DeviceRoute) -> bool {
    left == right || left.shares_transport(right)
}

pub(crate) struct Operation {
    ownership: Ownership,
    id: u64,
}

impl Drop for Operation {
    fn drop(&mut self) {
        self.ownership.lock().operations.remove(&self.id);
        self.ownership.changed();
    }
}

/// The manager retains this reporter until its tasks and recovery have stopped.
pub(crate) struct Reporter {
    ownership: Ownership,
    owner: Owner,
    epoch: u64,
}

impl Reporter {
    pub(crate) fn observe(
        &self,
        requests: &Requests,
        routes: Vec<DeviceRoute>,
        nodes: HashSet<NodeId>,
    ) {
        let mut state = self.ownership.lock();
        if let Some((epoch, ack)) = state.owners.get_mut(&self.owner)
            && *epoch == self.epoch
        {
            let changed = ack.as_ref().is_none_or(|old| {
                old.revision != requests.revision || old.routes != routes || old.nodes != nodes
            });
            if changed {
                *ack = Some(Acknowledgement {
                    revision: requests.revision,
                    routes,
                    nodes,
                });
                self.ownership.changed();
            }
        }
    }
}

impl Drop for Reporter {
    fn drop(&mut self) {
        let mut state = self.ownership.lock();
        if state
            .owners
            .get(&self.owner)
            .is_some_and(|(epoch, _)| *epoch == self.epoch)
        {
            state.owners.remove(&self.owner);
            self.ownership.changed();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use openlogi_core::peripheral::EndpointId;

    fn session(generation: u64) -> SessionId {
        SessionId {
            endpoint: EndpointId("receiver/group".into()),
            generation,
        }
    }

    fn route(uid: &str, slot: u8) -> DeviceRoute {
        DeviceRoute::Bolt {
            receiver_uid: uid.into(),
            slot,
        }
    }

    #[test]
    fn handoff_waits_for_restoration_io_and_native_channel_release() {
        let ownership = Ownership::default();
        let owners: Vec<_> = [
            Owner::Gesture,
            Owner::Keyboard,
            Owner::HostSwitch,
            Owner::Runtime,
        ]
        .into_iter()
        .map(|owner| ownership.owner(owner))
        .collect();
        let inventory = ownership.owner(Owner::Inventory);
        let nodes = HashSet::from([NodeId::from("receiver".to_owned())]);
        let unrelated = ownership.operation(&route("other", 1)).unwrap();
        let write = ownership.operation(&route("one", 2)).unwrap();
        ownership
            .reserve(&session(1), nodes.clone(), vec![route("one", 1)])
            .unwrap();
        assert!(
            ownership.operation(&route("one", 3)).is_err(),
            "every receiver child shares the reserved transport"
        );
        assert!(
            !ownership.ready(&session(1)),
            "a pre-request idle state is not acknowledgement"
        );
        let requests = ownership.requests();
        for owner in &owners {
            owner.observe(&requests, Vec::new(), HashSet::new());
        }
        owners[0].observe(&requests, vec![route("one", 1)], HashSet::new());
        inventory.observe(&requests, Vec::new(), nodes.clone());
        assert!(
            !ownership.ready(&session(1)),
            "draining firmware still owns the device"
        );
        assert!(
            ownership.requests().excluded().is_empty(),
            "restoration must retain the native channel"
        );
        owners[0].observe(&requests, Vec::new(), HashSet::new());
        assert!(
            !ownership.ready(&session(1)),
            "an admitted write must finish before retiring its channel"
        );
        drop(write);
        assert!(
            !ownership.ready(&session(1)),
            "channel retirement requires a separate acknowledgement"
        );
        let requests = ownership.requests();
        assert_eq!(requests.excluded(), &nodes);
        for owner in &owners {
            owner.observe(&requests, Vec::new(), HashSet::new());
        }
        inventory.observe(&requests, Vec::new(), nodes);
        assert!(
            !ownership.ready(&session(1)),
            "an outstanding native reader prevents handoff"
        );
        inventory.observe(&requests, Vec::new(), HashSet::new());
        assert!(
            ownership.ready(&session(1)),
            "an unrelated device operation must not prevent handoff"
        );
        ownership.release(&session(0));
        assert!(
            !ownership.requests().allows(&route("one", 1)),
            "stale cleanup cannot release the replacement"
        );
        ownership.release(&session(1));
        ownership.operation(&route("one", 1)).unwrap();
        drop(unrelated);
    }

    #[test]
    fn replacing_a_manager_requires_its_new_epoch_to_acknowledge() {
        let ownership = Ownership::default();
        let old = ownership.owner(Owner::Gesture);
        let new = ownership.owner(Owner::Gesture);
        ownership
            .reserve(&session(1), HashSet::new(), vec![route("one", 1)])
            .unwrap();
        old.observe(&ownership.requests(), Vec::new(), HashSet::new());
        drop(old);
        assert!(ownership.lock().owners[&Owner::Gesture].1.is_none());
        new.observe(&ownership.requests(), Vec::new(), HashSet::new());
        assert!(ownership.lock().owners[&Owner::Gesture].1.is_some());
        drop(new);
        assert!(
            !ownership.ready(&session(1)),
            "a dead manager cannot acknowledge firmware cleanup"
        );
    }

    #[test]
    fn node_only_reservations_request_both_handoff_acknowledgements() {
        let ownership = Ownership::default();
        let owners: Vec<_> = [
            Owner::Gesture,
            Owner::Keyboard,
            Owner::HostSwitch,
            Owner::Runtime,
        ]
        .into_iter()
        .map(|owner| ownership.owner(owner))
        .collect();
        let inventory = ownership.owner(Owner::Inventory);
        let node = NodeId::from("unprobed-receiver".to_owned());
        let admitted = ownership.node_operation(&node).unwrap();
        ownership
            .reserve(&session(1), HashSet::from([node.clone()]), Vec::new())
            .unwrap();
        assert!(
            ownership.requests().has_requests(),
            "managers must observe a handoff before receiver identity is known"
        );
        assert!(ownership.node_operation(&node).is_none());
        for owner in &owners {
            owner.observe(&ownership.requests(), Vec::new(), HashSet::new());
        }
        assert!(!ownership.ready(&session(1)));
        drop(admitted);
        assert!(!ownership.ready(&session(1)));
        for owner in &owners {
            owner.observe(&ownership.requests(), Vec::new(), HashSet::new());
        }
        inventory.observe(&ownership.requests(), Vec::new(), HashSet::new());
        assert!(ownership.ready(&session(1)));
        ownership.release(&session(1));
        assert!(!ownership.requests().has_requests());
        ownership.node_operation(&node).unwrap();
    }
}
