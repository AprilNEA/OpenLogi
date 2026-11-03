//! `resolve_dpi`'s precedence — a live read beats the persisted config value,
//! which beats the hardcoded default — plus the `AppState`-level wiring that
//! feeds it the configured value, and the DPI write: the editor saves a value
//! only after the mouse took it.

use super::fn_lock::drain;
use super::*;
use crate::services::ipc::{Command, ReadDpi, SetDpi};
use crate::state::{DEFAULT_DPI, DpiLoad};

fn ready(dpi: Dpi) -> DpiLoad {
    Load::Ready(std::sync::Arc::new(DpiInfo {
        current: dpi,
        capabilities: DpiCapabilities::new(vec![dpi.into_inner()]).expect("valid DPI list"),
    }))
}

#[test]
fn a_ready_live_read_wins_over_a_different_configured_value() {
    assert_eq!(
        resolve_dpi(Some(&ready(Dpi::new(1200))), Some(Dpi::new(400))),
        Dpi::new(1200),
        "the sensor's real value must win — firmware can clamp or reject a \
         requested DPI, so the live read is the only source of truth once \
         it lands"
    );
}

#[test]
fn the_configured_value_is_used_while_the_live_read_is_pending() {
    for pending in [Load::Unknown, Load::Loading] {
        assert_eq!(
            resolve_dpi(Some(&pending), Some(Dpi::new(400))),
            Dpi::new(400)
        );
    }
    assert_eq!(resolve_dpi(None, Some(Dpi::new(400))), Dpi::new(400));
}

#[test]
fn the_configured_value_is_used_when_the_live_read_failed() {
    let failed = Load::Failed("device unreachable".to_string());
    assert_eq!(
        resolve_dpi(Some(&failed), Some(Dpi::new(400))),
        Dpi::new(400)
    );
}

#[test]
fn the_hardcoded_default_is_used_with_neither_a_live_read_nor_a_configured_value() {
    assert_eq!(resolve_dpi(None, None), DEFAULT_DPI);
    assert_eq!(resolve_dpi(Some(&Load::Unknown), None), DEFAULT_DPI);
}

#[test]
fn the_persisted_config_value_is_shown_before_the_live_read_resolves() {
    let mut state = state_with_a_known_mouse();
    state
        .config
        .edit(|config| config.set_dpi(KNOWN_MOUSE_KEY, Dpi::new(400)));

    assert_eq!(
        state.dpi_for_current(),
        Dpi::new(400),
        "the value the user configured must be shown while the agent's live \
         DPI read is still pending, not an unrelated hardcoded default"
    );
}

#[test]
fn the_hardcoded_default_is_shown_with_neither_a_live_read_nor_a_configured_value() {
    let state = state_with_a_known_mouse();

    assert_eq!(state.dpi_for_current(), DEFAULT_DPI);
}

/// The DPI saved for the known mouse before each write test.
const SAVED: Dpi = Dpi::new(800);

/// The known mouse with [`SAVED`] in its config, and the commands it sends.
fn mouse_with_a_saved_dpi() -> (AppState, tokio::sync::mpsc::UnboundedReceiver<Command>) {
    let resolver = AssetResolver::new();
    let (commands, mut receiver) = tokio::sync::mpsc::unbounded_channel();
    let mut config = Config::ephemeral();
    config.set_dpi(KNOWN_MOUSE_KEY, SAVED);
    let state = AppState::new(Sources {
        inventories: &[direct_inventory([0xa3, 0x93, 0xca, 0xe0])],
        ..Sources::in_memory(config, &resolver, commands)
    });
    drain(&mut receiver);
    (state, receiver)
}

/// Commit `dpi` on the selected mouse and return the number of the one DPI
/// write it sends. No config reload is sent: the config is not touched yet.
/// Reads of other settings may still be in flight and are dropped.
fn commit(
    state: &mut AppState,
    receiver: &mut tokio::sync::mpsc::UnboundedReceiver<Command>,
    dpi: Dpi,
) -> u64 {
    let record = state.current_record().expect("the mouse is selected");
    let key = record.device_key();
    let route = record.route.clone().expect("a direct route");

    let events = state.commit_dpi(dpi);

    assert_eq!(events, [StateEvent::DpiChanged(key.clone())]);
    let commands: Vec<Command> = drain(receiver)
        .into_iter()
        .filter(|command| matches!(command, Command::SetDpi(_) | Command::ReloadConfig(_)))
        .collect();
    let [
        Command::SetDpi(SetDpi {
            route: written_route,
            dpi: written,
            key: written_key,
            write,
        }),
    ] = commands.as_slice()
    else {
        panic!("expected one DPI write and no config reload");
    };
    assert_eq!(*written_route, route);
    assert_eq!(*written, dpi);
    assert_eq!(*written_key, key);
    *write
}

fn mouse_key(state: &AppState) -> DeviceKey {
    state
        .current_record()
        .expect("the mouse is selected")
        .device_key()
}

#[test]
fn a_commit_writes_the_mouse_and_saves_nothing_before_it_answers() {
    let (mut state, mut receiver) = mouse_with_a_saved_dpi();

    commit(&mut state, &mut receiver, Dpi::new(1600));

    assert_eq!(state.dpi(), Dpi::new(1600), "the editor shows the request");
    assert_eq!(state.config.dpi(KNOWN_MOUSE_KEY), Some(SAVED));
}

#[test]
fn a_dpi_the_mouse_took_is_saved_and_the_agent_reloads() {
    let (mut state, mut receiver) = mouse_with_a_saved_dpi();
    let key = mouse_key(&state);
    let write = commit(&mut state, &mut receiver, Dpi::new(1600));

    let events = state.apply_dpi_written(&key, write, Dpi::new(1600), Ok(()));

    assert_eq!(events, [StateEvent::DpiChanged(key)]);
    assert_eq!(state.config.dpi(KNOWN_MOUSE_KEY), Some(Dpi::new(1600)));
    assert_eq!(state.dpi(), Dpi::new(1600));
    assert!(matches!(
        drain(&mut receiver).as_slice(),
        [Command::ReloadConfig(_)]
    ));
}

/// The audit case: the mouse acknowledged the write but reads back another
/// DPI. Saving the request would re-send it at every reconnect.
#[test]
fn a_dpi_the_mouse_did_not_apply_is_not_saved() {
    let (mut state, mut receiver) = mouse_with_a_saved_dpi();
    let key = mouse_key(&state);
    let requested = Dpi::new(1600);
    let actual = Dpi::new(1200);
    let write = commit(&mut state, &mut receiver, requested);

    let _ = state.apply_dpi_written(
        &key,
        write,
        requested,
        Err(WriteError::DpiNotApplied { requested, actual }),
    );

    assert_ne!(state.config.dpi(KNOWN_MOUSE_KEY), Some(requested));
    assert_eq!(state.config.dpi(KNOWN_MOUSE_KEY), Some(SAVED));
    assert_eq!(
        state.dpi(),
        actual,
        "the editor shows what the mouse runs at"
    );
    assert!(drain(&mut receiver).is_empty(), "no config reload");
}

#[test]
fn a_dpi_the_mouse_refused_is_not_saved() {
    let (mut state, mut receiver) = mouse_with_a_saved_dpi();
    let key = mouse_key(&state);
    let requested = Dpi::new(1650);
    let write = commit(&mut state, &mut receiver, requested);

    let _ = state.apply_dpi_written(
        &key,
        write,
        requested,
        Err(WriteError::DpiUnsupported { requested }),
    );

    assert_eq!(state.config.dpi(KNOWN_MOUSE_KEY), Some(SAVED));
    assert_eq!(
        state.dpi(),
        SAVED,
        "nothing was written, so the mouse keeps its DPI"
    );
    assert!(drain(&mut receiver).is_empty());
}

#[test]
fn a_write_that_did_not_reach_the_mouse_is_not_saved() {
    let (mut state, mut receiver) = mouse_with_a_saved_dpi();
    let key = mouse_key(&state);
    let write = commit(&mut state, &mut receiver, Dpi::new(1600));

    let _ = state.apply_dpi_written(
        &key,
        write,
        Dpi::new(1600),
        Err(WriteError::DeviceUnreachable { index: 0xff }),
    );

    assert_eq!(state.config.dpi(KNOWN_MOUSE_KEY), Some(SAVED));
    assert_eq!(state.dpi(), SAVED);
}

/// Writes are answered in order. An older answer records what the mouse did
/// but leaves the newer request on screen.
#[test]
fn an_older_answer_leaves_the_newer_request_on_screen() {
    let (mut state, mut receiver) = mouse_with_a_saved_dpi();
    let key = mouse_key(&state);
    let first = commit(&mut state, &mut receiver, Dpi::new(1600));
    let second = commit(&mut state, &mut receiver, Dpi::new(2400));
    assert_ne!(first, second);

    let _ = state.apply_dpi_written(&key, first, Dpi::new(1600), Ok(()));

    assert_eq!(state.dpi(), Dpi::new(2400));
    assert_eq!(state.config.dpi(KNOWN_MOUSE_KEY), Some(Dpi::new(1600)));

    let _ = state.apply_dpi_written(
        &key,
        second,
        Dpi::new(2400),
        Err(WriteError::DpiUnsupported {
            requested: Dpi::new(2400),
        }),
    );

    assert_eq!(
        state.config.dpi(KNOWN_MOUSE_KEY),
        Some(Dpi::new(1600)),
        "the mouse took the first write and refused the second"
    );
    assert_eq!(state.dpi(), Dpi::new(1600));
}

/// Answer the DPI read the state has queued with `current` out of
/// `supported`, returning the route it was asked on. Every other read is
/// left unanswered.
fn answer_dpi_read(
    receiver: &mut tokio::sync::mpsc::UnboundedReceiver<Command>,
    current: Dpi,
    supported: &[u16],
) -> DeviceRoute {
    let mut answered = None;
    for command in drain(receiver) {
        if let Command::ReadDpi(ReadDpi { route, reply }) = command {
            assert!(answered.is_none(), "one DPI read at a time");
            let _ = reply.send(Ok(DpiInfo {
                current,
                capabilities: DpiCapabilities::new(supported.to_vec()).expect("valid DPI list"),
            }));
            answered = Some(route);
        }
    }
    answered.expect("the state asked the mouse for its DPI")
}

/// Install `state` with live swr-backed reads, as the app does, and have the
/// selected mouse answer its DPI read with [`SAVED`].
fn install(
    state: AppState,
    receiver: &mut tokio::sync::mpsc::UnboundedReceiver<Command>,
    cx: &mut gpui::TestAppContext,
) -> DeviceRoute {
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
    let route = answer_dpi_read(receiver, SAVED, &[800, 1200, 1600]);
    cx.run_until_parked();
    route
}

/// Commit `dpi` on the installed state, answer the write with `result`, and
/// return the reading the state then holds.
fn commit_and_answer(
    cx: &mut gpui::TestAppContext,
    receiver: &mut tokio::sync::mpsc::UnboundedReceiver<Command>,
    dpi: Dpi,
    result: Result<(), WriteError>,
) -> Option<Dpi> {
    let write = cx.update(|cx| AppState::update(cx, |state, _| commit(state, receiver, dpi)));
    cx.update(|cx| {
        AppState::update(cx, |state, _| {
            let key = mouse_key(state);
            let _ = state.apply_dpi_written(&key, write, dpi, result);
        });
    });
    cx.run_until_parked();
    reading(cx)
}

fn reading(cx: &gpui::TestAppContext) -> Option<Dpi> {
    cx.read(|cx| {
        let state = AppState::try_read(cx).expect("state installed");
        match state.dpi_load_for(&mouse_key(state)) {
            Load::Ready(info) => Some(info.current),
            _ => None,
        }
    })
}

fn shown_and_saved(cx: &gpui::TestAppContext) -> (Dpi, Dpi, Option<Dpi>) {
    cx.read(|cx| {
        let state = AppState::try_read(cx).expect("state installed");
        (
            state.dpi(),
            state.dpi_for_current(),
            state.config.dpi(KNOWN_MOUSE_KEY),
        )
    })
}

#[gpui::test]
fn a_dpi_the_mouse_took_becomes_its_reading(cx: &mut gpui::TestAppContext) {
    let (state, mut receiver) = mouse_with_a_saved_dpi();
    install(state, &mut receiver, cx);

    let reading = commit_and_answer(cx, &mut receiver, Dpi::new(1600), Ok(()));

    assert_eq!(reading, Some(Dpi::new(1600)));
    assert_eq!(
        shown_and_saved(cx),
        (Dpi::new(1600), Dpi::new(1600), Some(Dpi::new(1600))),
        "a later device switch seeds the editor with the new DPI"
    );
}

#[gpui::test]
fn a_dpi_the_mouse_did_not_apply_leaves_its_actual_dpi_everywhere(cx: &mut gpui::TestAppContext) {
    let (state, mut receiver) = mouse_with_a_saved_dpi();
    install(state, &mut receiver, cx);
    let requested = Dpi::new(1600);
    let actual = Dpi::new(1200);

    let reading = commit_and_answer(
        cx,
        &mut receiver,
        requested,
        Err(WriteError::DpiNotApplied { requested, actual }),
    );

    assert_eq!(reading, Some(actual));
    assert_eq!(shown_and_saved(cx), (actual, actual, Some(SAVED)));
}

#[gpui::test]
fn a_write_that_did_not_reach_the_mouse_reads_it_again(cx: &mut gpui::TestAppContext) {
    let (state, mut receiver) = mouse_with_a_saved_dpi();
    let route = install(state, &mut receiver, cx);

    let reading = commit_and_answer(
        cx,
        &mut receiver,
        Dpi::new(1600),
        Err(WriteError::DeviceUnreachable { index: 0xff }),
    );

    assert_eq!(reading, Some(SAVED), "the last reading stands meanwhile");
    assert_eq!(shown_and_saved(cx), (SAVED, SAVED, Some(SAVED)));
    // The write may have landed after all: the editor follows the mouse,
    // and still saves nothing it did not confirm.
    assert_eq!(
        answer_dpi_read(&mut receiver, Dpi::new(1600), &[800, 1200, 1600]),
        route
    );
    cx.run_until_parked();
    assert_eq!(
        shown_and_saved(cx),
        (Dpi::new(1600), Dpi::new(1600), Some(SAVED))
    );
}
