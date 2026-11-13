//! Read the live keypad keys' sources and hand the values to the
//! orchestrator, which draws them. One look a second; each key is read only
//! as often as its own interval says. Reads run concurrently off the async
//! threads, because a source may be a command, and each value is drawn as
//! soon as it is read: a slow command delays no other key, and an active
//! key's time runs out on schedule whatever is being read.

use std::sync::Arc;
use std::time::{Duration, Instant};

use openlogi_agent_core::live_keys::{LiveReader, read_source};
use openlogi_agent_core::orchestrator::Orchestrator;
use tokio::sync::Mutex;
use tokio::time::MissedTickBehavior;

/// How often the watcher looks for due keys and ended activations.
const TICK: Duration = Duration::from_secs(1);

/// Keep the live keys current until the process exits.
pub(crate) async fn watch(orchestrator: Arc<Mutex<Orchestrator>>) {
    let mut reader = LiveReader::new(read_source);
    let mut tick = tokio::time::interval(TICK);
    tick.set_missed_tick_behavior(MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            _ = tick.tick() => {
                let reads = {
                    let mut orchestrator = orchestrator.lock().await;
                    let now = Instant::now();
                    orchestrator.expire_live_keys(now);
                    orchestrator.live_key_reads(now)
                };
                reader.start(reads);
            }
            Some(done) = reader.next() => {
                orchestrator.lock().await.set_live_value(done);
            }
        }
    }
}
