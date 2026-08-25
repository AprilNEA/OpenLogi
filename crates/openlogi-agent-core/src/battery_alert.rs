//! The low-battery alert state machine.
//!
//! Pure: no OS calls, no I/O, no async, no clock. Fed an inventory snapshot it
//! returns the alerts to raise *now*, and remembers just enough not to raise
//! them again until the device actually recovers.
//!
//! Triggering needs both of the device's signals to agree. [`BatteryLevel`]
//! alone is not enough: only the unified `0x1004` feature reports a level the
//! firmware authored, while the legacy `0x1000` and voltage `0x1001` paths
//! have `openlogi-hid` derive one from the percentage using display buckets
//! that already call 20-49% `Low` — a reasonable ramp for picking an icon, far
//! too eager for interrupting someone. The percentage alone is not enough
//! either, since a device can report a sane level beside a junk reading. So a
//! device alerts on a `Low` level only when the reading agrees. `Critical`
//! needs no corroboration — no path can raise it spuriously.

use std::collections::{HashMap, HashSet};

use openlogi_core::device::{
    BatteryInfo, BatteryLevel, BatteryStatus, DeviceInventory, ReceiverInfo,
};

/// Stable per-device identity for alert bookkeeping: the receiver's USB
/// identity plus the device's pairing slot.
///
/// Deliberately *not* `DeviceRoute`: that type crosses the IPC wire and does
/// not derive `Hash`, and adding a derive to a wire type for a bookkeeping
/// convenience is the wrong trade. A direct (Bluetooth/wired) device carries a
/// synthetic receiver mirroring its own identity, so this key works there too.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct DeviceKey {
    vendor_id: u16,
    product_id: u16,
    unique_id: Option<String>,
    slot: u8,
}

impl DeviceKey {
    fn new(receiver: &ReceiverInfo, slot: u8) -> Self {
        Self {
            vendor_id: receiver.vendor_id,
            product_id: receiver.product_id,
            unique_id: receiver.unique_id.clone(),
            slot,
        }
    }
}

/// One alert to surface to the user.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BatteryAlert {
    /// The device's user-facing name (see `PairedDevice::display_name`).
    pub name: String,
    /// The severity this alert was raised at, derived from
    /// [`percentage`](Self::percentage) — not necessarily the level the
    /// device reported.
    pub level: BatteryLevel,
    /// The reading at the moment of crossing.
    pub percentage: u8,
}

/// Remembers which devices have already alerted, and at what level.
///
/// An entry exists only while a device is in an alerted state; recovering,
/// charging, or disappearing removes it, which is what re-arms the alert.
#[derive(Debug, Default)]
pub struct BatteryAlerts {
    fired: HashMap<DeviceKey, BatteryLevel>,
}

impl BatteryAlerts {
    /// Fold one inventory snapshot in and return the alerts to raise now.
    ///
    /// Fires on `→ Low` and on `Low → Critical`, with the severity taken from
    /// the reading rather than the reported level. Never fires twice for the
    /// same severity, never fires on a level the firmware calls
    /// [`BatteryLevel::Unknown`], never fires while the reading is above the
    /// crate's `LOW_PERCENTAGE` threshold, and never fires for a device on
    /// power.
    pub fn evaluate(&mut self, inventories: &[DeviceInventory]) -> Vec<BatteryAlert> {
        let mut alerts = Vec::new();
        let mut present: HashSet<DeviceKey> = HashSet::new();

        for inventory in inventories {
            for device in &inventory.paired {
                let key = DeviceKey::new(&inventory.receiver, device.slot);
                // Recorded before the online/battery checks: a device that is
                // merely asleep has not recovered, so it keeps its state and
                // must not re-alert at the same level when it comes back.
                present.insert(key.clone());

                if !device.online {
                    continue;
                }
                let Some(battery) = device.battery.as_ref() else {
                    continue;
                };

                // On power is not a problem, whatever the level says.
                if matches!(
                    battery.status,
                    BatteryStatus::Charging | BatteryStatus::ChargingSlow | BatteryStatus::Full
                ) {
                    self.fired.remove(&key);
                    continue;
                }

                match battery.level {
                    BatteryLevel::Low | BatteryLevel::Critical => {
                        // A bare `Low` is not enough on its own: on the
                        // legacy and voltage paths it is a display bucket
                        // `openlogi-hid` derived from this very percentage,
                        // and that bucket starts at 49%. Below, the reading
                        // has to agree. Disagreement neither fires nor
                        // re-arms: a device that is simply not low yet has
                        // not recovered from anything.
                        let Some(level) = alert_level(battery.level, battery.percentage) else {
                            continue;
                        };
                        let fire = match self.fired.get(&key) {
                            None => true,
                            Some(BatteryLevel::Low) => level == BatteryLevel::Critical,
                            Some(_) => false,
                        };
                        if fire {
                            self.fired.insert(key, level);
                            alerts.push(BatteryAlert {
                                name: device.display_name(),
                                level,
                                percentage: battery.percentage,
                            });
                        }
                    }
                    BatteryLevel::Good | BatteryLevel::Full => {
                        self.fired.remove(&key);
                    }
                    // No opinion: neither fires nor re-arms. A device that
                    // flickers Low → Unknown → Low would otherwise alert twice
                    // for one drain.
                    BatteryLevel::Unknown => {}
                }
            }
        }

        // A device that left the inventory is genuinely new when it returns.
        self.fired.retain(|key, _| present.contains(key));
        alerts
    }
}

/// Reading at or below which a discharging device is worth interrupting the
/// user about.
///
/// The same threshold the GUI device card and the tray glyph warn at — an
/// alert for a device neither of them is flagging would look like a bug.
const LOW_PERCENTAGE: u8 = BatteryInfo::ATTENTION_PERCENTAGE;

/// Reading at or below which it is worth interrupting them a second time.
const CRITICAL_PERCENTAGE: u8 = 10;

/// The alert this reading justifies, or `None` when it justifies none.
///
/// `Critical` is honoured wherever it comes from, because no producer can
/// raise it spuriously: `0x1004` reports it from firmware, `0x1001` sets it
/// from the firmware's own critical marker, and `0x1000`'s bucket cannot
/// reach it above 19%.
///
/// `Low` is the eager one — the `0x1000` and `0x1001` bucket already calls
/// 20-49% `Low` — so it has to clear a threshold of its own. Fixed rather
/// than the device's: `0x1000`'s capability query (function 1), which would
/// give the firmware's own thresholds, is not wired up.
fn alert_level(level: BatteryLevel, percentage: u8) -> Option<BatteryLevel> {
    if level == BatteryLevel::Critical || percentage <= CRITICAL_PERCENTAGE {
        return Some(BatteryLevel::Critical);
    }
    (percentage <= LOW_PERCENTAGE).then_some(BatteryLevel::Low)
}

#[cfg(test)]
mod tests {
    use openlogi_core::device::{
        BatteryInfo, BatteryLevel, BatteryStatus, DeviceInventory, DeviceKind, PairedDevice,
        ReceiverInfo,
    };

    use super::BatteryAlerts;

    #[test]
    fn crossing_into_low_fires_once() {
        let mut alerts = BatteryAlerts::default();
        let good = inventory(&[(
            "MX Master 3S",
            1,
            80,
            BatteryLevel::Good,
            BatteryStatus::Discharging,
        )]);
        assert!(alerts.evaluate(&good).is_empty());

        let low = inventory(&[(
            "MX Master 3S",
            1,
            18,
            BatteryLevel::Low,
            BatteryStatus::Discharging,
        )]);
        let fired = alerts.evaluate(&low);
        assert_eq!(fired.len(), 1);
        assert_eq!(fired[0].name, "MX Master 3S");
        assert_eq!(fired[0].percentage, 18);

        // Same reading again must stay silent.
        assert!(alerts.evaluate(&low).is_empty());
    }

    #[test]
    fn low_then_critical_fires_a_second_time() {
        let mut alerts = BatteryAlerts::default();
        let low = inventory(&[(
            "MX Keys",
            1,
            18,
            BatteryLevel::Low,
            BatteryStatus::Discharging,
        )]);
        let critical = inventory(&[(
            "MX Keys",
            1,
            4,
            BatteryLevel::Critical,
            BatteryStatus::Discharging,
        )]);
        assert_eq!(alerts.evaluate(&low).len(), 1);
        let fired = alerts.evaluate(&critical);
        assert_eq!(fired.len(), 1);
        assert_eq!(fired[0].level, BatteryLevel::Critical);
    }

    #[test]
    fn critical_back_to_low_is_silent() {
        let mut alerts = BatteryAlerts::default();
        let critical = inventory(&[(
            "MX Keys",
            1,
            4,
            BatteryLevel::Critical,
            BatteryStatus::Discharging,
        )]);
        let low = inventory(&[(
            "MX Keys",
            1,
            15,
            BatteryLevel::Low,
            BatteryStatus::Discharging,
        )]);
        assert_eq!(alerts.evaluate(&critical).len(), 1);
        assert!(alerts.evaluate(&low).is_empty());
    }

    #[test]
    fn recovering_to_good_rearms() {
        let mut alerts = BatteryAlerts::default();
        let low = inventory(&[("Lift", 1, 18, BatteryLevel::Low, BatteryStatus::Discharging)]);
        let good = inventory(&[(
            "Lift",
            1,
            70,
            BatteryLevel::Good,
            BatteryStatus::Discharging,
        )]);
        assert_eq!(alerts.evaluate(&low).len(), 1);
        assert!(alerts.evaluate(&good).is_empty());
        assert_eq!(
            alerts.evaluate(&low).len(),
            1,
            "a recovered device must be able to alert again"
        );
    }

    #[test]
    fn charging_rearms_even_while_still_low() {
        let mut alerts = BatteryAlerts::default();
        let low = inventory(&[("Lift", 1, 18, BatteryLevel::Low, BatteryStatus::Discharging)]);
        let charging = inventory(&[("Lift", 1, 18, BatteryLevel::Low, BatteryStatus::Charging)]);
        assert_eq!(alerts.evaluate(&low).len(), 1);
        assert!(alerts.evaluate(&charging).is_empty());
        assert_eq!(
            alerts.evaluate(&low).len(),
            1,
            "unplugging while still low must alert again"
        );
    }

    #[test]
    fn unknown_level_never_fires_and_does_not_rearm() {
        let mut alerts = BatteryAlerts::default();
        let low = inventory(&[("Lift", 1, 18, BatteryLevel::Low, BatteryStatus::Discharging)]);
        let unknown = inventory(&[("Lift", 1, 0, BatteryLevel::Unknown, BatteryStatus::Unknown)]);
        assert_eq!(alerts.evaluate(&low).len(), 1);
        assert!(
            alerts.evaluate(&unknown).is_empty(),
            "Unknown must not fire"
        );
        assert!(
            alerts.evaluate(&low).is_empty(),
            "a flicker through Unknown must not re-arm and double-fire"
        );
    }

    #[test]
    fn a_vanished_device_rearms_on_return() {
        let mut alerts = BatteryAlerts::default();
        let low = inventory(&[("Lift", 1, 18, BatteryLevel::Low, BatteryStatus::Discharging)]);
        assert_eq!(alerts.evaluate(&low).len(), 1);
        assert!(alerts.evaluate(&[]).is_empty());
        assert_eq!(
            alerts.evaluate(&low).len(),
            1,
            "an unpaired-then-repaired device is new again"
        );
    }

    #[test]
    fn an_offline_device_keeps_its_fired_state() {
        let mut alerts = BatteryAlerts::default();
        let low = inventory(&[("Lift", 1, 18, BatteryLevel::Low, BatteryStatus::Discharging)]);
        assert_eq!(alerts.evaluate(&low).len(), 1);

        let mut offline = low.clone();
        offline[0].paired[0].online = false;
        offline[0].paired[0].battery = None;
        assert!(alerts.evaluate(&offline).is_empty());
        assert!(
            alerts.evaluate(&low).is_empty(),
            "a device that merely slept must not re-alert at the same level"
        );
    }

    #[test]
    fn two_devices_alert_independently() {
        let mut alerts = BatteryAlerts::default();
        let both = inventory(&[
            (
                "MX Master 3S",
                1,
                18,
                BatteryLevel::Low,
                BatteryStatus::Discharging,
            ),
            (
                "MX Keys",
                2,
                70,
                BatteryLevel::Good,
                BatteryStatus::Discharging,
            ),
        ]);
        assert_eq!(alerts.evaluate(&both).len(), 1);

        let both_low = inventory(&[
            (
                "MX Master 3S",
                1,
                17,
                BatteryLevel::Low,
                BatteryStatus::Discharging,
            ),
            (
                "MX Keys",
                2,
                15,
                BatteryLevel::Low,
                BatteryStatus::Discharging,
            ),
        ]);
        let fired = alerts.evaluate(&both_low);
        assert_eq!(fired.len(), 1, "only the newly-low device alerts");
        assert_eq!(fired[0].name, "MX Keys");
    }

    #[test]
    fn a_device_without_battery_is_ignored() {
        let mut alerts = BatteryAlerts::default();
        let mut none = inventory(&[("G502", 1, 0, BatteryLevel::Unknown, BatteryStatus::Unknown)]);
        none[0].paired[0].battery = None;
        assert!(alerts.evaluate(&none).is_empty());
    }

    /// Regression: a G502 LIGHTSPEED on the legacy `0x1000` feature has no
    /// firmware level, so `openlogi-hid` derives one from the percentage with
    /// display buckets where 20-49% is already `Low`. Half a charge is not an
    /// alert.
    #[test]
    fn a_derived_low_level_at_half_charge_does_not_fire() {
        let mut alerts = BatteryAlerts::default();
        let half = inventory(&[("G502", 1, 49, BatteryLevel::Low, BatteryStatus::Discharging)]);
        assert!(alerts.evaluate(&half).is_empty());
    }

    #[test]
    fn the_low_threshold_is_inclusive() {
        let mut alerts = BatteryAlerts::default();
        let above = inventory(&[("G502", 1, 21, BatteryLevel::Low, BatteryStatus::Discharging)]);
        assert!(alerts.evaluate(&above).is_empty());

        let at = inventory(&[("G502", 1, 20, BatteryLevel::Low, BatteryStatus::Discharging)]);
        assert_eq!(alerts.evaluate(&at).len(), 1);
    }

    #[test]
    fn a_reading_above_the_threshold_does_not_rearm() {
        let mut alerts = BatteryAlerts::default();
        let low = inventory(&[("G502", 1, 18, BatteryLevel::Low, BatteryStatus::Discharging)]);
        let bucket_low =
            inventory(&[("G502", 1, 40, BatteryLevel::Low, BatteryStatus::Discharging)]);
        assert_eq!(alerts.evaluate(&low).len(), 1);
        assert!(alerts.evaluate(&bucket_low).is_empty());
        assert!(
            alerts.evaluate(&low).is_empty(),
            "a device that never recovered must not alert twice"
        );
    }

    /// A G502 on `0x1001` has its level forced to `Critical` by the
    /// firmware's own marker, which outranks the voltage-curve estimate. That
    /// signal must survive the threshold that exists to suppress the derived
    /// `Low` bucket.
    #[test]
    fn a_firmware_critical_marker_fires_above_the_low_threshold() {
        let mut alerts = BatteryAlerts::default();
        let marked = inventory(&[(
            "G502",
            1,
            30,
            BatteryLevel::Critical,
            BatteryStatus::Discharging,
        )]);
        let fired = alerts.evaluate(&marked);
        assert_eq!(fired.len(), 1);
        assert_eq!(fired[0].level, BatteryLevel::Critical);
        assert_eq!(fired[0].percentage, 30);
    }

    #[test]
    fn a_low_reading_alone_is_critical_below_ten() {
        let mut alerts = BatteryAlerts::default();
        // The legacy bucket calls 15% `Low`; the reading is above the critical
        // threshold, so the alert agrees.
        let low = inventory(&[("G502", 1, 15, BatteryLevel::Low, BatteryStatus::Discharging)]);
        assert_eq!(alerts.evaluate(&low)[0].level, BatteryLevel::Low);

        let mut alerts = BatteryAlerts::default();
        let deep = inventory(&[("G502", 1, 8, BatteryLevel::Low, BatteryStatus::Discharging)]);
        assert_eq!(alerts.evaluate(&deep)[0].level, BatteryLevel::Critical);
    }

    /// One receiver holding the described devices.
    /// Tuple: (codename, slot, percentage, level, status).
    fn inventory(devices: &[(&str, u8, u8, BatteryLevel, BatteryStatus)]) -> Vec<DeviceInventory> {
        vec![DeviceInventory {
            receiver: ReceiverInfo {
                name: "Logi Bolt Receiver".to_string(),
                vendor_id: 0x046d,
                product_id: 0xc548,
                unique_id: Some("bolt-1".to_string()),
            },
            paired: devices
                .iter()
                .map(
                    |&(codename, slot, percentage, level, status)| PairedDevice {
                        slot,
                        codename: Some(codename.to_string()),
                        wpid: None,
                        kind: DeviceKind::Mouse,
                        online: true,
                        battery: Some(BatteryInfo {
                            percentage,
                            level,
                            status,
                        }),
                        model_info: None,
                        capabilities: None,
                    },
                )
                .collect(),
        }]
    }
}
