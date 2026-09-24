//! The Batteries widget exists only on macOS. Every operation is a no-op.

use openlogi_agent_core::observable::ObservableState;
use openlogi_agent_core::orchestrator::Orchestrator;
use tokio::sync::{Mutex, watch};

/// Stand-in for the macOS publisher.
pub(crate) struct PowerSources;

#[expect(
    clippy::unused_self,
    clippy::unused_async,
    reason = "mirrors the macOS signatures so the lifecycle needs no platform checks"
)]
impl PowerSources {
    pub(crate) fn new() -> Self {
        Self
    }

    pub(crate) fn watch_config(&mut self, _changes: watch::Receiver<()>) {}

    /// Never resolves: there is nothing to publish.
    pub(crate) async fn wake(&mut self) {
        std::future::pending::<()>().await;
    }

    pub(crate) async fn reconcile(
        &mut self,
        _orchestrator: &Mutex<Orchestrator>,
        _observable: &ObservableState,
    ) {
    }

    pub(crate) fn clear_all(&mut self) {}
}
