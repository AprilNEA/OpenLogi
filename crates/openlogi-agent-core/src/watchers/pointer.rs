//! Off-tap pointer-window discovery with a coalescing publication.

use std::{thread, time::Duration};

use futures_lite::future;
use openlogi_hook::{PointerContext, PointerTarget};
use tokio::sync::watch;
use tracing::warn;

/// Whether anything reads the pointer context, as the orchestrator publishes
/// it from the mouse-profile policy.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PointerDemand {
    /// Mouse bindings follow the window under the pointer, so it is sampled.
    Wanted,
    /// Nothing reads the context; the watcher parks without sampling.
    Idle,
}

/// The pointer-context demand, as the orchestrator publishes it.
pub type PointerDemands = watch::Receiver<PointerDemand>;

/// How often the window under the pointer is sampled while it is wanted.
const PERIOD: Duration = Duration::from_millis(50);

/// Observe the window or desktop under the pointer while `demand` wants it.
/// Only context transitions are published; pointer motion within one window
/// does not rebuild mappings. Native queries never run on the input callback,
/// and a slow consumer retains only the latest snapshot rather than an
/// unbounded history of hover changes.
#[must_use]
pub fn spawn(demand: &PointerDemands) -> watch::Receiver<PointerContext> {
    let supported = openlogi_hook::pointer_context_supported();
    let (tx, rx) = watch::channel(PointerContext {
        app: None,
        target: if supported {
            PointerTarget::Unavailable
        } else {
            PointerTarget::Unsupported
        },
    });
    if !supported {
        return rx;
    }
    let demand = demand.clone();
    let spawned = thread::Builder::new()
        .name("openlogi-pointer-context".into())
        .spawn(move || track(&tx, demand, openlogi_hook::pointer_context, PERIOD));
    if let Err(error) = spawned {
        warn!(%error, "could not start pointer context watcher — pointer context remains unidentified");
    }
    rx
}

/// Sample every `period` while the demand wants it and park while it is
/// idle, until the consumer stops listening.
fn track(
    tx: &watch::Sender<PointerContext>,
    mut demand: PointerDemands,
    mut sample: impl FnMut() -> PointerContext,
    period: Duration,
) {
    while !tx.is_closed() {
        if *demand.borrow_and_update() == PointerDemand::Idle {
            // The last sample goes stale while parked. Withdrawing it makes
            // the first sample after resuming a transition, even when the
            // pointer is back over the window it left.
            publish(
                tx,
                PointerContext {
                    app: None,
                    target: PointerTarget::Unavailable,
                },
            );
            if !park(tx, &mut demand) {
                return;
            }
            continue;
        }
        publish(tx, sample());
        thread::sleep(period);
    }
}

fn publish(tx: &watch::Sender<PointerContext>, current: PointerContext) {
    tx.send_if_modified(|previous| {
        if *previous == current {
            return false;
        }
        *previous = current;
        true
    });
}

/// Block until the demand changes. `false` once the consumer stopped
/// listening; a demand that can no longer change parks until then.
fn park(tx: &watch::Sender<PointerContext>, demand: &mut PointerDemands) -> bool {
    future::block_on(async {
        let changed = future::or(async { demand.changed().await.is_ok() }, async {
            tx.closed().await;
            false
        })
        .await;
        if !changed {
            tx.closed().await;
        }
        changed
    })
}

#[cfg(test)]
mod tests;
