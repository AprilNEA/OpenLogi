//! Backlight: a change writes the keyboard through the agent — only one its
//! last reading accepts — and nothing is persisted on this computer.

use super::fn_lock::drain;
use super::*;
use crate::services::ipc::{Command, ReadBacklight, SetBacklight};
use crate::state::BacklightLoad;
use crate::state::backlight::change_target;
use crate::state::events::StateEvents;
use openlogi_core::hid::{
    BacklightChange, BacklightControls, BacklightMode, BacklightState, BacklightStatus, DeviceRoute,
};

/// A keyboard lit at level 2 of 0–4 by its light sensor, offering every
/// control.
fn lit() -> BacklightState {
    BacklightState {
        enabled: true,
        mode: BacklightMode::Automatic,
        status: BacklightStatus::AlsAutomatic,
        current_level: 2,
        nb_levels: 5,
        power_save: true,
        controls: BacklightControls {
            automatic: true,
            level: true,
            power_save: true,
        },
    }
}

/// The fixture device on its cable, with or without a backlight control.
fn keyboard_inventory(backlight: bool) -> DeviceInventory {
    let mut inventory = direct_inventory([0xa3, 0x93, 0xca, 0xe0]);
    inventory.paired[0]
        .capabilities
        .as_mut()
        .expect("the fixture device has capabilities")
        .backlight = backlight;
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

fn keyboard_state(backlight: bool) -> (AppState, tokio::sync::mpsc::UnboundedReceiver<Command>) {
    let resolver = AssetResolver::new();
    let (commands, receiver) = tokio::sync::mpsc::unbounded_channel();
    let state = AppState::new(Sources {
        inventories: &[keyboard_inventory(backlight)],
        ..Sources::in_memory(Config::ephemeral(), &resolver, commands)
    });
    (state, receiver)
}

/// Answer the backlight read the state has queued, returning the route it
/// was asked on. Every other read is left unanswered.
fn answer_backlight_read(
    receiver: &mut tokio::sync::mpsc::UnboundedReceiver<Command>,
    reading: BacklightState,
) -> DeviceRoute {
    let mut answered = None;
    for command in drain(receiver) {
        if let Command::ReadBacklight(ReadBacklight { route, reply }) = command {
            assert!(answered.is_none(), "one backlight read at a time");
            let _ = reply.send(Ok(reading));
            answered = Some(route);
        }
    }
    answered.expect("the state asked the keyboard for its backlight")
}

/// The backlight writes and config reloads among `commands` — what a change
/// may send. The fixture device's other reads are retried on their own.
fn writes(commands: Vec<Command>) -> Vec<Command> {
    commands
        .into_iter()
        .filter(|command| matches!(command, Command::SetBacklight(_) | Command::ReloadConfig(_)))
        .collect()
}

/// The backlight changes among the queued commands, with the numbers they
/// were sent under, oldest first.
fn sent(
    receiver: &mut tokio::sync::mpsc::UnboundedReceiver<Command>,
) -> Vec<(BacklightChange, u64)> {
    writes(drain(receiver))
        .into_iter()
        .filter_map(|command| match command {
            Command::SetBacklight(SetBacklight { change, write, .. }) => Some((change, write)),
            _ => None,
        })
        .collect()
}

fn backlight_reads(commands: &[Command]) -> usize {
    commands
        .iter()
        .filter(|command| matches!(command, Command::ReadBacklight(_)))
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

/// Install a keyboard whose backlight read answered `reading`; returns its
/// key and the route it was read on.
fn installed_and_read(
    cx: &mut gpui::TestAppContext,
    receiver: &mut tokio::sync::mpsc::UnboundedReceiver<Command>,
    state: AppState,
    reading: BacklightState,
) -> (DeviceKey, DeviceRoute) {
    let key = state.current_record().expect("keyboard").device_key();
    install(state, cx);
    let route = answer_backlight_read(receiver, reading);
    cx.run_until_parked();
    assert_eq!(current_load(cx), Load::Ready(Arc::new(reading)));
    (key, route)
}

fn commit(cx: &mut gpui::TestAppContext, change: BacklightChange) -> StateEvents {
    cx.update(|cx| AppState::update(cx, |state, _| state.commit_backlight(change)))
}

fn written(
    cx: &mut gpui::TestAppContext,
    key: &DeviceKey,
    route: &DeviceRoute,
    write: u64,
    result: Result<BacklightState, WriteError>,
) -> StateEvents {
    cx.update(|cx| {
        AppState::update(cx, |state, _| {
            state.apply_backlight_written(key, route, write, result)
        })
    })
}

fn current_load(cx: &gpui::TestAppContext) -> BacklightLoad {
    cx.read(|cx| {
        AppState::try_read(cx)
            .expect("state installed")
            .current_backlight_load()
    })
}

#[test]
fn a_change_targets_a_reading_that_accepts_it_and_is_altered_by_it() {
    let reading = Load::Ready(Arc::new(lit()));
    assert_eq!(
        change_target(&reading, BacklightChange::Level(4)),
        Some(lit().changed(BacklightChange::Level(4)))
    );
    // Already lit: nothing to write.
    assert_eq!(
        change_target(&reading, BacklightChange::Enabled(true)),
        None
    );
    // A level the keyboard does not have.
    assert_eq!(change_target(&reading, BacklightChange::Level(5)), None);
    // A control this keyboard does not offer.
    let no_level = Load::Ready(Arc::new(BacklightState {
        controls: BacklightControls {
            level: false,
            ..lit().controls
        },
        ..lit()
    }));
    assert_eq!(change_target(&no_level, BacklightChange::Level(1)), None);
    for unread in [
        Load::Unknown,
        Load::Loading,
        Load::Failed("unreachable".to_string()),
        Load::Unsupported("no backlight feature".to_string()),
    ] {
        assert_eq!(
            change_target(&unread, BacklightChange::Enabled(false)),
            None
        );
    }
}

#[gpui::test]
fn a_change_shows_at_once_and_writes_the_keyboard_without_touching_the_config(
    cx: &mut gpui::TestAppContext,
) {
    let (state, mut receiver) = keyboard_state(true);
    let (key, route) = installed_and_read(cx, &mut receiver, state, lit());

    let events = commit(cx, BacklightChange::Level(4));

    assert_eq!(events, [StateEvent::BacklightChanged(key.clone())]);
    assert_eq!(
        current_load(cx),
        Load::Ready(Arc::new(lit().changed(BacklightChange::Level(4))))
    );
    let commands = writes(drain(&mut receiver));
    let [
        Command::SetBacklight(SetBacklight {
            route: written_route,
            change,
            key: written_key,
            write: _,
        }),
    ] = commands.as_slice()
    else {
        // No `ReloadConfig`: the keyboard keeps its backlight itself.
        panic!("expected one backlight write and no config reload");
    };
    assert_eq!(*written_route, route);
    assert_eq!(*change, BacklightChange::Level(4));
    assert_eq!(*written_key, key);
}

#[gpui::test]
fn a_change_the_reading_does_not_accept_writes_nothing(cx: &mut gpui::TestAppContext) {
    let (state, mut receiver) = keyboard_state(true);
    let reading = BacklightState {
        controls: BacklightControls {
            automatic: false,
            ..lit().controls
        },
        ..lit()
    };
    let (_key, _route) = installed_and_read(cx, &mut receiver, state, reading);

    let _ = commit(cx, BacklightChange::Automatic(true));

    assert!(writes(drain(&mut receiver)).is_empty());
    assert_eq!(current_load(cx), Load::Ready(Arc::new(reading)));
}

#[test]
fn a_change_is_inert_on_a_device_without_the_control() {
    let (mut state, mut receiver) = keyboard_state(false);
    drain(&mut receiver);
    assert!(!state.current_backlight_supported());

    let _ = state.commit_backlight(BacklightChange::Enabled(false));

    assert!(drain(&mut receiver).is_empty());
}

#[gpui::test]
fn a_device_without_the_control_is_never_asked_for_its_backlight(cx: &mut gpui::TestAppContext) {
    let (state, mut receiver) = keyboard_state(false);
    install(state, cx);

    assert_eq!(backlight_reads(&drain(&mut receiver)), 0);
    assert_eq!(current_load(cx), Load::Unknown);
}

#[test]
fn a_change_before_the_keyboard_was_read_writes_nothing() {
    let (mut state, mut receiver) = keyboard_state(true);
    drain(&mut receiver);
    assert!(state.current_backlight_supported());
    assert_eq!(state.current_backlight_load(), Load::Unknown);

    let _ = state.commit_backlight(BacklightChange::Enabled(false));

    assert!(drain(&mut receiver).is_empty());
}

#[gpui::test]
fn the_read_back_replaces_the_prediction_and_a_refusal_re_reads(cx: &mut gpui::TestAppContext) {
    let (state, mut receiver) = keyboard_state(true);
    let (key, route) = installed_and_read(cx, &mut receiver, state, lit());
    let _ = commit(cx, BacklightChange::Automatic(false));
    let [(_, first)] = sent(&mut receiver)[..] else {
        panic!("one change sent");
    };

    // The keyboard answers with what it really holds.
    let read_back = BacklightState {
        mode: BacklightMode::PermanentManual,
        status: BacklightStatus::PermanentManual,
        current_level: 3,
        ..lit()
    };
    assert_eq!(
        written(cx, &key, &route, first, Ok(read_back)),
        [StateEvent::BacklightChanged(key.clone())]
    );
    assert_eq!(current_load(cx), Load::Ready(Arc::new(read_back)));

    // A refusal asks the keyboard again rather than leave a guess on screen.
    let _ = commit(cx, BacklightChange::Level(1));
    let [(_, second)] = sent(&mut receiver)[..] else {
        panic!("one change sent");
    };
    assert_eq!(
        written(cx, &key, &route, second, Err(WriteError::AgentUnavailable)),
        [StateEvent::BacklightChanged(key)]
    );
    cx.run_until_parked();
    assert_eq!(backlight_reads(&drain(&mut receiver)), 1);
}

#[test]
fn an_answer_no_read_is_on_is_dropped() {
    let (mut state, _receiver) = keyboard_state(true);
    let record = state.current_record().expect("keyboard");
    let key = record.device_key();
    let route = record.route.clone().expect("a direct route");

    assert!(
        state
            .apply_backlight_written(&key, &route, 1, Ok(lit()))
            .is_empty()
    );
    assert!(
        state
            .apply_backlight_written(&key, &route, 1, Err(WriteError::AgentUnavailable))
            .is_empty()
    );
}

/// The cache is keyed by the keyboard, not its connection: a write answered
/// over the cable after the keyboard moved onto its receiver must leave the
/// receiver read on screen.
#[gpui::test]
fn a_write_answered_after_a_route_change_is_dropped(cx: &mut gpui::TestAppContext) {
    let (state, mut receiver) = keyboard_state(true);
    let (key, cable) = installed_and_read(cx, &mut receiver, state, lit());
    let _ = commit(cx, BacklightChange::Level(4));
    let commands = drain(&mut receiver);
    let Some(write) = commands.iter().find_map(|command| match command {
        Command::SetBacklight(write) if write.route == cable => Some(write.write),
        _ => None,
    }) else {
        panic!("the change is written over the cable");
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
            let record = state.current_record().expect("the keyboard stays selected");
            assert_eq!(
                record.device_key(),
                key,
                "the move keeps the keyboard's key"
            );
            record.route.clone().expect("a receiver route")
        });
        AppState::load_current_device_reads(cx);
        moved
    });
    assert_ne!(moved, cable);
    cx.run_until_parked();
    let receiver_reading = BacklightState {
        current_level: 1,
        ..lit()
    };
    assert_eq!(
        answer_backlight_read(&mut receiver, receiver_reading),
        moved
    );
    cx.run_until_parked();

    assert!(
        written(
            cx,
            &key,
            &cable,
            write,
            Ok(lit().changed(BacklightChange::Level(4)))
        )
        .is_empty()
    );
    assert!(written(cx, &key, &cable, write, Err(WriteError::AgentUnavailable)).is_empty());
    cx.run_until_parked();
    assert_eq!(current_load(cx), Load::Ready(Arc::new(receiver_reading)));
    assert_eq!(
        backlight_reads(&drain(&mut receiver)),
        0,
        "a late refusal does not re-read the keyboard"
    );
}

/// The keyboard changes its backlight itself — its keys, the light sensor —
/// and says so with a `0x1982` info broadcast, which the agent counts in
/// `settings_revision`. The panel re-reads instead of showing the old level.
#[gpui::test]
fn a_change_the_keyboard_reports_re_reads_the_backlight(cx: &mut gpui::TestAppContext) {
    let (state, mut receiver) = keyboard_state(true);
    let (_key, route) = installed_and_read(cx, &mut receiver, state, lit());

    cx.update(|cx| {
        AppState::update(cx, |state, _| {
            let resolver = AssetResolver::new();
            let mut changed = keyboard_inventory(true);
            changed.paired[0].settings_revision = 1;
            let _ = state.refresh_inventories(&[changed], &[], &resolver, &[]);
        });
    });
    cx.run_until_parked();

    let dimmed = BacklightState {
        mode: BacklightMode::TemporaryManual,
        status: BacklightStatus::TemporaryManual,
        current_level: 0,
        ..lit()
    };
    assert_eq!(answer_backlight_read(&mut receiver, dimmed), route);
    cx.run_until_parked();
    assert_eq!(current_load(cx), Load::Ready(Arc::new(dimmed)));
}

/// Two quick level clicks: the first one's answer arrives while the second is
/// still being written. The panel keeps showing the second, so a third click
/// back to the first level is a change to write, not a click on what is
/// already shown.
#[gpui::test]
fn an_earlier_answer_keeps_a_later_click_shown_and_the_next_one_written(
    cx: &mut gpui::TestAppContext,
) {
    let (state, mut receiver) = keyboard_state(true);
    let (key, route) = installed_and_read(cx, &mut receiver, state, lit());
    let at = |level| lit().changed(BacklightChange::Level(level));
    let _ = commit(cx, BacklightChange::Level(3));
    let _ = commit(cx, BacklightChange::Level(4));
    let [(_, three), (_, four)] = sent(&mut receiver)[..] else {
        panic!("both clicks are written");
    };

    let _ = written(cx, &key, &route, three, Ok(at(3)));
    assert_eq!(
        current_load(cx),
        Load::Ready(Arc::new(at(3).changed(BacklightChange::Level(4))))
    );

    let _ = commit(cx, BacklightChange::Level(3));
    let [(BacklightChange::Level(3), back)] = sent(&mut receiver)[..] else {
        panic!("the click back to level 3 is written");
    };
    let _ = written(cx, &key, &route, four, Ok(at(4)));
    assert_eq!(
        current_load(cx),
        Load::Ready(Arc::new(at(4).changed(BacklightChange::Level(3))))
    );
    let _ = written(cx, &key, &route, back, Ok(at(3)));
    assert_eq!(current_load(cx), Load::Ready(Arc::new(at(3))));
}

/// The agent answers changes in the order they were sent; one that arrives
/// after a newer change's answer anyway says nothing new and is dropped.
#[gpui::test]
fn an_answer_older_than_one_already_taken_is_dropped(cx: &mut gpui::TestAppContext) {
    let (state, mut receiver) = keyboard_state(true);
    let (key, route) = installed_and_read(cx, &mut receiver, state, lit());
    let at = |level| lit().changed(BacklightChange::Level(level));
    let _ = commit(cx, BacklightChange::Level(3));
    let _ = commit(cx, BacklightChange::Level(4));
    let [(_, three), (_, four)] = sent(&mut receiver)[..] else {
        panic!("both clicks are written");
    };

    let _ = written(cx, &key, &route, four, Ok(at(4)));
    assert!(written(cx, &key, &route, three, Ok(at(3))).is_empty());
    assert!(written(cx, &key, &route, three, Err(WriteError::AgentUnavailable)).is_empty());
    cx.run_until_parked();
    assert_eq!(current_load(cx), Load::Ready(Arc::new(at(4))));
    assert_eq!(backlight_reads(&drain(&mut receiver)), 0);
}

/// A re-read the keyboard's own broadcast started can answer with a reading
/// taken before an unanswered change landed. The change stays shown until
/// its own answer arrives.
#[gpui::test]
fn a_re_read_while_a_change_is_unanswered_keeps_the_change_shown(cx: &mut gpui::TestAppContext) {
    let (state, mut receiver) = keyboard_state(true);
    let (key, route) = installed_and_read(cx, &mut receiver, state, lit());
    let raised = lit().changed(BacklightChange::Level(4));
    let _ = commit(cx, BacklightChange::Level(4));
    let [(_, four)] = sent(&mut receiver)[..] else {
        panic!("the click is written");
    };

    cx.update(|cx| {
        AppState::update(cx, |state, _| {
            let resolver = AssetResolver::new();
            let mut changed = keyboard_inventory(true);
            changed.paired[0].settings_revision = 1;
            let _ = state.refresh_inventories(&[changed], &[], &resolver, &[]);
        });
    });
    cx.run_until_parked();
    assert_eq!(answer_backlight_read(&mut receiver, lit()), route);
    cx.run_until_parked();
    assert_eq!(current_load(cx), Load::Ready(Arc::new(raised)));

    let _ = written(cx, &key, &route, four, Ok(raised));
    assert_eq!(current_load(cx), Load::Ready(Arc::new(raised)));
}

/// The answer to a change is what the keyboard holds, even when that is not
/// what the change asked for — its battery saving may have dimmed it since.
/// An answered change is no longer applied over it.
#[gpui::test]
fn the_answer_to_a_change_shows_what_the_keyboard_holds(cx: &mut gpui::TestAppContext) {
    let (state, mut receiver) = keyboard_state(true);
    let (key, route) = installed_and_read(cx, &mut receiver, state, lit());
    let _ = commit(cx, BacklightChange::Level(4));
    let [(_, four)] = sent(&mut receiver)[..] else {
        panic!("the click is written");
    };

    let dimmed = lit().changed(BacklightChange::Level(1));
    let _ = written(cx, &key, &route, four, Ok(dimmed));
    assert_eq!(current_load(cx), Load::Ready(Arc::new(dimmed)));
}
