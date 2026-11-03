//! SmartShift write feedback against stale reads, and the re-read after the
//! mouse changes its wheel mode itself, and the config floor on every write.

use openlogi_core::config::{SMARTSHIFT_AUTO_DISENGAGE_DEFAULT, SmartShift};

use super::fn_lock::drain;
use super::*;
use crate::services::ipc::{Command, ReadSmartShift, SetSmartShift};
use crate::state::SmartShiftLoad;

#[test]
fn smartshift_write_feedback_requires_the_written_value() {
    let expected = SmartShiftStatus {
        mode: SmartShiftMode::Ratchet,
        auto_disengage: SmartShiftAutoDisengage::Threshold(SmartShiftThreshold::from_rounded(12.0)),
        tunable_torque: None,
    };
    assert_eq!(smartshift_write_outcome(expected, None), None);
    assert_eq!(
        smartshift_write_outcome(expected, Some(&Load::Ready(Arc::new(expected)))),
        Some(ConfirmationOutcome::Confirmed)
    );
    assert_eq!(
        smartshift_write_outcome(
            expected,
            Some(&Load::Ready(Arc::new(SmartShiftStatus {
                auto_disengage: SmartShiftAutoDisengage::Threshold(
                    SmartShiftThreshold::from_rounded(13.0),
                ),
                ..expected
            }))),
        ),
        Some(ConfirmationOutcome::Failed)
    );
    assert_eq!(
        smartshift_write_outcome(
            expected,
            Some(&Load::<Arc<SmartShiftStatus>>::Failed(
                "timeout".to_string(),
            ))
        ),
        Some(ConfirmationOutcome::Failed)
    );
}

#[test]
fn stale_smartshift_reads_do_not_resolve_newer_writes() {
    let expected = SmartShiftStatus {
        mode: SmartShiftMode::Ratchet,
        auto_disengage: SmartShiftAutoDisengage::Threshold(SmartShiftThreshold::from_rounded(12.0)),
        tunable_torque: None,
    };
    let mut write = SmartShiftDeviceState::default();
    write.queue(expected, 2);

    assert!(
        !smartshift_read_is_current(Some(2), Some(&write)),
        "a tagged read cannot land before its confirmation request starts"
    );
    assert!(!smartshift_read_is_current(None, Some(&write)));
    assert_eq!(write.begin_confirmation(), Some(2));
    assert!(smartshift_read_is_current(Some(2), Some(&write)));
    assert!(!smartshift_read_is_current(Some(1), Some(&write)));
    assert!(!smartshift_read_is_current(None, Some(&write)));
    write.reset();
    assert!(!smartshift_read_is_current(Some(2), Some(&write)));
    assert!(smartshift_read_is_current(None, None));
}

/// Firmware can report a threshold below the configuration floor (an MX Master 3
/// reports `4`, #2079), and the free-spin pill writes the read threshold back.
/// The write must not save a config the next launch refuses to parse: the
/// device and the config both get the default instead. A threshold the config
/// accepts, and permanent ratchet, pass through unchanged.
#[test]
fn a_write_saves_only_a_threshold_the_config_loader_accepts() {
    let threshold =
        |raw: f32| SmartShiftAutoDisengage::Threshold(SmartShiftThreshold::from_rounded(raw));
    let cases = [
        (
            threshold(4.0),
            SmartShiftAutoDisengage::Threshold(SMARTSHIFT_AUTO_DISENGAGE_DEFAULT),
        ),
        (threshold(8.0), threshold(8.0)),
        (
            SmartShiftAutoDisengage::Permanent,
            SmartShiftAutoDisengage::Permanent,
        ),
    ];
    for (reported, saved) in cases {
        let resolver = AssetResolver::new();
        let (commands, mut receiver) = tokio::sync::mpsc::unbounded_channel();
        let mut state = AppState::new(Sources {
            inventories: &[direct_inventory([0xa3, 0x93, 0xca, 0xe0])],
            ..Sources::in_memory(Config::ephemeral(), &resolver, commands)
        });
        drain(&mut receiver);
        let free_spin = SmartShiftStatus {
            mode: SmartShiftMode::Free,
            auto_disengage: reported,
            tunable_torque: None,
        };
        let expected = SmartShiftStatus {
            auto_disengage: saved,
            ..free_spin
        };

        let _ = state.commit_smartshift(free_spin);

        let persisted = state
            .config
            .smartshift(KNOWN_MOUSE_KEY)
            .expect("the write is saved for the known mouse");
        assert!(
            SmartShift::accepts_auto_disengage(persisted.auto_disengage),
            "{reported:?} saved a threshold the config loader rejects"
        );
        assert_eq!(persisted, SmartShift::from(expected));
        let written: Vec<SmartShiftStatus> = drain(&mut receiver)
            .into_iter()
            .filter_map(|command| match command {
                Command::SetSmartShift(SetSmartShift { status, .. }) => Some(status),
                _ => None,
            })
            .collect();
        assert_eq!(written, [expected], "the device gets what is saved");
    }
}

fn status(mode: SmartShiftMode) -> SmartShiftStatus {
    SmartShiftStatus {
        mode,
        auto_disengage: SmartShiftAutoDisengage::Threshold(SmartShiftThreshold::from_rounded(12.0)),
        tunable_torque: None,
    }
}

/// The SmartShift reads the state has queued. Every other read is dropped.
fn smartshift_reads(
    receiver: &mut tokio::sync::mpsc::UnboundedReceiver<Command>,
) -> Vec<ReadSmartShift> {
    drain(receiver)
        .into_iter()
        .filter_map(|command| match command {
            Command::ReadSmartShift(read) => Some(read),
            _ => None,
        })
        .collect()
}

/// Answer the SmartShift read the state has queued, returning the route it
/// was asked on. Every other read is left unanswered.
fn answer_smartshift_read(
    receiver: &mut tokio::sync::mpsc::UnboundedReceiver<Command>,
    status: SmartShiftStatus,
) -> DeviceRoute {
    let mut reads = smartshift_reads(receiver);
    assert!(reads.len() <= 1, "one SmartShift read at a time");
    let ReadSmartShift { route, reply } = reads
        .pop()
        .expect("the state asked the mouse for its SmartShift mode");
    let _ = reply.send(Ok(status));
    route
}

/// Install `state` with live swr-backed reads, as the app does, and have it
/// read the selected mouse.
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

fn smartshift_load(cx: &gpui::TestAppContext, key: &DeviceKey) -> SmartShiftLoad {
    cx.read(|cx| {
        AppState::try_read(cx)
            .expect("state installed")
            .smartshift_load_for(key)
    })
}

/// The mouse's mode-shift button switches the wheel itself, and the mouse
/// reports it with a `0x2121` ratchet switch, which the agent counts in
/// `settings_revision`. The SmartShift card re-reads instead of showing the
/// old mode.
#[gpui::test]
fn a_mode_change_the_mouse_reports_re_reads_smartshift(cx: &mut gpui::TestAppContext) {
    let unit_id = [0xa3, 0x93, 0xca, 0xe0];
    let resolver = AssetResolver::new();
    let (commands, mut receiver) = tokio::sync::mpsc::unbounded_channel();
    let state = AppState::new(Sources {
        inventories: &[direct_inventory(unit_id)],
        ..Sources::in_memory(Config::ephemeral(), &resolver, commands)
    });
    let key = state.current_record().expect("mouse").device_key();
    install(state, cx);
    let ratchet = status(SmartShiftMode::Ratchet);
    let route = answer_smartshift_read(&mut receiver, ratchet);
    cx.run_until_parked();
    assert_eq!(smartshift_load(cx, &key), Load::Ready(Arc::new(ratchet)));

    cx.update(|cx| {
        AppState::update(cx, |state, _| {
            let mut switched = direct_inventory(unit_id);
            switched.paired[0].settings_revision = 1;
            let _ = state.refresh_inventories(&[switched], &[], &resolver, &[]);
        });
    });
    cx.run_until_parked();

    let freespin = status(SmartShiftMode::Free);
    assert_eq!(answer_smartshift_read(&mut receiver, freespin), route);
    cx.run_until_parked();
    assert_eq!(smartshift_load(cx, &key), Load::Ready(Arc::new(freespin)));
}

/// The mouse can change its mode while a SmartShift read is still out. That
/// read may carry the old mode, so the change starts a new read instead of
/// waiting for it, and a late answer to the old read does not win.
#[gpui::test]
fn a_mode_change_during_a_read_starts_a_new_read(cx: &mut gpui::TestAppContext) {
    let unit_id = [0xa3, 0x93, 0xca, 0xe0];
    let resolver = AssetResolver::new();
    let (commands, mut receiver) = tokio::sync::mpsc::unbounded_channel();
    let state = AppState::new(Sources {
        inventories: &[direct_inventory(unit_id)],
        ..Sources::in_memory(Config::ephemeral(), &resolver, commands)
    });
    let key = state.current_record().expect("mouse").device_key();
    install(state, cx);
    let [first_read] = <[ReadSmartShift; 1]>::try_from(smartshift_reads(&mut receiver))
        .unwrap_or_else(|_| panic!("the state asked the mouse once"));

    cx.update(|cx| {
        AppState::update(cx, |state, _| {
            let mut switched = direct_inventory(unit_id);
            switched.paired[0].settings_revision = 1;
            let _ = state.refresh_inventories(&[switched], &[], &resolver, &[]);
        });
    });
    cx.run_until_parked();

    let freespin = status(SmartShiftMode::Free);
    assert_eq!(
        answer_smartshift_read(&mut receiver, freespin),
        first_read.route
    );
    cx.run_until_parked();
    let _ = first_read.reply.send(Ok(status(SmartShiftMode::Ratchet)));
    cx.run_until_parked();
    assert_eq!(smartshift_load(cx, &key), Load::Ready(Arc::new(freespin)));
}
