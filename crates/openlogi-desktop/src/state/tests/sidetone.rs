//! Sidetone: committing updates state, sends SetSidetone, handles failure, and rejects stale reads.

use std::sync::Arc;
use swr_core::SwrClient;
use swr_gpui::GpuiRuntime;

use super::*;
use crate::services::ipc::{Command, SetSidetone};
use openlogi_core::hid::WriteError;

fn headset_inventory(sidetone: bool) -> DeviceInventory {
    DeviceInventory {
        receiver: ReceiverInfo {
            name: "PRO X 2 Lightspeed".to_string(),
            vendor_id: 0x046d,
            product_id: 0x0af7,
            unique_id: None,
        },
        paired: vec![PairedDevice {
            slot: openlogi_core::hid::DIRECT_DEVICE_INDEX,
            codename: Some("PRO X 2 Lightspeed".to_string()),
            wpid: None,
            kind: DeviceKind::Headset,
            online: true,
            battery: None,
            model_info: Some(DeviceModelInfo {
                entity_count: 1,
                serial_number: None,
                unit_id: [0x48, 0x53, 0x00, 0x01],
                transports: DeviceTransports::default(),
                model_ids: [0x0af7, 0, 0],
                extended_model_id: 0,
            }),
            capabilities: Some(Capabilities {
                sidetone,
                ..Capabilities::presumed_from_kind(DeviceKind::Headset)
            }),
        }],
    }
}

fn headset_state(sidetone: bool) -> (AppState, tokio::sync::mpsc::UnboundedReceiver<Command>) {
    let resolver = AssetResolver::new();
    let (commands, receiver) = tokio::sync::mpsc::unbounded_channel();
    let state = AppState::new(Sources {
        inventories: &[headset_inventory(sidetone)],
        ..Sources::in_memory(Config::ephemeral(), &resolver, commands)
    });
    (state, receiver)
}

fn drain(receiver: &mut tokio::sync::mpsc::UnboundedReceiver<Command>) -> Vec<Command> {
    std::iter::from_fn(|| receiver.try_recv().ok()).collect()
}

#[test]
fn sidetone_commit_updates_shown_level_and_sends_ipc_command() {
    let (mut state, mut receiver) = headset_state(true);
    drain(&mut receiver);
    let record = state.current_record().expect("the headset is selected");
    let key = record.device_key();
    let route = record.route.clone().expect("a direct route");

    let events = state.commit_sidetone(45);

    assert_eq!(events, [StateEvent::SidetoneChanged(key.clone())]);
    assert_eq!(state.current_sidetone_shown(), Some(45));

    let commands = drain(&mut receiver);
    assert_eq!(commands.len(), 1);
    match &commands[0] {
        Command::SetSidetone(SetSidetone {
            key: sent_key,
            route: sent_route,
            level,
        }) => {
            assert_eq!(sent_key, &key);
            assert_eq!(sent_route, &route);
            assert_eq!(*level, 45);
        }
        _ => panic!("expected SetSidetone command"),
    }
}

#[test]
fn sidetone_commit_is_inert_without_capability() {
    let (mut state, mut receiver) = headset_state(false);
    drain(&mut receiver);

    let events = state.commit_sidetone(50);
    assert!(events.is_empty());
    assert_eq!(state.current_sidetone_shown(), None);
    assert!(drain(&mut receiver).is_empty());
}

#[test]
fn sidetone_write_failure_refreshes_query_and_emits_event() {
    let (mut state, _receiver) = headset_state(true);
    let record = state.current_record().expect("the headset is selected");
    let key = record.device_key();

    // Optimistic commit sets ready state
    let _ = state.commit_sidetone(60);
    assert_eq!(state.current_sidetone_shown(), Some(60));

    // A successful write land emits nothing and keeps optimistic value
    let events = state.apply_sidetone_written(&key, 60, &Ok(()));
    assert!(events.is_empty());
    assert_eq!(state.current_sidetone_shown(), Some(60));

    // A failed write emits SidetoneChanged and triggers refresh
    let fail_events = state.apply_sidetone_written(&key, 60, &Err(WriteError::AgentUnavailable));
    assert_eq!(fail_events, [StateEvent::SidetoneChanged(key.clone())]);
}

#[gpui::test]
fn stale_sidetone_flight_cannot_overwrite_newer_edit(cx: &mut gpui::TestAppContext) {
    let resolver = AssetResolver::new();
    let (commands, mut receiver) = tokio::sync::mpsc::unbounded_channel();
    let mut state = AppState::new(Sources {
        inventories: &[headset_inventory(true)],
        ..Sources::in_memory(Config::ephemeral(), &resolver, commands)
    });
    drain(&mut receiver);

    let (swr, runtime) = cx.update(|cx| {
        let swr_runtime: Arc<dyn swr_core::Runtime> = Arc::new(GpuiRuntime::new(cx));
        let swr = SwrClient::builder()
            .default_options(crate::services::assets::queries::default_options())
            .build(swr_runtime.clone());
        (swr, swr_runtime)
    });

    state.connect_device_reads(swr, runtime);

    let record = state.current_record().expect("the headset is selected");
    let key = record.device_key();

    // Start a real sidetone query
    let app_state = cx.update(|cx| {
        let app_state = cx.new(|_| state);
        AppState::set_global(app_state.clone(), cx);
        app_state.update(cx, |state, cx| {
            state.load_current_sidetone(cx);
        });
        app_state
    });

    cx.executor().run_until_parked();

    // The query started and is currently loading; ReadSidetone command was sent
    let commands = drain(&mut receiver);
    assert_eq!(commands.len(), 1);

    cx.update(|cx| {
        // While read reply is pending, user commits 80%
        app_state.update(cx, |state, _cx| {
            let _ = state.commit_sidetone(80);
            assert_eq!(state.current_sidetone_shown(), Some(80));

            // Simulating late arrival of read callback with an invalid / superseded flight
            let stale_flight = 999;
            let accepted =
                state
                    .pointer
                    .reads
                    .update_sidetone(&key, stale_flight, Load::Ready(Arc::new(20)));
            assert!(
                !accepted,
                "late read callback must be rejected by flight check"
            );

            // The user's newer edit of 80% remains intact!
            assert_eq!(state.current_sidetone_shown(), Some(80));
        });
    });
}
