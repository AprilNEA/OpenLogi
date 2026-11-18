//! A physical link observation can override an older inventory scan. A return
//! also owes the agent one settings replay, even if a scan sees the unit first.

use std::sync::Weak;
use std::time::Instant;

use hidpp::channel::HidppChannel;
use openlogi_core::device::{DeviceInventory, PairedDevice};

use crate::DeviceRoute;

pub(super) struct ConfirmedLink {
    pub(super) route: DeviceRoute,
    pub(super) channel: Weak<HidppChannel>,
    unit: [u8; 4],
    at: Instant,
    phase: LinkPhase,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum LinkPhase {
    Departed,
    ArrivedPendingReplay,
    ArrivedReplayed,
    /// A newer healthy scan now owns presence; only replay remains.
    ScanConfirmedPendingReplay,
}

impl ConfirmedLink {
    pub(super) fn new(
        route: DeviceRoute,
        channel: Weak<HidppChannel>,
        unit: [u8; 4],
        online: bool,
    ) -> Self {
        Self {
            route,
            channel,
            unit,
            at: Instant::now(),
            phase: if online {
                LinkPhase::ArrivedPendingReplay
            } else {
                LinkPhase::Departed
            },
        }
    }

    fn paired_index(&self, inventory: &DeviceInventory) -> Option<usize> {
        inventory.paired.iter().position(|p| {
            p.model_info
                .as_ref()
                .is_some_and(|m| m.unit_id == self.unit)
                && DeviceRoute::for_slot(inventory, p.slot).as_ref() == Some(&self.route)
        })
    }

    fn apply_presence(&self, device: &mut PairedDevice) {
        match self.phase {
            LinkPhase::Departed => device.online = false,
            LinkPhase::ArrivedPendingReplay | LinkPhase::ArrivedReplayed => device.online = true,
            LinkPhase::ScanConfirmedPendingReplay => {}
        }
    }

    fn owes_replay(&self) -> bool {
        matches!(
            self.phase,
            LinkPhase::ArrivedPendingReplay | LinkPhase::ScanConfirmedPendingReplay
        )
    }

    /// Return whether the registry still needs this observation after the scan.
    pub(super) fn observe_scan(
        &mut self,
        inventory: &mut DeviceInventory,
        started: Instant,
        healthy: bool,
    ) -> bool {
        let index = self.paired_index(inventory);
        if healthy && started >= self.at {
            if self.owes_replay() && index.is_some_and(|i| inventory.paired[i].online) {
                self.phase = LinkPhase::ScanConfirmedPendingReplay;
                return true;
            }
            return false;
        }
        if let Some(index) = index {
            self.apply_presence(&mut inventory.paired[index]);
        }
        true
    }

    /// Project presence into every matching row, issue replay once, and retire
    /// the observation once neither presence nor replay needs it.
    pub(super) fn reconcile(
        &mut self,
        inventories: &mut [DeviceInventory],
        returned: &mut Vec<(DeviceRoute, [u8; 4])>,
    ) -> bool {
        let mut online = false;
        for inventory in inventories {
            if let Some(index) = self.paired_index(inventory) {
                let device = &mut inventory.paired[index];
                self.apply_presence(device);
                online |= device.online;
            }
        }
        if online && self.owes_replay() {
            returned.push((self.route.clone(), self.unit));
            if self.phase == LinkPhase::ScanConfirmedPendingReplay {
                return false;
            }
            self.phase = LinkPhase::ArrivedReplayed;
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::ConfirmedLink;
    use crate::DeviceRoute;
    use crate::channel::scripted::bolt_inventory;
    use std::slice;
    use std::sync::Weak;
    use std::time::Duration;

    const UNIT: [u8; 4] = [1, 2, 3, 4];

    fn arrival() -> ConfirmedLink {
        let inventory = bolt_inventory(UNIT, false);
        let route = DeviceRoute::for_slot(&inventory, 1).unwrap();
        ConfirmedLink::new(route, Weak::new(), UNIT, true)
    }

    #[test]
    fn replay_before_scan_preserves_presence_without_reissuing_replay() {
        let mut link = arrival();
        let mut offline = bolt_inventory(UNIT, false);
        let earlier = link.at.checked_sub(Duration::from_millis(1)).unwrap();
        let mut returned = Vec::new();
        assert!(link.observe_scan(&mut offline, earlier, true));
        assert!(
            offline.paired[0].online,
            "older scan cannot revoke the arrival"
        );
        assert!(link.reconcile(slice::from_mut(&mut offline), &mut returned));
        assert_eq!(returned, [(link.route.clone(), UNIT)]);

        offline.paired[0].online = false;
        assert!(link.observe_scan(&mut offline, link.at, false));
        assert!(
            offline.paired[0].online,
            "partial scan cannot revoke the arrival"
        );
        returned.clear();
        assert!(link.reconcile(slice::from_mut(&mut offline), &mut returned));
        assert!(
            returned.is_empty(),
            "presence override must not repeat replay"
        );

        offline.paired[0].online = false;
        assert!(!link.observe_scan(&mut offline, link.at, true));
        assert!(!offline.paired[0].online, "newer healthy scan takes over");
    }

    #[test]
    fn scan_before_replay_retires_the_link_only_after_replay() {
        let mut link = arrival();
        let mut scanned = bolt_inventory(UNIT, true);
        let mut returned = Vec::new();
        assert!(link.observe_scan(&mut scanned, link.at, true));

        let mut offline = bolt_inventory(UNIT, false);
        assert!(link.reconcile(slice::from_mut(&mut offline), &mut returned));
        assert!(!offline.paired[0].online, "scan now owns presence");
        assert!(
            returned.is_empty(),
            "offline snapshot cannot consume replay"
        );

        assert!(!link.reconcile(slice::from_mut(&mut scanned), &mut returned));
        assert_eq!(returned, [(link.route.clone(), UNIT)]);
    }

    #[test]
    fn a_healthy_scan_can_retire_an_unreplayed_or_replaced_return() {
        for mut inventory in [bolt_inventory(UNIT, false), bolt_inventory([9; 4], true)] {
            let mut link = arrival();
            assert!(!link.observe_scan(&mut inventory, link.at, true));
        }
    }
}
