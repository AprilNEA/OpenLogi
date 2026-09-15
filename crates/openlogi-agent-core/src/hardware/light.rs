//! Serialized standalone-light writes and reconnect re-application.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, LazyLock, Mutex, mpsc};
use std::thread;

use openlogi_core::config::LightSettings;
use openlogi_core::device::LightCapabilities;
use openlogi_hid::{
    DeviceRoute, HidppOperation, LightCommand, WriteError, commands_for_light_settings,
    litra_model_for_route,
};
use tracing::{debug, info, warn};

use super::HardwareContext;

struct LightApplyRequest {
    settings: LightSettings,
    capabilities: LightCapabilities,
    generation: u64,
    hardware: HardwareContext,
}

#[derive(Clone)]
struct LightWorkerHandle {
    sender: mpsc::Sender<LightApplyRequest>,
    generation: Arc<AtomicU64>,
}

/// One coalescing worker per physical light. Reconnect and config transitions
/// can overlap. Keeping one worker per route gives us ordered writes for that
/// light, coalesces a burst to the latest desired state, and avoids creating a
/// Tokio runtime and OS thread for every transition.
static LIGHT_WORKERS: LazyLock<Mutex<HashMap<String, LightWorkerHandle>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

type LightWriteLock = Arc<tokio::sync::Mutex<()>>;

/// Serialize complete light-setting sequences with individual user commands.
/// The HID layer already serializes each packet, while reconnect/config
/// re-application writes power, brightness, and temperature as one operation.
static LIGHT_WRITE_LOCKS: LazyLock<Mutex<HashMap<String, LightWriteLock>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// Apply standalone-light settings during reconnect or config re-application.
/// Failures are logged because this path is best-effort; an explicit IPC
/// command returns the typed error to the caller instead.
pub(super) fn set_light_in_background(
    hardware: &HardwareContext,
    target: Option<DeviceRoute>,
    light: &LightSettings,
    capabilities: LightCapabilities,
) {
    let Some(target) = target else {
        debug!("no target device — light write skipped");
        return;
    };
    let key = target.to_string();
    let Some(worker) = light_worker(&key, target) else {
        return;
    };
    let generation = worker.generation.fetch_add(1, Ordering::AcqRel) + 1;
    if worker
        .sender
        .send(LightApplyRequest {
            settings: *light,
            capabilities,
            generation,
            hardware: hardware.clone(),
        })
        .is_err()
    {
        warn!(route = %key, "light re-apply worker stopped");
        remove_light_worker(&key, generation);
    }
}

/// Invalidate pending best-effort writes before an explicit user command.
/// Already-running writes are serialized by the HID driver's device lock; a
/// newer explicit command therefore remains the final state.
pub(super) fn cancel_light_reapply(target: &DeviceRoute) {
    let key = target.to_string();
    let Ok(workers) = LIGHT_WORKERS.lock() else {
        warn!(route = %key, "light worker registry poisoned — cannot cancel stale write");
        return;
    };
    if let Some(worker) = workers.get(&key) {
        worker.generation.fetch_add(1, Ordering::AcqRel);
    }
}

fn light_worker(key: &str, target: DeviceRoute) -> Option<LightWorkerHandle> {
    let Ok(mut workers) = LIGHT_WORKERS.lock() else {
        warn!(
            route = key,
            "light worker registry poisoned — write skipped"
        );
        return None;
    };
    if let Some(worker) = workers.get(key) {
        return Some(worker.clone());
    }

    let (sender, receiver) = mpsc::channel();
    let generation = Arc::new(AtomicU64::new(0));
    let worker_generation = Arc::clone(&generation);
    let worker_key = key.to_string();
    if let Err(error) = thread::Builder::new()
        .name(format!("openlogi-light-{}", key.replace(':', "-")))
        .spawn(move || light_worker_loop(target, receiver, worker_generation))
    {
        warn!(route = %worker_key, error = %error, "could not spawn light worker");
        return None;
    }
    let worker = LightWorkerHandle { sender, generation };
    workers.insert(worker_key, worker.clone());
    Some(worker)
}

fn remove_light_worker(key: &str, generation: u64) {
    let Ok(mut workers) = LIGHT_WORKERS.lock() else {
        return;
    };
    if workers
        .get(key)
        .is_some_and(|worker| worker.generation.load(Ordering::Acquire) == generation)
    {
        workers.remove(key);
    }
}

#[expect(
    clippy::needless_pass_by_value,
    reason = "the worker thread must own its route, receiver, and generation state"
)]
fn light_worker_loop(
    target: DeviceRoute,
    receiver: mpsc::Receiver<LightApplyRequest>,
    generation: Arc<AtomicU64>,
) {
    let rt = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(error) => {
            warn!(route = %target, error = %error, "light worker runtime init failed");
            return;
        }
    };
    while let Ok(mut request) = receiver.recv() {
        while let Ok(next) = receiver.try_recv() {
            request = next;
        }
        if generation.load(Ordering::Acquire) != request.generation {
            debug!(route = %target, "skipping superseded light re-apply");
            continue;
        }
        if !request.hardware.device_io().allows_io() {
            debug!(route = %target, "host device I/O suspended — light re-apply skipped");
            continue;
        }
        let result = rt.block_on(apply_light_settings(
            &target,
            &request,
            &generation,
            |command| apply_light_unlocked(&request.hardware, &target, command),
        ));
        match result {
            Ok(true) => info!(
                route = %target,
                enabled = request.settings.enabled,
                brightness = request.settings.brightness_percent,
                temperature = ?request.settings.temperature_kelvin,
                "light re-apply completed"
            ),
            Ok(false) => debug!(route = %target, "skipping canceled light re-apply"),
            Err(error) => warn!(route = %target, error = ?error, "light settings re-apply failed"),
        }
    }
}

async fn apply_light_settings(
    target: &DeviceRoute,
    request: &LightApplyRequest,
    generation: &AtomicU64,
    mut write: impl AsyncFnMut(LightCommand) -> Result<(), WriteError>,
) -> Result<bool, WriteError> {
    let lock = light_write_lock(target);
    let _guard = lock.lock().await;
    // The request may have passed the queue check while an explicit command
    // held the route lock. Re-check under that lock before writing anything so
    // a canceled re-apply cannot overwrite the newer explicit state.
    if generation.load(Ordering::Acquire) != request.generation {
        return Ok(false);
    }
    let mut first_error = None;
    for command in commands_for_light_settings(request.settings, request.capabilities) {
        // Power-off is last to avoid flashing the light. If a value fails,
        // skip the remaining values but still try to leave the device dark.
        if first_error.is_some() && command != LightCommand::Power(false) {
            continue;
        }
        if !request.hardware.device_io().allows_io() {
            return first_error.map_or(Ok(false), Err);
        }
        if let Err(error) = write(command).await {
            if request.settings.enabled || !request.capabilities.power {
                return Err(error);
            }
            first_error.get_or_insert(error);
        }
    }
    first_error.map_or(Ok(true), Err)
}

/// Apply a semantic command to a supported standalone light.
pub(super) async fn apply_light(
    hardware: &HardwareContext,
    route: &DeviceRoute,
    command: LightCommand,
) -> Result<(), WriteError> {
    if !hardware.device_io().allows_io() {
        return Err(WriteError::DeviceNotFound);
    }
    let lock = light_write_lock(route);
    let _guard = lock.lock().await;
    if !hardware.device_io().allows_io() {
        return Err(WriteError::DeviceNotFound);
    }
    apply_light_unlocked(hardware, route, command).await
}

async fn apply_light_unlocked(
    hardware: &HardwareContext,
    route: &DeviceRoute,
    command: LightCommand,
) -> Result<(), WriteError> {
    let Some(model) = litra_model_for_route(route) else {
        return Err(WriteError::LightUnsupported {
            control: "raw_hid_route".into(),
        });
    };
    super::timed(
        HidppOperation::Light,
        hardware.apply_litra(route, model, command),
    )
    .await
}

fn light_write_lock(route: &DeviceRoute) -> LightWriteLock {
    let key = route.to_string();
    let Ok(mut locks) = LIGHT_WRITE_LOCKS.lock() else {
        warn!(route = %key, "light write lock registry poisoned — using an isolated lock");
        return Arc::new(tokio::sync::Mutex::new(()));
    };
    locks
        .entry(key)
        .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
        .clone()
}

#[cfg(test)]
mod tests {
    use super::*;

    use openlogi_core::device::{LightValueRange, LightValueUnit};
    use openlogi_hid::replay::{
        ChannelConnection, NodePresence, OpenOutcome, RawWriterAvailability, ReplayBackend,
        ReplayNode, ReplayTopology,
    };
    use openlogi_hid::{LitraModel, NodeId, NodeInfo, device_io_channel, encode_litra_command};

    const PRODUCT_ID: u16 = 0xc900;
    const USAGE_PAGE: u16 = 0xff43;
    const USAGE_ID: u16 = 0x0202;

    #[tokio::test]
    async fn injected_light_uses_raw_writer_and_context_gate_without_opening_hidpp() {
        let node_id = NodeId::from("agent-litra-node".to_string());
        let route = DeviceRoute::RawHid {
            vendor_id: 0x046d,
            product_id: PRODUCT_ID,
            usage_page: USAGE_PAGE,
            usage_id: USAGE_ID,
            identity: "serial:agent-litra".to_string(),
        };
        let backend = Arc::new(
            ReplayBackend::new(litra_topology(node_id.clone()), Vec::new())
                .expect("valid raw-light replay topology"),
        );
        let writer = backend
            .raw_writer_handle(&node_id)
            .expect("known raw-light node");
        let (device_io_signal, device_io) = device_io_channel();
        let hardware = HardwareContext::injected(backend.clone(), device_io);

        hardware
            .apply_light(&route, LightCommand::TemperatureKelvin(4600))
            .await
            .expect("semantic light command succeeds through replay raw writer");
        let mut expected = vec![0; 20];
        expected[..6].copy_from_slice(&[0x11, 0xff, 0x04, 0x9c, 0x11, 0xf8]);
        assert_eq!(writer.written_reports(), vec![expected]);
        assert_eq!(
            backend.open_count(&node_id).expect("known raw-light node"),
            0,
            "a standalone light command must not open a HID++ channel"
        );

        writer.set_connection(ChannelConnection::Disconnected);
        let disconnected = hardware
            .apply_light(&route, LightCommand::Power(false))
            .await
            .expect_err("a disconnected raw writer must fail");
        assert!(matches!(
            disconnected,
            WriteError::Hid(message) if message.contains("not connected")
        ));
        assert_eq!(writer.written_reports().len(), 1);

        writer.set_connection(ChannelConnection::Connected);
        assert!(device_io_signal.suspend());
        let suspended = hardware
            .apply_light(&route, LightCommand::Power(false))
            .await
            .expect_err("the context gate must stop raw-light I/O");
        assert!(matches!(suspended, WriteError::DeviceNotFound));
        assert_eq!(writer.written_reports().len(), 1);
        assert_eq!(
            backend.open_count(&node_id).expect("known raw-light node"),
            0
        );
        backend
            .require_complete()
            .expect("raw-light replay has no HID++ cassette traffic");
    }

    fn litra_topology(node_id: NodeId) -> ReplayTopology {
        ReplayTopology {
            nodes: vec![ReplayNode {
                info: NodeInfo {
                    id: node_id,
                    vendor_id: 0x046d,
                    product_id: PRODUCT_ID,
                    usage_page: USAGE_PAGE,
                    usage_id: USAGE_ID,
                    name: "Agent Replay Litra Glow".to_string(),
                    manufacturer: Some("Logitech".to_string()),
                    serial_number: Some("AGENT-LITRA".to_string()),
                },
                presence: NodePresence::Present,
                open_outcome: OpenOutcome::NotHidpp,
                channel: None,
                raw_writer: RawWriterAvailability::Capture,
                receiver_slots: Vec::new(),
            }],
            channels: Vec::new(),
        }
    }

    fn route() -> DeviceRoute {
        DeviceRoute::RawHid {
            vendor_id: 0x046d,
            product_id: 0xc900,
            usage_page: 0xff43,
            usage_id: 0x0202,
            identity: "serial:light-write-test".into(),
        }
    }

    fn request(settings: LightSettings) -> LightApplyRequest {
        LightApplyRequest {
            settings,
            capabilities: LightCapabilities {
                power: true,
                brightness: Some(LightValueRange::new(20, 250, 1, LightValueUnit::Lumens).unwrap()),
                temperature: Some(
                    LightValueRange::new(2700, 6500, 100, LightValueUnit::Kelvin).unwrap(),
                ),
                ..LightCapabilities::default()
            },
            generation: 1,
            hardware: HardwareContext::injected(
                Arc::new(
                    ReplayBackend::new(
                        ReplayTopology {
                            nodes: Vec::new(),
                            channels: Vec::new(),
                        },
                        Vec::new(),
                    )
                    .expect("valid empty replay topology"),
                ),
                device_io_channel().1,
            ),
        }
    }

    #[tokio::test]
    async fn successful_reapply_preserves_power_order() {
        for (enabled, expected) in [
            (
                true,
                vec![
                    LightCommand::Power(true),
                    LightCommand::BrightnessPercent(60),
                    LightCommand::TemperatureKelvin(4600),
                ],
            ),
            (
                false,
                vec![
                    LightCommand::BrightnessPercent(60),
                    LightCommand::TemperatureKelvin(4600),
                    LightCommand::Power(false),
                ],
            ),
        ] {
            let request = request(LightSettings::new(enabled, 60, Some(4600)));
            let mut written = Vec::new();
            let result =
                apply_light_settings(&route(), &request, &AtomicU64::new(1), async |command| {
                    encode_litra_command(LitraModel::Glow, command)?;
                    written.push(command);
                    Ok(())
                })
                .await;

            assert!(result.unwrap());
            assert_eq!(written, expected);
        }
    }

    #[tokio::test]
    async fn rejected_temperature_does_not_prevent_power_off() {
        // Config accepts 2750 K, but the Litra encoder requires 100 K steps.
        let request = request(LightSettings::new(false, 60, Some(2750)));
        let mut written = Vec::new();
        let result =
            apply_light_settings(&route(), &request, &AtomicU64::new(1), async |command| {
                encode_litra_command(LitraModel::Glow, command)?;
                written.push(command);
                Ok(())
            })
            .await;

        assert_eq!(
            written,
            vec![
                LightCommand::BrightnessPercent(60),
                LightCommand::Power(false)
            ]
        );
        assert!(
            matches!(result, Err(WriteError::InvalidLightValue { control, value: 2750 }) if control == "temperature_kelvin")
        );
    }

    #[tokio::test]
    async fn failed_brightness_skips_to_power_off_and_retains_first_error() {
        for power_off_result in [Ok(()), Err(WriteError::DeviceNotFound)] {
            let request = request(LightSettings::new(false, 60, Some(4600)));
            let mut attempted = Vec::new();
            let result =
                apply_light_settings(&route(), &request, &AtomicU64::new(1), async |command| {
                    attempted.push(command);
                    if command == LightCommand::BrightnessPercent(60) {
                        Err(WriteError::RequestTimedOut {
                            operation: HidppOperation::Light,
                        })
                    } else {
                        power_off_result.clone()
                    }
                })
                .await;

            assert_eq!(
                attempted,
                vec![
                    LightCommand::BrightnessPercent(60),
                    LightCommand::Power(false)
                ]
            );
            assert!(matches!(
                result,
                Err(WriteError::RequestTimedOut {
                    operation: HidppOperation::Light
                })
            ));
        }
    }

    #[tokio::test]
    async fn switching_on_still_stops_at_the_first_failure() {
        for (failed, expected) in [
            (LightCommand::Power(true), vec![LightCommand::Power(true)]),
            (
                LightCommand::BrightnessPercent(60),
                vec![
                    LightCommand::Power(true),
                    LightCommand::BrightnessPercent(60),
                ],
            ),
        ] {
            let request = request(LightSettings::new(true, 60, Some(4600)));
            let mut attempted = Vec::new();
            let result =
                apply_light_settings(&route(), &request, &AtomicU64::new(1), async |command| {
                    attempted.push(command);
                    if command == failed {
                        Err(WriteError::DeviceNotFound)
                    } else {
                        Ok(())
                    }
                })
                .await;

            assert_eq!(attempted, expected);
            assert!(matches!(result, Err(WriteError::DeviceNotFound)));
        }
    }

    #[tokio::test]
    async fn suspension_stops_writes_even_during_power_off_recovery() {
        for first_result in [Ok(()), Err(WriteError::DeviceNotFound)] {
            let (signal, device_io) = device_io_channel();
            let mut request = request(LightSettings::new(false, 60, Some(4600)));
            request.hardware = HardwareContext::injected(
                Arc::new(
                    ReplayBackend::new(
                        ReplayTopology {
                            nodes: Vec::new(),
                            channels: Vec::new(),
                        },
                        Vec::new(),
                    )
                    .expect("valid empty replay topology"),
                ),
                device_io,
            );
            let mut attempted = Vec::new();
            let result =
                apply_light_settings(&route(), &request, &AtomicU64::new(1), async |command| {
                    attempted.push(command);
                    assert!(signal.suspend());
                    first_result.clone()
                })
                .await;

            assert_eq!(attempted, vec![LightCommand::BrightnessPercent(60)]);
            match first_result {
                Ok(()) => assert!(!result.unwrap()),
                Err(_) => assert!(matches!(result, Err(WriteError::DeviceNotFound))),
            }
        }
    }

    #[tokio::test]
    async fn canceled_reapply_is_rechecked_after_acquiring_the_write_lock() {
        let route = route();
        let request = request(LightSettings::new(false, 60, Some(4600)));
        let generation = AtomicU64::new(1);
        let lock = light_write_lock(&route);
        let guard = lock.lock().await;
        let mut attempted = Vec::new();
        let result = {
            let apply = apply_light_settings(&route, &request, &generation, async |command| {
                attempted.push(command);
                Ok(())
            });
            let mut apply = std::pin::pin!(apply);
            assert!(
                futures_lite::future::poll_once(apply.as_mut())
                    .await
                    .is_none()
            );
            generation.store(2, Ordering::Release);
            drop(guard);
            apply.await
        };

        assert!(!result.unwrap());
        assert!(attempted.is_empty());
    }
}
