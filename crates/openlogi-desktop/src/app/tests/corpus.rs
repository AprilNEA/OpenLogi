//! Every contributed profile reaches the desktop snapshot and panel-selection boundaries.

use super::super::DetailTab;
use crate::services::assets::AssetResolver;
use crate::state::{AppState, DeviceRecord, Sources, tests::device_list::snapshot_candidate};
use openlogi_core::{config::Config, hid::DeviceRoute};

#[test]
fn contributed_corpus_projects_identity_battery_capabilities_and_detail_tabs() {
    let Some(corpus) = openlogi_fixture::fs::repository_corpus().expect("valid corpus") else {
        return;
    };
    for fixture in corpus {
        let resolver = AssetResolver::new();
        let (commands, _receiver) = tokio::sync::mpsc::unbounded_channel();
        let mut state = AppState::new(Sources::in_memory(Config::ephemeral(), &resolver, commands));
        let mut snapshot = snapshot_candidate(fixture.profile());
        let changes = state.apply_agent_snapshot(&snapshot, &resolver, &[]);
        assert!(changes.inventory_ready);
        assert_eq!(
            state.devices().len(),
            fixture
                .profile()
                .inventories
                .iter()
                .map(|inventory| inventory.paired.len())
                .sum::<usize>()
                + fixture.profile().standalone.len()
        );
        for inventory in &fixture.profile().inventories {
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
                let expected = match fixture.manifest().id.as_str() {
                    "mx-ergo-001" => vec![
                        DetailTab::Buttons,
                        DetailTab::ActionsRing,
                        DetailTab::Device,
                    ],
                    "mx-master-4-001" | "mx-master-3s-001" | "mx-anywhere-3s-001" => vec![
                        DetailTab::Buttons,
                        DetailTab::ActionsRing,
                        DetailTab::Pointer,
                        DetailTab::Device,
                    ],
                    id => panic!("{id}: add reviewed desktop panel expectations"),
                };
                assert_eq!(
                    DetailTab::tabs_for(record),
                    expected,
                    "{}",
                    fixture.manifest().id
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
        state.apply_agent_snapshot(&snapshot_candidate(fixture.profile()), &resolver, &[]);
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
}
