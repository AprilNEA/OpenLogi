//! `0x8071` software control the agent took this session, owed back to the
//! firmware when it leaves.

use std::collections::HashMap;
use std::sync::{LazyLock, Mutex, MutexGuard, PoisonError};

use openlogi_hid::write::RgbControlLease;
use openlogi_hid::{DeviceRoute, WeakSharedChannel};
use tokio::sync::watch;
use tokio::time::Instant;
use tracing::warn;

/// Every route the agent's lighting writes claimed since startup, bound to the
/// connection each claim went through.
pub(crate) static SESSION: LazyLock<Leases<WeakSharedChannel, RgbControlLease>> =
    LazyLock::new(Leases::default);

/// Leases keyed by route, each bound to the connection `B` it was taken
/// through, and the gate that lets the exit wait for writes still running.
pub(crate) struct Leases<B, L> {
    held: Mutex<HashMap<String, Held<B, L>>>,
    gate: watch::Sender<Gate>,
}

struct Held<B, L> {
    route: DeviceRoute,
    binding: B,
    lease: L,
}

#[derive(Default)]
struct Gate {
    closed: bool,
    in_flight: usize,
}

impl<B, L> Default for Leases<B, L> {
    fn default() -> Self {
        Self {
            held: Mutex::new(HashMap::new()),
            gate: watch::Sender::new(Gate::default()),
        }
    }
}

impl<B, L> Leases<B, L> {
    fn lock(&self) -> MutexGuard<'_, HashMap<String, Held<B, L>>> {
        self.held.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Admit one lighting write, or `None` once the agent is leaving. Hold the
    /// ticket until the write records its lease or gives up.
    pub(crate) fn admit(&self) -> Option<WriteTicket<'_, B, L>> {
        let mut admitted = false;
        self.gate.send_if_modified(|gate| {
            admitted = !gate.closed;
            if admitted {
                gate.in_flight += 1;
            }
            admitted
        });
        admitted.then(|| WriteTicket { leases: self })
    }

    /// Void `route`'s lease: the device it was taken on has left the route.
    pub(crate) fn forget(&self, route: &DeviceRoute) {
        self.lock().remove(&route.to_string());
    }

    /// Refuse new writes, wait until `deadline` for admitted ones to record
    /// their leases, then remove and return every lease. A write still running
    /// at the deadline keeps the control it takes.
    pub(crate) async fn close(&self, deadline: Instant) -> Vec<(DeviceRoute, B, L)> {
        self.gate.send_modify(|gate| gate.closed = true);
        let mut gate = self.gate.subscribe();
        let settled = tokio::time::timeout_at(deadline, gate.wait_for(|gate| gate.in_flight == 0))
            .await
            .is_ok();
        if !settled {
            let in_flight = self.gate.borrow().in_flight;
            warn!(
                in_flight,
                "lighting writes still running at exit keep their RGB control"
            );
        }
        self.lock()
            .drain()
            .map(|(_, held)| (held.route, held.binding, held.lease))
            .collect()
    }
}

/// One admitted lighting write. [`Leases::close`] waits until it is recorded
/// or dropped.
pub(crate) struct WriteTicket<'a, B, L> {
    leases: &'a Leases<B, L>,
}

impl<B: PartialEq, L> WriteTicket<'_, B, L> {
    /// Record `lease`, taken on `route` through `binding`. The first lease per
    /// connection is kept: later writes find control already held, so only
    /// the first knows the flags from before the claim. A lease through
    /// another connection replaces it, because another device now answers on
    /// the route, or the same one after a replug dropped its volatile state.
    pub(crate) fn record(self, route: &DeviceRoute, binding: B, lease: L) {
        let mut held = self.leases.lock();
        let key = route.to_string();
        if held.get(&key).is_none_or(|held| held.binding != binding) {
            held.insert(
                key,
                Held {
                    route: route.clone(),
                    binding,
                    lease,
                },
            );
        }
    }
}

impl<B, L> Drop for WriteTicket<'_, B, L> {
    fn drop(&mut self) {
        self.leases.gate.send_modify(|gate| gate.in_flight -= 1);
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    fn route(slot: u8) -> DeviceRoute {
        DeviceRoute::Unifying {
            receiver_uid: "rgb-lease".into(),
            slot,
        }
    }

    fn sorted(mut leases: Vec<(DeviceRoute, u8, u8)>) -> Vec<(DeviceRoute, u8, u8)> {
        leases.sort_by_key(|(route, ..)| route.to_string());
        leases
    }

    fn record(leases: &Leases<u8, u8>, slot: u8, connection: u8, lease: u8) {
        leases
            .admit()
            .unwrap()
            .record(&route(slot), connection, lease);
    }

    #[tokio::test(start_paused = true)]
    async fn keeps_the_first_lease_per_connection_and_replaces_another_connection() {
        let leases = Leases::default();
        record(&leases, 1, 1, 0x82);
        record(&leases, 1, 1, 0x83);
        record(&leases, 2, 1, 0x84);
        record(&leases, 2, 2, 0x85);

        let closed = sorted(leases.close(Instant::now()).await);
        assert_eq!(closed, [(route(1), 1, 0x82), (route(2), 2, 0x85)]);
        assert!(leases.close(Instant::now()).await.is_empty());
    }

    #[tokio::test(start_paused = true)]
    async fn forget_voids_the_route_it_names() {
        let leases = Leases::default();
        record(&leases, 1, 1, 0x82);
        record(&leases, 2, 1, 0x82);
        leases.forget(&route(1));

        let closed = leases.close(Instant::now()).await;
        assert_eq!(closed, [(route(2), 1, 0x82)]);
    }

    #[tokio::test(start_paused = true)]
    async fn close_waits_for_admitted_writes_and_refuses_new_ones() {
        let leases = Leases::default();
        let recorded = leases.admit().unwrap();
        let abandoned = leases.admit().unwrap();
        let write = async {
            tokio::time::sleep(Duration::from_millis(300)).await;
            drop(abandoned);
            tokio::time::sleep(Duration::from_millis(300)).await;
            recorded.record(&route(1), 1, 0x82);
        };
        let deadline = Instant::now() + Duration::from_secs(2);
        let ((), closed) = tokio::join!(write, leases.close(deadline));

        assert_eq!(closed, [(route(1), 1, 0x82)]);
        assert!(
            Instant::now() < deadline,
            "close returns once writes settle"
        );
        assert!(leases.admit().is_none(), "no write starts after close");
    }

    #[tokio::test(start_paused = true)]
    async fn close_gives_up_on_a_write_still_running_at_the_deadline() {
        let leases = Leases::<u8, u8>::default();
        let running = leases.admit().unwrap();
        let deadline = Instant::now() + Duration::from_secs(2);

        assert!(leases.close(deadline).await.is_empty());
        assert_eq!(Instant::now(), deadline);
        drop(running);
    }
}
