//! Every contributed profile reaches the desktop snapshot and panel-selection boundaries.

use super::super::DetailTab;
use crate::services::assets::AssetResolver;
use crate::state::{
    AppState, DeviceRecord, Sources,
    tests::device_list::{canonical_device_profile, snapshot_candidate},
};
use openlogi_core::{
    config::Config,
    hid::{DIRECT_DEVICE_INDEX, DeviceRoute},
};
use openlogi_fixture::DeviceProfile;

const CANONICAL_RECEIVER: &str = "openlogi-canonical-synthetic-001";

#[test]
fn contributed_corpus_projects_identity_battery_capabilities_and_detail_tabs() {
    let Some(corpus) = openlogi_fixture::fs::repository_corpus().expect("valid corpus") else {
        return;
    };
    for fixture in corpus {
        check_projection(&fixture.manifest().id, fixture.profile());
    }
}

#[test]
fn each_device_on_one_receiver_keeps_its_own_reviewed_tabs() {
    // A receiver capture records every paired device. Keep the canonical Bolt
    // receiver's mouse (slot 1) and keyboard (slot 3); its offline slot 2 has
    // no identity, so a capture drops it.
    let mut profile = canonical_device_profile();
    profile.inventories.truncate(1);
    profile.inventories[0]
        .paired
        .retain(|device| device.model_info.is_some());
    profile.standalone.clear();
    assert_eq!(
        profile.inventories[0]
            .paired
            .iter()
            .map(|device| device.slot)
            .collect::<Vec<_>>(),
        [1, 3]
    );
    assert_ne!(
        reviewed_detail_tabs(CANONICAL_RECEIVER, 1),
        reviewed_detail_tabs(CANONICAL_RECEIVER, 3)
    );
    check_projection(CANONICAL_RECEIVER, &profile);
}

/// Project one recorded profile and compare every paired device with its review.
fn check_projection(id: &str, profile: &DeviceProfile) {
    let resolver = AssetResolver::new();
    let (commands, _receiver) = tokio::sync::mpsc::unbounded_channel();
    let mut state = AppState::new(Sources::in_memory(Config::ephemeral(), &resolver, commands));
    let mut snapshot = snapshot_candidate(profile);
    let changes = state.apply_agent_snapshot(&snapshot, &resolver, &[]);
    assert!(changes.inventory_ready);
    assert_eq!(
        state.devices().len(),
        profile
            .inventories
            .iter()
            .map(|inventory| inventory.paired.len())
            .sum::<usize>()
            + profile.standalone.len()
    );
    for inventory in &profile.inventories {
        for device in &inventory.paired {
            let route = DeviceRoute::for_slot(inventory, device.slot).expect("recorded route");
            let record = state
                .devices()
                .iter()
                .find(|record| record.route.as_ref() == Some(&route))
                .expect("recorded device is visible");
            let model = device.model_info.as_ref().expect("recorded model identity");
            assert_eq!(record.unit_id, model.unit_id);
            assert_eq!(record.serial_number, model.serial_number);
            assert_eq!(record.model_info.as_ref(), Some(model));
            assert_eq!(record.kind, device.kind);
            assert_eq!(record.capabilities, device.capabilities);
            assert_eq!(record.battery, device.battery);
            assert_eq!(record.online, device.online);
            assert_eq!(
                DetailTab::tabs_for(record),
                reviewed_detail_tabs(id, device.slot),
                "{id} slot {}",
                device.slot
            );
        }
    }
    // A sleeping device retains measured feature availability and stable identity.
    let keys: Vec<_> = state
        .devices()
        .iter()
        .map(DeviceRecord::device_key)
        .collect();
    let tabs: Vec<_> = state.devices().iter().map(DetailTab::tabs_for).collect();
    for inventory in &mut snapshot.inventory {
        for device in &mut inventory.paired {
            device.online = false;
        }
    }
    state.apply_agent_snapshot(&snapshot, &resolver, &[]);
    assert!(state.devices().iter().all(|record| !record.online));
    assert_eq!(
        state
            .devices()
            .iter()
            .map(DeviceRecord::device_key)
            .collect::<Vec<_>>(),
        keys
    );
    assert_eq!(
        state
            .devices()
            .iter()
            .map(DetailTab::tabs_for)
            .collect::<Vec<_>>(),
        tabs
    );
    state.apply_agent_snapshot(&snapshot_candidate(profile), &resolver, &[]);
    assert!(state.devices().iter().all(|record| record.online));
    assert_eq!(
        state
            .devices()
            .iter()
            .map(DeviceRecord::device_key)
            .collect::<Vec<_>>(),
        keys
    );
}

/// The detail tabs reviewed for one recorded device, keyed by profile and slot.
///
/// A receiver capture records every paired device, and a mouse and a keyboard
/// on one receiver get different tabs. A direct device uses
/// [`DIRECT_DEVICE_INDEX`].
fn reviewed_detail_tabs(id: &str, slot: u8) -> Vec<DetailTab> {
    match (id, slot) {
        // The MX Ergo has no DPI or wheel feature. Its Pointer tab carries
        // the software scroll inversion, which Windows does not have.
        ("mx-ergo-001", 1) if openlogi_hook::SOFTWARE_SCROLL_INVERSION => vec![
            DetailTab::Buttons,
            DetailTab::ActionsRing,
            DetailTab::Pointer,
            DetailTab::Device,
        ],
        ("mx-ergo-001", 1) => vec![
            DetailTab::Buttons,
            DetailTab::ActionsRing,
            DetailTab::Device,
        ],
        ("mx-master-4-001", 2)
        | ("mx-master-3s-001" | "mx-master-3-001" | "mx-anywhere-3s-001", DIRECT_DEVICE_INDEX)
        | (CANONICAL_RECEIVER, 1) => vec![
            DetailTab::Buttons,
            DetailTab::ActionsRing,
            DetailTab::Pointer,
            DetailTab::Device,
        ],
        // A keyboard with HID++ controls and no pointer sensor gets Keys and
        // the ring editor. Its Fn lock, OS layout, backlight, Easy-Switch, and
        // disabled-key settings are cards on the Device tab, not extra tabs.
        // The keypad (kind Numpad, a measured 0x1b04 table, no pointer
        // sensor) is configured on Keys too, and a key can open the Actions
        // Ring, so the ring editor follows it.
        (
            "mx-mechanical-mini-mac-001" | "mx-keys-s-001" | "mx-creative-keypad-001",
            DIRECT_DEVICE_INDEX,
        ) => {
            vec![DetailTab::Keys, DetailTab::ActionsRing, DetailTab::Device]
        }
        // The canonical keyboard records lighting but no HID++ controls and no
        // depot asset, so it has nothing for Keys and keeps only Lighting.
        (CANONICAL_RECEIVER, 3) => vec![DetailTab::Lighting, DetailTab::Device],
        // The G915 is the same kind of keyboard, and its RGB effect engine
        // (0x8071) and per-key lighting (0x8081) add the Lighting tab.
        ("g915-001", 1) => vec![
            DetailTab::Keys,
            DetailTab::ActionsRing,
            DetailTab::Lighting,
            DetailTab::Device,
        ],
        (id, slot) => panic!("{id} slot {slot}: add reviewed desktop panel expectations"),
    }
}
