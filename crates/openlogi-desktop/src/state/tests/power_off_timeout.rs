//! Automatic power-off: a release writes the headset through the agent,
//! nothing is saved, and the slider shows the headset's own reading. A
//! headset that lists `0x1F20` but refuses the timeout read shows as
//! unsupported instead of retrying forever.

use super::fn_lock::drain;
use super::pointer_speed::install;
use super::*;
use crate::services::ipc::{Command, ReadPowerOffTimeout, SetPowerOffTimeout};
use crate::state::PowerOffTimeoutLoad;
use openlogi_core::hid::{DeviceRoute, PowerOffTimeout};
use std::time::Duration;

fn minutes(raw: u8) -> PowerOffTimeout {
    PowerOffTimeout::try_new(raw).expect("a timeout in range")
}

/// A direct headset that lists `0x1F20`, or one that does not.
fn headset_inventory(power_off_timeout: bool) -> DeviceInventory {
    let mut inventory = direct_inventory([0x0a, 0x5b, 0x00, 0x01]);
    inventory.paired[0].kind = DeviceKind::Headset;
    inventory.paired[0].capabilities = Some(Capabilities {
        power_off_timeout,
        ..Capabilities::default()
    });
    inventory
}

fn headset_state(
    power_off_timeout: bool,
) -> (AppState, tokio::sync::mpsc::UnboundedReceiver<Command>) {
    let resolver = AssetResolver::new();
    let (commands, receiver) = tokio::sync::mpsc::unbounded_channel();
    let state = AppState::new(Sources {
        inventories: &[headset_inventory(power_off_timeout)],
        ..Sources::in_memory(Config::ephemeral(), &resolver, commands)
    });
    (state, receiver)
}

/// Answer the power-off read the state has queued with `answer`, returning
/// the route it was asked on. Every other read is left unanswered.
fn answer_read(
    receiver: &mut tokio::sync::mpsc::UnboundedReceiver<Command>,
    answer: &Result<PowerOffTimeout, WriteError>,
) -> DeviceRoute {
    let mut answered = None;
    for command in drain(receiver) {
        if let Command::ReadPowerOffTimeout(ReadPowerOffTimeout { route, reply }) = command {
            assert!(answered.is_none(), "one power-off read at a time");
            let _ = reply.send(answer.clone());
            answered = Some(route);
        }
    }
    answered.expect("the state asked the headset for its power-off timeout")
}

fn current_load(cx: &gpui::TestAppContext) -> PowerOffTimeoutLoad {
    cx.read(|cx| {
        AppState::try_read(cx)
            .expect("state installed")
            .current_power_off_timeout_load()
    })
}

#[test]
fn a_release_writes_the_headset_and_saves_nothing() {
    let (mut state, mut receiver) = headset_state(true);
    drain(&mut receiver);
    let record = state.current_record().expect("the headset is selected");
    let key = record.device_key();
    let route = record.route.clone().expect("a direct route");

    let events = state.commit_power_off_timeout(minutes(30));

    assert_eq!(events, [StateEvent::PowerOffTimeoutChanged(key.clone())]);
    let commands = drain(&mut receiver);
    let [
        Command::SetPowerOffTimeout(SetPowerOffTimeout {
            route: written_route,
            timeout: written,
            key: written_key,
        }),
    ] = commands.as_slice()
    else {
        panic!("expected one power-off write and no config reload: the headset keeps the timeout");
    };
    assert_eq!(*written_route, route);
    assert_eq!(*written, minutes(30));
    assert_eq!(*written_key, key);
}

/// "Never" is a timeout like any other: 0 reaches the headset instead of
/// being dropped as an empty value.
#[test]
fn never_is_written_as_zero_minutes() {
    let (mut state, mut receiver) = headset_state(true);
    drain(&mut receiver);

    let _ = state.commit_power_off_timeout(PowerOffTimeout::NEVER);

    let commands = drain(&mut receiver);
    let [Command::SetPowerOffTimeout(SetPowerOffTimeout { timeout, .. })] = commands.as_slice()
    else {
        panic!("expected one power-off write");
    };
    assert_eq!(timeout.minutes(), 0);
}

#[test]
fn a_release_is_inert_on_a_device_without_power_off() {
    let (mut state, mut receiver) = headset_state(false);
    drain(&mut receiver);
    assert!(!state.current_power_off_timeout_supported());

    let _ = state.commit_power_off_timeout(minutes(30));

    assert!(drain(&mut receiver).is_empty());
}

/// A wireless headset that is switched off keeps its dongle's route: the
/// slider hides and a release writes nothing.
#[test]
fn an_offline_headset_is_not_written() {
    let mut inventory = headset_inventory(true);
    inventory.paired[0].online = false;
    let resolver = AssetResolver::new();
    let (commands, mut receiver) = tokio::sync::mpsc::unbounded_channel();
    let mut state = AppState::new(Sources {
        inventories: &[inventory],
        ..Sources::in_memory(Config::ephemeral(), &resolver, commands)
    });
    drain(&mut receiver);
    let record = state.current_record().expect("the headset is selected");
    assert!(!record.is_online() && record.route.is_some());
    assert!(state.current_power_off_timeout_supported());
    assert_eq!(state.current_power_off_timeout_route(), None);

    let _ = state.commit_power_off_timeout(minutes(30));

    assert!(drain(&mut receiver).is_empty());
}

#[gpui::test]
fn the_reading_lands_and_a_refusal_reads_the_headset_again(cx: &mut gpui::TestAppContext) {
    let (state, mut receiver) = headset_state(true);
    let key = state.current_record().expect("headset").device_key();
    install(state, cx);
    let route = answer_read(&mut receiver, &Ok(minutes(15)));
    cx.run_until_parked();
    assert_eq!(current_load(cx), Load::Ready(Arc::new(minutes(15))));

    let events = cx.update(|cx| {
        AppState::update(cx, |state, _| {
            state.apply_power_off_timeout_written(&key, Ok(minutes(30)))
        })
    });
    assert_eq!(events, [StateEvent::PowerOffTimeoutChanged(key.clone())]);
    assert_eq!(current_load(cx), Load::Ready(Arc::new(minutes(30))));

    let events = cx.update(|cx| {
        AppState::update(cx, |state, _| {
            state.apply_power_off_timeout_written(&key, Err(WriteError::AgentUnavailable))
        })
    });
    assert_eq!(events, [StateEvent::PowerOffTimeoutChanged(key)]);
    cx.run_until_parked();
    assert_eq!(
        answer_read(&mut receiver, &Ok(minutes(30))),
        route,
        "the refusal sends the read back to the headset"
    );
    cx.run_until_parked();
    assert_eq!(current_load(cx), Load::Ready(Arc::new(minutes(30))));
}

/// Some headsets list `0x1F20` for the battery but refuse the timeout
/// functions. That answer is final: the row says so instead of offering a
/// retry that can never succeed.
#[gpui::test]
fn a_headset_that_refuses_the_read_is_unsupported(cx: &mut gpui::TestAppContext) {
    let (state, mut receiver) = headset_state(true);
    install(state, cx);
    answer_read(
        &mut receiver,
        &Err(WriteError::FeatureUnsupported {
            feature_hex: 0x1f20,
        }),
    );
    // Let any retry the read policy would schedule come due.
    cx.executor().advance_clock(Duration::from_secs(60));
    cx.run_until_parked();

    let load = current_load(cx);
    assert!(
        matches!(load, Load::Unsupported(_)),
        "expected Unsupported, got {load:?}"
    );
    assert!(
        drain(&mut receiver)
            .iter()
            .all(|command| !matches!(command, Command::ReadPowerOffTimeout(_))),
        "an unsupported answer is not retried"
    );
}

/// The agent changed the timeout itself — from the menu-bar menu — and
/// moved the headset's settings revision, so the slider reads it again.
#[gpui::test]
fn a_moved_settings_revision_reads_the_headset_again(cx: &mut gpui::TestAppContext) {
    let (state, mut receiver) = headset_state(true);
    install(state, cx);
    let route = answer_read(&mut receiver, &Ok(minutes(15)));
    cx.run_until_parked();

    let mut moved = headset_inventory(true);
    moved.paired[0].settings_revision = 1;
    let _ = cx.update(|cx| {
        AppState::update(cx, |state, _| {
            state.refresh_inventories(&[moved], &[], &AssetResolver::new(), &[])
        })
    });
    cx.run_until_parked();
    assert_eq!(answer_read(&mut receiver, &Ok(minutes(60))), route);
    cx.run_until_parked();
    assert_eq!(current_load(cx), Load::Ready(Arc::new(minutes(60))));
}

/// The headset switched itself off after its own timeout and came back.
/// The reading from before may be stale, so the slider reads it again.
#[gpui::test]
fn a_reconnect_reads_the_headset_again(cx: &mut gpui::TestAppContext) {
    let (state, mut receiver) = headset_state(true);
    install(state, cx);
    let route = answer_read(&mut receiver, &Ok(minutes(15)));
    cx.run_until_parked();

    let mut offline = headset_inventory(true);
    offline.paired[0].online = false;
    for inventory in [offline, headset_inventory(true)] {
        let _ = cx.update(|cx| {
            AppState::update(cx, |state, _| {
                state.refresh_inventories(&[inventory], &[], &AssetResolver::new(), &[])
            })
        });
        cx.run_until_parked();
    }
    assert_eq!(
        answer_read(&mut receiver, &Ok(minutes(30))),
        route,
        "the reconnect reads the headset again"
    );
    cx.run_until_parked();
    assert_eq!(current_load(cx), Load::Ready(Arc::new(minutes(30))));
}
