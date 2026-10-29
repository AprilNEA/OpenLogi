//! USB microphones: their device records, the controls the panel reads, and
//! the writes it sends.

use openlogi_core::microphone::{
    GainStage, Microphone, MicrophoneChange, MicrophoneControls, MicrophoneError, MilliDecibels,
};

use super::*;
use crate::services::ipc::Command;

fn yeti(muted: bool) -> Microphone {
    Microphone {
        id: "yeti-1".into(),
        vendor_id: 0x046d,
        product_id: 0x0ab7,
        name: "Yeti".into(),
        registry_model_id: Some("0ab7".into()),
        controls: MicrophoneControls {
            gain: Some(GainStage {
                level: MilliDecibels::new(12_000),
                min: MilliDecibels::new(-12_000),
                max: MilliDecibels::new(21_000),
            }),
            muted: Some(muted),
            ..MicrophoneControls::default()
        },
    }
}

fn snapshot(microphones: Vec<Microphone>) -> AgentSnapshot {
    AgentSnapshot {
        status: AgentStatus {
            accessibility_granted: true,
            hook_installed: true,
            launch_at_login: false,
            inventory: InventoryHealth::Ready,
            protocol_version: PROTOCOL_VERSION,
            agent_version: "microphone-test".to_string(),
            input_monitoring_granted: true,
            hid_open_failures: false,
            secure_input: openlogi_ipc::SecureInput::Off,
            virtual_keyboard: openlogi_ipc::VirtualKeyboardStatus::Unsupported,
        },
        inventory: Vec::new(),
        standalone: Vec::new(),
        camera_active: false,
        pairing: None,
        foreground: ForegroundApps::default(),
        lights: Vec::new(),
        microphones,
    }
}

fn microphone_state() -> (AppState, tokio::sync::mpsc::UnboundedReceiver<Command>) {
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    let state = AppState::new(Sources::in_memory(
        Config::ephemeral(),
        &AssetResolver::new(),
        tx,
    ));
    (state, rx)
}

fn microphone_record(state: &AppState) -> Option<&DeviceRecord> {
    state
        .devices
        .records
        .iter()
        .find(|record| record.kind == DeviceKind::Microphone)
}

#[test]
fn a_reported_microphone_gets_a_record_and_its_controls() {
    let (mut state, _rx) = microphone_state();

    let changes =
        state.apply_agent_snapshot(&snapshot(vec![yeti(false)]), &AssetResolver::new(), &[]);

    assert!(changes.events.contains(&StateEvent::MicrophoneChanged));
    let record = microphone_record(&state)
        .expect("microphone record")
        .clone();
    assert_eq!(record.config_key, "microphone:yeti-1");
    assert_eq!(record.display_name, "Yeti");
    assert!(
        record.route.is_none(),
        "a microphone never takes a HID++ route"
    );
    assert_eq!(state.microphone_for(&record), Some(&yeti(false)));
}

#[test]
fn an_unchanged_microphone_announces_nothing() {
    let (mut state, _rx) = microphone_state();
    let _ = state.apply_agent_snapshot(&snapshot(vec![yeti(false)]), &AssetResolver::new(), &[]);

    let same = state.apply_agent_snapshot(&snapshot(vec![yeti(false)]), &AssetResolver::new(), &[]);
    let muted = state.apply_agent_snapshot(&snapshot(vec![yeti(true)]), &AssetResolver::new(), &[]);

    assert!(!same.events.contains(&StateEvent::MicrophoneChanged));
    assert!(muted.events.contains(&StateEvent::MicrophoneChanged));
}

#[test]
fn an_unplugged_microphone_loses_its_controls() {
    let (mut state, _rx) = microphone_state();
    let _ = state.apply_agent_snapshot(&snapshot(vec![yeti(false)]), &AssetResolver::new(), &[]);
    let record = microphone_record(&state)
        .expect("microphone record")
        .clone();

    let _ = state.apply_agent_snapshot(&snapshot(Vec::new()), &AssetResolver::new(), &[]);

    assert_eq!(state.microphone_for(&record), None);
}

#[test]
fn a_change_goes_to_the_agent() {
    let (mut state, mut rx) = microphone_state();

    let _ = state.change_microphone("yeti-1", MicrophoneChange::Muted(true));

    match rx.try_recv() {
        Ok(Command::SetMicrophone(request)) => {
            assert_eq!(request.id, "yeti-1");
            assert_eq!(request.change, MicrophoneChange::Muted(true));
        }
        _ => panic!("expected a SetMicrophone command"),
    }
}

#[test]
fn the_read_back_replaces_the_listed_microphone() {
    let (mut state, _rx) = microphone_state();
    let _ = state.apply_agent_snapshot(&snapshot(vec![yeti(false)]), &AssetResolver::new(), &[]);

    let changes = state.apply_microphone_written("yeti-1".into(), Some(Ok(yeti(true))));

    assert!(changes.contains(&StateEvent::MicrophoneChanged));
    assert_eq!(state.microphones(), &[yeti(true)]);
}

#[test]
fn a_refusal_shows_until_the_next_change() {
    let (mut state, _rx) = microphone_state();
    let _ = state.apply_agent_snapshot(&snapshot(vec![yeti(false)]), &AssetResolver::new(), &[]);

    let refused =
        state.apply_microphone_written("yeti-1".into(), Some(Err(MicrophoneError::Rejected(-1))));
    assert!(refused.contains(&StateEvent::MicrophoneChanged));
    assert_eq!(
        state.microphone_failure("yeti-1"),
        Some(MicrophoneError::Rejected(-1))
    );
    assert_eq!(state.microphone_failure("other"), None);

    let retried = state.change_microphone("yeti-1", MicrophoneChange::Muted(false));
    assert!(retried.contains(&StateEvent::MicrophoneChanged));
    assert_eq!(state.microphone_failure("yeti-1"), None);
}

#[test]
fn an_unreachable_agent_changes_nothing() {
    let (mut state, _rx) = microphone_state();
    let _ = state.apply_agent_snapshot(&snapshot(vec![yeti(false)]), &AssetResolver::new(), &[]);

    let changes = state.apply_microphone_written("yeti-1".into(), None);

    assert!(changes.is_empty());
    assert_eq!(state.microphones(), &[yeti(false)]);
    assert_eq!(state.microphone_failure("yeti-1"), None);
}
