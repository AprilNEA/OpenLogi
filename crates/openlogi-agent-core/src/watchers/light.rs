//! Watch standalone lights for changes made on the light itself.
//!
//! The orchestrator publishes the online Litra lights. This manager keeps one
//! task per light that watches it over HID++ `0x1990` and sends what the
//! light reads to the agent's event loop, which keeps those values over the
//! saved settings until the GUI saves them — Logi Options+ treats a change
//! made with the light's buttons as the new setting too. A light reached only
//! through its raw report announces nothing, so its task ends.

use std::sync::Arc;
use std::time::Duration;

use openlogi_core::device::LightState;
use openlogi_hid::{DeviceRoute, LightChange, LitraModel};
use tokio::sync::{mpsc, oneshot, watch};
use tokio::task::JoinHandle;
use tracing::debug;

use super::shutdown::{ManagerCompletion, WatcherHandle};
use crate::hardware::HardwareContext;

/// An online light the agent watches.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WatchedLight {
    pub route: DeviceRoute,
    pub model: LitraModel,
}

/// The lights to watch, as the orchestrator publishes them.
pub type WatchedLights = watch::Receiver<Arc<Vec<WatchedLight>>>;

/// What a watched light reported.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LightEvent {
    /// The light was read as its watch began.
    Read {
        route: DeviceRoute,
        state: LightState,
    },
    /// The light announced a change.
    Changed {
        route: DeviceRoute,
        change: LightChange,
    },
}

/// How long a light waits to be watched again after its watch failed.
const RETRY: Duration = Duration::from_secs(5);
/// How long a light waits to be watched again after its link was replaced.
const REWATCH: Duration = Duration::from_millis(250);

/// Spawn the light watcher manager.
#[must_use]
pub fn spawn(
    hardware: HardwareContext,
    lights: &WatchedLights,
    events: mpsc::UnboundedSender<LightEvent>,
) -> WatcherHandle {
    let lights = lights.clone();
    WatcherHandle::spawn("openlogi-light-watcher", move |shutdown| {
        manage(hardware, lights, events, shutdown)
    })
}

async fn manage(
    hardware: HardwareContext,
    mut lights: WatchedLights,
    events: mpsc::UnboundedSender<LightEvent>,
    mut shutdown: oneshot::Receiver<()>,
) -> ManagerCompletion {
    let mut tasks = Vec::new();
    loop {
        let wanted = Arc::clone(&lights.borrow_and_update());
        sync(&hardware, &wanted, &events, &mut tasks);
        tokio::select! {
            _ = &mut shutdown => break,
            changed = lights.changed() => {
                if changed.is_err() {
                    // The orchestrator is gone; nothing new will be wanted.
                    sync(&hardware, &[], &events, &mut tasks);
                    let _ = (&mut shutdown).await;
                    break;
                }
            }
        }
    }
    sync(&hardware, &[], &events, &mut tasks);
    ManagerCompletion::Graceful
}

/// Stop the tasks of lights no longer wanted and start one for each new one.
/// A task that ended — its light is reached only raw — stays in place, so it
/// is not started again until the light goes away and comes back.
fn sync(
    hardware: &HardwareContext,
    wanted: &[WatchedLight],
    events: &mpsc::UnboundedSender<LightEvent>,
    tasks: &mut Vec<(WatchedLight, JoinHandle<()>)>,
) {
    tasks.retain(|(light, task)| {
        let keep = wanted.contains(light);
        if !keep {
            task.abort();
        }
        keep
    });
    for light in wanted {
        if tasks.iter().any(|(watched, _)| watched == light) {
            continue;
        }
        let task = tokio::spawn(watch_light(hardware.clone(), light.clone(), events.clone()));
        tasks.push((light.clone(), task));
    }
}

/// Watch one light until it turns out to be reachable only raw or the agent
/// stops listening.
async fn watch_light(
    hardware: HardwareContext,
    light: WatchedLight,
    events: mpsc::UnboundedSender<LightEvent>,
) {
    loop {
        let wait = match hardware.watch_litra(&light.route, light.model).await {
            Ok(Some(mut watch)) => {
                let read = LightEvent::Read {
                    route: light.route.clone(),
                    state: watch.state(),
                };
                if events.send(read).is_err() {
                    return;
                }
                while let Some(change) = watch.next().await {
                    let changed = LightEvent::Changed {
                        route: light.route.clone(),
                        change,
                    };
                    if events.send(changed).is_err() {
                        return;
                    }
                }
                debug!(route = %light.route, "the Litra link was replaced — watching it again");
                REWATCH
            }
            Ok(None) => {
                debug!(route = %light.route, "the Litra is reached only raw — nothing to watch");
                return;
            }
            Err(error) => {
                debug!(route = %light.route, %error, "could not watch the Litra — trying again");
                RETRY
            }
        };
        tokio::time::sleep(wait).await;
    }
}

#[cfg(test)]
mod tests;
