//! Open HID++ links to Litra lights, shared by commands and watchers.
//!
//! A light reached over HID++ `0x1990` keeps its channel open: later commands
//! reuse it instead of resolving the feature again, and a [`LightWatch`] hears
//! the light's own notifications on it, so the light has one writer. Logi
//! Options+ does the same: its light keeps a cache that both its setters and
//! the light's notifications update.
//!
//! A watcher must tell OpenLogi's own writes from a change made on the light,
//! because the light announces both. A command records the value it wrote
//! before it releases the route's device lock, and the watcher reads the light
//! under that lock, so the echo of a write compares equal and is not reported.

use std::collections::HashMap;
use std::sync::{Arc, LazyLock, Mutex, MutexGuard, PoisonError};

use futures_lite::StreamExt;
use futures_lite::stream::Boxed;
use hidpp::feature::illumination::IlluminationEvent;
use openlogi_core::device::LightState;
use tokio::sync::watch;
use tracing::debug;

use crate::backend::NodeId;
use crate::channel::route::DeviceRoute;

use super::illumination::{Illumination, RESOLVE_TIMEOUT};
use super::{LightCommand, device_lock};

/// A link is kept per route and OS node: a light that comes back as another
/// node gets a new link.
pub(super) type LinkKey = (String, NodeId);

/// A light reached over HID++ `0x1990`, kept open.
pub(super) struct LiveLink {
    pub(super) light: Illumination,
    /// What the light last read as, for the watcher to compare against.
    /// Written under the route's device lock.
    last: Mutex<LightState>,
    /// Set once a newer link replaces this one or it stops answering, so a
    /// watcher on it reconnects.
    retired: watch::Sender<bool>,
}

static LINKS: LazyLock<Mutex<HashMap<LinkKey, Arc<LiveLink>>>> = LazyLock::new(Mutex::default);

fn links() -> MutexGuard<'static, HashMap<LinkKey, Arc<LiveLink>>> {
    LINKS.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The link a command or watcher opened for `key`, if it is still kept.
pub(super) fn kept(key: &LinkKey) -> Option<Arc<LiveLink>> {
    links().get(key).cloned()
}

/// Keep `light` open for `key`, last read as `state`. A link the same route
/// had on another node is retired: the light came back as a new node.
pub(super) fn keep(key: LinkKey, light: Illumination, state: LightState) -> Arc<LiveLink> {
    let link = Arc::new(LiveLink {
        light,
        last: Mutex::new(state),
        retired: watch::Sender::new(false),
    });
    let mut links = links();
    links.retain(|(route, node), old| {
        let moved = *route == key.0 && *node != key.1;
        if moved {
            old.retired.send_replace(true);
        }
        !moved
    });
    if let Some(old) = links.insert(key, Arc::clone(&link)) {
        old.retired.send_replace(true);
    }
    link
}

/// Stop keeping the link for `key`, which stopped answering. A watcher on it
/// reconnects, and the next command opens the light again.
pub(super) fn retire(key: &LinkKey) {
    if let Some(old) = links().remove(key) {
        old.retired.send_replace(true);
    }
}

impl LiveLink {
    fn last(&self) -> MutexGuard<'_, LightState> {
        self.last.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Record the control `command` wrote at the value the light read back,
    /// so the watcher takes the light's echo for OpenLogi's own write. The
    /// other controls keep their last value: a change made on the light
    /// meanwhile is still the watcher's to report.
    pub(super) fn note_written(&self, command: LightCommand, state: &LightState) {
        let mut last = self.last();
        match command {
            LightCommand::Power(_) => last.on = state.on,
            LightCommand::BrightnessPercent(_) | LightCommand::BrightnessNative(_) => {
                last.brightness = state.brightness;
            }
            LightCommand::TemperatureKelvin(_) => {
                last.temperature_kelvin = state.temperature_kelvin;
            }
        }
    }
}

/// Which of a light's controls a change touched.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct LightControls {
    /// Whether the light was turned on or off.
    pub power: bool,
    /// Whether the brightness changed.
    pub brightness: bool,
    /// Whether the colour temperature changed.
    pub temperature: bool,
}

/// A light's state after it announced a change.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LightChange {
    /// What the light reads back now.
    pub state: LightState,
    /// The controls changed on the light itself — its buttons or another
    /// program — rather than by OpenLogi or by a brightness limit.
    pub on_light: LightControls,
}

/// The notifications of one light reached over HID++ `0x1990`.
pub struct LightWatch {
    route: DeviceRoute,
    link: Arc<LiveLink>,
    events: Boxed<IlluminationEvent>,
    retired: watch::Receiver<bool>,
    state: LightState,
}

impl LightWatch {
    /// Watch `link`, whose light was just read as `state`. Called under the
    /// route's device lock.
    pub(super) fn new(route: DeviceRoute, link: Arc<LiveLink>, state: LightState) -> Self {
        *link.last() = state;
        let events = link.light.listen().boxed();
        let retired = link.retired.subscribe();
        Self {
            route,
            link,
            events,
            retired,
            state,
        }
    }

    /// The light's state as last read.
    #[must_use]
    pub fn state(&self) -> LightState {
        self.state
    }

    /// Wait for the light to change, and read it.
    ///
    /// The light is read again rather than trusted from the notification, and
    /// only a value that differs from the last one read or written counts. A
    /// brightness the light lowered to a new limit is a change of state, not
    /// one made on the light.
    ///
    /// Returns `None` once the link is gone — the light stopped answering, or
    /// came back as another node — and the caller should watch it again.
    pub async fn next(&mut self) -> Option<LightChange> {
        loop {
            let event = tokio::select! {
                event = self.events.next() => event?,
                _ = self.retired.wait_for(|retired| *retired) => return None,
            };
            let _guard = device_lock(&self.route).await;
            let state = match tokio::time::timeout(RESOLVE_TIMEOUT, self.link.light.read_state())
                .await
            {
                Ok(Ok(state)) => state,
                Ok(Err(error)) => {
                    debug!(route = %self.route, %error, "could not read the Litra after its notification");
                    return None;
                }
                Err(_elapsed) => {
                    debug!(route = %self.route, "the Litra did not answer after its notification");
                    return None;
                }
            };
            let before = std::mem::replace(&mut *self.link.last(), state);
            self.state = state;
            if before == state {
                continue;
            }
            let limited = matches!(
                event,
                IlluminationEvent::BrightnessEffectiveMaxChanged(_)
                    | IlluminationEvent::BrightnessClamped { .. }
            );
            return Some(LightChange {
                state,
                on_light: LightControls {
                    power: before.on != state.on,
                    brightness: !limited && before.brightness != state.brightness,
                    temperature: before.temperature_kelvin != state.temperature_kelvin,
                },
            });
        }
    }
}
