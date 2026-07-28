//! Keep configured keyboard → pointing-device host-switch links armed.

use std::sync::{Arc, RwLock};
use std::thread;
use std::time::Duration;

use openlogi_hid::{DeviceRoute, run_host_switch_session};
use tokio::sync::{mpsc, oneshot};
use tracing::{debug, warn};

/// One resolved link. Config keys are converted to live routes by the
/// orchestrator so the transport watcher never needs to understand inventory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostSwitchLink {
    /// Keyboard whose host switch keys initiate the transition.
    pub keyboard: DeviceRoute,
    /// Pointing devices that follow the keyboard.
    pub targets: Vec<DeviceRoute>,
}

/// Shared resolved links, refreshed with config and inventory.
pub type HostSwitchLinks = Arc<RwLock<Vec<HostSwitchLink>>>;

/// Spawn the host switch session manager.
pub fn spawn(links: HostSwitchLinks) {
    thread::spawn(move || {
        let runtime = match tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
        {
            Ok(runtime) => runtime,
            Err(error) => {
                warn!(%error, "host switch watcher: could not build tokio runtime");
                return;
            }
        };
        runtime.block_on(manage(links));
    });
}

async fn manage(links: HostSwitchLinks) {
    let mut current = Vec::new();
    let mut stops = Vec::new();
    let (done_tx, mut done_rx) = mpsc::unbounded_channel();
    let mut ticker = tokio::time::interval(Duration::from_secs(1));

    loop {
        tokio::select! {
            _ = ticker.tick() => {
                let wanted = links.read().map_or_else(|_| Vec::new(), |guard| guard.clone());
                if wanted == current {
                    continue;
                }
                stop_all(&mut stops);
                current.clone_from(&wanted);
                for link in wanted {
                    let (stop_tx, stop_rx) = oneshot::channel();
                    stops.push(stop_tx);
                    let done = done_tx.clone();
                    tokio::spawn(async move {
                        let keyboard = link.keyboard.clone();
                        if let Err(error) =
                            run_host_switch_session(link.keyboard, link.targets, stop_rx).await
                        {
                            debug!(%error, route = %keyboard, "host switch session ended");
                        }
                        let _ = done.send(());
                    });
                }
            }
            Some(()) = done_rx.recv() => {
                // A host switch intentionally disconnects the keyboard. Clear
                // the local snapshot so the next tick attempts to arm it again;
                // this also recovers transient setup/read failures.
                stop_all(&mut stops);
                current.clear();
            }
        }
    }
}

fn stop_all(stops: &mut Vec<oneshot::Sender<()>>) {
    for stop in stops.drain(..) {
        let _ = stop.send(());
    }
}
