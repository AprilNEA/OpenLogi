//! Report rate: a pick persists the setting and writes the mouse through the
//! agent, but only for a rate the mouse's last reading lists.

use super::fn_lock::drain;
use super::*;
use crate::services::ipc::{Command, SetReportRate};
use crate::state::ReportRateLoad;
use crate::state::report_rate::pick_target;
use openlogi_core::hid::{ReportRate, ReportRateInfo};

fn rates(hz: &[u16]) -> Vec<ReportRate> {
    hz.iter().copied().map(ReportRate::new).collect()
}

fn reading(current: u16, supported: &[u16]) -> ReportRateLoad {
    Load::Ready(Arc::new(ReportRateInfo {
        current: ReportRate::new(current),
        supported: rates(supported),
    }))
}

/// The known mouse, with or without a report-rate control, and the channel
/// its writes go out on.
fn mouse_state(report_rate: bool) -> (AppState, tokio::sync::mpsc::UnboundedReceiver<Command>) {
    let resolver = AssetResolver::new();
    let (commands, receiver) = tokio::sync::mpsc::unbounded_channel();
    let mut inventory = direct_inventory([0xa3, 0x93, 0xca, 0xe0]);
    inventory.paired[0]
        .capabilities
        .as_mut()
        .expect("the fixture mouse has capabilities")
        .report_rate = report_rate;
    let state = AppState::new(Sources {
        inventories: &[inventory],
        ..Sources::in_memory(Config::ephemeral(), &resolver, commands)
    });
    (state, receiver)
}

#[test]
fn only_a_rate_the_last_reading_lists_is_a_pick_target() {
    let listed = reading(1000, &[125, 250, 500, 1000]);
    assert_eq!(
        pick_target(&listed, ReportRate::new(500)).map(|info| info.current),
        Some(ReportRate::new(1000))
    );
    // A rate the current connection does not offer — 2 kHz on a mouse whose
    // receiver tops out at 1 kHz.
    assert!(pick_target(&listed, ReportRate::new(2000)).is_none());
    for unread in [
        Load::Unknown,
        Load::Loading,
        Load::Failed("unreachable".to_string()),
        Load::Unsupported("no report-rate feature".to_string()),
    ] {
        assert!(pick_target(&unread, ReportRate::new(1000)).is_none());
    }
}

#[test]
fn a_pick_persists_the_rate_and_writes_the_mouse() {
    let (mut state, mut receiver) = mouse_state(true);
    drain(&mut receiver);
    let record = state.current_record().expect("the mouse is selected");
    let key = record.device_key();
    let route = record.route.clone().expect("a direct route");

    let events = state.write_report_rate(ReportRate::new(500), rates(&[125, 500, 1000]));

    assert_eq!(events, [StateEvent::ReportRateChanged(key.clone())]);
    assert_eq!(
        state.config.report_rate(KNOWN_MOUSE_KEY),
        Some(ReportRate::new(500))
    );
    let commands = drain(&mut receiver);
    let [
        Command::ReloadConfig(_),
        Command::SetReportRate(SetReportRate {
            route: written_route,
            rate,
            key: written_key,
        }),
    ] = commands.as_slice()
    else {
        panic!("expected a config reload followed by one report-rate write");
    };
    assert_eq!(*written_route, route);
    assert_eq!(*rate, ReportRate::new(500));
    assert_eq!(*written_key, key);
}

#[test]
fn a_pick_is_inert_on_a_mouse_without_the_control() {
    let (mut state, mut receiver) = mouse_state(false);
    drain(&mut receiver);
    assert!(!state.current_report_rate_supported());

    let _ = state.commit_report_rate(ReportRate::new(1000));

    assert_eq!(state.config.report_rate(KNOWN_MOUSE_KEY), None);
    assert!(drain(&mut receiver).is_empty());
}

#[test]
fn a_pick_before_the_mouse_was_read_writes_nothing() {
    let (mut state, mut receiver) = mouse_state(true);
    drain(&mut receiver);
    assert!(state.current_report_rate_supported());
    assert_eq!(state.current_report_rate_load(), Load::Unknown);

    let _ = state.commit_report_rate(ReportRate::new(1000));

    // Without a reading there is no list to check the rate against, and the
    // panel offers no pills to click.
    assert_eq!(state.config.report_rate(KNOWN_MOUSE_KEY), None);
    assert!(drain(&mut receiver).is_empty());
}

#[test]
fn the_mouses_answer_repaints_the_pointer_card_either_way() {
    let (mut state, _receiver) = mouse_state(true);
    let key = state.current_record().expect("mouse").device_key();
    let read_back = ReportRateInfo {
        current: ReportRate::new(500),
        supported: rates(&[125, 500, 1000]),
    };

    assert_eq!(
        state.apply_report_rate_written(&key, Ok(read_back)),
        [StateEvent::ReportRateChanged(key.clone())]
    );
    assert_eq!(
        state.apply_report_rate_written(&key, Err(WriteError::AgentUnavailable)),
        [StateEvent::ReportRateChanged(key)]
    );
}
