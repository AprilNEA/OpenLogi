//! Serialized standalone-light writes and reconnect re-application.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, LazyLock, Mutex, mpsc};
use std::thread;

use openlogi_core::config::LightSettings;
use openlogi_core::device::LightCapabilities;
use openlogi_hid::{
    DeviceRoute, HidppOperation, LightCommand, LitraModel, WriteError, commands_for_light_settings,
};
use tracing::{debug, info, warn};

struct LightApplyRequest {
    settings: LightSettings,
    capabilities: LightCapabilities,
    generation: u64,
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
pub fn set_light_in_background(
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
pub fn cancel_light_reapply(target: &DeviceRoute) {
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
        let result = rt.block_on(apply_light_settings(
            &target,
            &request.settings,
            request.capabilities,
            &generation,
            request.generation,
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
    light: &LightSettings,
    capabilities: LightCapabilities,
    generation: &AtomicU64,
    expected_generation: u64,
) -> Result<bool, WriteError> {
    let lock = light_write_lock(target);
    let _guard = lock.lock().await;
    // The request may have passed the queue check while an explicit command
    // held the route lock. Re-check under that lock before writing anything so
    // a canceled re-apply cannot overwrite the newer explicit state.
    if generation.load(Ordering::Acquire) != expected_generation {
        return Ok(false);
    }
    for command in commands_for_light_settings(*light, capabilities) {
        apply_light_unlocked(target, command).await?;
    }
    Ok(true)
}

/// Apply a semantic command to a supported standalone light.
pub async fn apply_light(route: &DeviceRoute, command: LightCommand) -> Result<(), WriteError> {
    let lock = light_write_lock(route);
    let _guard = lock.lock().await;
    apply_light_unlocked(route, command).await
}

async fn apply_light_unlocked(
    route: &DeviceRoute,
    command: LightCommand,
) -> Result<(), WriteError> {
    let Some(model) = LitraModel::from_route(route) else {
        return Err(WriteError::LightUnsupported {
            control: "raw_hid_route".into(),
        });
    };
    super::timed(
        HidppOperation::Light,
        openlogi_hid::apply_litra(route, model, command),
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
