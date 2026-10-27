//! Backlight writes and publication to the other connected clients.

use std::sync::Arc;

use openlogi_agent_core::orchestrator::{Orchestrator, SharedHandles};
use openlogi_hid::{BacklightChange, BacklightState, DeviceRoute, WriteError};
use tokio::sync::Mutex;

/// Keep the ordered hardware attempt and its settings invalidation together,
/// independently of the requesting client's lifetime.
pub(super) async fn write(
    orchestrator: Arc<Mutex<Orchestrator>>,
    shared: &SharedHandles,
    route: DeviceRoute,
    change: BacklightChange,
) -> Result<BacklightState, WriteError> {
    // Admission is synchronous, before the worker can be scheduled behind a
    // later request. Keep the write and its notification alive when tarpc
    // drops the caller: the keyboard may already have applied the command.
    let write = shared.set_backlight(&route, change);
    tokio::spawn(async move {
        let result = write.await;
        orchestrator.lock().await.announce_settings_changed(&route);
        result
    })
    .await
    .map_err(|_| WriteError::AgentUnavailable)?
}

#[cfg(test)]
mod tests;
