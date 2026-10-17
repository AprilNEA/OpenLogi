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
//! under that lock, so the echo of a write is not a change on the light. The
//! watcher still reports the state the write read back, whether or not the
//! light announces the write.

use std::collections::HashMap;
use std::sync::{Arc, LazyLock, Mutex, MutexGuard, PoisonError};

use futures_lite::StreamExt;
use futures_lite::future::poll_once;
use futures_lite::stream::Boxed;
use hidpp::feature::illumination::{BrightnessClampedSource, IlluminationEvent};
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
    /// What the light last read as, with the controls OpenLogi wrote since,
    /// for the watcher to tell a change made on the light from its own write.
    /// Written under the route's device lock.
    last: Mutex<LightState>,
    /// What the light read back after OpenLogi's latest write.
    written: watch::Sender<LightState>,
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
        written: watch::Sender::new(state),
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
    /// meanwhile is still the watcher's to report. The whole state read back
    /// goes to the watcher too.
    pub(super) fn note_written(&self, command: LightCommand, state: &LightState) {
        {
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
        self.written.send_replace(*state);
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

/// A light's state after it changed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LightChange {
    /// What the light reads back now.
    pub state: LightState,
    /// The controls changed on the light itself — its buttons or another
    /// program — rather than by OpenLogi or by a brightness limit. None for a
    /// write OpenLogi made or a new brightness limit.
    pub on_light: LightControls,
}

/// Whether the light's brightness limit explains its brightness moving from
/// `before` to `after`: the light lowered it to a limit below the old
/// brightness or below the old limit, or raised it from the limit it sat at
/// once that limit rose or went away. A limit change the brightness does not
/// follow explains nothing, so a press read with it is still the user's.
fn limit_explains(before: &LightState, after: &LightState) -> bool {
    let (Some(was), Some(now)) = (before.brightness, after.brightness) else {
        return false;
    };
    let lowered = after.brightness_limit.is_some_and(|limit| {
        now == limit && (was > limit || before.brightness_limit.is_none_or(|old| limit < old))
    });
    let released = before.brightness_limit.is_some_and(|old| {
        was == old && now > was && after.brightness_limit.is_none_or(|limit| limit > old)
    });
    lowered || released
}

/// What woke a [`LightWatch`].
enum Woke {
    /// The light sent this notification.
    Notified(IlluminationEvent),
    /// OpenLogi wrote to the light.
    Written,
}

/// How many times one wake reads the light while notifications keep
/// arriving during the reads. The next call reads again without waiting.
const READS_PER_WAKE: usize = 3;

/// The notifications of one light reached over HID++ `0x1990`.
pub struct LightWatch {
    route: DeviceRoute,
    link: Arc<LiveLink>,
    events: Boxed<IlluminationEvent>,
    written: watch::Receiver<LightState>,
    retired: watch::Receiver<bool>,
    /// The state last reported.
    state: LightState,
    /// The brightness a clamp reported since the watcher last compared a
    /// read with the state before it, when a request rather than the light's
    /// button made it.
    clamped: Option<u16>,
    /// Notifications kept arriving while the light was read, so the next
    /// call reads it without waiting for another one.
    read_again: bool,
}

impl LightWatch {
    /// Watch `link`, whose light was just read as `state`. Called under the
    /// route's device lock.
    pub(super) fn new(route: DeviceRoute, link: Arc<LiveLink>, state: LightState) -> Self {
        *link.last() = state;
        let events = link.light.listen().fuse().boxed();
        let written = link.written.subscribe();
        let retired = link.retired.subscribe();
        Self {
            route,
            link,
            events,
            written,
            retired,
            state,
            clamped: None,
            read_again: false,
        }
    }

    /// The light's state as last read.
    #[must_use]
    pub fn state(&self) -> LightState {
        self.state
    }

    /// Wait for the light to change, and read it.
    ///
    /// After a notification the light is read again rather than trusted from
    /// the notification, and only a value that differs from the last one read
    /// or written counts as changed on the light. A brightness the light's
    /// limit explains — lowered to a lower limit, or back up once the limit
    /// it sat at rose — is a change of state, not one made on the light,
    /// whichever notification is read first. So is a brightness a clamp
    /// reports: a request above the limit leaves the light at its limit, which
    /// nobody chose; a clamp the light's button made still follows a press on
    /// it. A write OpenLogi made is reported
    /// with the state it read back and no control changed on the light, once,
    /// whether the light announces it or not — unless a read after a
    /// notification already took its place.
    ///
    /// A notification that arrives while the light is read makes the watcher
    /// read it again, so a press the read had already passed is not lost. A
    /// light that keeps notifying during the reads is read again on the next
    /// call, without waiting for another notification.
    ///
    /// Returns `None` once the link is gone — the light stopped answering, or
    /// came back as another node — and the caller should watch it again.
    pub async fn next(&mut self) -> Option<LightChange> {
        loop {
            if !std::mem::take(&mut self.read_again) {
                // In this order: a notification the light sent before a write
                // finished — its echo among them — is matched against the value
                // noted for the write before the write's own state is taken.
                let woke = tokio::select! {
                    biased;
                    _ = self.retired.wait_for(|retired| *retired) => return None,
                    event = self.events.next() => match event {
                        Some(event) => Woke::Notified(event),
                        None => return None,
                    },
                    Ok(()) = self.written.changed() => Woke::Written,
                };
                match woke {
                    Woke::Notified(event) => self.note(event),
                    Woke::Written => {
                        let state = *self.written.borrow_and_update();
                        match self.report(state, LightControls::default()) {
                            Some(change) => return Some(change),
                            None => continue,
                        }
                    }
                }
            }
            let _guard = device_lock(&self.route).await;
            // Each write read the light back and noted it under this lock, so
            // the read below is newer than every state a write noted so far.
            self.written.mark_unchanged();
            let mut state = self.read().await?;
            // A notification taken after a read may come from a control the
            // read had already passed, so read again until none is waiting:
            // the last read then shows every notification taken.
            let mut reads = 1;
            while self.take_queued().await {
                if reads == READS_PER_WAKE {
                    self.read_again = true;
                    break;
                }
                state = self.read().await?;
                reads += 1;
            }
            // A clamp taken after the last read may explain the next one.
            let clamped = if self.read_again {
                self.clamped
            } else {
                self.clamped.take()
            };
            let before = std::mem::replace(&mut *self.link.last(), state);
            // The limit shows in the state read even when another
            // notification is read before the light's own limit notification.
            let on_light = LightControls {
                power: before.on != state.on,
                brightness: before.brightness != state.brightness
                    && !limit_explains(&before, &state)
                    && clamped.is_none_or(|lumens| state.brightness != Some(lumens)),
                temperature: before.temperature_kelvin != state.temperature_kelvin,
            };
            if let Some(change) = self.report(state, on_light) {
                return Some(change);
            }
        }
    }

    /// Read the light, or `None` when it does not answer. Takes `&mut self`
    /// so that [`Self::next`] stays `Send`: the event stream is not `Sync`.
    async fn read(&mut self) -> Option<LightState> {
        match tokio::time::timeout(RESOLVE_TIMEOUT, self.link.light.read_state()).await {
            Ok(Ok(state)) => Some(state),
            Ok(Err(error)) => {
                debug!(route = %self.route, %error, "could not read the Litra after its notification");
                None
            }
            Err(_elapsed) => {
                debug!(route = %self.route, "the Litra did not answer after its notification");
                None
            }
        }
    }

    /// Take the notifications already waiting, and whether there were any.
    async fn take_queued(&mut self) -> bool {
        let mut took = false;
        while let Some(Some(event)) = poll_once(self.events.next()).await {
            self.note(event);
            took = true;
        }
        took
    }

    /// Keep the evidence of a clamp a request made. A clamp the button made,
    /// or a later brightness other than the clamped one, drops it: that
    /// clamp no longer explains the light's brightness.
    fn note(&mut self, event: IlluminationEvent) {
        match event {
            IlluminationEvent::BrightnessClamped { source, brightness } => {
                self.clamped = match source {
                    BrightnessClampedSource::Button => None,
                    _ => Some(brightness),
                };
            }
            IlluminationEvent::BrightnessChanged(lumens)
                if self.clamped.is_some_and(|clamped| clamped != lumens) =>
            {
                self.clamped = None;
            }
            _ => {}
        }
    }

    /// Take `state` as the one reported, and the change to report: one when
    /// it differs from the state last reported or a control was changed on
    /// the light.
    fn report(&mut self, state: LightState, on_light: LightControls) -> Option<LightChange> {
        let reported = std::mem::replace(&mut self.state, state);
        (reported != state || on_light != LightControls::default())
            .then_some(LightChange { state, on_light })
    }
}
