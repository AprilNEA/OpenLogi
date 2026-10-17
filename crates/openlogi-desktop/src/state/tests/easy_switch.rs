//! Easy-Switch hosts: a rename writes the device through the agent — only one
//! its last reading takes — and nothing is persisted on this computer.

use super::fn_lock::drain;
use super::*;
use crate::services::ipc::{Command, ReadEasySwitchHosts, RenameEasySwitchHost};
use crate::state::easy_switch::rename_target;
use crate::state::events::StateEvents;
use crate::state::{EasySwitchLoad, HostRenameRefusal};
use openlogi_core::hid::{
    DeviceRoute, EasySwitchHosts, EasySwitchSlot, HostConnection, HostOsInfo, HostOsKind,
};

/// A keyboard on channel 1 of three: this Mac over Bluetooth, a laptop on a
/// Bolt receiver, and an empty channel. Names fit 16 bytes.
fn hosts() -> EasySwitchHosts {
    let slot = |index, connection, name: Option<&str>| EasySwitchSlot {
        index,
        paired: name.is_some(),
        connection,
        name: name.map(str::to_owned),
        name_max_len: 16,
        os: name.map(|_| HostOsInfo {
            kind: HostOsKind::MacOs,
            major: 15,
            minor: 1,
            patch: 0,
        }),
    };
    EasySwitchHosts {
        current: Some(0),
        slots: vec![
            slot(0, HostConnection::Bluetooth, Some("Studio")),
            slot(1, HostConnection::Bolt, Some("Laptop")),
            slot(2, HostConnection::Unknown, None),
        ],
        can_rename: true,
    }
}

/// `hosts` with channel `slot` showing `name`.
fn named(mut hosts: EasySwitchHosts, slot: u8, name: &str) -> EasySwitchHosts {
    hosts
        .slots
        .iter_mut()
        .find(|entry| entry.index == slot)
        .expect("the slot exists")
        .name = Some(name.to_owned());
    hosts
}

/// The fixture device on its cable, with or without Easy-Switch hosts.
fn keyboard_inventory(easy_switch: bool) -> DeviceInventory {
    let mut inventory = direct_inventory([0xa3, 0x93, 0xca, 0xe0]);
    inventory.paired[0]
        .capabilities
        .as_mut()
        .expect("the fixture device has capabilities")
        .easy_switch = easy_switch;
    inventory
}

/// The same device moved onto a Bolt receiver: its key stays, its route does
/// not.
fn on_receiver(mut inventory: DeviceInventory) -> DeviceInventory {
    inventory.receiver = ReceiverInfo {
        name: "Bolt Receiver".to_string(),
        vendor_id: 0x046d,
        product_id: 0xc548,
        unique_id: Some("82839805".to_string()),
    };
    inventory.paired[0].slot = 1;
    inventory
}

fn keyboard_state(easy_switch: bool) -> (AppState, tokio::sync::mpsc::UnboundedReceiver<Command>) {
    let resolver = AssetResolver::new();
    let (commands, receiver) = tokio::sync::mpsc::unbounded_channel();
    let state = AppState::new(Sources {
        inventories: &[keyboard_inventory(easy_switch)],
        ..Sources::in_memory(Config::ephemeral(), &resolver, commands)
    });
    (state, receiver)
}

/// Answer the Easy-Switch read the state has queued, returning the route it
/// was asked on. Every other read is left unanswered.
fn answer_hosts_read(
    receiver: &mut tokio::sync::mpsc::UnboundedReceiver<Command>,
    reading: &EasySwitchHosts,
) -> DeviceRoute {
    let mut answered = None;
    for command in drain(receiver) {
        if let Command::ReadEasySwitchHosts(ReadEasySwitchHosts { route, reply }) = command {
            assert!(answered.is_none(), "one Easy-Switch read at a time");
            let _ = reply.send(Ok(reading.clone()));
            answered = Some(route);
        }
    }
    answered.expect("the state asked the device for its Easy-Switch hosts")
}

/// The renames and config reloads among `commands` — what a rename may send.
/// The fixture device's other reads are retried on their own.
fn writes(commands: Vec<Command>) -> Vec<Command> {
    commands
        .into_iter()
        .filter(|command| {
            matches!(
                command,
                Command::RenameEasySwitchHost(_) | Command::ReloadConfig(_)
            )
        })
        .collect()
}

/// The renames among the queued commands — slot, name and the number they
/// were sent under — oldest first. Panics on a config reload: a rename never
/// touches `config.toml`.
fn sent(receiver: &mut tokio::sync::mpsc::UnboundedReceiver<Command>) -> Vec<(u8, String, u64)> {
    writes(drain(receiver))
        .into_iter()
        .map(|command| match command {
            Command::RenameEasySwitchHost(RenameEasySwitchHost {
                slot, name, write, ..
            }) => (slot, name, write),
            _ => panic!("a rename does not reload the config"),
        })
        .collect()
}

fn hosts_reads(commands: &[Command]) -> usize {
    commands
        .iter()
        .filter(|command| matches!(command, Command::ReadEasySwitchHosts(_)))
        .count()
}

/// Install `state` with live swr-backed reads, as the app does, and have it
/// read the selected device.
fn install(state: AppState, cx: &mut gpui::TestAppContext) {
    cx.update(|cx| {
        let runtime: Arc<dyn swr_core::Runtime> = Arc::new(swr_gpui::GpuiRuntime::new(cx));
        let client = swr_core::SwrClient::builder().build(runtime.clone());
        let state = cx.new(|_| {
            let mut state = state;
            state.connect_device_reads(client, runtime);
            state
        });
        AppState::set_global(state, cx);
        AppState::load_current_device_reads(cx);
    });
    cx.run_until_parked();
}

/// Install a keyboard whose Easy-Switch read answered `reading`; returns its
/// key and the route it was read on.
fn installed_and_read(
    cx: &mut gpui::TestAppContext,
    receiver: &mut tokio::sync::mpsc::UnboundedReceiver<Command>,
    state: AppState,
    reading: EasySwitchHosts,
) -> (DeviceKey, DeviceRoute) {
    let key = state.current_record().expect("keyboard").device_key();
    install(state, cx);
    let route = answer_hosts_read(receiver, &reading);
    cx.run_until_parked();
    assert_eq!(current_load(cx), Load::Ready(Arc::new(reading)));
    (key, route)
}

fn rename(cx: &mut gpui::TestAppContext, key: &DeviceKey, slot: u8, name: &str) -> StateEvents {
    cx.update(|cx| {
        AppState::update(cx, |state, _| {
            state.rename_easy_switch_host(key, slot, name)
        })
    })
}

fn renamed(
    cx: &mut gpui::TestAppContext,
    key: &DeviceKey,
    route: &DeviceRoute,
    write: u64,
    result: Result<EasySwitchHosts, WriteError>,
) -> StateEvents {
    cx.update(|cx| {
        AppState::update(cx, |state, _| {
            state.apply_easy_switch_host_renamed(key, route, write, result)
        })
    })
}

fn current_load(cx: &gpui::TestAppContext) -> EasySwitchLoad {
    cx.read(|cx| {
        AppState::try_read(cx)
            .expect("state installed")
            .current_easy_switch_load()
    })
}

fn current_refusal(cx: &gpui::TestAppContext) -> Option<HostRenameRefusal> {
    cx.read(|cx| {
        AppState::try_read(cx)
            .expect("state installed")
            .current_host_rename_refusal()
    })
}

#[test]
fn a_rename_target_is_a_name_the_slot_takes_and_does_not_already_show() {
    let hosts = hosts();
    assert_eq!(rename_target(&hosts, 0, "  Desk \n"), Some("Desk"));
    assert_eq!(rename_target(&hosts, 0, "Studio"), None, "already shown");
    assert_eq!(
        rename_target(&hosts, 0, " Studio "),
        None,
        "shown once trimmed"
    );
    assert_eq!(rename_target(&hosts, 0, "   "), None);
    assert_eq!(rename_target(&hosts, 0, &"x".repeat(17)), None);
    assert_eq!(
        rename_target(&hosts, 0, &"x".repeat(16)),
        Some("x".repeat(16).as_str())
    );
    assert_eq!(rename_target(&hosts, 2, "Desk"), None, "an empty channel");
    assert_eq!(rename_target(&hosts, 3, "Desk"), None, "no such channel");
    let fixed = EasySwitchHosts {
        can_rename: false,
        ..hosts
    };
    assert_eq!(rename_target(&fixed, 0, "Desk"), None);
}

#[gpui::test]
fn a_device_without_easy_switch_is_never_asked_for_its_hosts(cx: &mut gpui::TestAppContext) {
    let (state, mut receiver) = keyboard_state(false);
    let key = state.current_record().expect("keyboard").device_key();
    install(state, cx);
    assert_eq!(hosts_reads(&drain(&mut receiver)), 0);

    assert_eq!(
        rename(cx, &key, 0, "Desk"),
        [StateEvent::EasySwitchChanged(key)]
    );
    assert!(sent(&mut receiver).is_empty());
}

#[test]
fn a_rename_before_the_device_was_read_writes_nothing() {
    let (mut state, mut receiver) = keyboard_state(true);
    let key = state.current_record().expect("keyboard").device_key();
    let _ = state.rename_easy_switch_host(&key, 0, "Desk");
    assert!(writes(drain(&mut receiver)).is_empty());
}

#[gpui::test]
fn a_rename_shows_at_once_and_writes_the_device_without_touching_the_config(
    cx: &mut gpui::TestAppContext,
) {
    let (state, mut receiver) = keyboard_state(true);
    let (key, route) = installed_and_read(cx, &mut receiver, state, hosts());

    assert_eq!(
        rename(cx, &key, 0, "  Desk "),
        [StateEvent::EasySwitchChanged(key.clone())]
    );
    let commands = writes(drain(&mut receiver));
    let [Command::RenameEasySwitchHost(write)] = &commands[..] else {
        panic!(
            "one rename and no config reload, got {} commands",
            commands.len()
        );
    };
    assert_eq!(
        (&write.route, write.slot, write.name.as_str(), &write.key),
        (&route, 0, "Desk", &key),
        "the trimmed name goes to the channel over the device's connection"
    );
    assert_eq!(
        current_load(cx),
        Load::Ready(Arc::new(named(hosts(), 0, "Desk")))
    );
    assert_eq!(current_refusal(cx), None);
}

#[gpui::test]
fn a_rename_the_reading_does_not_take_writes_nothing(cx: &mut gpui::TestAppContext) {
    let (state, mut receiver) = keyboard_state(true);
    let (key, _route) = installed_and_read(cx, &mut receiver, state, hosts());
    for (slot, name) in [
        (0, "Studio".to_owned()),
        (0, " ".to_owned()),
        (0, "x".repeat(17)),
        (2, "Desk".to_owned()),
    ] {
        let _ = rename(cx, &key, slot, &name);
        assert!(sent(&mut receiver).is_empty(), "{slot} {name:?}");
    }
    assert_eq!(current_load(cx), Load::Ready(Arc::new(hosts())));

    let (state, mut receiver) = keyboard_state(true);
    let fixed = EasySwitchHosts {
        can_rename: false,
        ..hosts()
    };
    let (key, _route) = installed_and_read(cx, &mut receiver, state, fixed);
    let _ = rename(cx, &key, 0, "Desk");
    assert!(
        sent(&mut receiver).is_empty(),
        "a device that cannot rename"
    );
}

/// A receiver keyboard that falls asleep, leaves range or is switched to
/// another computer keeps its route but answers nothing: its last reading
/// stays on the card, and a rename is not sent until it is back.
#[gpui::test]
fn an_offline_receiver_device_keeps_its_channels_and_takes_no_rename(
    cx: &mut gpui::TestAppContext,
) {
    let resolver = AssetResolver::new();
    let (commands, mut receiver) = tokio::sync::mpsc::unbounded_channel();
    let state = AppState::new(Sources {
        inventories: &[on_receiver(keyboard_inventory(true))],
        ..Sources::in_memory(Config::ephemeral(), &resolver, commands)
    });
    let (key, route) = installed_and_read(cx, &mut receiver, state, hosts());
    let reachable = |cx: &gpui::TestAppContext| {
        cx.read(|cx| {
            AppState::try_read(cx)
                .expect("state installed")
                .current_easy_switch_reachable()
        })
    };
    assert!(reachable(cx));

    let mut asleep = on_receiver(keyboard_inventory(true));
    asleep.paired[0].online = false;
    assert_eq!(refresh(cx, &mut receiver, asleep), Some(route));
    assert!(!reachable(cx));
    assert_eq!(current_load(cx), Load::Ready(Arc::new(hosts())));

    let _ = rename(cx, &key, 0, "Desk");
    assert!(sent(&mut receiver).is_empty());
    assert_eq!(current_load(cx), Load::Ready(Arc::new(hosts())));
}

/// A dialog opened for one keyboard and saved after another was selected
/// must not write either: the rename names a device that is not on screen,
/// and its reading is on its own connection, not the selected one's.
#[gpui::test]
fn a_rename_for_a_device_no_longer_selected_writes_nothing(cx: &mut gpui::TestAppContext) {
    let resolver = AssetResolver::new();
    let (commands, mut receiver) = tokio::sync::mpsc::unbounded_channel();
    let mut other_keyboard = keyboard_inventory(true);
    other_keyboard.receiver.product_id = 0xb35c;
    other_keyboard.paired[0]
        .model_info
        .as_mut()
        .expect("the fixture device has model info")
        .unit_id = [0x01, 0x02, 0x03, 0x04];
    let state = AppState::new(Sources {
        inventories: &[keyboard_inventory(true), other_keyboard],
        ..Sources::in_memory(Config::ephemeral(), &resolver, commands)
    });
    let (first, first_route) = installed_and_read(cx, &mut receiver, state, hosts());

    let second = cx.update(|cx| {
        let second = AppState::update(cx, |state, _| {
            for index in 0..2 {
                let _ = state.select_device(index);
                if !state.is_current_device(&first) {
                    break;
                }
            }
            state
                .current_record()
                .expect("a keyboard is selected")
                .device_key()
        });
        AppState::load_current_device_reads(cx);
        second
    });
    assert_ne!(second, first);
    cx.run_until_parked();
    assert_ne!(answer_hosts_read(&mut receiver, &hosts()), first_route);
    cx.run_until_parked();

    let _ = rename(cx, &first, 0, "Desk");
    assert!(sent(&mut receiver).is_empty());
    assert_eq!(current_load(cx), Load::Ready(Arc::new(hosts())));
}

#[gpui::test]
fn the_read_back_replaces_the_shown_name_and_a_refusal_is_kept_and_re_reads(
    cx: &mut gpui::TestAppContext,
) {
    let (state, mut receiver) = keyboard_state(true);
    let (key, route) = installed_and_read(cx, &mut receiver, state, hosts());
    let _ = rename(cx, &key, 0, "Desk");
    let [(0, _, first)] = sent(&mut receiver)[..] else {
        panic!("one rename sent");
    };

    // The device answers with what it really holds.
    let read_back = named(hosts(), 0, "Desk-2");
    assert_eq!(
        renamed(cx, &key, &route, first, Ok(read_back.clone())),
        [StateEvent::EasySwitchChanged(key.clone())]
    );
    assert_eq!(current_load(cx), Load::Ready(Arc::new(read_back.clone())));

    // A refusal is kept for the card and asks the device again rather than
    // leave the new name on screen.
    let _ = rename(cx, &key, 1, "Work");
    let [(1, _, second)] = sent(&mut receiver)[..] else {
        panic!("one rename sent");
    };
    assert_eq!(
        renamed(cx, &key, &route, second, Err(WriteError::AgentUnavailable)),
        [StateEvent::EasySwitchChanged(key.clone())]
    );
    assert_eq!(current_load(cx), Load::Ready(Arc::new(read_back)));
    assert_eq!(
        current_refusal(cx),
        Some(HostRenameRefusal {
            slot: 1,
            error: WriteError::AgentUnavailable,
        })
    );
    cx.run_until_parked();
    assert_eq!(hosts_reads(&drain(&mut receiver)), 1);

    // The next rename clears it.
    let _ = rename(cx, &key, 1, "Work");
    assert_eq!(current_refusal(cx), None);
}

#[test]
fn an_answer_no_read_is_on_is_dropped() {
    let (mut state, _receiver) = keyboard_state(true);
    let record = state.current_record().expect("keyboard");
    let key = record.device_key();
    let route = record.route.clone().expect("a direct route");

    assert!(
        state
            .apply_easy_switch_host_renamed(&key, &route, 1, Ok(hosts()))
            .is_empty()
    );
    assert!(
        state
            .apply_easy_switch_host_renamed(&key, &route, 1, Err(WriteError::AgentUnavailable))
            .is_empty()
    );
    assert_eq!(state.current_host_rename_refusal(), None);
}

/// The cache is keyed by the device, not its connection: a rename answered
/// over the cable after the device moved onto its receiver must leave the
/// receiver read on screen.
#[gpui::test]
fn a_rename_answered_after_a_route_change_is_dropped(cx: &mut gpui::TestAppContext) {
    let (state, mut receiver) = keyboard_state(true);
    let (key, cable) = installed_and_read(cx, &mut receiver, state, hosts());
    let _ = rename(cx, &key, 0, "Desk");
    let [(0, _, write)] = sent(&mut receiver)[..] else {
        panic!("the rename is written over the cable");
    };

    let moved = cx.update(|cx| {
        let moved = AppState::update(cx, |state, _| {
            let resolver = AssetResolver::new();
            let _ = state.refresh_inventories(
                &[on_receiver(keyboard_inventory(true))],
                &[],
                &resolver,
                &[],
            );
            let record = state.current_record().expect("the device stays selected");
            assert_eq!(record.device_key(), key, "the move keeps the device's key");
            record.route.clone().expect("a receiver route")
        });
        AppState::load_current_device_reads(cx);
        moved
    });
    assert_ne!(moved, cable);
    cx.run_until_parked();
    let receiver_reading = EasySwitchHosts {
        current: Some(1),
        ..hosts()
    };
    assert_eq!(answer_hosts_read(&mut receiver, &receiver_reading), moved);
    cx.run_until_parked();

    assert!(renamed(cx, &key, &cable, write, Ok(named(hosts(), 0, "Desk"))).is_empty());
    assert!(renamed(cx, &key, &cable, write, Err(WriteError::AgentUnavailable)).is_empty());
    cx.run_until_parked();
    assert_eq!(current_load(cx), Load::Ready(Arc::new(receiver_reading)));
    assert_eq!(current_refusal(cx), None);
    assert_eq!(
        hosts_reads(&drain(&mut receiver)),
        0,
        "a late refusal does not re-read the device"
    );
}

/// Refresh the inventory to `inventory` without starting the selected
/// device's reads again, as between an inventory change and the reads it
/// leads to. Whatever the refresh itself queued is dropped.
fn refresh(
    cx: &mut gpui::TestAppContext,
    receiver: &mut tokio::sync::mpsc::UnboundedReceiver<Command>,
    inventory: DeviceInventory,
) -> Option<DeviceRoute> {
    let route = cx.update(|cx| {
        AppState::update(cx, |state, _| {
            let resolver = AssetResolver::new();
            let _ = state.refresh_inventories(&[inventory], &[], &resolver, &[]);
            state
                .current_record()
                .and_then(|record| record.route.clone())
        })
    });
    let _ = drain(receiver);
    route
}

/// Moving the device drops the reading it had over the cable; until its
/// receiver connection is read, there is nothing to show a rename on or match
/// its answer to.
#[gpui::test]
fn a_rename_before_the_read_follows_the_device_to_its_new_connection_writes_nothing(
    cx: &mut gpui::TestAppContext,
) {
    let (state, mut receiver) = keyboard_state(true);
    let (key, cable) = installed_and_read(cx, &mut receiver, state, hosts());
    let moved = refresh(cx, &mut receiver, on_receiver(keyboard_inventory(true)));
    assert_ne!(moved.as_ref(), Some(&cable));

    let _ = rename(cx, &key, 0, "Desk");
    assert!(sent(&mut receiver).is_empty());
    assert_eq!(current_load(cx), Load::Unknown);
}

/// A device re-probed without `0x1815` keeps its old reading cached, but
/// takes no rename on it.
#[gpui::test]
fn a_device_that_stops_reporting_easy_switch_takes_no_rename(cx: &mut gpui::TestAppContext) {
    let (state, mut receiver) = keyboard_state(true);
    let (key, cable) = installed_and_read(cx, &mut receiver, state, hosts());
    assert_eq!(
        refresh(cx, &mut receiver, keyboard_inventory(false)),
        Some(cable)
    );

    let _ = rename(cx, &key, 0, "Desk");
    assert!(sent(&mut receiver).is_empty());
}

/// Two quick renames: the first one's answer arrives while the second is
/// still being written. The card keeps showing the second.
#[gpui::test]
fn an_earlier_answer_keeps_a_later_rename_shown(cx: &mut gpui::TestAppContext) {
    let (state, mut receiver) = keyboard_state(true);
    let (key, route) = installed_and_read(cx, &mut receiver, state, hosts());
    let _ = rename(cx, &key, 0, "Desk");
    let _ = rename(cx, &key, 1, "Work");
    let [(0, _, desk), (1, _, work)] = sent(&mut receiver)[..] else {
        panic!("both renames are written");
    };
    let both = named(named(hosts(), 0, "Desk"), 1, "Work");
    assert_eq!(current_load(cx), Load::Ready(Arc::new(both.clone())));

    let _ = renamed(cx, &key, &route, desk, Ok(named(hosts(), 0, "Desk")));
    assert_eq!(current_load(cx), Load::Ready(Arc::new(both.clone())));
    let _ = renamed(cx, &key, &route, work, Ok(both.clone()));
    assert_eq!(current_load(cx), Load::Ready(Arc::new(both)));
}

/// The agent answers renames in the order they were sent; one that arrives
/// after a newer rename's answer anyway says nothing new and is dropped.
#[gpui::test]
fn an_answer_older_than_one_already_taken_is_dropped(cx: &mut gpui::TestAppContext) {
    let (state, mut receiver) = keyboard_state(true);
    let (key, route) = installed_and_read(cx, &mut receiver, state, hosts());
    let _ = rename(cx, &key, 0, "Desk");
    let _ = rename(cx, &key, 0, "Work");
    let [(0, _, desk), (0, _, work)] = sent(&mut receiver)[..] else {
        panic!("both renames are written");
    };

    let _ = renamed(cx, &key, &route, work, Ok(named(hosts(), 0, "Work")));
    assert!(renamed(cx, &key, &route, desk, Ok(named(hosts(), 0, "Desk"))).is_empty());
    assert!(renamed(cx, &key, &route, desk, Err(WriteError::AgentUnavailable)).is_empty());
    cx.run_until_parked();
    assert_eq!(
        current_load(cx),
        Load::Ready(Arc::new(named(hosts(), 0, "Work")))
    );
    assert_eq!(current_refusal(cx), None);
    assert_eq!(hosts_reads(&drain(&mut receiver)), 0);
}

/// The re-read a refused rename starts can answer before a later rename has
/// landed. The later name stays shown until its own answer arrives.
#[gpui::test]
fn a_re_read_while_a_rename_is_unanswered_keeps_it_shown(cx: &mut gpui::TestAppContext) {
    let (state, mut receiver) = keyboard_state(true);
    let (key, route) = installed_and_read(cx, &mut receiver, state, hosts());
    let _ = rename(cx, &key, 0, "Desk");
    let _ = rename(cx, &key, 1, "Work");
    let [(0, _, desk), (1, _, work)] = sent(&mut receiver)[..] else {
        panic!("both renames are written");
    };

    let _ = renamed(cx, &key, &route, desk, Err(WriteError::AgentUnavailable));
    let working = named(hosts(), 1, "Work");
    assert_eq!(current_load(cx), Load::Ready(Arc::new(working.clone())));
    cx.run_until_parked();
    assert_eq!(answer_hosts_read(&mut receiver, &hosts()), route);
    cx.run_until_parked();
    assert_eq!(current_load(cx), Load::Ready(Arc::new(working.clone())));
    assert_eq!(
        current_refusal(cx).map(|refusal| refusal.slot),
        Some(0),
        "the refusal stays until another rename is sent or lands"
    );

    let _ = renamed(cx, &key, &route, work, Ok(working.clone()));
    assert_eq!(current_load(cx), Load::Ready(Arc::new(working)));
    assert_eq!(current_refusal(cx), None);
}
