//! Route-scoped teardown debts. The payload is opaque to the scheduler so its
//! transitions can be tested without opening a device or synthesizing input.

use std::collections::HashSet;

use openlogi_core::device_order::PhysicalDeviceKey;
use openlogi_hid::DeviceRoute;
use tokio::time::Instant;

use crate::capture_plan::DeviceCapturePlan;

pub(super) struct PendingRestore<T> {
    pub(super) key: PhysicalDeviceKey,
    pub(super) route: DeviceRoute,
    pub(super) token: T,
    pub(super) retry_at: Instant,
}

pub(super) struct RestoreQueue<T> {
    pending: Vec<PendingRestore<T>>,
}

impl<T> Default for RestoreQueue<T> {
    fn default() -> Self {
        Self {
            pending: Vec::new(),
        }
    }
}

impl<T> RestoreQueue<T> {
    pub(super) fn is_empty(&self) -> bool {
        self.pending.is_empty()
    }

    pub(super) fn retain(&mut self, pending: PendingRestore<T>) {
        self.pending.push(pending);
    }

    pub(super) fn blocks(&self, key: &PhysicalDeviceKey, route: &DeviceRoute) -> bool {
        self.pending
            .iter()
            .any(|pending| pending.key == *key && pending.route == *route)
    }

    pub(super) fn expedite(&mut self, now: Instant) {
        for pending in &mut self.pending {
            pending.retry_at = now;
        }
    }

    pub(super) fn next_deadline(
        &self,
        busy: &HashSet<PhysicalDeviceKey>,
        published: &[DeviceCapturePlan],
    ) -> Option<Instant> {
        self.pending
            .iter()
            .filter(|pending| pending.eligible(busy, published))
            .map(|pending| pending.retry_at)
            .min()
    }

    pub(super) fn take_due(
        &mut self,
        now: Instant,
        busy: &HashSet<PhysicalDeviceKey>,
        published: &[DeviceCapturePlan],
    ) -> Vec<PendingRestore<T>> {
        self.pending
            .extract_if(.., |pending| {
                pending.retry_at <= now && pending.eligible(busy, published)
            })
            .collect()
    }
}

impl<T> PendingRestore<T> {
    fn eligible(&self, busy: &HashSet<PhysicalDeviceKey>, published: &[DeviceCapturePlan]) -> bool {
        if busy.contains(&self.key) {
            return false;
        }
        // Never clear diversion underneath another live route of this device,
        // nor restore stale state into a route now occupied by another device.
        !published.iter().any(|plan| {
            let target = &plan.target;
            (target.physical_key == self.key && target.route != self.route)
                || (target.route == self.route && target.physical_key != self.key)
        })
    }
}

#[cfg(test)]
mod tests;
