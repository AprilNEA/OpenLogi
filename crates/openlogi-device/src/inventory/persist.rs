//! The immutable probe cache's persistable form, and the port that keeps it.
//!
//! A device's expensive probe result (model info, capabilities, feature
//! indexes) is immutable, so it only ever needs to be read once per device —
//! but the in-memory cache dies with the process, forcing every agent restart
//! to re-interview every device. Persisting the cache means a device that was
//! fully probed once keeps its identity across restarts, even on transports
//! where a fresh walk is slow or failing (see `BOLT_SLOT_PROBE_TIMEOUT`).
//!
//! Only Bolt identities are persisted, because only they are keyed on the
//! device's *own* identity (the pairing-register unit id), which no re-pairing
//! can silently reassign. A `CacheKey::UnifyingSlot` is `receiver + slot`: a
//! different device paired into that slot while the agent is down would
//! inherit the previous occupant's probe on warm start. A `CacheKey::Direct`
//! is an OS-runtime node id with no cross-boot stability. Loaded entries
//! restart the elapsed refresh window, so the regular self-healing pass
//! re-walks them on schedule; until (and unless) that walk succeeds, the
//! persisted data serves exactly like an in-memory cache hit.
//!
//! *Where* a snapshot is kept is the host's business, not this module's: the
//! enumerator writes through a [`ProbeCacheStore`]; `openlogi-hid` supplies
//! the file-backed one every native build uses. So is the encoding it is kept
//! in. What a stored snapshot means is this module's: a store decodes one only
//! through [`ProbeCacheSnapshot::read`].

use std::collections::HashMap;
use std::time::Instant;

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use super::cache::{CacheKey, Cached};
use super::events::EventFeatureIndices;
use super::features::{BatteryProbe, ProbedFeatures};

/// Bumped when the persisted shape changes; a mismatched snapshot is discarded
/// (the cache is a warm-start optimization, not data anyone must keep).
/// v2 dropped the `UnifyingSlot` key (slot-keyed, so not re-pair-safe).
/// v3 adds event-capable feature indexes discovered by the immutable walk.
/// v4 adds the `0x1F20` ADC-measurement event index.
/// v5 adds the legacy `0x1000` and voltage `0x1001` battery event indexes.
/// v6 records whether a `0x1004` battery reports a percentage.
/// v7 adds the Fn-inversion `0x40a2`/`0x40a3` and extended-DPI `0x2202`
/// event indexes.
/// v8 adds the `0x4531` multi-platform event index and records whether the
/// keyboard lets software set its platform (`Capabilities::os_layout`).
/// v9 adds the extended report-rate `0x8061` event index and the
/// `Capabilities::report_rate` flag.
/// v10 adds the `0x1982` backlight event index.
/// v11 adds the `Capabilities::easy_switch` flag (`0x1815` host slots).
/// v12 adds the `Capabilities::virtual_thumbwheel` flag (Back/Forward
/// raw-wheel reporting).
/// v13 adds the `0x4220` lock-key event index.
/// v14 adds the `Capabilities::pointer_scaling` flag (`0x2205` pointer
/// motion scaling on a mouse without adjustable DPI).
/// v15 adds the `0x4521` disable-keys event index.
/// v16 adds the `0x2121` hi-res wheel event index.
/// v17 records whether a legacy `0x1000` battery reports mileage.
/// v18 adds the `Capabilities::force_sensing` flag (`0x19c0` press force
/// software can change).
/// v19 folds `capabilities` and `capabilities_incomplete` into `protocol`,
/// which also records HID++ 1.0, and adds the HID++ 1.0 battery registers to
/// `BatteryProbe`.
/// v20 adds the `Capabilities::change_host` flag (`0x1814` host switching).
/// v21 folds `model_info` and `identity_incomplete` into `identity`, one state
/// per DeviceInformation outcome.
const SCHEMA_VERSION: u32 = 21;

impl ProbeCacheError {
    /// Report why a store could not keep a snapshot.
    #[must_use]
    pub fn new(reason: impl std::fmt::Display) -> Self {
        Self(reason.to_string())
    }
}

/// A probe-cache store could not keep a snapshot.
///
/// Carries only a message: nothing branches on why a best-effort write failed,
/// and the reasons differ per store (a filesystem error, a browser storage
/// quota).
#[derive(Debug, Error)]
#[error("{0}")]
pub struct ProbeCacheError(String);

/// Where an [`Enumerator`](super::Enumerator)'s probe cache lives between runs.
///
/// A port, like [`HidBackend`](crate::backend::HidBackend): the enumerator
/// knows what is worth keeping and when it changed, and nothing about where it
/// goes. `openlogi-hid` supplies the file-backed one native builds use.
pub trait ProbeCacheStore: Send + Sync {
    /// The last snapshot saved here, or an empty one.
    ///
    /// Never fails: an absent, torn or foreign-schema store is a cold start,
    /// which the enumerator handles by re-probing — not an error worth
    /// propagating into device discovery.
    fn load(&self) -> ProbeCacheSnapshot;

    /// Persist `snapshot`.
    ///
    /// An `Err` is logged by the caller and the snapshot retried on the next
    /// pass that dirties the cache, so a store may fail freely.
    fn save(&self, snapshot: &ProbeCacheSnapshot) -> Result<(), ProbeCacheError>;
}

/// A snapshot as a store keeps it, still in the store's encoding.
///
/// The store picks the encoding; [`ProbeCacheSnapshot::read`] picks what to
/// decode from it, and in which order.
pub trait EncodedSnapshot {
    /// Why the stored content does not decode as the type asked for.
    type Error: std::fmt::Display;

    /// The stored content, decoded as `T`.
    ///
    /// The encoding must be self-describing, so that a `T` naming only some
    /// of the stored fields skips the others: [`ProbeCacheSnapshot::read`]
    /// decodes the schema version alone before anything else.
    fn decode<T: DeserializeOwned>(&self) -> Result<T, Self::Error>;
}

/// Why a stored snapshot gives this build nothing to warm-start from. Either
/// way the enumerator starts cold and re-probes every device.
#[derive(Debug, Error)]
pub enum SnapshotRejected {
    /// A build with another schema wrote it: the cold start expected once
    /// after an upgrade or a downgrade across a schema change.
    #[error("the probe cache was written with schema version {version}")]
    ForeignSchema {
        /// The schema version the snapshot was written with.
        version: u32,
    },
    /// It does not decode as a snapshot of this build's schema, or of any:
    /// a torn write, a damaged file, or a hand edit.
    #[error("the probe cache does not decode: {reason}")]
    Unreadable {
        /// What the encoding reported.
        reason: String,
    },
}

impl SnapshotRejected {
    fn unreadable(reason: impl std::fmt::Display) -> Self {
        Self::Unreadable {
            reason: reason.to_string(),
        }
    }
}

/// The persistable subset of the probe cache, in the shape a store keeps.
///
/// It does not implement `Deserialize`: decoding it whole would report a
/// snapshot from another schema as damaged. [`Self::read`] is the way back.
#[derive(Serialize)]
pub struct ProbeCacheSnapshot {
    version: u32,
    entries: Vec<PersistedEntry>,
}

/// The one field every schema has kept, decoded before the entries whose
/// shape depends on it.
#[derive(Deserialize)]
struct SchemaHeader {
    version: u32,
}

/// The rest of a snapshot whose [`SchemaHeader`] names this build's schema.
#[derive(Deserialize)]
struct SchemaBody {
    entries: Vec<PersistedEntry>,
}

#[derive(Serialize, Deserialize)]
struct PersistedEntry {
    key: PersistedKey,
    probe: ProbedFeatures,
    battery: Option<BatteryProbe>,
    events: EventFeatureIndices,
}

/// The persistable subset of [`CacheKey`] — Bolt only (see the module docs).
#[derive(Clone, Copy, Serialize, Deserialize)]
enum PersistedKey {
    Bolt { unit_id: [u8; 4] },
}

fn persistable(key: &CacheKey) -> Option<PersistedKey> {
    match key {
        CacheKey::Bolt { unit_id } => Some(PersistedKey::Bolt { unit_id: *unit_id }),
        CacheKey::UnifyingSlot { .. } | CacheKey::Direct(_) => None,
    }
}

/// Whether a cache change under `key` affects the persisted snapshot at all —
/// gates `cache_dirty` so churn on never-persisted keys (e.g. a direct-only
/// system's full refresh) doesn't rewrite an unchanged snapshot every pass.
pub(super) fn is_persistable(key: &CacheKey) -> bool {
    persistable(key).is_some()
}

fn runtime_key(key: PersistedKey) -> CacheKey {
    match key {
        PersistedKey::Bolt { unit_id } => CacheKey::Bolt { unit_id },
    }
}

impl ProbeCacheSnapshot {
    /// A snapshot carrying nothing — what a store with no readable content
    /// returns, and a cold start for the enumerator.
    #[must_use]
    pub fn empty() -> Self {
        Self {
            version: SCHEMA_VERSION,
            entries: Vec::new(),
        }
    }

    /// Whether this snapshot carries nothing — a store may skip writing one.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Everything in `cache` worth keeping across restarts.
    pub(super) fn of(cache: &HashMap<CacheKey, Cached>) -> Self {
        let entries = cache
            .iter()
            .filter_map(|(key, cached)| {
                persistable(key).map(|key| {
                    // The battery *reading* is volatile and re-read live on
                    // every cache hit — persisting it would resurrect a stale
                    // value after a restart. The battery *feature index*
                    // (`PersistedEntry::battery`) is immutable and kept.
                    let mut probe = cached.probe.clone();
                    probe.battery = None;
                    PersistedEntry {
                        key,
                        probe,
                        battery: cached.battery,
                        events: cached.events,
                    }
                })
            })
            .collect();
        Self {
            version: SCHEMA_VERSION,
            entries,
        }
    }

    /// Decode a snapshot a store kept, if this build's schema wrote it.
    ///
    /// # Errors
    ///
    /// [`SnapshotRejected::ForeignSchema`] when another schema wrote it, and
    /// [`SnapshotRejected::Unreadable`] when it does not decode.
    pub fn read(stored: &impl EncodedSnapshot) -> Result<Self, SnapshotRejected> {
        let SchemaHeader { version } = stored.decode().map_err(SnapshotRejected::unreadable)?;
        if version != SCHEMA_VERSION {
            return Err(SnapshotRejected::ForeignSchema { version });
        }
        let SchemaBody { entries } = stored.decode().map_err(SnapshotRejected::unreadable)?;
        Ok(Self { version, entries })
    }

    /// Fold this snapshot back into runtime cache entries.
    pub(super) fn into_entries(self) -> HashMap<CacheKey, Cached> {
        self.entries
            .into_iter()
            .map(|entry| {
                (
                    runtime_key(entry.key),
                    Cached {
                        probe: entry.probe,
                        battery: entry.battery,
                        events: entry.events,
                        // Restart the refresh clock: the entry serves
                        // immediately as a cache hit, and the periodic
                        // self-healing re-walk decides when it is due for a
                        // fresh read.
                        probed_at: Instant::now(),
                    },
                )
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::time::Instant;

    use openlogi_core::device::{
        BatteryInfo, BatteryLevel, BatteryStatus, DeviceModelInfo, DeviceTransports,
    };
    use serde::de::DeserializeOwned;

    use super::super::cache::{CacheKey, Cached};
    use super::super::events::EventFeatureIndices;
    use super::super::features::{BatteryProbe, ProbedFeatures};
    use super::super::identity::ProbedIdentity;
    use super::{
        EncodedSnapshot, ProbeCacheSnapshot, SCHEMA_VERSION, SchemaBody, SnapshotRejected,
    };

    /// A device fully probed once keeps its identity across restarts — that is
    /// the whole point of the snapshot — but only the parts that are actually
    /// immutable, and only for keys a re-pair cannot silently reassign.
    #[test]
    fn a_snapshot_keeps_bolt_identity_and_drops_the_volatile_reading() {
        let model = DeviceModelInfo {
            entity_count: 1,
            serial_number: Some("TESTSERIAL01".into()),
            unit_id: [0xaa, 0xbb, 0xcc, 0xdd],
            transports: DeviceTransports::default(),
            model_ids: [0xb042, 0, 0],
            extended_model_id: 0,
        };
        let events = EventFeatureIndices {
            wireless_status: Some(7),
            unified_battery: Some(9),
            battery_status: Some(12),
            battery_voltage: Some(13),
            adc_measurement: Some(11),
            fn_inversion: None,
            fn_inversion_multi_host: None,
            extended_dpi: None,
            multi_platform: Some(14),
            disable_keys: Some(18),
            extended_report_rate: Some(15),
            backlight: Some(16),
            lock_key_state: Some(17),
            hires_wheel: Some(19),
        };
        let mut cache = HashMap::new();
        cache.insert(
            CacheKey::Bolt {
                unit_id: [0xaa, 0xbb, 0xcc, 0xdd],
            },
            Cached {
                probe: ProbedFeatures {
                    identity: ProbedIdentity::Known(model.clone()),
                    // A live reading at snapshot time.
                    battery: Some(BatteryInfo {
                        percentage: Some(55),
                        level: BatteryLevel::Good,
                        status: BatteryStatus::Discharging,
                    }),
                    ..Default::default()
                },
                battery: Some(BatteryProbe::Unified {
                    index: 9,
                    state_of_charge: true,
                }),
                events,
                probed_at: Instant::now(),
            },
        );
        cache.insert(
            CacheKey::UnifyingSlot {
                receiver_uid: "DA2699E1".into(),
                slot: 2,
            },
            Cached {
                probe: ProbedFeatures::default(),
                battery: None,
                events: EventFeatureIndices::default(),
                probed_at: Instant::now(),
            },
        );

        let snapshot = ProbeCacheSnapshot::of(&cache);
        let restored_after = Instant::now();
        let restored = snapshot.into_entries();

        let bolt = restored
            .get(&CacheKey::Bolt {
                unit_id: [0xaa, 0xbb, 0xcc, 0xdd],
            })
            .expect("a Bolt entry survives");
        assert_eq!(bolt.probe.identity, ProbedIdentity::Known(model));
        assert_eq!(
            bolt.battery,
            Some(BatteryProbe::Unified {
                index: 9,
                state_of_charge: true,
            }),
            "the battery *feature index* is immutable and kept"
        );
        assert_eq!(
            bolt.events, events,
            "event feature indexes are immutable and kept"
        );
        assert!(
            bolt.probe.battery.is_none(),
            "the battery *reading* is volatile — restoring it would resurrect a stale value"
        );
        assert!(
            bolt.probed_at >= restored_after,
            "a restored entry restarts the refresh clock"
        );
        assert!(
            !restored.contains_key(&CacheKey::UnifyingSlot {
                receiver_uid: "DA2699E1".into(),
                slot: 2,
            }),
            "unifying entries are slot-keyed, so a re-pair while the agent is \
             down could hand them to a different device — never persisted"
        );
    }

    /// A snapshot kept as JSON, the encoding the file store uses.
    struct Json<'a>(&'a str);

    impl EncodedSnapshot for Json<'_> {
        type Error = serde_json::Error;

        fn decode<T: DeserializeOwned>(&self) -> Result<T, serde_json::Error> {
            serde_json::from_str(self.0)
        }
    }

    /// How many entries a store's JSON gives this build, or why it gives none.
    fn read_json(json: &str) -> Result<usize, SnapshotRejected> {
        ProbeCacheSnapshot::read(&Json(json)).map(|snapshot| snapshot.into_entries().len())
    }

    /// This build's JSON for one Bolt device.
    fn one_bolt_device() -> String {
        let cache = HashMap::from([(
            CacheKey::Bolt {
                unit_id: [0xaa, 0xbb, 0xcc, 0xdd],
            },
            Cached {
                probe: ProbedFeatures::default(),
                battery: None,
                events: EventFeatureIndices::default(),
                probed_at: Instant::now(),
            },
        )]);
        serde_json::to_string(&ProbeCacheSnapshot::of(&cache)).expect("a snapshot encodes")
    }

    #[test]
    fn a_snapshot_this_build_wrote_reads_back() {
        let read = read_json(&one_bolt_device());

        assert!(matches!(read, Ok(1)), "{read:?}");
    }

    /// What v0.9.0 wrote for a Bolt mouse. Its users meet this snapshot on
    /// their first launch after an upgrade, which must start cold as the
    /// expected outcome it is, not as a damaged file.
    #[test]
    fn a_snapshot_the_v0_9_0_release_wrote_is_another_schema() {
        const V19: &str = r#"{"version":19,"entries":[{
            "key":{"Bolt":{"unit_id":[170,187,204,221]}},
            "probe":{
                "battery":null,
                "model_info":{"entity_count":1,"serial_number":"TESTSERIAL01",
                    "unit_id":[170,187,204,221],
                    "transports":{"usb":false,"equad":false,"btle":false,"bluetooth":false},
                    "model_ids":[45122,0,0],"extended_model_id":0},
                "kind":null,"marketing_name":null,
                "protocol":{"Hidpp20":{"capabilities":{"buttons":false,"pointer":false,
                    "lighting":false,"scroll_inversion":false,"hires_wheel":false,
                    "thumbwheel":false,"haptic_feedback":false,"haptic_panel":false,
                    "dpi_gestures":false,"fn_lock":false,"os_layout":false,
                    "report_rate":false,"backlight":false,"easy_switch":false,
                    "virtual_thumbwheel":false,"pointer_scaling":false,
                    "force_sensing":false,"disable_keys":false,"onboard_profiles":false,
                    "button_spy":false,"sidetone":false},"incomplete":false}},
                "identity_incomplete":false},
            "battery":null,
            "events":{"wireless_status":null,"unified_battery":null,"battery_status":null,
                "battery_voltage":null,"adc_measurement":null,"fn_inversion":null,
                "fn_inversion_multi_host":null,"extended_dpi":null,"multi_platform":null,
                "disable_keys":null,"extended_report_rate":null,"backlight":null,
                "lock_key_state":null,"hires_wheel":null}}]}"#;

        let read = read_json(V19);

        assert!(
            matches!(read, Err(SnapshotRejected::ForeignSchema { version: 19 })),
            "{read:?}"
        );
    }

    /// What a newer build wrote, met after a downgrade. Its entries do not
    /// decode here, and that must not make the snapshot read as damaged: the
    /// schema decides first.
    #[test]
    fn a_snapshot_a_newer_build_wrote_is_another_schema() {
        let newer = SCHEMA_VERSION + 1;
        let json = format!(r#"{{"version":{newer},"entries":[{{"shape":"unknown here"}}]}}"#);

        let read = read_json(&json);

        assert!(
            matches!(read, Err(SnapshotRejected::ForeignSchema { version }) if version == newer),
            "{read:?}"
        );
    }

    /// Entries this build's schema does not decode are damage, not another
    /// schema: the version claims they are this build's.
    #[test]
    fn this_schema_with_entries_that_do_not_decode_is_unreadable() {
        let json =
            format!(r#"{{"version":{SCHEMA_VERSION},"entries":[{{"shape":"unknown here"}}]}}"#);

        let read = read_json(&json);

        assert!(
            matches!(read, Err(SnapshotRejected::Unreadable { .. })),
            "{read:?}"
        );
    }

    #[test]
    fn a_torn_or_foreign_file_is_unreadable() {
        let written = one_bolt_device();
        let torn = &written[..written.len() / 2];

        for stored in [torn, "not json at all", "{}"] {
            let read = read_json(stored);

            assert!(
                matches!(read, Err(SnapshotRejected::Unreadable { .. })),
                "{stored}: {read:?}"
            );
        }
    }

    /// What a v20 build wrote for a Bolt mouse: its identity as `model_info`
    /// beside an `identity_incomplete` flag. It is valid v20 in every other
    /// field, so its entries fail to decode here for that change alone.
    const V20: &str = r#"{"version":20,"entries":[{
        "key":{"Bolt":{"unit_id":[170,187,204,221]}},
        "probe":{
            "battery":null,
            "model_info":{"entity_count":1,"serial_number":"TESTSERIAL01",
                "unit_id":[170,187,204,221],
                "transports":{"usb":false,"equad":false,"btle":false,"bluetooth":false},
                "model_ids":[45122,0,0],"extended_model_id":0},
            "kind":null,"marketing_name":null,
            "protocol":{"Hidpp20":{"capabilities":{"buttons":false,"pointer":false,
                "lighting":false,"scroll_inversion":false,"hires_wheel":false,
                "thumbwheel":false,"haptic_feedback":false,"haptic_panel":false,
                "dpi_gestures":false,"fn_lock":false,"os_layout":false,
                "report_rate":false,"backlight":false,"easy_switch":false,
                "virtual_thumbwheel":false,"pointer_scaling":false,
                "force_sensing":false,"disable_keys":false,"onboard_profiles":false,
                "button_spy":false,"sidetone":false,"g_keys":false,"m_keys":false,
                "macro_record":false,"change_host":false},"incomplete":false}},
            "identity_incomplete":false},
        "battery":null,
        "events":{"wireless_status":null,"unified_battery":null,"battery_status":null,
            "battery_voltage":null,"adc_measurement":null,"fn_inversion":null,
            "fn_inversion_multi_host":null,"extended_dpi":null,"multi_platform":null,
            "disable_keys":null,"extended_report_rate":null,"backlight":null,
            "lock_key_state":null,"hires_wheel":null}}]}"#;

    /// The first launch after an upgrade from a v20 build meets this
    /// snapshot. Its entries no longer decode, and it must still start cold
    /// as another schema rather than as a damaged file.
    #[test]
    fn a_snapshot_a_v20_build_wrote_is_another_schema() {
        let read = read_json(V20);

        assert!(
            matches!(read, Err(SnapshotRejected::ForeignSchema { version: 20 })),
            "{read:?}"
        );
    }

    /// The two fields are not guessed into one of the states they meant,
    /// whatever version the snapshot claims.
    #[test]
    fn a_two_field_identity_does_not_decode() {
        let Err(error) = serde_json::from_str::<SchemaBody>(V20) else {
            panic!("v20 entries must not decode as this schema's");
        };
        assert!(
            error.to_string().contains("missing field `identity`"),
            "v20 entries must fail on their missing `identity`, not on: {error}"
        );
    }
}
