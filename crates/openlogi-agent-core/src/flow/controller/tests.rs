use std::sync::Arc;
use std::time::Duration;

use openlogi_core::config::{FlowConfig, FlowPublicKey};
use openlogi_ipc::{FlowLinkState, FlowStatus};

use super::{FlowController, FlowInput};
use crate::flow::test_support::{MOUSE, config_with_key, no_hardware, record};
use crate::observable::ObservableState;

const WAIT: Duration = Duration::from_secs(5);

/// A disabled configuration whose peer has key `[byte; 32]`. Disabled, so
/// the supervisor never binds a socket or touches the identity file.
fn disabled(byte: u8) -> FlowConfig {
    let mut config = config_with_key(FlowPublicKey::from_bytes([byte; 32]), Vec::new());
    config.enabled = false;
    config
}

fn controller(config: &FlowConfig) -> (FlowController, Arc<ObservableState>) {
    let observable = Arc::new(ObservableState::new("test".to_owned()));
    let (_device_io, hardware) = no_hardware();
    let controller = FlowController::new(
        config,
        Arc::clone(&observable),
        hardware.channel_pool,
        hardware.receiver_access,
        hardware.device_io,
    );
    (controller, observable)
}

async fn wait_for_status(observable: &ObservableState, reached: impl Fn(&FlowStatus) -> bool) {
    let mut observations = observable.subscribe();
    tokio::time::timeout(WAIT, async {
        while !reached(&observable.snapshot().flow) {
            observations.changed().await.expect("the observable lives");
        }
    })
    .await
    .expect("the Flow status changes in time");
}

#[tokio::test]
async fn a_dormant_controller_reports_its_peers_as_lost() {
    let config = disabled(1);
    let (controller, observable) = controller(&config);
    controller.arm();

    wait_for_status(&observable, |status| status.peers.len() == 1).await;
    let status = observable.snapshot().flow;
    assert!(!status.enabled);
    assert_eq!(status.peers[0].state, FlowLinkState::Lost);
    assert_eq!(
        status.peers[0].public_key,
        FlowPublicKey::from_bytes([1; 32])
    );
    tokio::time::timeout(WAIT, controller.shutdown())
        .await
        .expect("the supervisor stops in time");
}

#[tokio::test]
async fn a_configuration_change_reaches_the_supervisor() {
    let (controller, observable) = controller(&disabled(1));
    controller.arm();
    wait_for_status(&observable, |status| status.peers.len() == 1).await;

    controller.update_config(&disabled(2));
    wait_for_status(&observable, |status| {
        status
            .peers
            .first()
            .is_some_and(|peer| peer.public_key == FlowPublicKey::from_bytes([2; 32]))
    })
    .await;
    controller.shutdown().await;
}

#[tokio::test]
async fn equal_updates_wake_nobody() {
    let config = disabled(1);
    let (controller, _observable) = controller(&config);
    let mut configs = controller.inner.config.subscribe();
    let mut inventories = controller.inner.inventory.subscribe();

    controller.update_config(&config);
    controller.update_devices(Vec::new());
    assert!(!configs.has_changed().expect("the controller lives"));
    assert!(!inventories.has_changed().expect("the controller lives"));

    let mut changed = config.clone();
    changed.require_modifier = true;
    controller.update_config(&changed);
    let mouse = record(MOUSE, openlogi_core::device::DeviceKind::Mouse, 0x0a, true);
    controller.update_devices(vec![mouse.clone()]);
    assert!(configs.has_changed().expect("the controller lives"));
    assert!(inventories.has_changed().expect("the controller lives"));
    configs.mark_unchanged();
    inventories.mark_unchanged();

    controller.update_devices(vec![mouse]);
    assert!(!inventories.has_changed().expect("the controller lives"));
}

#[tokio::test]
async fn arming_after_shutdown_starts_nothing() {
    let (controller, observable) = controller(&disabled(1));
    controller.shutdown().await;
    controller.arm();

    tokio::time::sleep(Duration::from_millis(50)).await;
    assert!(observable.snapshot().flow.peers.is_empty());
    assert!(
        controller
            .inner
            .supervisor
            .lock()
            .expect("not poisoned")
            .is_none()
    );
}

#[test]
fn motion_without_a_generation_is_dropped() {
    FlowInput::default().moved();
}
