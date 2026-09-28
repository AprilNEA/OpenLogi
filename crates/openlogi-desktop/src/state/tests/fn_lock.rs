//! Saving a keyboard's Fn lock from the Keys tab, and ignoring other devices.

use super::*;
use crate::state::FnLockLoad;
use crate::state::events::StateEvents;

fn state_with_a_known_keyboard() -> AppState {
    let resolver = AssetResolver::new();
    let (commands, _receiver) = tokio::sync::mpsc::unbounded_channel();
    let mut inventory = direct_inventory([0xa3, 0x93, 0xca, 0xe0]);
    let keyboard = &mut inventory.paired[0];
    keyboard.kind = DeviceKind::Keyboard;
    keyboard.capabilities = Some(Capabilities::presumed_from_kind(DeviceKind::Keyboard));
    AppState::new(Sources {
        inventories: &[inventory],
        ..Sources::in_memory(Config::ephemeral(), &resolver, commands)
    })
}

#[test]
fn fn_lock_is_saved_for_the_selected_keyboard() {
    let mut state = state_with_a_known_keyboard();
    let key = state.current_record().expect("a keyboard").device_key();

    let events = state.commit_fn_lock(true);

    assert_eq!(state.config.fn_lock(KNOWN_MOUSE_KEY), Some(true));
    assert_eq!(events, StateEvents::from(StateEvent::FnLockChanged(key)));

    let _ = state.commit_fn_lock(false);
    assert_eq!(state.config.fn_lock(KNOWN_MOUSE_KEY), Some(false));
}

#[test]
fn fn_lock_is_never_saved_for_a_mouse() {
    let mut state = state_with_a_known_mouse();

    let events = state.commit_fn_lock(true);

    assert_eq!(state.config.fn_lock(KNOWN_MOUSE_KEY), None);
    assert_eq!(events, StateEvents::none());
}

/// Answer the next Fn-lock read the state sent, skipping config reloads.
fn answer_fn_lock_read(
    receiver: &mut tokio::sync::mpsc::UnboundedReceiver<crate::services::ipc::Command>,
    on: bool,
) {
    while let Ok(command) = receiver.try_recv() {
        if let crate::services::ipc::Command::ReadFnLock(read) = command {
            let _ = read.reply.send(Ok(on));
            return;
        }
    }
    panic!("no Fn-lock read was sent");
}

#[gpui::test]
fn reopening_the_keys_tab_rereads_fn_lock(cx: &mut gpui::TestAppContext) {
    let resolver = AssetResolver::new();
    let (commands, mut receiver) = tokio::sync::mpsc::unbounded_channel();
    let mut inventory = direct_inventory([0xa3, 0x93, 0xca, 0xe0]);
    inventory.paired[0].kind = DeviceKind::Keyboard;
    let state = AppState::new(Sources {
        inventories: &[inventory],
        ..Sources::in_memory(Config::ephemeral(), &resolver, commands)
    });
    cx.update(|cx| {
        let runtime: Arc<dyn swr_core::Runtime> = Arc::new(swr_gpui::GpuiRuntime::new(cx));
        let swr = swr_core::SwrClient::builder().build(runtime.clone());
        let entity = cx.new(|_| state);
        entity.update(cx, |state, _| state.connect_device_reads(swr, runtime));
        AppState::set_global(entity, cx);
        AppState::update(cx, AppState::load_current_fn_lock);
    });
    let current = |cx: &mut gpui::TestAppContext| {
        cx.update(|cx| {
            AppState::try_read(cx)
                .and_then(AppState::current_fn_lock)
                .cloned()
        })
    };

    cx.run_until_parked();
    answer_fn_lock_read(&mut receiver, false);
    cx.run_until_parked();
    assert_eq!(current(cx), Some(FnLockLoad::Ready(Arc::new(false))));

    // The Fn Lock key flipped it on the keyboard; opening the tab re-reads.
    cx.update(|cx| AppState::update(cx, |state, _| state.revalidate_current_fn_lock()));
    cx.run_until_parked();
    answer_fn_lock_read(&mut receiver, true);
    cx.run_until_parked();
    assert_eq!(current(cx), Some(FnLockLoad::Ready(Arc::new(true))));
}
