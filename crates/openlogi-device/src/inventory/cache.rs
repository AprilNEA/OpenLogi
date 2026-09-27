use std::sync::Arc;
use std::time::{Duration, Instant};

use hidpp::channel::HidppChannel;
use openlogi_core::device::{BatteryInfo, BatteryLevel, BatteryStatus};

use super::events::{EventFeatureIndices, EventSubscriptionHandle};
use super::features::{BatteryProbe, ProbedFeatures, probe_features, read_battery};
use crate::backend::NodeId;

/// How long a device's probe is reused before a fresh read.
/// The expensive part of a probe (the `enumerate_features` feature-table walk)
/// reads *immutable* data — model, capabilities, marketing type — so it never
/// needs re-reading for a known device; the periodic full probe is kept only as
/// a self-healing pass (e.g. a firmware update reshuffling the feature table).
/// The volatile battery does NOT ride this window: cache hits re-read it every
/// reconciliation through the memoized feature index (see [`read_battery`]).
/// Elapsed time rather than scan count preserves cache freshness now that
/// event-driven scans are intentionally irregular.
pub(super) const REFRESH_INTERVAL: Duration = Duration::from_secs(30);

/// Stable identity used to memoize a device's probe across `enumerate` ticks.
/// Keyed on the device's *own* identity (never its slot) so a re-paired or
/// moved device can't inherit another device's cached probe.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(super) enum CacheKey {
    /// Bolt: the unit id from the pairing register (cheap, read every tick).
    Bolt { unit_id: [u8; 4] },
    /// Unifying: keyed on the full receiver serial number + pairing slot.
    /// Using the complete serial (not just a prefix) avoids collisions between
    /// two receivers whose serials share a common prefix (e.g. "DA2699E1" and
    /// "DA2604F2" share "DA2").
    UnifyingSlot { receiver_uid: String, slot: u8 },
    /// Direct (Bluetooth/USB): the OS-assigned HID node id (macOS registry-entry
    /// id, Linux dev path, Windows interface path). Unique *per node*, so two
    /// units of the same model never collide, and stable while connected so the
    /// cache still hits across ticks.
    Direct(NodeId),
}

/// Enumeration ticks a device may be missing before its cache entry is evicted.
/// A small grace rides out a transient receiver timeout without dropping the
/// device's memoized data.
pub(super) const CACHE_MISS_GRACE: u8 = 3;

/// A memoized probe result plus the instant it was taken.
#[derive(Clone)]
pub(super) struct Cached {
    pub(super) probe: ProbedFeatures,
    /// Which battery feature this device exposes and its runtime index, captured
    /// by the full probe. Lets cache hits re-read the volatile battery in one
    /// round-trip — no `Device::new` ping, no table walk. `None` when the device
    /// exposes neither `0x1004` nor the legacy `0x1000`.
    pub(super) battery: Option<BatteryProbe>,
    /// Event-capable feature indexes found by the same immutable table walk.
    pub(super) events: EventFeatureIndices,
    pub(super) probed_at: Instant,
}

/// Carry the last-known percentage through a charge for battery sources that
/// cannot gauge charge under load, so the reading stays trackable.
///
/// - The legacy `0x1000` feature (MX2S-era mice) reports `discharge_level = 0`
///   while charging, which the GUI would show as a misleading "Charging · 0%".
///   Only that charging-and-zero sentinel is held; a genuine 0% while
///   discharging is untouched.
/// - The voltage sources `0x1001` and `0x1F20` report millivolt, and the
///   charge current lifts that reading: a G733 at an estimated 65% on battery
///   reads 96% seconds after being plugged in. Every charging reading is held.
///   Charge complete (`Full`) reads live again.
///
/// A *frozen* pre-charge value, not a live charging %, because none of these
/// sources exposes one. Cold edge: a device first seen while already charging
/// has no prior. `0x1000` then shows its own 0%, and a voltage source reports
/// the same 0% with an unknown level, the "charging, no reading" state, until
/// the first discharge read.
fn hold_percentage_while_charging(
    fresh: BatteryInfo,
    prev: Option<&BatteryInfo>,
    probe: BatteryProbe,
) -> BatteryInfo {
    let charging = matches!(
        fresh.status,
        BatteryStatus::Charging | BatteryStatus::ChargingSlow
    );
    if !charging {
        return fresh;
    }
    let held = |p: &BatteryInfo| BatteryInfo {
        percentage: p.percentage,
        level: p.level,
        status: fresh.status,
    };
    match (probe, prev) {
        (BatteryProbe::Legacy(_), Some(p)) if fresh.percentage == 0 && p.percentage > 0 => held(p),
        (BatteryProbe::Voltage(_) | BatteryProbe::Adc(_), Some(p)) => held(p),
        (BatteryProbe::Voltage(_) | BatteryProbe::Adc(_), None) => BatteryInfo {
            percentage: 0,
            level: BatteryLevel::Unknown,
            status: fresh.status,
        },
        // A `0x1004` device that legitimately reports 0% while charging must
        // surface that, not a stale prior reading.
        _ => fresh,
    }
}

/// What a probed device contributes to the cache this tick. The key lets stale
/// entries be evicted; `Fresh` (a full probe) and `Update` (a cache hit whose
/// volatile battery was re-read) also carry the value to insert. `Unkeyed` is a
/// device we can't (or won't) cache — an all-zero unit id, or a rejected
/// non-peripheral — so its key is neither inserted nor kept alive.
pub(super) enum CacheOutcome {
    Fresh(CacheKey, Cached),
    Update(CacheKey, Cached),
    Seen(CacheKey),
    Unkeyed,
}

impl CacheOutcome {
    /// The entry this outcome keeps alive, if it has one.
    pub(super) fn key(&self) -> Option<&CacheKey> {
        match self {
            Self::Fresh(key, _) | Self::Update(key, _) | Self::Seen(key) => Some(key),
            Self::Unkeyed => None,
        }
    }
}

/// `Seen` when the device has a stable key, else `Unkeyed`.
pub(super) fn seen(id: Option<CacheKey>) -> CacheOutcome {
    id.map_or(CacheOutcome::Unkeyed, CacheOutcome::Seen)
}

/// Whether `cached` is stale enough that the device should be re-probed.
pub(super) fn is_stale(cached: &Cached, now: Instant) -> bool {
    now.saturating_duration_since(cached.probed_at) >= REFRESH_INTERVAL
}

/// Decide a device's probe: reuse a fresh cache, or (online + miss/stale)
/// re-probe — but keep the last-known immutable data if the re-probe fails
/// rather than overwriting it with an empty default. An unprobed offline device
/// with no cache yields a default probe. Returns the probe plus its cache
/// contribution (only a *successful* probe is cached).
pub(super) async fn probe_or_reuse(
    channel: &Arc<HidppChannel>,
    index: u8,
    id: Option<CacheKey>,
    cached: Option<&Cached>,
    online: bool,
    now: Instant,
    subscriptions: Option<&EventSubscriptionHandle>,
) -> (ProbedFeatures, CacheOutcome) {
    if let (Some(cached), Some(subscriptions)) = (cached, subscriptions) {
        subscriptions.register_device(index, cached.events);
    }
    if online && cached.is_none_or(|c| is_stale(c, now)) {
        let (mut fresh, battery, events) = probe_features(channel, index, subscriptions).await;
        if let (Some(reading), Some(probe)) = (fresh.battery.take(), battery) {
            fresh.battery = Some(hold_percentage_while_charging(
                reading,
                cached.and_then(|c| c.probe.battery.as_ref()),
                probe,
            ));
        }
        // `capabilities` is `Some` exactly when the feature-table walk succeeded;
        // only then is the probe worth caching.
        if fresh.capabilities.is_some() {
            if let Some(c) = cached {
                backfill_identity(&mut fresh, &c.probe);
            }
            // A first-sight probe whose identity reads failed is served but not
            // memoized: caching it would pin a wrong (all-zero unit or
            // serial-less) config key for `REFRESH_INTERVAL` (#482). The next
            // reconciliation re-probes instead.
            if fresh.identity_incomplete && cached.is_none() {
                return (fresh, seen(id));
            }
            // Same reasoning for a capability read that failed part-way: the
            // walk understates the device, and memoizing that hides a panel in
            // the GUI for `REFRESH_INTERVAL`. A previous complete walk
            // outranks this partial one, so defer to it and re-probe next
            // reconciliation.
            if fresh.capabilities_incomplete {
                if let Some(c) = cached {
                    keep_known_capabilities(&mut fresh, &c.probe);
                }
                return (fresh, seen(id));
            }
            return match id {
                Some(key) => {
                    let value = Cached {
                        probe: fresh.clone(),
                        battery,
                        events,
                        probed_at: now,
                    };
                    (fresh, CacheOutcome::Fresh(key, value))
                }
                None => (fresh, CacheOutcome::Unkeyed),
            };
        }
        // Re-probe failed: don't cache the failure. Fall back to the last-known
        // data so a transient glitch doesn't drop the device or its battery.
        // No battery re-read either — the device just proved unresponsive.
        return match cached {
            Some(c) => (c.probe.clone(), seen(id)),
            None => (fresh, seen(id)),
        };
    }
    match cached {
        Some(c) => {
            // Cache hit: the immutable data is reused as-is, but the battery is
            // volatile (#153) — re-read just it through the memoized feature
            // index and fold the reading back into the cache. A failed read
            // (asleep, mid-host-switch) keeps the last-known value.
            if online
                && let Some(probe) = c.battery
                && let Some(key) = id.clone()
                && let Some(battery) = read_battery(channel, index, probe).await
            {
                let battery =
                    hold_percentage_while_charging(battery, c.probe.battery.as_ref(), probe);
                let mut entry = c.clone();
                entry.probe.battery = Some(battery);
                return (entry.probe.clone(), CacheOutcome::Update(key, entry));
            }
            (c.probe.clone(), seen(id))
        }
        None => (ProbedFeatures::default(), seen(id)),
    }
}

/// Carry a previous *complete* capability walk forward over one that a lost
/// reply cut short.
///
/// A partial walk understates the device — a control-table read that failed
/// half way reads exactly like "this device has no haptic panel" — and the GUI
/// gates its panels on capabilities, so publishing the shrunken set makes a
/// feature vanish. A probe whose capability reads all succeeded is returned
/// untouched, including one that legitimately lost a capability.
pub(super) fn keep_known_capabilities(fresh: &mut ProbedFeatures, cached: &ProbedFeatures) {
    if fresh.capabilities_incomplete && cached.capabilities.is_some() {
        fresh.capabilities.clone_from(&cached.capabilities);
    }
}

/// Carry immutable identity data the fresh probe failed to read forward from
/// the cached probe, so a transient `DeviceInformation` failure can't flip the
/// device's config key (#482). A probe whose identity reads all succeeded is
/// returned untouched.
pub(super) fn backfill_identity(fresh: &mut ProbedFeatures, cached: &ProbedFeatures) {
    if fresh.kind.is_none() {
        fresh.kind = cached.kind;
    }
    if fresh.marketing_name.is_none() {
        fresh.marketing_name.clone_from(&cached.marketing_name);
    }
    if !fresh.identity_incomplete {
        return;
    }
    match (fresh.model_info.as_mut(), cached.model_info.as_ref()) {
        (None, Some(previous)) => {
            fresh.model_info = Some(previous.clone());
            fresh.identity_incomplete = false;
        }
        (Some(now), Some(previous))
            if now.serial_number.is_none() && previous.serial_number.is_some() =>
        {
            now.serial_number.clone_from(&previous.serial_number);
            fresh.identity_incomplete = false;
        }
        _ => {}
    }
}

#[cfg(test)]
mod hold_tests {
    use openlogi_core::device::{BatteryInfo, BatteryLevel, BatteryStatus};

    use super::{BatteryProbe, hold_percentage_while_charging};

    fn battery(percentage: u8, status: BatteryStatus) -> BatteryInfo {
        BatteryInfo {
            percentage,
            level: BatteryLevel::Good,
            status,
        }
    }

    #[test]
    fn charging_zero_holds_last_known_percentage() {
        let legacy = BatteryProbe::Legacy(0);
        let held = hold_percentage_while_charging(
            battery(0, BatteryStatus::Charging),
            Some(&battery(85, BatteryStatus::Discharging)),
            legacy,
        );
        assert_eq!(held.percentage, 85);
        assert_eq!(held.status, BatteryStatus::Charging);

        let discharging = hold_percentage_while_charging(
            battery(0, BatteryStatus::Discharging),
            Some(&battery(85, BatteryStatus::Discharging)),
            legacy,
        );
        assert_eq!(discharging.percentage, 0);

        let live = hold_percentage_while_charging(
            battery(40, BatteryStatus::Charging),
            Some(&battery(85, BatteryStatus::Discharging)),
            legacy,
        );
        assert_eq!(live.percentage, 40);

        let cold =
            hold_percentage_while_charging(battery(0, BatteryStatus::Charging), None, legacy);
        assert_eq!(cold.percentage, 0);
    }

    #[test]
    fn voltage_estimates_hold_the_last_reading_while_charging() {
        for probe in [BatteryProbe::Voltage(0), BatteryProbe::Adc(0)] {
            // G733: 3890 mV on battery estimates 65%; plugged in, the charge
            // current lifts the reading to 4134 mV, which would estimate 96%.
            let held = hold_percentage_while_charging(
                battery(96, BatteryStatus::Charging),
                Some(&battery(65, BatteryStatus::Discharging)),
                probe,
            );
            assert_eq!(held.percentage, 65, "{probe:?}");
            assert_eq!(held.status, BatteryStatus::Charging, "{probe:?}");

            // A held reading carries forward across charging polls.
            let still = hold_percentage_while_charging(
                battery(97, BatteryStatus::ChargingSlow),
                Some(&held),
                probe,
            );
            assert_eq!(still.percentage, 65, "{probe:?}");
            assert_eq!(still.status, BatteryStatus::ChargingSlow, "{probe:?}");

            // Charge complete and unplugged read the voltage live again.
            let full = hold_percentage_while_charging(
                battery(100, BatteryStatus::Full),
                Some(&still),
                probe,
            );
            assert_eq!(full.percentage, 100, "{probe:?}");
            let unplugged = hold_percentage_while_charging(
                battery(66, BatteryStatus::Discharging),
                Some(&still),
                probe,
            );
            assert_eq!(unplugged.percentage, 66, "{probe:?}");
        }
    }

    #[test]
    fn voltage_estimate_charging_from_cold_start_has_no_reading() {
        for probe in [BatteryProbe::Voltage(0), BatteryProbe::Adc(0)] {
            let cold =
                hold_percentage_while_charging(battery(96, BatteryStatus::Charging), None, probe);
            assert_eq!(cold.percentage, 0, "{probe:?}");
            assert_eq!(cold.level, BatteryLevel::Unknown, "{probe:?}");
            assert_eq!(cold.status, BatteryStatus::Charging, "{probe:?}");
        }
    }

    #[test]
    fn unified_charging_zero_is_not_held() {
        let live = hold_percentage_while_charging(
            battery(0, BatteryStatus::Charging),
            Some(&battery(85, BatteryStatus::Discharging)),
            BatteryProbe::Unified(0),
        );
        assert_eq!(live.percentage, 0);
    }
}
