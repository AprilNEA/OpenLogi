//! Haptics: a change persists the settings and writes the device through the
//! agent; the device's echo replaces what the controls show.

use super::fn_lock::drain;
use super::*;
use crate::services::ipc::{Command, ReadHaptics, SetHaptics};
use crate::state::HapticsLoad;
use openlogi_core::hid::{HapticIntensity, HapticLevels, HapticReading, HapticSettings};

fn settings(enabled: bool, intensity: u8) -> HapticSettings {
    HapticSettings {
        enabled,
        power_save: false,
        intensity: HapticIntensity::try_new(intensity).expect("in range"),
    }
}

/// The known mouse on its cable, with or without haptic feedback.
fn mouse_inventory(haptic_feedback: bool) -> DeviceInventory {
    let mut inventory = direct_inventory([0xa3, 0x93, 0xca, 0xe0]);
    inventory.paired[0]
        .capabilities
        .as_mut()
        .expect("the fixture mouse has capabilities")
        .haptic_feedback = haptic_feedback;
    inventory
}

pub(crate) fn mouse_state(
    haptic_feedback: bool,
    config: Config,
) -> (AppState, tokio::sync::mpsc::UnboundedReceiver<Command>) {
    let resolver = AssetResolver::new();
    let (commands, receiver) = tokio::sync::mpsc::unbounded_channel();
    let state = AppState::new(Sources {
        inventories: &[mouse_inventory(haptic_feedback)],
        ..Sources::in_memory(config, &resolver, commands)
    });
    (state, receiver)
}

#[test]
fn commit_haptics_persists_and_sends() {
    let (mut state, mut receiver) = mouse_state(true, Config::ephemeral());
    drain(&mut receiver);
    let record = state.current_record().expect("the mouse is selected");
    let key = record.device_key();
    let route = record.route.clone().expect("a direct route");

    let events = state.commit_haptics(settings(true, 40), true);

    assert_eq!(events, [StateEvent::HapticsChanged(key.clone())]);
    assert_eq!(
        state.config.haptics(KNOWN_MOUSE_KEY),
        Some(settings(true, 40))
    );
    let commands = drain(&mut receiver);
    let [
        Command::ReloadConfig(_),
        Command::SetHaptics(SetHaptics {
            route: written_route,
            settings: written,
            preview,
            key: written_key,
            ..
        }),
    ] = commands.as_slice()
    else {
        panic!("expected a config reload followed by one haptics write");
    };
    assert_eq!(*written_route, route);
    assert_eq!(*written, settings(true, 40));
    assert!(*preview);
    assert_eq!(*written_key, key);
}

#[test]
fn commit_haptics_is_inert_on_a_device_without_haptics() {
    let (mut state, mut receiver) = mouse_state(false, Config::ephemeral());
    drain(&mut receiver);
    assert!(!state.current_haptics_supported());

    let _ = state.commit_haptics(settings(true, 40), false);

    assert_eq!(state.config.haptics(KNOWN_MOUSE_KEY), None);
    assert!(drain(&mut receiver).is_empty());
}

#[test]
fn a_silenced_engine_silences_the_ring_toggle() {
    let mut config = Config::ephemeral();
    let (state, _receiver) = mouse_state(true, config.clone());
    assert!(!state.current_haptics_silenced(), "nothing saved");
    config.set_haptics(KNOWN_MOUSE_KEY, settings(true, 0));
    let (state, _receiver) = mouse_state(true, config.clone());
    assert!(state.current_haptics_silenced(), "zero intensity");
    config.set_haptics(KNOWN_MOUSE_KEY, settings(false, 60));
    let (state, _receiver) = mouse_state(true, config);
    assert!(state.current_haptics_silenced(), "engine off");
}

/// Answer the haptics read the state has queued. Every other read is left
/// unanswered.
pub(crate) fn answer_haptics_read(
    receiver: &mut tokio::sync::mpsc::UnboundedReceiver<Command>,
    reading: HapticReading,
) {
    let mut answered = false;
    for command in drain(receiver) {
        if let Command::ReadHaptics(ReadHaptics { reply, .. }) = command {
            assert!(!answered, "one haptics read at a time");
            let _ = reply.send(Ok(reading));
            answered = true;
        }
    }
    assert!(answered, "the state asked the device for its haptics");
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

fn shown(cx: &gpui::TestAppContext) -> Option<HapticSettings> {
    cx.read(|cx| {
        AppState::try_read(cx)
            .expect("state installed")
            .current_haptics_shown()
    })
}

#[gpui::test]
fn written_echo_replaces_shown_value(cx: &mut gpui::TestAppContext) {
    let (state, mut receiver) = mouse_state(true, Config::ephemeral());
    let key = state.current_record().expect("mouse").device_key();
    install(state, cx);
    let reading = HapticReading {
        settings: settings(true, 60),
        levels: HapticLevels::Any,
        default_intensity: 60,
    };
    answer_haptics_read(&mut receiver, reading);
    cx.run_until_parked();
    assert_eq!(shown(cx), Some(settings(true, 60)));

    // The controls show the asked-for settings at once.
    let _ = cx.update(|cx| {
        AppState::update(cx, |state, _| {
            state.commit_haptics(settings(true, 30), true)
        })
    });
    assert_eq!(shown(cx), Some(settings(true, 30)));
    let write = drain(&mut receiver)
        .into_iter()
        .find_map(|command| match command {
            Command::SetHaptics(SetHaptics { write, .. }) => Some(write),
            _ => None,
        })
        .expect("the change writes the device");

    // The device kept power save on: its echo is what the controls show.
    let echoed = HapticSettings {
        power_save: true,
        ..settings(true, 30)
    };
    let events = cx.update(|cx| {
        AppState::update(cx, |state, _| {
            state.apply_haptics_written(&key, write, Ok(echoed))
        })
    });
    assert_eq!(events, [StateEvent::HapticsChanged(key.clone())]);
    assert_eq!(shown(cx), Some(echoed));
    let load = cx.read(|cx| {
        AppState::try_read(cx)
            .expect("state installed")
            .current_haptics_load()
    });
    let HapticsLoad::Ready(kept) = load else {
        panic!("the reading stays ready");
    };
    assert_eq!(
        kept.levels,
        HapticLevels::Any,
        "the levels come from the read"
    );
}

/// The haptic mouse on a Bolt receiver slot, online or asleep.
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

fn refresh(cx: &mut gpui::TestAppContext, inventory: DeviceInventory) {
    cx.update(|cx| {
        AppState::update(cx, |state, _| {
            let resolver = AssetResolver::new();
            let _ = state.refresh_inventories(&[inventory], &[], &resolver, &[]);
        });
    });
    cx.run_until_parked();
}

fn reads_haptics(receiver: &mut tokio::sync::mpsc::UnboundedReceiver<Command>) -> bool {
    drain(receiver)
        .iter()
        .any(|command| matches!(command, Command::ReadHaptics(_)))
}

fn reading(settings: HapticSettings) -> HapticReading {
    HapticReading {
        settings,
        levels: HapticLevels::Any,
        default_intensity: 60,
    }
}

/// The agent writes saved haptic settings again when the mouse reconnects,
/// and answers a read only once that write has finished. The app therefore
/// reads the mouse again, and shows its answer rather than the saved
/// settings it has not confirmed.
#[gpui::test]
fn a_reconnect_reads_the_settings_the_agent_wrote(cx: &mut gpui::TestAppContext) {
    let mut config = Config::ephemeral();
    config.set_haptics(KNOWN_MOUSE_KEY, settings(true, 40));
    let resolver = AssetResolver::new();
    let (commands, mut receiver) = tokio::sync::mpsc::unbounded_channel();
    let state = AppState::new(Sources {
        inventories: &[mouse_on_receiver(true)],
        ..Sources::in_memory(config, &resolver, commands)
    });
    install(state, cx);
    // The mouse holds other settings than the saved ones, as after a write
    // it refused.
    answer_haptics_read(&mut receiver, reading(settings(true, 60)));
    cx.run_until_parked();
    assert_eq!(shown(cx), Some(settings(true, 60)));

    refresh(cx, mouse_on_receiver(false));
    refresh(cx, mouse_on_receiver(true));

    answer_haptics_read(&mut receiver, reading(settings(true, 40)));
    cx.run_until_parked();
    assert_eq!(shown(cx), Some(settings(true, 40)));
}

/// With nothing saved the agent writes no haptics on reconnect, so the mouse
/// is read again: it may have been changed while away.
#[gpui::test]
fn a_reconnect_without_saved_settings_reads_the_mouse(cx: &mut gpui::TestAppContext) {
    let resolver = AssetResolver::new();
    let (commands, mut receiver) = tokio::sync::mpsc::unbounded_channel();
    let state = AppState::new(Sources {
        inventories: &[mouse_on_receiver(true)],
        ..Sources::in_memory(Config::ephemeral(), &resolver, commands)
    });
    install(state, cx);
    answer_haptics_read(&mut receiver, reading(settings(true, 60)));
    cx.run_until_parked();

    refresh(cx, mouse_on_receiver(false));
    refresh(cx, mouse_on_receiver(true));

    assert!(reads_haptics(&mut receiver), "the mouse is read again");
}

/// Two changes in a row: the first write's answer arrives after the second
/// change, and must not bring the first settings back, or the next switch
/// would write them again.
#[gpui::test]
fn an_older_answer_does_not_undo_a_newer_change(cx: &mut gpui::TestAppContext) {
    let (state, mut receiver) = mouse_state(true, Config::ephemeral());
    let key = state.current_record().expect("mouse").device_key();
    install(state, cx);
    answer_haptics_read(&mut receiver, reading(settings(true, 60)));
    cx.run_until_parked();

    let _ = cx.update(|cx| {
        AppState::update(cx, |state, _| {
            let _ = state.commit_haptics(settings(true, 30), true);
            state.commit_haptics(settings(true, 80), true)
        })
    });
    let writes: Vec<_> = drain(&mut receiver)
        .into_iter()
        .filter_map(|command| match command {
            Command::SetHaptics(write) => Some(write),
            _ => None,
        })
        .collect();
    let [first, second] = writes.as_slice() else {
        panic!("two haptics writes");
    };
    assert_eq!(first.settings, settings(true, 30));
    assert_eq!(second.settings, settings(true, 80));

    let _ = cx.update(|cx| {
        AppState::update(cx, |state, _| {
            state.apply_haptics_written(&key, first.write, Ok(settings(true, 30)))
        })
    });
    assert_eq!(
        shown(cx),
        Some(settings(true, 80)),
        "the older answer leaves the newer change shown"
    );
}
