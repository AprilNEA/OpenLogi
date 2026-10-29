//! Sidetone: a release writes the headset through the agent, nothing is
//! saved, and the slider shows the headset's own reading.

use super::fn_lock::drain;
use super::pointer_speed::install;
use super::*;
use crate::services::ipc::{Command, ReadSidetone, SetSidetone};
use crate::state::SidetoneLoad;
use openlogi_core::hid::{DeviceRoute, SidetoneLevel};

fn level(raw: u8) -> SidetoneLevel {
    SidetoneLevel::try_new(raw).expect("a level in range")
}

/// A direct device with the G535's capabilities: sidetone only, or none.
fn headset_state(sidetone: bool) -> (AppState, tokio::sync::mpsc::UnboundedReceiver<Command>) {
    let mut inventory = direct_inventory([0x0a, 0xc4, 0x00, 0x01]);
    inventory.paired[0].kind = DeviceKind::Headset;
    inventory.paired[0].capabilities = Some(Capabilities {
        sidetone,
        ..Capabilities::default()
    });
    let resolver = AssetResolver::new();
    let (commands, receiver) = tokio::sync::mpsc::unbounded_channel();
    let state = AppState::new(Sources {
        inventories: &[inventory],
        ..Sources::in_memory(Config::ephemeral(), &resolver, commands)
    });
    (state, receiver)
}

/// Answer the sidetone read the state has queued, returning the route it was
/// asked on. Every other read is left unanswered.
fn answer_read(
    receiver: &mut tokio::sync::mpsc::UnboundedReceiver<Command>,
    reading: SidetoneLevel,
) -> DeviceRoute {
    let mut answered = None;
    for command in drain(receiver) {
        if let Command::ReadSidetone(ReadSidetone { route, reply }) = command {
            assert!(answered.is_none(), "one sidetone read at a time");
            let _ = reply.send(Ok(reading));
            answered = Some(route);
        }
    }
    answered.expect("the state asked the headset for its sidetone")
}

fn current_load(cx: &gpui::TestAppContext) -> SidetoneLoad {
    cx.read(|cx| {
        AppState::try_read(cx)
            .expect("state installed")
            .current_sidetone_load()
    })
}

#[test]
fn a_release_writes_the_headset_and_saves_nothing() {
    let (mut state, mut receiver) = headset_state(true);
    drain(&mut receiver);
    let record = state.current_record().expect("the headset is selected");
    let key = record.device_key();
    let route = record.route.clone().expect("a direct route");

    let events = state.commit_sidetone(level(40));

    assert_eq!(events, [StateEvent::SidetoneChanged(key.clone())]);
    let commands = drain(&mut receiver);
    let [
        Command::SetSidetone(SetSidetone {
            route: written_route,
            level: written,
            key: written_key,
        }),
    ] = commands.as_slice()
    else {
        panic!("expected one sidetone write and no config reload: the headset keeps the level");
    };
    assert_eq!(*written_route, route);
    assert_eq!(*written, level(40));
    assert_eq!(*written_key, key);
}

#[test]
fn a_release_is_inert_on_a_device_without_sidetone() {
    let (mut state, mut receiver) = headset_state(false);
    drain(&mut receiver);
    assert!(!state.current_sidetone_supported());

    let _ = state.commit_sidetone(level(40));

    assert!(drain(&mut receiver).is_empty());
}

#[gpui::test]
fn the_reading_lands_and_a_refusal_reads_the_headset_again(cx: &mut gpui::TestAppContext) {
    let (state, mut receiver) = headset_state(true);
    let key = state.current_record().expect("headset").device_key();
    install(state, cx);
    let route = answer_read(&mut receiver, level(40));
    cx.run_until_parked();
    assert_eq!(current_load(cx), Load::Ready(Arc::new(level(40))));

    let events = cx.update(|cx| {
        AppState::update(cx, |state, _| {
            state.apply_sidetone_written(&key, Ok(level(25)))
        })
    });
    assert_eq!(events, [StateEvent::SidetoneChanged(key.clone())]);
    assert_eq!(current_load(cx), Load::Ready(Arc::new(level(25))));

    let events = cx.update(|cx| {
        AppState::update(cx, |state, _| {
            state.apply_sidetone_written(&key, Err(WriteError::AgentUnavailable))
        })
    });
    assert_eq!(events, [StateEvent::SidetoneChanged(key)]);
    cx.run_until_parked();
    assert_eq!(
        answer_read(&mut receiver, level(25)),
        route,
        "the refusal sends the read back to the headset"
    );
    cx.run_until_parked();
    assert_eq!(current_load(cx), Load::Ready(Arc::new(level(25))));
}
