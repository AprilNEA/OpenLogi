//! Serialized G600 onboard-DPI reads and writes.
//!
//! The G600 stores a profile report in onboard flash after accepting it, and
//! any request that reaches it in the meantime corrupts the stored profile.
//! The driver waits that out inside one profile write, a failed one that may
//! still have landed included, but only one caller at a time can rely on
//! that: every G600 operation on a route therefore holds that route's lock
//! for its whole driver call.
//!
//! Each operation runs as its own task, so a client that stops waiting (its
//! deadline passed, its connection dropped) cancels nothing: the task keeps
//! the lock until the driver call, commit wait included, has finished.

use std::collections::HashMap;
use std::future::Future;
use std::sync::{Arc, LazyLock, Mutex, PoisonError};

use openlogi_core::hid::g600::{DpiSettings, DpiStage, DpiState, OnboardProfile, StageAfterStore};
use openlogi_hid::{DeviceRoute, WriteError};
use tracing::warn;

use super::HardwareContext;

type G600Lock = Arc<tokio::sync::Mutex<()>>;

/// One lock per G600 route, shared by every operation on it.
static G600_LOCKS: LazyLock<Mutex<HashMap<String, G600Lock>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

pub(super) async fn read_dpi(
    hardware: &HardwareContext,
    route: &DeviceRoute,
) -> Result<DpiState, WriteError> {
    serialized(hardware, route, |hardware, route| async move {
        openlogi_hid::write::read_g600_dpi(hardware.backend(), &route)
            .await
            .map_err(|error| logged(&route, "read", error))
    })
    .await
}

pub(super) async fn set_profile_dpi(
    hardware: &HardwareContext,
    route: &DeviceRoute,
    profile: OnboardProfile,
    settings: DpiSettings,
    after: StageAfterStore,
) -> Result<(), WriteError> {
    serialized(hardware, route, move |hardware, route| async move {
        openlogi_hid::write::set_g600_profile_dpi(
            hardware.backend(),
            &route,
            profile,
            settings,
            after,
        )
        .await
        .map_err(|error| logged(&route, "profile write", error))
    })
    .await
}

pub(super) async fn set_active_stage(
    hardware: &HardwareContext,
    route: &DeviceRoute,
    stage: DpiStage,
) -> Result<(), WriteError> {
    serialized(hardware, route, move |hardware, route| async move {
        openlogi_hid::write::set_g600_active_stage(hardware.backend(), &route, stage)
            .await
            .map_err(|error| logged(&route, "stage selection", error))
    })
    .await
}

/// Run `operation` on its own task under `route`'s G600 lock, once the
/// host lets device I/O through.
async fn serialized<T, F, Fut>(
    hardware: &HardwareContext,
    route: &DeviceRoute,
    operation: F,
) -> Result<T, WriteError>
where
    T: Send + 'static,
    F: FnOnce(HardwareContext, DeviceRoute) -> Fut + Send + 'static,
    Fut: Future<Output = Result<T, WriteError>> + Send,
{
    if !hardware.device_io().allows_io() {
        return Err(WriteError::DeviceNotFound);
    }
    let lock = g600_lock(route);
    let hardware = hardware.clone();
    let route = route.clone();
    let task = tokio::spawn(async move {
        let _guard = lock.lock().await;
        if !hardware.device_io().allows_io() {
            return Err(WriteError::DeviceNotFound);
        }
        operation(hardware, route).await
    });
    match task.await {
        Ok(result) => result,
        Err(error) if error.is_panic() => std::panic::resume_unwind(error.into_panic()),
        Err(error) => Err(WriteError::Hid(format!("G600 operation stopped: {error}"))),
    }
}

/// Log `error` with the causes the IPC form drops, and convert it.
fn logged<E>(route: &DeviceRoute, operation: &str, error: E) -> WriteError
where
    E: std::fmt::Debug + Into<WriteError>,
{
    warn!(%route, operation, ?error, "G600 operation failed");
    error.into()
}

fn g600_lock(route: &DeviceRoute) -> G600Lock {
    let key = route.to_string();
    G600_LOCKS
        .lock()
        .unwrap_or_else(|poisoned| {
            warn!(route = %key, "G600 lock registry poisoned — recovering it");
            PoisonError::into_inner(poisoned)
        })
        .entry(key)
        .or_default()
        .clone()
}

#[cfg(test)]
mod tests;
