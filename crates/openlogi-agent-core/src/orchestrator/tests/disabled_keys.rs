//! Scripted HID regressions for the real shared writer and policy lifecycle.

use super::*;
use crate::orchestrator::SharedHandles;
use crate::receiver_access::ExclusiveAccessReason;
use openlogi_core::hid::{DisableKeysMask, HidppOperation, WriteError};
use openlogi_device::test_support::publish_scripted_channel;
use std::sync::atomic::{AtomicU8, Ordering};

struct Keyboard {
    route: DeviceRoute,
    disabled: Arc<AtomicU8>,
    writes: Arc<std::sync::Mutex<Vec<u8>>>,
    requested_features: Arc<std::sync::Mutex<Vec<u16>>>,
    reports: openlogi_device::replay::ReplayChannelHandle,
    write_notice: Arc<tokio::sync::Notify>,
}

impl Keyboard {
    async fn publish(shared: &SharedHandles) -> Self {
        let route = DeviceRoute::Direct {
            vendor_id: 0x046d,
            product_id: 0xb35b,
        };
        Self::publish_on(shared, route).await
    }

    async fn publish_on(shared: &SharedHandles, route: DeviceRoute) -> Self {
        let disabled = Arc::new(AtomicU8::new(0x81));
        let device_disabled = Arc::clone(&disabled);
        let writes = Arc::new(std::sync::Mutex::new(Vec::new()));
        let device_writes = Arc::clone(&writes);
        let requested_features = Arc::new(std::sync::Mutex::new(Vec::new()));
        let device_requested_features = Arc::clone(&requested_features);
        let write_notice = Arc::new(tokio::sync::Notify::new());
        let device_notice = Arc::clone(&write_notice);
        let (_, reports) = publish_scripted_channel(
            &shared.channel_registry,
            "keyboard",
            route.clone(),
            move |request| {
                if request.len() < 7 {
                    return None;
                }
                let value = match (request[2], request[3] >> 4) {
                    (0, 1) => 4,
                    (0, 0) => {
                        let feature = u16::from_be_bytes([request[4], request[5]]);
                        device_requested_features
                            .lock()
                            .expect("requested features")
                            .push(feature);
                        if feature == 0x4521 { 5 } else { 0 }
                    }
                    (5, 0) => 0xa1,
                    (5, 1) => device_disabled.load(Ordering::SeqCst),
                    (5, 2) => {
                        device_writes.lock().expect("writes").push(request[4]);
                        device_disabled.store(request[4], Ordering::SeqCst);
                        device_notice.notify_one();
                        0
                    }
                    _ => return None,
                };
                Some(vec![0x10, request[1], request[2], request[3], value, 0, 0])
            },
        )
        .await;
        Self {
            route,
            disabled,
            writes,
            requested_features,
            reports,
            write_notice,
        }
    }

    fn configure(&self, shared: &SharedHandles) {
        shared
            .disabled_keys_order
            .sync_policy(&self.route, Some(DisableKeysMask::CAPS_LOCK));
    }

    fn assert_writes(&self, expected: &[u8]) {
        assert_eq!(*self.writes.lock().expect("writes"), expected);
    }

    fn clear_requested_features(&self) {
        self.requested_features
            .lock()
            .expect("requested features")
            .clear();
    }

    fn assert_only_disable_keys_feature(&self) {
        assert_eq!(
            *self.requested_features.lock().expect("requested features"),
            vec![0x4521],
            "recovery must not look up any unrelated configured feature"
        );
    }

    fn hold_write_readback(&self) -> openlogi_device::replay::ReplayResponseBarrier {
        use openlogi_fixture::RequestMatch;

        let get_state = [0x10, 0xff, 5, 0x10, 0, 0, 0];
        self.reports
            .hold_next_response(RequestMatch::Hidpp20, &get_state)
            .release();
        self.reports
            .hold_next_response(RequestMatch::Hidpp20, &get_state)
    }
}

async fn finish(worker: Option<std::thread::JoinHandle<()>>) {
    let worker = worker.expect("background worker started");
    tokio::task::spawn_blocking(move || worker.join().expect("worker joins"))
        .await
        .expect("join task");
}

/// Start after the boot ladder has drained with `config` already published to
/// the Disable Keys policy owner.
async fn keyboard_after_boot(config: Config) -> (Orchestrator, Keyboard, DeviceInventory) {
    let mut orchestrator = orchestrator(config);
    let mut inventory = direct_inventory(None, [1, 2, 3, 4]);
    inventory.receiver.product_id = 0xb35b;
    inventory.paired[0].kind = DeviceKind::Keyboard;
    orchestrator.refresh_inventory(std::slice::from_ref(&inventory), &[], false);
    for _ in 0..VOLATILE_REAPPLY_CONFIRM_RETRIES {
        orchestrator.refresh_inventory_for_settings_confirmation(
            std::slice::from_ref(&inventory),
            &[],
            false,
        );
    }
    assert!(!orchestrator.needs_reapply_confirmation());
    let shared = orchestrator.shared();
    let keyboard = Keyboard::publish(&shared).await;
    (orchestrator, keyboard, inventory)
}

#[tokio::test]
async fn direct_identity_enrichment_preserves_pending_write_and_recovery() {
    use std::collections::BTreeMap;
    use std::time::Duration;

    let mut orchestrator = orchestrator(Config::default());
    let mut inventory = direct_inventory(None, [1, 2, 3, 4]);
    inventory.receiver.product_id = 0xb35b;
    inventory.paired[0].kind = DeviceKind::Keyboard;
    orchestrator.refresh_inventory(std::slice::from_ref(&inventory), &[], false);
    let shared = orchestrator.shared();
    let route = orchestrator.devices[0].route.clone().expect("direct route");
    drop(
        shared
            .disabled_keys_order
            .begin(&route, Some(DisableKeysMask::EMPTY)),
    );
    orchestrator.refresh_inventory_for_settings_confirmation(
        std::slice::from_ref(&inventory),
        &[],
        false,
    );
    let old_id = stable_id(&orchestrator.devices[0]);
    let spent_budget = VOLATILE_REAPPLY_CONFIRM_RETRIES - 1;
    assert_eq!(
        orchestrator.disabled_keys_recovery.get(&old_id),
        Some(&spent_budget)
    );
    assert_eq!(
        orchestrator.reapply_followup.get(&old_id),
        Some(&spent_budget)
    );

    let keyboard = Keyboard::publish(&shared).await;
    let lease = shared
        .receiver_access
        .acquire_exclusive(ExclusiveAccessReason::Pairing)
        .await;
    let mut manual = Box::pin(shared.set_disable_keys(&keyboard.route, DisableKeysMask::EMPTY));
    assert!(futures_lite::future::poll_once(&mut manual).await.is_none());
    inventory.paired[0]
        .model_info
        .as_mut()
        .expect("model")
        .serial_number = Some("keyboard".into());
    orchestrator.refresh_inventory(std::slice::from_ref(&inventory), &[], false);
    let next_id = stable_id(&orchestrator.devices[0]);
    assert_ne!(old_id, next_id);
    let expected = BTreeMap::from([(next_id, spent_budget)]);
    assert_eq!(orchestrator.disabled_keys_recovery, expected);
    assert_eq!(orchestrator.reapply_followup, expected);
    keyboard.assert_writes(&[]);
    drop(lease);
    let state = tokio::time::timeout(Duration::from_secs(2), manual)
        .await
        .expect("manual request finishes")
        .expect("identity enrichment must not retire a live request");
    assert_eq!(state.disabled.bits(), 0x80);
    keyboard.assert_writes(&[0x80]);
}

/// Start after the boot ladder has drained, then install real policy via reload.
async fn settled_keyboard() -> (Orchestrator, Keyboard, DeviceInventory) {
    let (mut orchestrator, keyboard, inventory) = keyboard_after_boot(Config::default()).await;
    let shared = orchestrator.shared();
    let mut config = Config::default();
    config.set_disabled_keys("unit:01020304", BTreeSet::from([DisableKey::CapsLock]));
    orchestrator.reload_config(config);
    tokio::time::timeout(
        std::time::Duration::from_secs(2),
        keyboard.write_notice.notified(),
    )
    .await
    .expect("reload applies saved policy");
    let (_, ticket) = shared
        .disabled_keys_order
        .policy(&keyboard.route)
        .expect("saved policy");
    assert!(ticket.turn().await.is_some());
    keyboard.assert_writes(&[0x81]);
    (orchestrator, keyboard, inventory)
}

#[tokio::test]
async fn failed_disable_keys_recovery_does_not_replay_fn_lock_policy() {
    use std::time::Duration;

    let mut config = Config::default();
    config.set_disabled_keys("unit:01020304", BTreeSet::from([DisableKey::CapsLock]));
    config.set_fn_lock("unit:01020304", true);
    let (mut orchestrator, keyboard, inventory) = keyboard_after_boot(config).await;
    let shared = orchestrator.shared();
    let (refresh, requested) =
        crate::watchers::inventory::InventoryRefresh::test_settings_confirmation();
    shared.publish_inventory_refresh(refresh);

    let error = shared
        .set_disable_keys(&keyboard.route, DisableKeysMask::from_bits_retain(0x40))
        .await
        .expect_err("unsupported manual mask");
    assert!(matches!(error, WriteError::UnsupportedMask { .. }));
    tokio::time::timeout(Duration::from_secs(2), requested)
        .await
        .expect("failed request schedules production confirmation");

    orchestrator.refresh_inventory(std::slice::from_ref(&inventory), &[], false);
    assert!(orchestrator.needs_reapply_confirmation());
    keyboard.clear_requested_features();
    orchestrator.refresh_inventory_for_settings_confirmation(
        std::slice::from_ref(&inventory),
        &[],
        false,
    );
    tokio::time::timeout(Duration::from_secs(2), keyboard.write_notice.notified())
        .await
        .expect("production recovery writes Disable Keys");
    keyboard.assert_only_disable_keys_feature();
}

#[tokio::test]
async fn failed_successor_automatically_restores_policy_after_boot_retries_drain() {
    use std::time::Duration;

    for unsaved_confirmed in [false, true] {
        for cancelled in [false, true] {
            let (mut orchestrator, keyboard, inventory) = settled_keyboard().await;
            let shared = orchestrator.shared();
            let (refresh, requested) =
                crate::watchers::inventory::InventoryRefresh::test_settings_confirmation();
            shared.publish_inventory_refresh(refresh);
            if unsaved_confirmed {
                shared
                    .set_disable_keys(&keyboard.route, DisableKeysMask::EMPTY)
                    .await
                    .expect("B is confirmed but not saved");
                keyboard.write_notice.notified().await;
            }
            let desired = if unsaved_confirmed {
                DisableKeysMask::CAPS_LOCK
            } else {
                DisableKeysMask::EMPTY
            };
            let readback = keyboard.hold_write_readback();
            let writer = shared.clone();
            let route = keyboard.route.clone();
            let first = tokio::spawn(async move { writer.set_disable_keys(&route, desired).await });
            tokio::time::timeout(Duration::from_secs(2), readback.request_written())
                .await
                .expect("unconfirmed write reached hardware");
            keyboard.write_notice.notified().await;
            let mut successor = Box::pin(
                shared.set_disable_keys(&keyboard.route, DisableKeysMask::from_bits_retain(0x40)),
            );
            assert!(
                futures_lite::future::poll_once(&mut successor)
                    .await
                    .is_none()
            );
            readback.release();
            assert!(matches!(
                first.await.unwrap().expect_err("superseded"),
                WriteError::WriteSuperseded { .. }
            ));
            tokio::time::timeout(Duration::from_secs(2), requested)
                .await
                .expect("abandoned RPC must schedule production inventory confirmation");
            // A confirmation while the successor owns priority must not retire
            // the eventual recovery of the accepted value.
            orchestrator.refresh_inventory_for_settings_confirmation(
                std::slice::from_ref(&inventory),
                &[],
                false,
            );
            let (refresh, requested) =
                crate::watchers::inventory::InventoryRefresh::test_settings_confirmation();
            shared.publish_inventory_refresh(refresh);
            if cancelled {
                drop(successor);
            } else {
                assert!(matches!(
                    successor.await.expect_err("unsupported"),
                    WriteError::UnsupportedMask { .. }
                ));
            }
            tokio::time::timeout(Duration::from_secs(2), requested)
                .await
                .expect("successor release must request confirmation again");
            orchestrator.refresh_inventory(std::slice::from_ref(&inventory), &[], false);
            assert!(
                orchestrator.needs_reapply_confirmation(),
                "failed manual request must arm recovery after boot retries drained"
            );
            orchestrator.refresh_inventory_for_settings_confirmation(
                std::slice::from_ref(&inventory),
                &[],
                false,
            );
            tokio::time::timeout(Duration::from_secs(2), keyboard.write_notice.notified())
                .await
                .expect("production confirmation restores accepted policy");
            let expected = if unsaved_confirmed { 0x80 } else { 0x81 };
            assert_eq!(keyboard.disabled.load(Ordering::SeqCst), expected);
            for _ in 1..VOLATILE_REAPPLY_CONFIRM_RETRIES {
                orchestrator.refresh_inventory_for_settings_confirmation(
                    std::slice::from_ref(&inventory),
                    &[],
                    false,
                );
                tokio::time::timeout(Duration::from_secs(2), keyboard.write_notice.notified())
                    .await
                    .expect("bounded confirming reapply");
            }
            assert!(!orchestrator.needs_reapply_confirmation());
            orchestrator.refresh_inventory(&[inventory], &[], false);
            assert!(
                !orchestrator.needs_reapply_confirmation(),
                "settled recovery does not rearm itself"
            );
        }
    }
}

#[tokio::test]
async fn successful_successor_retires_abandoned_request_recovery() {
    use std::time::Duration;

    let (mut orchestrator, keyboard, inventory) = settled_keyboard().await;
    let shared = orchestrator.shared();
    let readback = keyboard.hold_write_readback();
    let writer = shared.clone();
    let route = keyboard.route.clone();
    let first = tokio::spawn(async move {
        writer
            .set_disable_keys(&route, DisableKeysMask::EMPTY)
            .await
    });
    tokio::time::timeout(Duration::from_secs(2), readback.request_written())
        .await
        .unwrap();
    let mut successor = Box::pin(shared.set_disable_keys(&keyboard.route, DisableKeysMask::EMPTY));
    assert!(
        futures_lite::future::poll_once(&mut successor)
            .await
            .is_none()
    );
    readback.release();
    assert!(matches!(
        first.await.unwrap().expect_err("superseded"),
        WriteError::WriteSuperseded { .. }
    ));
    successor.await.expect("new accepted choice");
    orchestrator.refresh_inventory_for_settings_confirmation(&[inventory], &[], false);
    assert!(
        !orchestrator.needs_reapply_confirmation(),
        "success closes the recovery request"
    );
    keyboard.assert_writes(&[0x81, 0x80, 0x80]);
}

/// A recovery changes the keyboard without a `0x4521` report. The published
/// settings revision must move once the restoring write holds its turn, so a
/// client that read the abandoned mask reads again and sees the restored one.
#[tokio::test]
async fn recovery_moves_the_published_settings_revision() {
    use std::time::Duration;

    let (mut orchestrator, keyboard, inventory) = settled_keyboard().await;
    let shared = orchestrator.shared();
    let (refresh, requested) =
        crate::watchers::inventory::InventoryRefresh::test_settings_confirmation();
    shared.publish_inventory_refresh(refresh);
    let readback = keyboard.hold_write_readback();
    let writer = shared.clone();
    let route = keyboard.route.clone();
    let abandoned = tokio::spawn(async move {
        writer
            .set_disable_keys(&route, DisableKeysMask::EMPTY)
            .await
    });
    tokio::time::timeout(Duration::from_secs(2), readback.request_written())
        .await
        .expect("abandoned write reached hardware");
    abandoned.abort();
    assert!(abandoned.await.expect_err("cancelled").is_cancelled());
    readback.release();
    tokio::time::timeout(Duration::from_secs(2), requested)
        .await
        .expect("abandoned request schedules confirmation");
    assert_eq!(keyboard.disabled.load(Ordering::SeqCst), 0x80);

    let revision = |orchestrator: &Orchestrator| {
        orchestrator.observable.snapshot().inventory[0].paired[0].settings_revision
    };
    orchestrator.refresh_inventory(std::slice::from_ref(&inventory), &[], false);
    let before = revision(&orchestrator);
    orchestrator.refresh_inventory_for_settings_confirmation(
        std::slice::from_ref(&inventory),
        &[],
        false,
    );
    assert!(
        revision(&orchestrator) > before,
        "a started recovery must ask clients to read again"
    );
    assert_eq!(
        orchestrator.devices[0].settings_revision, 0,
        "agent decisions keep the device-reported revision"
    );
    let state = tokio::time::timeout(
        Duration::from_secs(2),
        shared.read_disable_keys(&keyboard.route),
    )
    .await
    .expect("read finishes")
    .expect("read after recovery");
    assert_eq!(
        state.disabled.bits(),
        0x81,
        "the read waits for the restoring write"
    );
}

#[tokio::test]
async fn retired_policy_cannot_be_recovered_by_late_manual_cancellation() {
    use std::time::Duration;

    for retirement in ["remove", "opt-out", "replacement"] {
        let (mut orchestrator, keyboard, mut inventory) = settled_keyboard().await;
        let shared = orchestrator.shared();
        let readback = keyboard.hold_write_readback();
        let writer = shared.clone();
        let route = keyboard.route.clone();
        let first = tokio::spawn(async move {
            writer
                .set_disable_keys(&route, DisableKeysMask::EMPTY)
                .await
        });
        tokio::time::timeout(Duration::from_secs(2), readback.request_written())
            .await
            .unwrap();
        match retirement {
            "replacement" => {
                inventory.paired[0].model_info.as_mut().unwrap().unit_id = [5, 6, 7, 8];
                orchestrator.refresh_inventory(std::slice::from_ref(&inventory), &[], false);
            }
            "opt-out" => {
                let mut config = orchestrator.config.clone();
                config.set_device_enabled("unit:01020304", false);
                orchestrator.reload_config(config);
            }
            _ => orchestrator.reload_config(Config::default()),
        }
        first.abort();
        assert!(first.await.expect_err("cancelled").is_cancelled());
        readback.release();
        for _ in 0..VOLATILE_REAPPLY_CONFIRM_RETRIES {
            orchestrator.refresh_inventory_for_settings_confirmation(
                std::slice::from_ref(&inventory),
                &[],
                false,
            );
        }
        assert!(!orchestrator.needs_reapply_confirmation());
        keyboard.assert_writes(&[0x81, 0x80]);
    }
}

#[tokio::test]
async fn an_old_confirmation_cannot_replace_a_completed_manual_choice() {
    let shared = orchestrator(Config::default()).shared();
    let keyboard = Keyboard::publish(&shared).await;
    keyboard.configure(&shared);
    let state = shared
        .set_disable_keys(&keyboard.route, DisableKeysMask::EMPTY)
        .await
        .expect("manual choice confirmed");
    assert_eq!(state.disabled.bits(), 0x80);

    // GUI persistence/reload follows the RPC; retry dispatch must not create
    // newer authority for the old saved value in that intervening window.
    finish(shared.reapply_disabled_keys(&keyboard.route)).await;
    assert_eq!(keyboard.disabled.load(Ordering::SeqCst), 0x80);
    keyboard.assert_writes(&[0x80]);
}

#[tokio::test]
async fn queued_reconnect_yields_to_a_newer_manual_request() {
    let shared = orchestrator(Config::default()).shared();
    let keyboard = Keyboard::publish(&shared).await;
    keyboard.configure(&shared);
    let lease = shared
        .receiver_access
        .acquire_exclusive(ExclusiveAccessReason::Pairing)
        .await;
    let old = shared.reapply_disabled_keys(&keyboard.route);
    let mut manual =
        std::pin::pin!(shared.set_disable_keys(&keyboard.route, DisableKeysMask::EMPTY));
    assert!(futures_lite::future::poll_once(&mut manual).await.is_none());
    drop(lease);
    manual.await.expect("new choice");
    finish(old).await;
    keyboard.assert_writes(&[0x80]);
}

#[tokio::test]
async fn cancelled_or_failed_manual_request_releases_saved_policy() {
    let shared = orchestrator(Config::default()).shared();
    let keyboard = Keyboard::publish(&shared).await;
    keyboard.configure(&shared);
    let (refresh, abandoned) =
        crate::watchers::inventory::InventoryRefresh::test_settings_confirmation();
    shared.publish_inventory_refresh(refresh);
    let lease = shared
        .receiver_access
        .acquire_exclusive(ExclusiveAccessReason::Pairing)
        .await;
    {
        let mut manual =
            std::pin::pin!(shared.set_disable_keys(&keyboard.route, DisableKeysMask::EMPTY));
        assert!(futures_lite::future::poll_once(&mut manual).await.is_none());
    }
    drop(lease);
    // Dropping the RPC signals cancellation, not native-owner completion.
    // Production recovery is requested only after the worker releases intent.
    tokio::time::timeout(std::time::Duration::from_secs(2), abandoned)
        .await
        .expect("cancelled worker released its pending intent");
    finish(shared.reapply_disabled_keys(&keyboard.route)).await;
    // Unsupported known/unknown bits fail before any write and release priority.
    let error = shared
        .set_disable_keys(&keyboard.route, DisableKeysMask::from_bits_retain(0x40))
        .await
        .expect_err("unsupported mask");
    assert!(matches!(error, WriteError::UnsupportedMask { .. }));
    finish(shared.reapply_disabled_keys(&keyboard.route)).await;
    keyboard.assert_writes(&[0x81, 0x81]);
}

#[tokio::test]
async fn a_superseded_completion_does_not_strand_policy_when_its_successor_fails() {
    use openlogi_fixture::RequestMatch;
    use std::time::Duration;

    // Cancellation, a rejected request, and a successful newer choice must
    // respectively restore saved policy, restore it, and keep the new choice.
    for successor in [None, Some(0x40), Some(0x00)] {
        let shared = orchestrator(Config::default()).shared();
        let keyboard = Keyboard::publish(&shared).await;
        keyboard.configure(&shared);
        let get_state = [0x10, 0xff, 5, 0x10, 0, 0, 0];
        keyboard
            .reports
            .hold_next_response(RequestMatch::Hidpp20, &get_state)
            .release();
        let readback = keyboard
            .reports
            .hold_next_response(RequestMatch::Hidpp20, &get_state);
        let writer = shared.clone();
        let route = keyboard.route.clone();
        let first = tokio::spawn(async move {
            writer
                .set_disable_keys(&route, DisableKeysMask::EMPTY)
                .await
        });
        tokio::time::timeout(Duration::from_secs(2), readback.request_written())
            .await
            .expect("first request reached readback");
        // Keep the cancelled successor in preflight; otherwise its detached
        // worker can mutate before the test gets to drop the requester.
        let cancelled_preflight = successor.is_none().then(|| {
            keyboard
                .reports
                .hold_next_response(RequestMatch::Hidpp20, &[0x10, 0xff, 0, 0x10, 0, 0, 0])
        });
        let mut second = Box::pin(shared.set_disable_keys(
            &keyboard.route,
            DisableKeysMask::from_bits_retain(successor.unwrap_or(0x00)),
        ));
        assert!(futures_lite::future::poll_once(&mut second).await.is_none());
        readback.release();
        assert_eq!(
            first.await.expect("first task").expect_err("superseded"),
            WriteError::WriteSuperseded {
                operation: HidppOperation::WriteDisableKeys,
            }
        );
        match successor {
            None => {
                let preflight = cancelled_preflight.unwrap();
                tokio::time::timeout(Duration::from_secs(2), preflight.request_written())
                    .await
                    .expect("successor reached preflight");
                let (refresh, abandoned) =
                    crate::watchers::inventory::InventoryRefresh::test_settings_confirmation();
                shared.publish_inventory_refresh(refresh);
                drop(second);
                preflight.release();
                tokio::time::timeout(Duration::from_secs(2), abandoned)
                    .await
                    .expect("successor released its pending intent");
            }
            Some(0x40) => assert!(matches!(
                second.await.expect_err("unsupported mask"),
                WriteError::UnsupportedMask { .. }
            )),
            Some(_) => assert_eq!(
                second.await.expect("new choice confirmed").disabled.bits(),
                0x80
            ),
        }
        finish(shared.reapply_disabled_keys(&keyboard.route)).await;
        let expected = if successor == Some(0x00) { 0x80 } else { 0x81 };
        assert_eq!(keyboard.disabled.load(Ordering::SeqCst), expected);
        keyboard.assert_writes(&[0x80, expected]);
    }
}

#[tokio::test]
async fn superseded_manual_request_reports_failure_without_writing() {
    let shared = orchestrator(Config::default()).shared();
    let keyboard = Keyboard::publish(&shared).await;
    let lease = shared
        .receiver_access
        .acquire_exclusive(ExclusiveAccessReason::Pairing)
        .await;
    let mut old =
        std::pin::pin!(shared.set_disable_keys(&keyboard.route, DisableKeysMask::CAPS_LOCK));
    assert!(futures_lite::future::poll_once(&mut old).await.is_none());
    let mut new = std::pin::pin!(shared.set_disable_keys(&keyboard.route, DisableKeysMask::EMPTY));
    assert!(futures_lite::future::poll_once(&mut new).await.is_none());
    drop(lease);
    assert_eq!(
        old.await.expect_err("superseded"),
        WriteError::WriteSuperseded {
            operation: HidppOperation::WriteDisableKeys,
        }
    );
    new.await.expect("new request");
    keyboard.assert_writes(&[0x80]);
}

#[tokio::test]
async fn queued_reapply_resolves_the_current_channel_after_receiver_access() {
    let shared = orchestrator(Config::default()).shared();
    let old_keyboard = Keyboard::publish(&shared).await;
    old_keyboard.configure(&shared);
    let lease = shared
        .receiver_access
        .acquire_exclusive(ExclusiveAccessReason::Pairing)
        .await;
    let worker = shared.reapply_disabled_keys(&old_keyboard.route);
    let new_keyboard = Keyboard::publish(&shared).await;
    drop(lease);
    finish(worker).await;
    old_keyboard.assert_writes(&[]);
    new_keyboard.assert_writes(&[0x81]);
}

#[tokio::test]
async fn offline_reload_invalidates_queued_policy_without_replacement() {
    for opt_out in [false, true] {
        let mut config = Config::default();
        config.set_disabled_keys("unit:01020304", BTreeSet::from([DisableKey::CapsLock]));
        config
            .devices
            .get_mut("unit:01020304")
            .expect("config")
            .links
            .insert("direct:046d:b35b".into(), LinkConfig::default());
        let mut orchestrator = orchestrator(config.clone());
        let shared = orchestrator.shared();
        let keyboard = Keyboard::publish(&shared).await;
        let mut inventory = direct_inventory(None, [0; 4]);
        inventory.receiver.product_id = 0xb35b;
        inventory.paired[0].kind = DeviceKind::Keyboard;
        inventory.paired[0].online = false;
        orchestrator.refresh_inventory(&[inventory], &[], false);
        let key = orchestrator.devices[0].config_key.clone();
        assert_eq!(key, "unit:01020304");
        let lease = shared
            .receiver_access
            .acquire_exclusive(ExclusiveAccessReason::Pairing)
            .await;
        let old = shared.reapply_disabled_keys(&keyboard.route);
        if opt_out {
            config.devices.get_mut(&key).expect("config").enabled = false;
        } else {
            config.clear_disabled_keys(&key);
        }
        orchestrator.reload_config(config);
        drop(lease);
        finish(old).await;
        assert!(shared.reapply_disabled_keys(&keyboard.route).is_none());
        keyboard.assert_writes(&[]);
    }
}

#[tokio::test]
async fn retiring_an_unmanaged_route_or_opting_out_invalidates_a_queued_manual_write() {
    for opt_out in [false, true] {
        let mut orchestrator = orchestrator(Config::default());
        let shared = orchestrator.shared();
        let keyboard = Keyboard::publish(&shared).await;
        let mut inventory = direct_inventory(None, [1, 2, 3, 4]);
        inventory.receiver.product_id = 0xb35b;
        inventory.paired[0].kind = DeviceKind::Keyboard;
        orchestrator.refresh_inventory(&[inventory], &[], false);
        let key = orchestrator.devices[0].config_key.clone();
        let lease = shared
            .receiver_access
            .acquire_exclusive(ExclusiveAccessReason::Pairing)
            .await;
        let mut manual =
            std::pin::pin!(shared.set_disable_keys(&keyboard.route, DisableKeysMask::EMPTY));
        assert!(futures_lite::future::poll_once(&mut manual).await.is_none());
        if opt_out {
            let mut config = Config::default();
            config.set_device_enabled(&key, false);
            orchestrator.reload_config(config);
        } else {
            orchestrator.refresh_inventory(&[], &[], false);
        }
        drop(lease);
        assert_eq!(
            manual.await.expect_err("retired"),
            WriteError::WriteSuperseded {
                operation: HidppOperation::WriteDisableKeys,
            }
        );
        keyboard.assert_writes(&[]);
    }
}

#[tokio::test]
async fn active_reapply_holds_its_turn_through_readback_and_manual_resolves_after_waiting() {
    use openlogi_fixture::RequestMatch;
    use std::time::Duration;

    for replace_channel in [false, true] {
        let shared = orchestrator(Config::default()).shared();
        let keyboard = Keyboard::publish(&shared).await;
        keyboard.configure(&shared);
        let get_state = [0x10, 0xff, 5, 0x10, 0, 0, 0];
        keyboard
            .reports
            .hold_next_response(RequestMatch::Hidpp20, &get_state)
            .release();
        let readback = keyboard
            .reports
            .hold_next_response(RequestMatch::Hidpp20, &get_state);
        let old = shared.reapply_disabled_keys(&keyboard.route);
        tokio::time::timeout(Duration::from_secs(2), readback.request_written())
            .await
            .expect("old write reached readback");
        let mut manual =
            std::pin::pin!(shared.set_disable_keys(&keyboard.route, DisableKeysMask::EMPTY));
        assert!(futures_lite::future::poll_once(&mut manual).await.is_none());
        let replacement = if replace_channel {
            Some(Keyboard::publish(&shared).await)
        } else {
            None
        };
        readback.release();
        let result = manual.await.expect("new choice confirmed");
        finish(old).await;
        assert_eq!(result.disabled.bits(), 0x80);
        if let Some(replacement) = replacement {
            keyboard.assert_writes(&[0x81]);
            replacement.assert_writes(&[0x80]);
        } else {
            keyboard.assert_writes(&[0x81, 0x80]);
        }
    }
}

#[tokio::test]
async fn replacing_the_physical_keyboard_at_the_same_route_retires_old_requests() {
    let mut orchestrator = orchestrator(Config::default());
    let shared = orchestrator.shared();
    let keyboard = Keyboard::publish(&shared).await;
    let mut inventory = direct_inventory(None, [1, 2, 3, 4]);
    inventory.receiver.product_id = 0xb35b;
    inventory.paired[0].kind = DeviceKind::Keyboard;
    orchestrator.refresh_inventory(&[inventory.clone()], &[], false);
    let lease = shared
        .receiver_access
        .acquire_exclusive(ExclusiveAccessReason::Pairing)
        .await;
    let mut manual =
        std::pin::pin!(shared.set_disable_keys(&keyboard.route, DisableKeysMask::EMPTY));
    assert!(futures_lite::future::poll_once(&mut manual).await.is_none());
    inventory.paired[0]
        .model_info
        .as_mut()
        .expect("model")
        .unit_id = [5, 6, 7, 8];
    orchestrator.refresh_inventory(&[inventory], &[], false);
    drop(lease);
    assert_eq!(
        manual.await.expect_err("old keyboard left"),
        WriteError::WriteSuperseded {
            operation: HidppOperation::WriteDisableKeys,
        }
    );
    keyboard.assert_writes(&[]);
}

#[tokio::test]
async fn adopting_a_route_applies_changed_policy_without_another_inventory_pass() {
    use openlogi_core::device_order::PhysicalDeviceKey;
    use std::time::Duration;

    let mut config = Config::default();
    let legacy = "receiver:aa00:slot:1";
    config.set_disabled_keys(legacy, BTreeSet::from([DisableKey::CapsLock]));
    let mut inventory = direct_inventory(None, [1, 2, 3, 4]);
    inventory.receiver.product_id = 0xc548;
    inventory.receiver.unique_id = Some("AA00".into());
    inventory.paired[0].slot = 1;
    inventory.paired[0].kind = DeviceKind::Keyboard;
    let route = DeviceRoute::for_slot(&inventory, 1).expect("receiver route");
    let mut orchestrator = orchestrator(config.clone());
    orchestrator.refresh_inventory(&[inventory.clone()], &[], false);
    for _ in 0..VOLATILE_REAPPLY_CONFIRM_RETRIES {
        orchestrator.refresh_inventory_for_settings_confirmation(&[inventory.clone()], &[], false);
    }
    assert!(!orchestrator.needs_reapply_confirmation());
    assert_eq!(orchestrator.devices[0].config_key, legacy);
    let shared = orchestrator.shared();
    let keyboard = Keyboard::publish_on(&shared, route.clone()).await;
    let canonical = PhysicalDeviceKey::parse("unit:01020304").expect("key");
    assert!(config.adopt_route(&canonical, legacy, None));
    let (_, before_adoption) = shared
        .disabled_keys_order
        .policy(&route)
        .expect("old policy");
    orchestrator.reload_config(config.clone());
    assert!(
        before_adoption.is_current(),
        "unchanged adoption is not a new intent"
    );
    config.set_disabled_keys(canonical.as_str(), BTreeSet::new());
    orchestrator.reload_config(config.clone());
    let (policy, ticket) = shared.disabled_keys_order.policy(&route).expect("policy");
    assert_eq!(policy, Some(DisableKeysMask::EMPTY));
    tokio::time::timeout(Duration::from_secs(2), async {
        keyboard.write_notice.notified().await;
        assert!(ticket.turn().await.is_some());
    })
    .await
    .expect("reload write completed");
    keyboard.assert_writes(&[0x80]);

    // The remembered key is still legacy until inventory refreshes. Opt-out
    // must nevertheless be read from the newly canonical config entry.
    let lease = shared
        .receiver_access
        .acquire_exclusive(ExclusiveAccessReason::Pairing)
        .await;
    let mut manual = std::pin::pin!(shared.set_disable_keys(&route, DisableKeysMask::CAPS_LOCK));
    assert!(futures_lite::future::poll_once(&mut manual).await.is_none());
    config.set_device_enabled(canonical.as_str(), false);
    orchestrator.reload_config(config);
    drop(lease);
    assert_eq!(
        manual.await.expect_err("canonical device opted out"),
        WriteError::WriteSuperseded {
            operation: HidppOperation::WriteDisableKeys,
        }
    );
    keyboard.assert_writes(&[0x80]);
}
