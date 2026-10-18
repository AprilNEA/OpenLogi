//! Revisions delivered by the agent after another client changes the keyboard.

use super::*;

fn dark() -> BacklightState {
    BacklightState {
        enabled: false,
        current_level: 0,
        ..lit()
    }
}

fn snapshot(cx: &mut gpui::TestAppContext, inventory: Vec<DeviceInventory>) {
    let snapshot = AgentSnapshot {
        status: AgentStatus {
            accessibility_granted: true,
            hook_installed: true,
            launch_at_login: false,
            inventory: InventoryHealth::Ready,
            protocol_version: PROTOCOL_VERSION,
            agent_version: "backlight-freshness-test".to_string(),
            input_monitoring_granted: true,
            hid_open_failures: false,
            secure_input: openlogi_ipc::SecureInput::Off,
            virtual_keyboard: openlogi_ipc::VirtualKeyboardStatus::Unsupported,
        },
        inventory,
        standalone: Vec::new(),
        camera_active: false,
        pairing: None,
        foreground: ForegroundApps::default(),
        lights: Vec::new(),
        flow: openlogi_ipc::FlowStatus::default(),
    };
    cx.update(|cx| {
        AppState::update(cx, |state, _| {
            state.apply_agent_snapshot(&snapshot, &AssetResolver::new(), &[])
        });
        AppState::load_current_device_reads(cx);
    });
    cx.run_until_parked();
}

fn changed(cx: &mut gpui::TestAppContext, revision: u64) {
    let mut inventory = keyboard_inventory(true);
    inventory.paired[0].settings_revision = revision;
    snapshot(cx, vec![inventory]);
}

fn take_read(receiver: &mut tokio::sync::mpsc::UnboundedReceiver<Command>) -> ReadBacklight {
    let mut reads = drain(receiver).into_iter().filter_map(|command| {
        if let Command::ReadBacklight(read) = command {
            Some(read)
        } else {
            None
        }
    });
    let read = reads.next().expect("a fresh backlight read was requested");
    assert!(reads.next().is_none(), "only one fresh read is needed");
    read
}

#[gpui::test]
fn external_off_on_refreshes_the_open_panel_without_another_write(cx: &mut gpui::TestAppContext) {
    let (state, mut receiver) = keyboard_state(true);
    let (_, route) = installed_and_read(cx, &mut receiver, state, lit());
    for (revision, reading) in [(1, dark()), (2, lit())] {
        changed(cx, revision);
        assert_eq!(answer_backlight_read(&mut receiver, reading), route);
        cx.run_until_parked();
        assert_eq!(current_load(cx), Load::Ready(Arc::new(reading)));
        changed(cx, revision);
        let commands = drain(&mut receiver);
        assert_eq!(backlight_reads(&commands), 0, "same snapshot is a no-op");
        assert!(writes(commands).is_empty(), "refresh never writes settings");
    }
}

#[gpui::test]
fn external_revision_discards_a_read_started_before_the_write(cx: &mut gpui::TestAppContext) {
    let (state, mut receiver) = keyboard_state(true);
    install(state, cx);
    let earlier = take_read(&mut receiver);
    changed(cx, 1);
    let fresh = take_read(&mut receiver);
    assert_eq!(fresh.route, earlier.route);
    fresh
        .reply
        .send(Ok(dark()))
        .expect("fresh response accepted");
    cx.run_until_parked();
    let _ = earlier.reply.send(Ok(lit()));
    cx.run_until_parked();
    assert_eq!(current_load(cx), Load::Ready(Arc::new(dark())));
}

#[gpui::test]
fn delayed_local_reply_cannot_hide_a_newer_external_write(cx: &mut gpui::TestAppContext) {
    let (state, mut receiver) = keyboard_state(true);
    let (key, route) = installed_and_read(cx, &mut receiver, state, dark());
    let _ = commit(cx, BacklightChange::Enabled(true));
    let [(_, write)] = sent(&mut receiver)[..] else {
        panic!("one local On request");
    };

    // The local On landed, then CLI Off landed, but the On RPC reply is delayed.
    changed(cx, 1);
    answer_backlight_read(&mut receiver, dark());
    cx.run_until_parked();
    let _ = written(cx, &key, &route, write, Ok(lit()));
    cx.run_until_parked();
    answer_backlight_read(&mut receiver, dark());
    cx.run_until_parked();
    assert_eq!(current_load(cx), Load::Ready(Arc::new(dark())));
    assert!(writes(drain(&mut receiver)).is_empty());
}

#[gpui::test]
fn revision_during_local_writes_reconciles_after_the_last_reply(cx: &mut gpui::TestAppContext) {
    let (state, mut receiver) = keyboard_state(true);
    let (key, route) = installed_and_read(cx, &mut receiver, state, dark());
    let _ = commit(cx, BacklightChange::Enabled(true));
    let _ = commit(cx, BacklightChange::PowerSave(false));
    let [(_, first), (_, last)] = sent(&mut receiver)[..] else {
        panic!("two local requests");
    };
    changed(cx, 1);
    answer_backlight_read(&mut receiver, dark());
    cx.run_until_parked();
    let _ = written(cx, &key, &route, first, Ok(lit()));
    cx.run_until_parked();
    assert_eq!(
        current_load(cx),
        Load::Ready(Arc::new(lit().changed(BacklightChange::PowerSave(false)))),
        "newer local intent is preserved"
    );
    assert_eq!(backlight_reads(&drain(&mut receiver)), 0);
    let _ = written(cx, &key, &route, last, Err(WriteError::AgentUnavailable));
    cx.run_until_parked();
    answer_backlight_read(&mut receiver, dark());
    cx.run_until_parked();
    assert_eq!(current_load(cx), Load::Ready(Arc::new(dark())));
}

#[gpui::test]
fn another_keyboard_revision_does_not_refresh_the_selected_keyboard(cx: &mut gpui::TestAppContext) {
    let (state, mut receiver) = keyboard_state(true);
    installed_and_read(cx, &mut receiver, state, lit());
    let mut other = direct_inventory([1, 2, 3, 4]);
    other.receiver.product_id = 0xb369;
    snapshot(cx, vec![keyboard_inventory(true), other.clone()]);
    let _ = drain(&mut receiver);
    other.paired[0].settings_revision = 1;
    snapshot(cx, vec![keyboard_inventory(true), other]);
    assert_eq!(backlight_reads(&drain(&mut receiver)), 0);
    assert_eq!(current_load(cx), Load::Ready(Arc::new(lit())));
}

#[gpui::test]
fn late_read_on_replaced_route_does_not_overwrite_current_backlight(cx: &mut gpui::TestAppContext) {
    let (state, mut receiver) = keyboard_state(true);
    install(state, cx);
    let old = take_read(&mut receiver);
    snapshot(cx, vec![on_receiver(keyboard_inventory(true))]);
    assert_ne!(answer_backlight_read(&mut receiver, dark()), old.route);
    cx.run_until_parked();
    let _ = old.reply.send(Ok(lit()));
    cx.run_until_parked();
    assert_eq!(current_load(cx), Load::Ready(Arc::new(dark())));
}

#[gpui::test]
fn replaced_route_discards_deferred_freshness_and_old_local_reply(cx: &mut gpui::TestAppContext) {
    let (state, mut receiver) = keyboard_state(true);
    let (key, old_route) = installed_and_read(cx, &mut receiver, state, dark());
    let _ = commit(cx, BacklightChange::Enabled(true));
    let [(_, old_write)] = sent(&mut receiver)[..] else {
        panic!("one old-route request");
    };
    changed(cx, 1);
    let earlier = take_read(&mut receiver);
    snapshot(cx, vec![on_receiver(keyboard_inventory(true))]);
    let route = answer_backlight_read(&mut receiver, dark());
    cx.run_until_parked();
    let _ = earlier.reply.send(Ok(lit()));
    assert!(written(cx, &key, &old_route, old_write, Ok(lit())).is_empty());
    let _ = commit(cx, BacklightChange::Enabled(true));
    let [(_, new_write)] = sent(&mut receiver)[..] else {
        panic!("one new-route request");
    };
    let _ = written(cx, &key, &route, new_write, Ok(lit()));
    cx.run_until_parked();
    assert_eq!(current_load(cx), Load::Ready(Arc::new(lit())));
    assert_eq!(backlight_reads(&drain(&mut receiver)), 0);
}

#[gpui::test]
fn failed_external_refresh_uses_the_existing_retry_budget(cx: &mut gpui::TestAppContext) {
    let (state, mut receiver) = keyboard_state(true);
    installed_and_read(cx, &mut receiver, state, lit());
    changed(cx, 1);
    for _ in 0..3 {
        take_read(&mut receiver)
            .reply
            .send(Err(WriteError::AgentUnavailable))
            .expect("read failure delivered");
        cx.run_until_parked();
    }
    assert!(matches!(current_load(cx), Load::Failed(_)));
    let commands = drain(&mut receiver);
    assert_eq!(backlight_reads(&commands), 0, "retry budget is exhausted");
    assert!(writes(commands).is_empty());
}
