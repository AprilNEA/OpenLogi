//! Pointer speed: a release persists the speed and writes the mouse through
//! the agent; the slider shows the saved speed over the mouse's reading.

use super::fn_lock::drain;
use super::*;
use crate::services::ipc::{Command, ReadPointerSpeed, SetPointerSpeed};
use crate::state::PointerSpeedLoad;
use openlogi_core::hid::{DeviceRoute, PointerSpeed};

fn speed(raw: u16) -> PointerSpeed {
    PointerSpeed::try_new(raw).expect("a speed in range")
}

/// The known mouse on its cable, scaling its pointer with `0x2205` instead
/// of adjustable DPI, or with neither.
fn mouse_inventory(pointer_scaling: bool) -> DeviceInventory {
    let mut inventory = direct_inventory([0xa3, 0x93, 0xca, 0xe0]);
    let capabilities = inventory.paired[0]
        .capabilities
        .as_mut()
        .expect("the fixture mouse has capabilities");
    capabilities.pointer = false;
    capabilities.pointer_scaling = pointer_scaling;
    inventory
}

fn mouse_state(
    pointer_scaling: bool,
    config: Config,
) -> (AppState, tokio::sync::mpsc::UnboundedReceiver<Command>) {
    let resolver = AssetResolver::new();
    let (commands, receiver) = tokio::sync::mpsc::unbounded_channel();
    let state = AppState::new(Sources {
        inventories: &[mouse_inventory(pointer_scaling)],
        ..Sources::in_memory(config, &resolver, commands)
    });
    (state, receiver)
}

#[test]
fn a_release_persists_the_speed_and_writes_the_mouse() {
    let (mut state, mut receiver) = mouse_state(true, Config::ephemeral());
    drain(&mut receiver);
    let record = state.current_record().expect("the mouse is selected");
    let key = record.device_key();
    let route = record.route.clone().expect("a direct route");

    let events = state.commit_pointer_speed(speed(0x0180));

    assert_eq!(events, [StateEvent::PointerSpeedChanged(key.clone())]);
    assert_eq!(
        state.config.pointer_speed(KNOWN_MOUSE_KEY),
        Some(speed(0x0180))
    );
    let commands = drain(&mut receiver);
    let [
        Command::ReloadConfig(_),
        Command::SetPointerSpeed(SetPointerSpeed {
            route: written_route,
            speed: written,
            key: written_key,
        }),
    ] = commands.as_slice()
    else {
        panic!("expected a config reload followed by one pointer-speed write");
    };
    assert_eq!(*written_route, route);
    assert_eq!(*written, speed(0x0180));
    assert_eq!(*written_key, key);
}

#[test]
fn a_release_is_inert_on_a_mouse_without_pointer_scaling() {
    let (mut state, mut receiver) = mouse_state(false, Config::ephemeral());
    drain(&mut receiver);
    assert!(!state.current_pointer_speed_supported());

    let _ = state.commit_pointer_speed(speed(0x0180));

    assert_eq!(state.config.pointer_speed(KNOWN_MOUSE_KEY), None);
    assert!(drain(&mut receiver).is_empty());
}

/// Answer the pointer-speed read the state has queued, returning the route
/// it was asked on. Every other read is left unanswered.
fn answer_speed_read(
    receiver: &mut tokio::sync::mpsc::UnboundedReceiver<Command>,
    reading: PointerSpeed,
) -> DeviceRoute {
    let mut answered = None;
    for command in drain(receiver) {
        if let Command::ReadPointerSpeed(ReadPointerSpeed { route, reply }) = command {
            assert!(answered.is_none(), "one pointer-speed read at a time");
            let _ = reply.send(Ok(reading));
            answered = Some(route);
        }
    }
    answered.expect("the state asked the mouse for its pointer speed")
}

/// Install `state` with live swr-backed reads, as the app does, and have it
/// read the selected mouse.
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

fn shown(cx: &gpui::TestAppContext) -> Option<PointerSpeed> {
    cx.read(|cx| {
        AppState::try_read(cx)
            .expect("state installed")
            .current_pointer_speed()
    })
}

fn current_load(cx: &gpui::TestAppContext) -> PointerSpeedLoad {
    cx.read(|cx| {
        AppState::try_read(cx)
            .expect("state installed")
            .current_pointer_speed_load()
    })
}

fn refused(cx: &gpui::TestAppContext) -> bool {
    cx.read(|cx| {
        AppState::try_read(cx)
            .expect("state installed")
            .current_pointer_speed_refused()
    })
}

/// Deliver the agent's answer to a pointer-speed write for `key`.
fn answer_write(
    cx: &mut gpui::TestAppContext,
    key: &DeviceKey,
    result: Result<PointerSpeed, WriteError>,
) {
    let events = cx.update(|cx| {
        AppState::update(cx, |state, _| {
            state.apply_pointer_speed_written(key, result)
        })
    });
    assert_eq!(events, [StateEvent::PointerSpeedChanged(key.clone())]);
    cx.run_until_parked();
}

#[gpui::test]
fn a_mouse_with_no_saved_speed_shows_its_reading(cx: &mut gpui::TestAppContext) {
    let (state, mut receiver) = mouse_state(true, Config::ephemeral());
    install(state, cx);
    assert_eq!(shown(cx), None, "nothing to show before the read lands");

    answer_speed_read(&mut receiver, speed(0x0140));
    cx.run_until_parked();

    assert_eq!(shown(cx), Some(speed(0x0140)));
}

/// Precision mode moves the pointer at the slowest speed, which the mouse
/// reads back. The slider still shows the speed precision mode returns to.
#[gpui::test]
fn the_saved_speed_shows_over_a_precision_mode_reading(cx: &mut gpui::TestAppContext) {
    let mut config = Config::ephemeral();
    config.set_pointer_speed(KNOWN_MOUSE_KEY, speed(0x0180));
    let (state, mut receiver) = mouse_state(true, config);
    install(state, cx);

    answer_speed_read(&mut receiver, PointerSpeed::MIN);
    cx.run_until_parked();

    assert_eq!(current_load(cx), Load::Ready(Arc::new(PointerSpeed::MIN)));
    assert_eq!(shown(cx), Some(speed(0x0180)));
}

#[gpui::test]
fn the_mouses_answer_lands_and_a_refusal_reads_it_again(cx: &mut gpui::TestAppContext) {
    let (state, mut receiver) = mouse_state(true, Config::ephemeral());
    let key = state.current_record().expect("mouse").device_key();
    install(state, cx);
    let route = answer_speed_read(&mut receiver, PointerSpeed::DEFAULT);
    cx.run_until_parked();

    let events = cx.update(|cx| {
        AppState::update(cx, |state, _| {
            state.apply_pointer_speed_written(&key, Ok(speed(0x0200)))
        })
    });
    assert_eq!(events, [StateEvent::PointerSpeedChanged(key.clone())]);
    assert_eq!(current_load(cx), Load::Ready(Arc::new(speed(0x0200))));

    let events = cx.update(|cx| {
        AppState::update(cx, |state, _| {
            state.apply_pointer_speed_written(&key, Err(WriteError::AgentUnavailable))
        })
    });
    assert_eq!(events, [StateEvent::PointerSpeedChanged(key)]);
    cx.run_until_parked();
    assert_eq!(
        answer_speed_read(&mut receiver, speed(0x0100)),
        route,
        "the refusal sends the read back to the mouse"
    );
}

/// The slider keeps the saved speed after a refusal, so the state marks the
/// mouse while it reads another speed. Until the re-read lands, the last
/// reading stands, as for every swr read.
#[gpui::test]
fn a_refused_write_is_marked_while_the_mouse_reads_another_speed(cx: &mut gpui::TestAppContext) {
    let mut config = Config::ephemeral();
    config.set_pointer_speed(KNOWN_MOUSE_KEY, speed(0x0180));
    let (state, mut receiver) = mouse_state(true, config);
    let key = state.current_record().expect("mouse").device_key();
    install(state, cx);
    answer_speed_read(&mut receiver, speed(0x0180));
    cx.run_until_parked();
    assert!(!refused(cx));

    answer_write(cx, &key, Err(WriteError::AgentUnavailable));
    answer_speed_read(&mut receiver, speed(0x0100));
    cx.run_until_parked();
    assert!(refused(cx), "the mouse moves at another speed");
    assert_eq!(shown(cx), Some(speed(0x0180)));

    // A reading of the saved speed hides the mark.
    answer_write(cx, &key, Err(WriteError::AgentUnavailable));
    answer_speed_read(&mut receiver, speed(0x0180));
    cx.run_until_parked();
    assert!(!refused(cx));

    // A write that lands clears it.
    answer_write(cx, &key, Err(WriteError::AgentUnavailable));
    answer_speed_read(&mut receiver, speed(0x0100));
    cx.run_until_parked();
    assert!(refused(cx));
    answer_write(cx, &key, Ok(speed(0x0180)));
    assert!(!refused(cx));
}

/// With nothing saved the slider shows the reading, so a refusal needs no
/// mark: the slider moves back to the mouse's own speed.
#[gpui::test]
fn a_refusal_is_not_marked_when_nothing_is_saved(cx: &mut gpui::TestAppContext) {
    let (state, mut receiver) = mouse_state(true, Config::ephemeral());
    let key = state.current_record().expect("mouse").device_key();
    install(state, cx);
    answer_speed_read(&mut receiver, PointerSpeed::DEFAULT);
    cx.run_until_parked();

    answer_write(cx, &key, Err(WriteError::AgentUnavailable));

    assert!(!refused(cx));
}

/// The scaling mouse on a Bolt receiver, online or asleep.
fn mouse_on_receiver(online: bool) -> DeviceInventory {
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
}

/// Deliver an inventory snapshot to the installed state.
fn refresh(cx: &mut gpui::TestAppContext, inventory: DeviceInventory) {
    cx.update(|cx| {
        AppState::update(cx, |state, _| {
            let resolver = AssetResolver::new();
            let _ = state.refresh_inventories(&[inventory], &[], &resolver, &[]);
        });
    });
    cx.run_until_parked();
}

/// The agent writes the saved speed again when the mouse reconnects on the
/// same slot. The refusal then no longer stands, and the mouse is read
/// again.
#[gpui::test]
fn a_reconnect_clears_a_refusal_and_reads_the_mouse_again(cx: &mut gpui::TestAppContext) {
    let mut config = Config::ephemeral();
    config.set_pointer_speed(KNOWN_MOUSE_KEY, speed(0x0180));
    let resolver = AssetResolver::new();
    let (commands, mut receiver) = tokio::sync::mpsc::unbounded_channel();
    let state = AppState::new(Sources {
        inventories: &[mouse_on_receiver(true)],
        ..Sources::in_memory(config, &resolver, commands)
    });
    let key = state.current_record().expect("mouse").device_key();
    install(state, cx);
    let route = answer_speed_read(&mut receiver, speed(0x0180));
    cx.run_until_parked();
    answer_write(cx, &key, Err(WriteError::AgentUnavailable));
    answer_speed_read(&mut receiver, speed(0x0100));
    cx.run_until_parked();
    assert!(refused(cx));

    refresh(cx, mouse_on_receiver(false));
    assert!(refused(cx), "the mouse slept at the other speed");

    refresh(cx, mouse_on_receiver(true));
    assert!(
        !refused(cx),
        "the agent writes the saved speed on reconnect"
    );
    assert_eq!(
        answer_speed_read(&mut receiver, speed(0x0180)),
        route,
        "the reconnect reads the mouse again"
    );
    cx.run_until_parked();
    assert_eq!(current_load(cx), Load::Ready(Arc::new(speed(0x0180))));
}
