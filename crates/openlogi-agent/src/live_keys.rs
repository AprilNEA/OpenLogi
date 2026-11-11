//! Read the live keypad keys' sources and hand the values to the
//! orchestrator, which draws them. One look a second; each key is read only
//! as often as its own interval says, and the reads run off the async
//! threads because a source may be a command.

use std::sync::Arc;
use std::time::{Duration, Instant};

use openlogi_agent_core::live_keys::read_source;
use openlogi_agent_core::orchestrator::Orchestrator;
use tokio::sync::Mutex;

/// How often the watcher looks for due keys and ended activations.
const TICK: Duration = Duration::from_secs(1);

/// Keep the live keys current until the process exits.
pub(crate) async fn watch(orchestrator: Arc<Mutex<Orchestrator>>) {
    loop {
        let reads = orchestrator.lock().await.live_key_reads(Instant::now());
        let values = if reads.is_empty() {
            Vec::new()
        } else {
            tokio::task::spawn_blocking(move || {
                reads
                    .iter()
                    .map(|read| (read.id.clone(), read_source(read)))
                    .collect()
            })
            .await
            .unwrap_or_default()
        };
        orchestrator
            .lock()
            .await
            .set_live_values(values, Instant::now());
        tokio::time::sleep(TICK).await;
    }
}
