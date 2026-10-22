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

    // The device kept power save on: its echo is what the controls show.
    let echoed = HapticSettings {
        power_save: true,
        ..settings(true, 30)
    };
    let events = cx.update(|cx| {
        AppState::update(cx, |state, _| state.apply_haptics_written(&key, Ok(echoed)))
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
