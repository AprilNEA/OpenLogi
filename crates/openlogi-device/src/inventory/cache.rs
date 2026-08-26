use std::sync::{Arc, Weak};

use hidpp::channel::HidppChannel;
use openlogi_core::device::{BatteryInfo, BatteryStatus};

use super::features::{BatteryProbe, ProbedFeatures, probe_features, read_battery};
use crate::backend::NodeId;

/// Stable identity used to memoize a device's probe across `enumerate` ticks.
/// Keyed on the device's *own* identity (never its slot) so a re-paired or
/// moved device can't inherit another device's cached probe.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(super) enum CacheKey {
    /// Bolt: the unit id from the pairing register (cheap, read every tick).
    Bolt { unit_id: [u8; 4] },
    /// Unifying: the unit id from the extended pairing register.
    Unifying { unit_id: [u8; 4] },
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

/// A memoized immutable probe result and the channel generation that produced
/// (or last validated) it.
///
/// `None` is a persisted warm-start entry. The first live channel adopts it
/// without a feature-table walk. A replacement channel validates it once
/// before reuse, preventing runtime feature indexes from leaking across
/// reconnects without periodically interrupting live control capture.
#[derive(Clone)]
pub(super) struct Cached {
    pub(super) probe: ProbedFeatures,
    /// Which battery feature this device exposes and its runtime index, captured
    /// by the full probe. Lets cache hits re-read the volatile battery in one
    /// round-trip — no `Device::new` ping, no table walk. `None` when the device
    /// exposes neither `0x1004` nor the legacy `0x1000`.
    pub(super) battery: Option<BatteryProbe>,
    pub(super) channel: Option<Weak<HidppChannel>>,
}

impl Cached {
    fn belongs_to(&self, channel: &Arc<HidppChannel>) -> bool {
        self.channel.as_ref().is_some_and(|cached| {
            cached
                .upgrade()
                .is_some_and(|cached| Arc::ptr_eq(&cached, channel))
        })
    }

    fn bind_to(&mut self, channel: &Arc<HidppChannel>) {
        self.channel = Some(Arc::downgrade(channel));
    }

    pub(super) fn needs_validation(&self, channel: &Arc<HidppChannel>) -> bool {
        self.channel.is_some() && !self.belongs_to(channel)
    }
}

/// The legacy `0x1000` battery feature (MX2S-era mice) reports `discharge_level
/// = 0` while charging — the firmware can't gauge charge under load, so the GUI
/// would show a misleading "Charging · 0%". Carry the last-known percentage
/// forward for the charge so the reading stays trackable.
///
/// A *frozen* pre-charge value, not a live charging %, because no device exposes
/// that on `0x1000`. Only kicks in for the charging-and-zero sentinel; a genuine
/// 0% while discharging (status != Charging) is untouched. Cold edge: app
/// started while already charging has no prior, so it shows 0% until the first
/// discharge read.
fn hold_percentage_while_charging(
    fresh: BatteryInfo,
    prev: Option<&BatteryInfo>,
    probe: BatteryProbe,
) -> BatteryInfo {
    // Scoped to the legacy 0x1000 quirk: a 0x1004 device that legitimately
    // reports 0% while charging must surface that, not a stale prior reading.
    if !matches!(probe, BatteryProbe::Legacy(_)) {
        return fresh;
    }
    let charging = matches!(
        fresh.status,
        BatteryStatus::Charging | BatteryStatus::ChargingSlow
    );
    if charging
        && fresh.percentage == 0
        && let Some(p) = prev.filter(|p| p.percentage > 0)
    {
        return BatteryInfo {
            percentage: p.percentage,
            level: p.level,
            status: fresh.status,
        };
    }
    fresh
}

/// What a probed device contributes to the cache this tick. The key lets stale
/// entries be evicted; `Fresh` (a full probe), `Update` (a cache hit whose
/// volatile battery was re-read), and `Bind` (runtime channel association
/// only) also carry the value to insert. `Unkeyed` is a device we can't (or
/// won't) cache — an all-zero unit id, or a rejected non-peripheral — so its
/// key is neither inserted nor kept alive.
pub(super) enum CacheOutcome {
    Fresh(CacheKey, Cached),
    Update(CacheKey, Cached),
    /// Associate immutable data with a runtime channel without claiming that
    /// volatile device I/O succeeded.
    Bind(CacheKey, Cached),
    Seen(CacheKey),
    Unkeyed,
}

/// `Seen` when the device has a stable key, else `Unkeyed`.
pub(super) fn seen(id: Option<CacheKey>) -> CacheOutcome {
    id.map_or(CacheOutcome::Unkeyed, CacheOutcome::Seen)
}

/// Decide a device's probe: reuse a cache associated with this channel, or
/// (online + miss/replacement channel) re-probe — but keep the last-known
/// immutable data if the re-probe fails
/// rather than overwriting it with an empty default. An unprobed offline device
/// with no cache yields a default probe. Returns the probe plus its cache
/// contribution (only a *successful* probe is cached).
pub(super) async fn probe_or_reuse(
    channel: &Arc<HidppChannel>,
    index: u8,
    id: Option<CacheKey>,
    cached: Option<&Cached>,
    online: bool,
) -> (ProbedFeatures, CacheOutcome) {
    let channel_replaced = cached.is_some_and(|c| c.needs_validation(channel));
    if online && (cached.is_none() || channel_replaced) {
        let (mut fresh, battery) = probe_features(channel, index).await;
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
            // serial-less) config key for this channel's lifetime (#482). The
            // next tick re-probes instead.
            if fresh.identity_incomplete && cached.is_none() {
                return (fresh, seen(id));
            }
            // Same reasoning for a capability read that failed part-way: the
            // walk understates the device, and memoizing it would hide a panel
            // for this channel's lifetime. A previous complete walk outranks
            // this partial one and is rebound to the new channel.
            if fresh.capabilities_incomplete {
                if let Some(c) = cached {
                    keep_known_capabilities(&mut fresh, &c.probe);
                    if let Some(key) = id {
                        let mut value = c.clone();
                        value.probe = fresh.clone();
                        value.battery = battery.or(c.battery);
                        value.bind_to(channel);
                        return (fresh, CacheOutcome::Bind(key, value));
                    }
                }
                return (fresh, seen(id));
            }
            return match id {
                Some(key) => {
                    let value = Cached {
                        probe: fresh.clone(),
                        battery,
                        channel: Some(Arc::downgrade(channel)),
                    };
                    (fresh, CacheOutcome::Fresh(key, value))
                }
                None => (fresh, CacheOutcome::Unkeyed),
            };
        }
        // Re-probe failed: don't cache the failure. Fall back to the last-known
        // data so a transient glitch doesn't drop the device or its battery.
        // No battery re-read either — the device just proved unresponsive.
        return match (cached, id) {
            (Some(c), Some(key)) => {
                let mut value = c.clone();
                value.bind_to(channel);
                (c.probe.clone(), CacheOutcome::Bind(key, value))
            }
            (Some(c), None) => (c.probe.clone(), CacheOutcome::Unkeyed),
            (None, id) => (fresh, seen(id)),
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
                entry.bind_to(channel);
                return (entry.probe.clone(), CacheOutcome::Update(key, entry));
            }
            if online
                && c.channel.is_none()
                && let Some(key) = id
            {
                let mut entry = c.clone();
                entry.bind_to(channel);
                return (entry.probe.clone(), CacheOutcome::Bind(key, entry));
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
    fn unified_charging_zero_is_not_held() {
        let live = hold_percentage_while_charging(
            battery(0, BatteryStatus::Charging),
            Some(&battery(85, BatteryStatus::Discharging)),
            BatteryProbe::Unified(0),
        );
        assert_eq!(live.percentage, 0);
    }
}

#[cfg(test)]
mod channel_generation_tests {
    use std::sync::Arc;

    use openlogi_core::device::Capabilities;

    use super::{CacheKey, CacheOutcome, Cached, ProbedFeatures, probe_or_reuse};
    use crate::channel::scripted::{ScriptedRawHidChannel, feature_error, scripted_channel};

    fn reject_feature_request(request: &[u8]) -> Option<Vec<u8>> {
        let _report_id = request.first()?;
        Some(feature_error(request, 2))
    }

    async fn rejecting_channel() -> (
        Arc<hidpp::channel::HidppChannel>,
        crate::channel::scripted::ScriptedRawHidHandle,
    ) {
        let (raw, handle) = ScriptedRawHidChannel::with_responder(reject_feature_request);
        (scripted_channel(raw).await, handle)
    }

    fn cached_for(channel: &Arc<hidpp::channel::HidppChannel>) -> Cached {
        Cached {
            probe: ProbedFeatures {
                capabilities: Some(Capabilities::default()),
                ..ProbedFeatures::default()
            },
            battery: None,
            channel: Some(Arc::downgrade(channel)),
        }
    }

    #[tokio::test]
    async fn immutable_probe_is_reused_for_the_whole_channel_generation() {
        let (channel, handle) = rejecting_channel().await;
        let cached = cached_for(&channel);
        let id = CacheKey::Bolt { unit_id: [1; 4] };

        for _ in 0..32 {
            let (_, outcome) =
                probe_or_reuse(&channel, 1, Some(id.clone()), Some(&cached), true).await;
            assert!(matches!(outcome, CacheOutcome::Seen(_)));
        }

        assert!(
            handle.written_reports().is_empty(),
            "time alone must never trigger another feature-table walk"
        );
    }

    #[tokio::test]
    async fn replacement_channel_is_validated_once_even_when_probe_fails() {
        let (old_channel, _) = rejecting_channel().await;
        let cached = cached_for(&old_channel);
        let (replacement, handle) = rejecting_channel().await;
        let id = CacheKey::Bolt { unit_id: [2; 4] };

        let (_, outcome) =
            probe_or_reuse(&replacement, 1, Some(id.clone()), Some(&cached), true).await;
        let rebound = match outcome {
            CacheOutcome::Bind(key, rebound) => {
                assert_eq!(key, id);
                rebound
            }
            _ => panic!("a cached fallback must bind to the replacement channel"),
        };
        assert!(!rebound.needs_validation(&replacement));
        assert!(!handle.written_reports().is_empty());

        let writes_after_validation = handle.written_reports().len();
        let (_, outcome) = probe_or_reuse(&replacement, 1, Some(id), Some(&rebound), true).await;
        assert!(matches!(outcome, CacheOutcome::Seen(_)));
        assert_eq!(
            handle.written_reports().len(),
            writes_after_validation,
            "a failed validation must not become a two-second retry loop"
        );
    }

    #[tokio::test]
    async fn persisted_cache_adoption_does_not_claim_live_io() {
        let (channel, handle) = rejecting_channel().await;
        let cached = Cached {
            probe: ProbedFeatures::default(),
            battery: None,
            channel: None,
        };
        let id = CacheKey::Bolt { unit_id: [3; 4] };

        let (_, outcome) = probe_or_reuse(&channel, 1, Some(id.clone()), Some(&cached), true).await;
        let rebound = match outcome {
            CacheOutcome::Bind(key, rebound) => {
                assert_eq!(key, id);
                rebound
            }
            _ => panic!("adoption without live I/O must not claim a volatile update"),
        };
        assert!(!rebound.needs_validation(&channel));
        assert!(
            handle.written_reports().is_empty(),
            "warm-start adoption should not repeat the immutable probe"
        );
    }
}
