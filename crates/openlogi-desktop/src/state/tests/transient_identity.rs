//! Transient probe identities: folded into a known card, adopted or dropped, never persisted.

use super::*;

#[test]
fn transient_identity_is_not_persisted_or_retained_after_resolution() {
    let resolver = AssetResolver::new();
    let transient_inventory = half_read_direct_inventory();
    let (commands, _receiver) = tokio::sync::mpsc::unbounded_channel();
    let mut state = AppState::new(Sources {
        inventories: &[transient_inventory],
        ..Sources::in_memory(Config::ephemeral(), &resolver, commands)
    });
    let transient_key = "direct:046d:b023:unit:00000000";

    assert_eq!(state.devices().len(), 1);
    assert!(state.config.device_identity(transient_key).is_none());
    let _ = state.commit_dpi(Dpi::new(2400));
    assert!(state.config.dpi(transient_key).is_none());

    let stable_list = build_device_list(
        &[direct_inventory([0xa3, 0x93, 0xca, 0xe0])],
        &[],
        &resolver,
        &state.config,
        &[],
    );
    let merged = state.merge_inventory_snapshot(stable_list);

    assert_eq!(merged.len(), 1);
    // The device's own unit id is known and online: the transport-free
    // identity key wins over the direct-route runtime key.
    assert_eq!(merged[0].config_key, "unit:a393cae0");
    assert!(merged[0].is_persistent());
}

#[test]
fn transient_probe_folds_into_its_known_card() {
    // #482: a half-read probe (all-zero unit id) of the only known device
    // with that vid/pid must not evict the known card or appear beside it —
    // the card keeps its identity and takes the live volatile state.
    let resolver = AssetResolver::new();
    let (commands, _receiver) = tokio::sync::mpsc::unbounded_channel();
    let mut state = AppState::new(Sources {
        inventories: &[direct_inventory([0xa3, 0x93, 0xca, 0xe0])],
        ..Sources::in_memory(Config::ephemeral(), &resolver, commands)
    });
    // The device's own unit id is known and online: the transport-free
    // identity key wins over the direct-route runtime key.
    let stable_key = "unit:a393cae0";
    assert_eq!(state.devices()[0].config_key, stable_key);

    let transient_list = build_device_list(
        &[half_read_direct_inventory()],
        &[],
        &resolver,
        &state.config,
        &[],
    );
    let merged = state.merge_inventory_snapshot(transient_list);

    assert_eq!(merged.len(), 1, "no second card for the half-read probe");
    assert_eq!(merged[0].config_key, stable_key);
    assert!(merged[0].is_persistent());
    assert!(
        merged[0].is_online(),
        "the live probe supplies volatile state"
    );
    assert!(merged[0].route.is_some(), "the live route is kept usable");
}

#[test]
fn transient_record_beside_its_live_device_is_dropped() {
    // Both a full and a half-read probe of the same wire product in one
    // snapshot: the transient record is probe noise, not a second device.
    let resolver = AssetResolver::new();
    let (commands, _receiver) = tokio::sync::mpsc::unbounded_channel();
    let mut state = AppState::new(Sources {
        inventories: &[direct_inventory([0xa3, 0x93, 0xca, 0xe0])],
        ..Sources::in_memory(Config::ephemeral(), &resolver, commands)
    });

    let both = build_device_list(
        &[
            direct_inventory([0xa3, 0x93, 0xca, 0xe0]),
            half_read_direct_inventory(),
        ],
        &[],
        &resolver,
        &state.config,
        &[],
    );
    assert_eq!(both.len(), 2);
    let merged = state.merge_inventory_snapshot(both);

    assert_eq!(merged.len(), 1);
    assert_eq!(merged[0].config_key, "unit:a393cae0");
    assert!(merged[0].is_online());
}

#[test]
fn transient_probe_beside_a_live_twin_is_not_adopted() {
    // Two same-model devices; one probes complete, and a half-read node
    // answers on their route. A direct route is only `vid:pid`, so that node
    // may be the live twin's second one while the other mouse is unplugged:
    // the absent card stays offline, and the node is dropped as probe noise.
    let resolver = AssetResolver::new();
    let (commands, _receiver) = tokio::sync::mpsc::unbounded_channel();
    let mut state = AppState::new(Sources {
        inventories: &[
            direct_inventory([1, 1, 1, 1]),
            direct_inventory([2, 2, 2, 2]),
        ],
        ..Sources::in_memory(Config::ephemeral(), &resolver, commands)
    });

    let snapshot = build_device_list(
        &[direct_inventory([1, 1, 1, 1]), half_read_direct_inventory()],
        &[],
        &resolver,
        &state.config,
        &[],
    );
    let merged = state.merge_inventory_snapshot(snapshot);

    assert_eq!(merged.len(), 2, "no third card for the half-read probe");
    assert_online_and_absent(&merged, &["unit:01010101"], "unit:02020202");
}

/// The half-read node is a second node of an online mouse, and another is
/// unplugged. The unplugged mouse's last live record names the shared route,
/// which proves its model and not that it answered.
#[test]
fn partial_node_with_offline_twin() {
    let resolver = AssetResolver::new();
    let (commands, _receiver) = tokio::sync::mpsc::unbounded_channel();
    let mut state = AppState::new(Sources {
        inventories: &[
            direct_inventory([1, 1, 1, 1]),
            direct_inventory([2, 2, 2, 2]),
            direct_inventory([3, 3, 3, 3]),
        ],
        ..Sources::in_memory(Config::ephemeral(), &resolver, commands)
    });

    let snapshot = build_device_list(
        &[
            direct_inventory([2, 2, 2, 2]),
            half_read_direct_inventory(),
            direct_inventory([3, 3, 3, 3]),
        ],
        &[],
        &resolver,
        &state.config,
        &[],
    );
    let merged = state.merge_inventory_snapshot(snapshot);

    assert_eq!(merged.len(), 3, "no fourth card for the half-read node");
    assert_online_and_absent(
        &merged,
        &["unit:02020202", "unit:03030303"],
        "unit:01010101",
    );
}

/// With no twin online, the half-read probe takes the one card last seen on
/// its route, even when the card's offline placeholder carries nothing else
/// that names the route.
#[test]
fn transient_probe_adopts_its_last_live_card_with_no_twin_online() {
    let resolver = AssetResolver::new();
    let (commands, _receiver) = tokio::sync::mpsc::unbounded_channel();
    let mut state = AppState::new(Sources {
        inventories: &[direct_inventory([1, 1, 1, 1])],
        ..Sources::in_memory(Config::ephemeral(), &resolver, commands)
    });
    state.config.edit(|config| {
        config
            .devices
            .get_mut("unit:01010101")
            .expect("the device has an entry")
            .links
            .clear();
    });

    let snapshot = build_device_list(
        &[half_read_direct_inventory()],
        &[],
        &resolver,
        &state.config,
        &[],
    );
    let merged = state.merge_inventory_snapshot(snapshot);

    assert_eq!(merged.len(), 1, "no second card for the half-read probe");
    assert_eq!(merged[0].config_key, "unit:01010101");
    assert!(merged[0].is_online(), "the half-read probe keeps it online");
    assert!(
        merged[0].addressable_route().is_some(),
        "the live route stays usable"
    );
}

/// `online` keep their cards and their route, and `absent` shows no device to
/// write to.
fn assert_online_and_absent(merged: &[DeviceRecord], online: &[&str], absent: &str) {
    for key in online {
        let record = merged
            .iter()
            .find(|record| record.config_key == *key)
            .unwrap_or_else(|| panic!("{key} must keep its card"));
        assert!(record.is_online(), "{key} stays online");
        assert!(record.addressable_route().is_some(), "{key} stays writable");
    }
    let absent = merged
        .iter()
        .find(|record| record.config_key == absent)
        .expect("the unplugged device keeps its card");
    assert!(!absent.is_online(), "the unplugged device stays offline");
    assert!(
        absent.addressable_route().is_none(),
        "nothing may be written to the unplugged device"
    );
}

#[test]
fn ambiguous_transient_probe_is_not_adopted() {
    // Two same-model devices are known; a half-read probe could be either,
    // so neither card may steal it.
    let resolver = AssetResolver::new();
    let (commands, _receiver) = tokio::sync::mpsc::unbounded_channel();
    let mut state = AppState::new(Sources {
        inventories: &[
            direct_inventory([1, 1, 1, 1]),
            direct_inventory([2, 2, 2, 2]),
        ],
        ..Sources::in_memory(Config::ephemeral(), &resolver, commands)
    });
    assert_eq!(state.devices().len(), 2);

    let transient_list = build_device_list(
        &[half_read_direct_inventory()],
        &[],
        &resolver,
        &state.config,
        &[],
    );
    let merged = state.merge_inventory_snapshot(transient_list);

    assert_eq!(merged.len(), 3, "both known cards survive on grace");
    assert_eq!(
        merged.iter().filter(|r| !r.is_persistent()).count(),
        1,
        "the transient card stays its own record"
    );
}

#[test]
fn a_route_shared_by_two_online_twins_is_never_adopted() {
    // #482 corollary: `route_key` for a Direct route strips the device's own
    // identity, so two same-model direct devices online in the same
    // snapshot report the *same* route key. `Config::adopt_route` is
    // exclusive per route, so adopting it for either twin would just get it
    // stolen back by the other on the very next tick — a persist-and-reload
    // storm. The route cannot be attributed to either by route alone, so
    // neither claims it, and nothing is persisted or reloaded.
    let resolver = AssetResolver::new();
    let (commands, mut receiver) = tokio::sync::mpsc::unbounded_channel();
    let mut state = AppState::new(Sources::in_memory(Config::ephemeral(), &resolver, commands));

    let _ = state.refresh_inventories(
        &[
            direct_inventory([1, 1, 1, 1]),
            direct_inventory([2, 2, 2, 2]),
        ],
        &[],
        &resolver,
        &[],
    );

    for key in ["unit:01010101", "unit:02020202"] {
        assert!(
            !state
                .config
                .devices
                .get(key)
                .is_some_and(|device| device.links.contains_key("direct:046d:b023")),
            "{key} must not claim a route its twin equally owns"
        );
    }
    assert!(
        receiver.try_recv().is_err(),
        "a route that was never adopted must not trigger a persist/reload"
    );
}

#[test]
fn two_devices_with_their_own_units_on_one_route_stay_two_cards() {
    let resolver = AssetResolver::new();
    let (commands, _receiver) = tokio::sync::mpsc::unbounded_channel();
    let mut state = AppState::new(Sources::in_memory(Config::ephemeral(), &resolver, commands));

    let _ = state.refresh_inventories(
        &[
            direct_inventory([1, 1, 1, 1]),
            direct_inventory([2, 2, 2, 2]),
        ],
        &[],
        &resolver,
        &[],
    );

    let keys: Vec<&str> = state
        .devices()
        .iter()
        .map(|record| record.config_key.as_str())
        .collect();
    assert_eq!(keys, ["unit:01010101", "unit:02020202"]);
}

#[test]
fn historical_transient_lighting_is_not_exposed_without_a_live_record() {
    let transient_key = "direct:046d:b023:unit:00000000";
    let mut config = Config::ephemeral();
    config.set_lighting(transient_key, Lighting::default());
    assert!(config.lighting(transient_key).is_some());
    let (commands, _receiver) = tokio::sync::mpsc::unbounded_channel();
    let state = AppState::new(Sources::in_memory(config, &AssetResolver::new(), commands));

    assert!(state.devices().is_empty());
    assert!(state.lighting_for(transient_key, transient_key).is_none());
}
