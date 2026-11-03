//! Report rate: a pick persists the setting and writes the mouse through the
//! agent, but only for a rate the mouse's last reading lists.

use super::fn_lock::drain;
use super::*;
use crate::services::ipc::{Command, ReadReportRate, SetReportRate};
use crate::state::ReportRateLoad;
use crate::state::events::StateEvents;
use crate::state::report_rate::pick_target;
use openlogi_core::hid::{DeviceRoute, ReportRate, ReportRateInfo};

fn rates(hz: &[u16]) -> Vec<ReportRate> {
    hz.iter().copied().map(ReportRate::new).collect()
}

fn reading(current: u16, supported: &[u16]) -> ReportRateLoad {
    Load::Ready(Arc::new(ReportRateInfo {
        current: ReportRate::new(current),
        supported: rates(supported),
    }))
}

fn info(current: u16, supported: &[u16]) -> ReportRateInfo {
    ReportRateInfo {
        current: ReportRate::new(current),
        supported: rates(supported),
    }
}

/// The known mouse on its cable, with or without a report-rate control.
fn mouse_inventory(report_rate: bool) -> DeviceInventory {
    let mut inventory = direct_inventory([0xa3, 0x93, 0xca, 0xe0]);
    inventory.paired[0]
        .capabilities
        .as_mut()
        .expect("the fixture mouse has capabilities")
        .report_rate = report_rate;
    inventory
}

/// The same mouse moved onto a Bolt receiver: its unit id, and so its key,
/// stays; its route does not.
fn on_receiver(mut inventory: DeviceInventory) -> DeviceInventory {
    inventory.receiver = ReceiverInfo {
        name: "Bolt Receiver".to_string(),
        vendor_id: 0x046d,
        product_id: 0xc548,
        unique_id: Some("82839805".to_string()),
    };
    inventory.paired[0].slot = 1;
    inventory
}

/// The known mouse, with or without a report-rate control, and the channel
/// its writes go out on.
fn mouse_state(report_rate: bool) -> (AppState, tokio::sync::mpsc::UnboundedReceiver<Command>) {
    let resolver = AssetResolver::new();
    let (commands, receiver) = tokio::sync::mpsc::unbounded_channel();
    let state = AppState::new(Sources {
        inventories: &[mouse_inventory(report_rate)],
        ..Sources::in_memory(Config::ephemeral(), &resolver, commands)
    });
    (state, receiver)
}

/// Move the fixture mouse onto its receiver and return its new route, after
/// checking the move kept its key.
fn move_onto_receiver(state: &mut AppState, key: &DeviceKey) -> DeviceRoute {
    let resolver = AssetResolver::new();
    let _ = state.refresh_inventories(&[on_receiver(mouse_inventory(true))], &[], &resolver, &[]);
    let record = state.current_record().expect("the mouse stays selected");
    assert_eq!(record.device_key(), *key, "the move keeps the mouse's key");
    record.route.clone().expect("a receiver route")
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

/// Answer the report-rate read the state has queued, returning the route it
/// was asked on. Every other read is left unanswered.
fn answer_rate_read(
    receiver: &mut tokio::sync::mpsc::UnboundedReceiver<Command>,
    reading: &ReportRateInfo,
) -> DeviceRoute {
    let mut answered = None;
    for command in drain(receiver) {
        if let Command::ReadReportRate(ReadReportRate { route, reply }) = command {
            assert!(answered.is_none(), "one report-rate read at a time");
            let _ = reply.send(Ok(reading.clone()));
            answered = Some(route);
        }
    }
    answered.expect("the state asked the mouse for its report rate")
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

/// Hand the mouse's answer to the state and return what it announced.
fn written(
    cx: &mut gpui::TestAppContext,
    key: &DeviceKey,
    route: &DeviceRoute,
    result: Result<ReportRateInfo, WriteError>,
) -> StateEvents {
    cx.update(|cx| {
        AppState::update(cx, |state, _| {
            state.apply_report_rate_written(key, route, result)
        })
    })
}

fn current_load(cx: &gpui::TestAppContext) -> ReportRateLoad {
    cx.read(|cx| {
        AppState::try_read(cx)
            .expect("state installed")
            .current_report_rate_load()
    })
}

#[gpui::test]
fn the_mouses_answer_repaints_the_pointer_card_either_way(cx: &mut gpui::TestAppContext) {
    let (state, mut receiver) = mouse_state(true);
    let key = state.current_record().expect("mouse").device_key();
    install(state, cx);
    let route = answer_rate_read(&mut receiver, &info(1000, &[125, 500, 1000]));
    cx.run_until_parked();

    let read_back = info(500, &[125, 500, 1000]);
    assert_eq!(
        written(cx, &key, &route, Ok(read_back.clone())),
        [StateEvent::ReportRateChanged(key.clone())]
    );
    assert_eq!(current_load(cx), Load::Ready(Arc::new(read_back)));
    assert_eq!(
        written(cx, &key, &route, Err(WriteError::AgentUnavailable)),
        [StateEvent::ReportRateChanged(key)]
    );
}

#[test]
fn an_answer_no_read_is_on_is_dropped() {
    let (mut state, _receiver) = mouse_state(true);
    let record = state.current_record().expect("mouse");
    let key = record.device_key();
    let route = record.route.clone().expect("a direct route");

    // Nothing has subscribed to this mouse's report rate, so nothing shows
    // an answer for it.
    assert!(
        state
            .apply_report_rate_written(&key, &route, Ok(info(500, &[125, 500])))
            .is_empty()
    );
    assert!(
        state
            .apply_report_rate_written(&key, &route, Err(WriteError::AgentUnavailable))
            .is_empty()
    );
}

/// The rate cache is keyed by the mouse, not its connection. A write the
/// mouse answered over its cable after it had moved onto its receiver must
/// leave the receiver connection's own reading on screen.
#[gpui::test]
fn a_write_answered_after_a_route_change_keeps_the_new_connections_rates(
    cx: &mut gpui::TestAppContext,
) {
    let (state, mut receiver) = mouse_state(true);
    let key = state.current_record().expect("mouse").device_key();
    install(state, cx);
    let cable_reading = info(1000, &[125, 250, 500, 1000]);
    let cable = answer_rate_read(&mut receiver, &cable_reading);
    cx.run_until_parked();
    assert_eq!(current_load(cx), Load::Ready(Arc::new(cable_reading)));

    cx.update(|cx| AppState::apply(cx, |state| state.commit_report_rate(ReportRate::new(500))));
    assert!(
        drain(&mut receiver).iter().any(
            |command| matches!(command, Command::SetReportRate(write) if write.route == cable)
        ),
        "the pick is written over the cable"
    );

    // The mouse moves before the write is answered; the receiver connection
    // offers its own rates, read over the receiver.
    let moved = cx.update(|cx| {
        let moved = AppState::update(cx, |state, _| move_onto_receiver(state, &key));
        AppState::load_current_device_reads(cx);
        moved
    });
    assert_ne!(moved, cable);
    cx.run_until_parked();
    let receiver_reading = info(1000, &[125, 250, 500, 1000, 2000, 4000]);
    assert_eq!(answer_rate_read(&mut receiver, &receiver_reading), moved);
    cx.run_until_parked();
    assert_eq!(
        current_load(cx),
        Load::Ready(Arc::new(receiver_reading.clone()))
    );

    // The cable write's answer — or its refusal — arrives late, and
    // concerns no connection the mouse is on.
    let cable_answer = info(500, &[125, 250, 500, 1000]);
    assert!(written(cx, &key, &cable, Ok(cable_answer)).is_empty());
    assert!(written(cx, &key, &cable, Err(WriteError::AgentUnavailable)).is_empty());
    cx.run_until_parked();
    assert_eq!(current_load(cx), Load::Ready(Arc::new(receiver_reading)));
    assert!(
        !drain(&mut receiver)
            .iter()
            .any(|command| matches!(command, Command::ReadReportRate(_))),
        "a late refusal does not re-read the mouse"
    );

    // An answer over the receiver still lands.
    let receiver_answer = info(2000, &[125, 250, 500, 1000, 2000, 4000]);
    assert_eq!(
        written(cx, &key, &moved, Ok(receiver_answer.clone())),
        [StateEvent::ReportRateChanged(key)]
    );
    assert_eq!(current_load(cx), Load::Ready(Arc::new(receiver_answer)));
}
