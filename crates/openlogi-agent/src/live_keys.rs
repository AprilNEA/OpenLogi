//! Read the live keypad keys' sources and hand the values to the
//! orchestrator, which draws them. One look a second; each key is read only
//! as often as its own interval says. Reads run concurrently off the async
//! threads, because a source may be a command, and each value is drawn as
//! soon as it is read: a slow command delays no other key, and an active
//! key's time runs out on schedule whatever is being read. The same tick
//! reads the key pictures' file versions — off the orchestrator's lock — so
//! a key whose picture was replaced on disk is redrawn.

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use openlogi_agent_core::live_keys::{LiveReader, read_source};
use openlogi_agent_core::orchestrator::Orchestrator;
use openlogi_agent_core::watchers::key_display::PictureRevisions;
use tokio::sync::{Mutex, mpsc};
use tokio::time::{Interval, MissedTickBehavior};
use tracing::debug;

/// How often the watcher looks for due keys and ended activations.
const TICK: Duration = Duration::from_secs(1);

/// Keep the live keys current until the process exits.
pub(crate) async fn watch(orchestrator: Arc<Mutex<Orchestrator>>) {
    let mut reader = LiveReader::new(read_source);
    let mut tick = live_tick();
    let (pictures_tx, mut pictures_rx) = mpsc::unbounded_channel();
    // At most one picture-version read at a time: one stuck on an
    // unavailable share holds back only the next one, never the watcher.
    let mut reading_pictures = false;
    loop {
        tokio::select! {
            _ = tick.tick() => {
                let (reads, pictures) = {
                    let mut orchestrator = orchestrator.lock().await;
                    let now = Instant::now();
                    orchestrator.expire_live_keys(now);
                    let pictures = (!reading_pictures).then(|| orchestrator.key_picture_paths());
                    (orchestrator.live_key_reads(now), pictures)
                };
                reader.start(reads);
                if let Some(paths) = pictures {
                    reading_pictures = true;
                    read_picture_revisions(paths, pictures_tx.clone());
                }
            }
            Some(done) = reader.next() => {
                orchestrator.lock().await.set_live_value(done);
            }
            Some(revisions) = pictures_rx.recv() => {
                reading_pictures = false;
                if let Some(revisions) = revisions {
                    orchestrator.lock().await.set_picture_revisions(revisions);
                }
            }
        }
    }
}

/// Read the versions of the key pictures at `paths` off the async threads,
/// without holding the orchestrator, and hand them back over `done`; `None`
/// when the read panicked, which leaves the versions as they were.
fn read_picture_revisions(
    paths: BTreeSet<PathBuf>,
    done: mpsc::UnboundedSender<Option<PictureRevisions>>,
) {
    tokio::spawn(async move {
        let revisions = tokio::task::spawn_blocking(move || PictureRevisions::read(paths))
            .await
            .ok();
        if done.send(revisions).is_err() {
            debug!("live-key watcher gone — picture versions dropped");
        }
    });
}

/// The watcher's schedule: every [`TICK`], the first one a full period after
/// the agent starts — the orchestrator publishes its first key faces itself,
/// so nothing is due at once — and a late tick delays the next rather than
/// bursting to catch up.
fn live_tick() -> Interval {
    let mut tick = tokio::time::interval_at(tokio::time::Instant::now() + TICK, TICK);
    tick.set_missed_tick_behavior(MissedTickBehavior::Delay);
    tick
}

#[cfg(test)]
mod tests {
    use super::*;

    /// With the clock paused, Tokio jumps straight to the next deadline: the
    /// first tick must land a full period after the watcher starts, not at
    /// once. Each wait is bounded, so a schedule that stops ticking fails
    /// here instead of hanging the test run.
    #[tokio::test(start_paused = true)]
    async fn live_watcher_waits_one_period() {
        let start = tokio::time::Instant::now();
        let mut tick = live_tick();
        tokio::time::timeout(TICK * 2, tick.tick())
            .await
            .expect("the first live tick never came");
        assert_eq!(start.elapsed(), TICK, "the first tick comes one period in");
        tokio::time::timeout(TICK * 2, tick.tick())
            .await
            .expect("the second live tick never came");
        assert_eq!(start.elapsed(), TICK * 2);
    }
}
