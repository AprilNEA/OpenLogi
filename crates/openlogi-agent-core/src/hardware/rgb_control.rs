//! `0x8071` software control the agent took this session, owed back to the
//! firmware when it leaves.

use std::collections::HashMap;
use std::sync::{LazyLock, Mutex, PoisonError};

use openlogi_hid::DeviceRoute;
use openlogi_hid::write::RgbControlLease;

/// Every route the agent's lighting writes claimed since startup.
pub(crate) static SESSION: LazyLock<Leases<RgbControlLease>> = LazyLock::new(Leases::default);

/// The first lease each route took. Later writes find control already held,
/// so only the first one knows what the device had before the agent.
pub(crate) struct Leases<L>(Mutex<HashMap<String, (DeviceRoute, L)>>);

impl<L> Default for Leases<L> {
    fn default() -> Self {
        Self(Mutex::new(HashMap::new()))
    }
}

impl<L> Leases<L> {
    /// Keep `lease` unless `route` already holds one.
    pub(crate) fn record(&self, route: &DeviceRoute, lease: L) {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .entry(route.to_string())
            .or_insert_with(|| (route.clone(), lease));
    }

    /// Remove and return every lease, for the exit hand-back.
    pub(crate) fn take(&self) -> Vec<(DeviceRoute, L)> {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .drain()
            .map(|(_, entry)| entry)
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn route(slot: u8) -> DeviceRoute {
        DeviceRoute::Unifying {
            receiver_uid: "rgb-lease".into(),
            slot,
        }
    }

    #[test]
    fn keeps_the_first_lease_per_route_and_drains_on_take() {
        let leases = Leases::default();
        let (first, held) = (0x82_u8, 0x83_u8);
        leases.record(&route(1), first);
        leases.record(&route(1), held);
        leases.record(&route(2), held);

        let mut taken = leases.take();
        taken.sort_by_key(|(route, _)| route.to_string());
        assert_eq!(taken, [(route(1), first), (route(2), held)]);
        assert!(leases.take().is_empty(), "take hands each lease back once");
    }
}
