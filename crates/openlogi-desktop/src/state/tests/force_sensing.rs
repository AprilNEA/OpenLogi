//! Press force: a release writes the device through the agent, and the force
//! the device reads back is shown and saved.

use super::fn_lock::drain;
use super::*;
use crate::services::ipc::{Command, ReadForceSensing, SetForceSensing};
use crate::state::ForceSensingLoad;
use openlogi_core::hid::ForceSensingInfo;

/// The MX Master 4's Haptic Sense Panel as one unit reported it.
pub(crate) const PANEL: ForceSensingInfo = ForceSensingInfo {
    changeable: true,
    default: 5089,
    min: 4071,
    max: 6768,
    current: 5089,
};

/// The known mouse on its cable, with a force-sensing button or without.
pub(crate) fn mouse_inventory(force_sensing: bool) -> DeviceInventory {
    let mut inventory = direct_inventory([0xa3, 0x93, 0xca, 0xe0]);
    inventory.paired[0]
        .capabilities
        .as_mut()
        .expect("the fixture mouse has capabilities")
        .force_sensing = force_sensing;
    inventory
}

fn mouse_state(
    force_sensing: bool,
    config: Config,
) -> (AppState, tokio::sync::mpsc::UnboundedReceiver<Command>) {
    let resolver = AssetResolver::new();
    let (commands, receiver) = tokio::sync::mpsc::unbounded_channel();
    let state = AppState::new(Sources {
        inventories: &[mouse_inventory(force_sensing)],
        ..Sources::in_memory(config, &resolver, commands)
    });
    (state, receiver)
}

/// Answer the press-force read the state has queued, returning the route it
/// was asked on. Every other command is dropped.
pub(crate) fn answer_force_read(
    receiver: &mut tokio::sync::mpsc::UnboundedReceiver<Command>,
    reading: ForceSensingInfo,
) -> DeviceRoute {
    let mut answered = None;
    for command in drain(receiver) {
        if let Command::ReadForceSensing(ReadForceSensing { route, reply }) = command {
            assert!(answered.is_none(), "one press-force read at a time");
            let _ = reply.send(Ok(reading));
            answered = Some(route);
        }
    }
    answered.expect("the state asked the device for its press force")
}

/// The press-force writes among `commands`, as `(route, force)`.
fn force_writes(commands: &[Command]) -> Vec<(DeviceRoute, u16)> {
    commands
        .iter()
        .filter_map(|command| match command {
            Command::SetForceSensing(SetForceSensing { route, force, .. }) => {
                Some((route.clone(), *force))
            }
            _ => None,
        })
        .collect()
}

fn reloads(commands: &[Command]) -> usize {
    commands
        .iter()
        .filter(|command| matches!(command, Command::ReloadConfig(_)))
        .count()
}

/// Install `state` with live swr-backed reads, as the app does, and have it
/// read the selected device.
pub(crate) fn install(state: AppState, cx: &mut gpui::TestAppContext) {
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

/// Install a mouse with a force-sensing button whose read answers `reading`.
fn installed(
    reading: ForceSensingInfo,
    cx: &mut gpui::TestAppContext,
) -> (DeviceKey, tokio::sync::mpsc::UnboundedReceiver<Command>) {
    let (state, mut receiver) = mouse_state(true, Config::ephemeral());
    let key = state.current_record().expect("mouse").device_key();
    install(state, cx);
    answer_force_read(&mut receiver, reading);
    cx.run_until_parked();
    (key, receiver)
}

fn with_state<R>(cx: &mut gpui::TestAppContext, f: impl FnOnce(&mut AppState) -> R) -> R {
    cx.update(|cx| AppState::update(cx, |state, _| f(state)))
}

fn current_load(cx: &mut gpui::TestAppContext) -> ForceSensingLoad {
    with_state(cx, |state| state.current_force_sensing_load())
}

fn saved(cx: &mut gpui::TestAppContext) -> Option<u16> {
    with_state(cx, |state| state.config.force_sensing(KNOWN_MOUSE_KEY))
}

fn refused(cx: &mut gpui::TestAppContext) -> bool {
    with_state(cx, |state| state.current_force_sensing_refused())
}

#[gpui::test]
fn a_release_writes_the_device_and_saves_the_force_it_reads_back(cx: &mut gpui::TestAppContext) {
    let (key, mut receiver) = installed(PANEL, cx);

    let events = with_state(cx, |state| state.commit_force_sensing(6000));

    assert_eq!(events, [StateEvent::ForceSensingChanged(key.clone())]);
    let commands = drain(&mut receiver);
    let [(_, 6000)] = force_writes(&commands).as_slice() else {
        panic!("expected one press-force write of 6000");
    };
    assert_eq!(
        reloads(&commands),
        0,
        "nothing is saved before the device answers"
    );
    assert_eq!(saved(cx), None);
    assert_eq!(
        current_load(cx),
        Load::Ready(Arc::new(ForceSensingInfo {
            current: 6000,
            ..PANEL
        })),
        "the slider stays where it was released"
    );

    // The device keeps a force near the one asked for.
    let confirmed = ForceSensingInfo {
        current: 5998,
        ..PANEL
    };
    let events = with_state(cx, |state| {
        state.apply_force_sensing_written(&key, Ok(confirmed))
    });

    assert_eq!(events, [StateEvent::ForceSensingChanged(key)]);
    assert_eq!(current_load(cx), Load::Ready(Arc::new(confirmed)));
    assert_eq!(saved(cx), Some(5998));
    assert_eq!(
        reloads(&drain(&mut receiver)),
        1,
        "the agent reloads the save"
    );
}

#[gpui::test]
fn a_release_is_clamped_into_the_range_the_device_reported(cx: &mut gpui::TestAppContext) {
    let (_, mut receiver) = installed(PANEL, cx);

    let _ = with_state(cx, |state| state.commit_force_sensing(u16::MAX));
    let _ = with_state(cx, |state| state.commit_force_sensing(0));

    let writes: Vec<u16> = force_writes(&drain(&mut receiver))
        .into_iter()
        .map(|(_, force)| force)
        .collect();
    assert_eq!(writes, [PANEL.max, PANEL.min]);
}

#[gpui::test]
fn a_release_is_inert_while_the_device_does_not_let_the_force_change(
    cx: &mut gpui::TestAppContext,
) {
    let (_, mut receiver) = installed(
        ForceSensingInfo {
            changeable: false,
            ..PANEL
        },
        cx,
    );

    let _ = with_state(cx, |state| state.commit_force_sensing(6000));

    assert!(force_writes(&drain(&mut receiver)).is_empty());
    assert_eq!(saved(cx), None);
}

#[test]
fn a_device_without_a_force_sensing_button_is_neither_read_nor_written() {
    let (mut state, mut receiver) = mouse_state(false, Config::ephemeral());
    drain(&mut receiver);
    assert!(!state.current_force_sensing_supported());

    let _ = state.commit_force_sensing(6000);

    assert!(drain(&mut receiver).is_empty());
    assert_eq!(state.config.force_sensing(KNOWN_MOUSE_KEY), None);
}

#[gpui::test]
fn a_refusal_reads_the_device_again_and_saves_nothing(cx: &mut gpui::TestAppContext) {
    let (key, mut receiver) = installed(PANEL, cx);
    let _ = with_state(cx, |state| state.commit_force_sensing(6000));
    drain(&mut receiver);

    let events = with_state(cx, |state| {
        state.apply_force_sensing_written(&key, Err(WriteError::AgentUnavailable))
    });
    cx.run_until_parked();

    assert_eq!(events, [StateEvent::ForceSensingChanged(key.clone())]);
    assert!(refused(cx));
    assert_eq!(saved(cx), None);
    answer_force_read(&mut receiver, PANEL);
    cx.run_until_parked();
    assert_eq!(
        current_load(cx),
        Load::Ready(Arc::new(PANEL)),
        "the slider returns to the device's own force"
    );

    // A write that lands clears the mark.
    let _ = with_state(cx, |state| {
        state.apply_force_sensing_written(&key, Ok(PANEL))
    });
    assert!(!refused(cx));
}

/// The agent writes the saved force again when the device reconnects, so a
/// refusal no longer stands and the device is read again.
#[gpui::test]
fn a_reconnect_clears_a_refusal_and_reads_the_device_again(cx: &mut gpui::TestAppContext) {
    let on_receiver = |online: bool| {
        let mut inventory = mouse_inventory(true);
        inventory.receiver = ReceiverInfo {
            name: "Bolt Receiver".to_string(),
            vendor_id: 0x046d,
            product_id: 0xc548,
            unique_id: Some("82839805".to_string()),
        };
        inventory.paired[0].slot = 1;
        inventory.paired[0].online = online;
        inventory
    };
    let refresh = |cx: &mut gpui::TestAppContext, inventory: DeviceInventory| {
        with_state(cx, |state| {
            let resolver = AssetResolver::new();
            let _ = state.refresh_inventories(&[inventory], &[], &resolver, &[]);
        });
        cx.run_until_parked();
    };
    let resolver = AssetResolver::new();
    let (commands, mut receiver) = tokio::sync::mpsc::unbounded_channel();
    let state = AppState::new(Sources {
        inventories: &[on_receiver(true)],
        ..Sources::in_memory(Config::ephemeral(), &resolver, commands)
    });
    let key = state.current_record().expect("mouse").device_key();
    install(state, cx);
    let route = answer_force_read(&mut receiver, PANEL);
    cx.run_until_parked();
    let _ = with_state(cx, |state| {
        state.apply_force_sensing_written(&key, Err(WriteError::AgentUnavailable))
    });
    cx.run_until_parked();
    answer_force_read(&mut receiver, PANEL);
    cx.run_until_parked();
    assert!(refused(cx));

    refresh(cx, on_receiver(false));
    assert!(refused(cx), "the device slept");

    refresh(cx, on_receiver(true));
    assert!(
        !refused(cx),
        "the agent writes the saved force on reconnect"
    );
    assert_eq!(
        answer_force_read(&mut receiver, PANEL),
        route,
        "the reconnect reads the device again"
    );
}
