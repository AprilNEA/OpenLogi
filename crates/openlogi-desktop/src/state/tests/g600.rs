//! G600 onboard DPI: a save or a stage switch writes the mouse through the
//! agent, one write at a time, saves nothing, and reads the mouse again
//! once it answers.

use super::fn_lock::drain;
use super::pointer_speed::install;
use super::*;
use crate::services::ipc::{Command, ReadG600Dpi, SetG600ActiveStage, SetG600ProfileDpi};
use crate::state::{G600DpiLoad, G600WriteStatus};
use openlogi_core::hid::DeviceRoute;
use openlogi_core::hid::g600::{
    ActiveSelection, DpiSettings, DpiStage, DpiState, DpiValue, G600_DRIVER_ID, G600Failure,
    OnboardProfile, ProfileDpi, StageAfterStore,
};

fn g600(online: bool) -> StandaloneDevice {
    StandaloneDevice {
        address: RawDeviceAddress {
            vendor_id: 0x046d,
            product_id: 0xc24a,
            usage_page: 0xff80,
            usage_id: 0x0080,
            identity: "serial:g600-test".into(),
        },
        display_name: "G600 Gaming Mouse".into(),
        manufacturer: Some("Logitech".into()),
        serial_number: None,
        unit_id: [0; 4],
        kind: DeviceKind::Mouse,
        online,
        capabilities: Some(Capabilities::default()),
        light_capabilities: None,
        driver_id: G600_DRIVER_ID.into(),
        registry_model_id: None,
    }
}

fn g600_state(online: bool) -> (AppState, tokio::sync::mpsc::UnboundedReceiver<Command>) {
    let resolver = AssetResolver::new();
    let (commands, receiver) = tokio::sync::mpsc::unbounded_channel();
    let state = AppState::new(Sources {
        standalone: &[g600(online)],
        ..Sources::in_memory(Config::ephemeral(), &resolver, commands)
    });
    (state, receiver)
}

fn stage(number: u8) -> DpiStage {
    DpiStage::from_number(number).expect("a stage")
}

fn profile(number: u8) -> OnboardProfile {
    OnboardProfile::from_number(number).expect("a profile")
}

fn settings() -> DpiSettings {
    let dpi = |value| Some(DpiValue::new(value).expect("a G600 DPI"));
    DpiSettings::new([dpi(400), dpi(1600), None, None], stage(2)).expect("valid settings")
}

fn reading(active_stage: u8) -> DpiState {
    let stored = ProfileDpi {
        stages: [Some(800), Some(1600), None, None],
        default_stage: Some(stage(1)),
        shift: None,
    };
    DpiState {
        active: ActiveSelection {
            profile: profile(1),
            dpi_stage: stage(active_stage),
        },
        profiles: [stored; 3],
    }
}

/// Answer the G600 read the state has queued, returning the route it was
/// asked on. Every other read is left unanswered.
fn answer_read(
    receiver: &mut tokio::sync::mpsc::UnboundedReceiver<Command>,
    answer: DpiState,
) -> DeviceRoute {
    let mut answered = None;
    for command in drain(receiver) {
        if let Command::ReadG600Dpi(ReadG600Dpi { route, reply }) = command {
            assert!(answered.is_none(), "one G600 read at a time");
            let _ = reply.send(Ok(answer));
            answered = Some(route);
        }
    }
    answered.expect("the state asked the G600 for its DPI")
}

fn current_load(cx: &gpui::TestAppContext) -> G600DpiLoad {
    cx.read(|cx| {
        AppState::try_read(cx)
            .expect("state installed")
            .current_g600_dpi_load()
    })
}

fn current_write(cx: &gpui::TestAppContext) -> Option<G600WriteStatus> {
    cx.read(|cx| {
        AppState::try_read(cx)
            .expect("state installed")
            .current_g600_write()
            .cloned()
    })
}

#[test]
fn a_save_writes_the_profile_once_until_the_mouse_answers() {
    let (mut state, mut receiver) = g600_state(true);
    drain(&mut receiver);
    assert!(state.current_g600_dpi_supported());
    let record = state.current_record().expect("the G600 is selected");
    let key = record.device_key();
    let route = record.route.clone().expect("a raw-HID route");

    let events = state.commit_g600_profile_dpi(profile(2), settings(), StageAfterStore::Keep);

    assert_eq!(events, [StateEvent::G600DpiChanged(key.clone())]);
    let commands = drain(&mut receiver);
    let [
        Command::SetG600ProfileDpi(SetG600ProfileDpi {
            route: written_route,
            profile: written_profile,
            settings: written,
            after,
            key: written_key,
        }),
    ] = commands.as_slice()
    else {
        panic!("expected one profile write and no config reload: the mouse keeps its DPI");
    };
    assert_eq!(
        (
            written_route,
            *written_profile,
            *written,
            *after,
            written_key
        ),
        (&route, profile(2), settings(), StageAfterStore::Keep, &key)
    );
    assert_eq!(
        state.current_g600_write(),
        Some(&G600WriteStatus::SavingProfile(profile(2)))
    );

    // Nothing else reaches the mouse while it stores the profile.
    assert!(state.commit_g600_active_stage(stage(1)).is_empty());
    assert!(
        state
            .commit_g600_profile_dpi(profile(3), settings(), StageAfterStore::Keep)
            .is_empty()
    );
    assert!(drain(&mut receiver).is_empty());
}

#[test]
fn a_stage_switch_goes_to_the_agent() {
    let (mut state, mut receiver) = g600_state(true);
    drain(&mut receiver);
    let key = state.current_record().expect("G600").device_key();

    let _ = state.commit_g600_active_stage(stage(2));

    let commands = drain(&mut receiver);
    let [
        Command::SetG600ActiveStage(SetG600ActiveStage {
            stage: written,
            key: written_key,
            ..
        }),
    ] = commands.as_slice()
    else {
        panic!("expected one stage switch");
    };
    assert_eq!((*written, written_key), (stage(2), &key));
    assert_eq!(
        state.current_g600_write(),
        Some(&G600WriteStatus::SelectingStage(stage(2)))
    );
}

#[test]
fn an_offline_g600_is_not_written() {
    let (mut state, mut receiver) = g600_state(false);
    drain(&mut receiver);
    assert!(state.current_g600_dpi_supported());
    assert_eq!(state.current_g600_route(), None);

    assert!(
        state
            .commit_g600_profile_dpi(profile(1), settings(), StageAfterStore::Keep)
            .is_empty()
    );
    assert!(state.commit_g600_active_stage(stage(1)).is_empty());

    assert!(drain(&mut receiver).is_empty());
}

#[test]
fn a_hidpp_mouse_has_no_g600_dpi() {
    let resolver = AssetResolver::new();
    let (commands, mut receiver) = tokio::sync::mpsc::unbounded_channel();
    let mut state = AppState::new(Sources {
        inventories: &[direct_inventory([0xa3, 0x93, 0xca, 0xe0])],
        ..Sources::in_memory(Config::ephemeral(), &resolver, commands)
    });
    drain(&mut receiver);

    assert!(!state.current_g600_dpi_supported());
    assert!(state.commit_g600_active_stage(stage(1)).is_empty());
    assert!(drain(&mut receiver).is_empty());
}

/// SmartShift is a HID++ feature, and a G600 answers no HID++: reading it
/// could only fail, so the state never asks.
#[gpui::test]
fn a_g600_is_never_asked_for_smartshift(cx: &mut gpui::TestAppContext) {
    let (state, mut receiver) = g600_state(true);
    install(state, cx);

    let commands = drain(&mut receiver);
    assert!(
        commands
            .iter()
            .any(|command| matches!(command, Command::ReadG600Dpi(_))),
        "the G600's own DPI is still read"
    );
    assert!(
        !commands
            .iter()
            .any(|command| matches!(command, Command::ReadSmartShift(_))),
        "a SmartShift read reached the agent"
    );
}

#[gpui::test]
fn every_answer_reads_the_mouse_again_and_a_failure_stays_shown(cx: &mut gpui::TestAppContext) {
    let (state, mut receiver) = g600_state(true);
    let key = state.current_record().expect("G600").device_key();
    install(state, cx);
    let route = answer_read(&mut receiver, reading(1));
    cx.run_until_parked();
    assert_eq!(current_load(cx), Load::Ready(Arc::new(reading(1))));

    let _ =
        cx.update(|cx| AppState::update(cx, |state, _| state.commit_g600_active_stage(stage(2))));
    drain(&mut receiver);
    let refused = WriteError::G600(G600Failure::StageDisabled {
        profile: profile(1),
        stage: stage(2),
    });
    let events = cx.update(|cx| {
        AppState::update(cx, |state, _| {
            state.apply_g600_dpi_written(&key, Err(refused.clone()))
        })
    });
    assert_eq!(events, [StateEvent::G600DpiChanged(key.clone())]);
    assert_eq!(
        current_write(cx),
        Some(G600WriteStatus::StageFailed(refused))
    );
    cx.run_until_parked();
    assert_eq!(answer_read(&mut receiver, reading(1)), route);
    cx.run_until_parked();

    // A failed write leaves the mouse free for the next one.
    let _ =
        cx.update(|cx| AppState::update(cx, |state, _| state.commit_g600_active_stage(stage(2))));
    assert_eq!(
        current_write(cx),
        Some(G600WriteStatus::SelectingStage(stage(2)))
    );
    drain(&mut receiver);
    let _ =
        cx.update(|cx| AppState::update(cx, |state, _| state.apply_g600_dpi_written(&key, Ok(()))));
    cx.run_until_parked();
    // Until the mouse answers, the reading on screen predates the write.
    assert_eq!(current_write(cx), Some(G600WriteStatus::Confirming));
    assert!(
        cx.update(|cx| AppState::update(cx, |state, _| state.commit_g600_active_stage(stage(1))))
            .is_empty(),
        "nothing is written on the reading from before the write"
    );
    assert_eq!(
        answer_read(&mut receiver, reading(2)),
        route,
        "a landed write reads the mouse again"
    );
    cx.run_until_parked();
    assert_eq!(current_load(cx), Load::Ready(Arc::new(reading(2))));
    assert_eq!(current_write(cx), None);
}

#[gpui::test]
fn a_reading_unchanged_by_a_landed_write_still_frees_the_mouse(cx: &mut gpui::TestAppContext) {
    let (state, mut receiver) = g600_state(true);
    let key = state.current_record().expect("G600").device_key();
    install(state, cx);
    answer_read(&mut receiver, reading(1));
    cx.run_until_parked();

    let _ = cx.update(|cx| {
        AppState::update(cx, |state, _| {
            state.commit_g600_profile_dpi(profile(2), settings(), StageAfterStore::Keep)
        })
    });
    drain(&mut receiver);
    let _ =
        cx.update(|cx| AppState::update(cx, |state, _| state.apply_g600_dpi_written(&key, Ok(()))));
    cx.run_until_parked();
    assert_eq!(current_write(cx), Some(G600WriteStatus::Confirming));

    answer_read(&mut receiver, reading(1));
    cx.run_until_parked();
    assert_eq!(current_write(cx), None);
}

#[gpui::test]
fn a_failed_profile_write_names_the_profile_it_was_for(cx: &mut gpui::TestAppContext) {
    let (state, mut receiver) = g600_state(true);
    let key = state.current_record().expect("G600").device_key();
    install(state, cx);
    answer_read(&mut receiver, reading(1));
    cx.run_until_parked();

    let _ = cx.update(|cx| {
        AppState::update(cx, |state, _| {
            state.commit_g600_profile_dpi(profile(3), settings(), StageAfterStore::Keep)
        })
    });
    drain(&mut receiver);
    let failed = WriteError::G600(G600Failure::ProfileMismatch {
        profile: profile(3),
    });
    let _ = cx.update(|cx| {
        AppState::update(cx, |state, _| {
            state.apply_g600_dpi_written(&key, Err(failed.clone()))
        })
    });

    assert_eq!(
        current_write(cx),
        Some(G600WriteStatus::ProfileFailed(profile(3), failed))
    );
    cx.run_until_parked();
    answer_read(&mut receiver, reading(1));
    cx.run_until_parked();
    assert!(
        matches!(current_write(cx), Some(G600WriteStatus::ProfileFailed(..))),
        "the reading after a failed write leaves its failure shown"
    );
}

/// The G600 commands the state sent since the last call, other than reads.
fn g600_writes(receiver: &mut tokio::sync::mpsc::UnboundedReceiver<Command>) -> Vec<Command> {
    drain(receiver)
        .into_iter()
        .filter(|command| {
            matches!(
                command,
                Command::SetG600ProfileDpi(_) | Command::SetG600ActiveStage(_)
            )
        })
        .collect()
}

/// The stage the mouse uses after a save travels with the profile write,
/// so a save is one write, then the reading after it: no stage switch of
/// its own follows.
#[gpui::test]
fn a_save_carries_the_stage_to_use_and_is_followed_only_by_a_reading(
    cx: &mut gpui::TestAppContext,
) {
    let (state, mut receiver) = g600_state(true);
    let key = state.current_record().expect("G600").device_key();
    install(state, cx);
    answer_read(&mut receiver, reading(1));
    cx.run_until_parked();

    let _ = cx.update(|cx| {
        AppState::update(cx, |state, _| {
            state.commit_g600_profile_dpi(profile(1), settings(), StageAfterStore::Use(stage(2)))
        })
    });
    let commands = g600_writes(&mut receiver);
    let [Command::SetG600ProfileDpi(SetG600ProfileDpi { after, .. })] = commands.as_slice() else {
        panic!("expected one profile write, got {} writes", commands.len());
    };
    assert_eq!(*after, StageAfterStore::Use(stage(2)));
    assert_eq!(
        current_write(cx),
        Some(G600WriteStatus::SavingProfile(profile(1)))
    );

    let _ =
        cx.update(|cx| AppState::update(cx, |state, _| state.apply_g600_dpi_written(&key, Ok(()))));
    cx.run_until_parked();
    assert_eq!(current_write(cx), Some(G600WriteStatus::Confirming));
    let commands = drain(&mut receiver);
    assert!(
        !commands
            .iter()
            .any(|command| matches!(command, Command::SetG600ActiveStage(_))),
        "no stage switch follows a save"
    );
    assert!(
        commands
            .iter()
            .any(|command| matches!(command, Command::ReadG600Dpi(_))),
        "the mouse is read once the save lands"
    );
    for command in commands {
        if let Command::ReadG600Dpi(ReadG600Dpi { reply, .. }) = command {
            let _ = reply.send(Ok(reading(2)));
        }
    }
    cx.run_until_parked();
    assert_eq!(current_write(cx), None);
    assert!(g600_writes(&mut receiver).is_empty());
}

#[gpui::test]
fn a_save_that_did_not_land_is_shown_against_its_profile(cx: &mut gpui::TestAppContext) {
    let (state, mut receiver) = g600_state(true);
    let key = state.current_record().expect("G600").device_key();
    install(state, cx);
    answer_read(&mut receiver, reading(1));
    cx.run_until_parked();

    let _ = cx.update(|cx| {
        AppState::update(cx, |state, _| {
            state.commit_g600_profile_dpi(profile(1), settings(), StageAfterStore::Use(stage(2)))
        })
    });
    drain(&mut receiver);
    let failed = WriteError::G600(G600Failure::ProfileNotInUse {
        profile: profile(1),
        active: profile(2),
    });
    let _ = cx.update(|cx| {
        AppState::update(cx, |state, _| {
            state.apply_g600_dpi_written(&key, Err(failed.clone()))
        })
    });
    cx.run_until_parked();

    assert!(g600_writes(&mut receiver).is_empty());
    assert_eq!(
        current_write(cx),
        Some(G600WriteStatus::ProfileFailed(profile(1), failed))
    );
}
