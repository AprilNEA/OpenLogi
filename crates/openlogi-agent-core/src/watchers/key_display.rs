//! Keypad LCD keys: keep each key's image in step with its binding.
//!
//! The orchestrator publishes one [`KeyDisplaySpec`] per online keypad whose
//! keys are displays — what every key should show, derived from the same
//! effective bindings the keyboard capture session dispatches. This watcher
//! owns the device side: it remembers what it last painted on each panel,
//! renders and writes only the keys that differ, and repaints a whole panel
//! when the spec's panel generation moves (a reconnect — observed or only
//! announced — or a system wake, after which the device shows its own
//! default again).
//!
//! The images are volatile panel state with nothing to restore: a stop
//! simply abandons any write in flight.
//!
//! A keypad whose write fails is left alone until its own retry deadline,
//! which doubles with each further failure up to `MAX_RETRY_DELAY`: a node
//! that refuses the image report outright (one that does not declare it)
//! fails the same way every time, so neither the timer nor a respec — an app
//! switch, a rebind — may hammer it. A reconnect or a wake (a moved
//! generation) clears its failure and retries at once.

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;
use std::time::Duration;

use openlogi_core::binding::GridKey;
use openlogi_hid::{DeviceRoute, HidppOperation, KeyDisplay, KeyImageJpeg, write_key_image_on};
use tokio::sync::{oneshot, watch};
use tokio::time::Instant;
use tracing::{debug, warn};

use backon::{BackoffBuilder as _, ExponentialBackoff, ExponentialBuilder};

use super::retry::{RETRY_DELAY, wait_for_deadline};
use super::shutdown::{ManagerCompletion, WatcherHandle};
use crate::hardware::DeviceAccess;

pub mod render;

pub use render::KeyFace;
use render::{RenderError, render_tile};

/// What one keypad's LCD keys should show.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyDisplaySpec {
    /// The keypad.
    pub route: DeviceRoute,
    /// Where its keys sit on its panel.
    pub display: KeyDisplay,
    /// The panel generation: it moves whenever the keypad's volatile
    /// settings re-apply — it came online or reconnected, or the system woke
    /// — and a moved generation means the panel must be assumed to have lost
    /// what was painted on it.
    pub generation: u64,
    /// Every key's face.
    pub faces: BTreeMap<GridKey, KeyFace>,
}

/// Read-only, coalescing view of the published key-display specs.
pub type SharedKeyDisplaySpecs = watch::Receiver<Arc<Vec<KeyDisplaySpec>>>;

/// The longest a failing keypad waits between write attempts.
const MAX_RETRY_DELAY: Duration = Duration::from_secs(60);

/// One key to paint, on the panel `spec` describes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Paint<'a> {
    spec: &'a KeyDisplaySpec,
    key: GridKey,
    face: KeyFace,
}

/// What the watcher believes is on one panel.
#[derive(Debug)]
struct PaintedPanel {
    route: DeviceRoute,
    generation: u64,
    faces: BTreeMap<GridKey, KeyFace>,
}

/// The next delay `backoff` yields. It is built without a retry limit, so it
/// never runs out; the cap stands in if it somehow did.
fn next_delay(backoff: &mut ExponentialBackoff) -> Duration {
    backoff.next().unwrap_or(MAX_RETRY_DELAY)
}

/// The keys of `spec` not known, from `panels`, to be showing their face.
fn unpainted<'a>(
    panels: &[PaintedPanel],
    spec: &'a KeyDisplaySpec,
) -> impl Iterator<Item = Paint<'a>> {
    let painted = panels
        .iter()
        .find(|panel| panel.route == spec.route)
        .map(|panel| &panel.faces);
    spec.faces
        .iter()
        .filter(move |(key, face)| painted.and_then(|faces| faces.get(key)) != Some(face))
        .map(move |(key, face)| Paint {
            spec,
            key: *key,
            face: *face,
        })
}

/// A keypad whose image writes keep failing, and when to try it again.
#[derive(Debug)]
struct FailingPanel {
    route: DeviceRoute,
    /// The generation the failures happened in: a moved one clears them.
    generation: u64,
    /// Doubling delays from [`RETRY_DELAY`] up to `MAX_RETRY_DELAY`.
    backoff: ExponentialBackoff,
    retry_at: Instant,
}

/// What the watcher believes is on every panel it has painted, and which
/// panels are refusing writes — the sans-I/O half of the watcher. Only
/// [`Self::record`] adds a painted face, and only for a write the device
/// acknowledged; [`Self::failed`] forgets the face of a key whose write
/// failed, since a lost acknowledgement may hide a write the device took.
#[derive(Debug, Default)]
struct Painted {
    panels: Vec<PaintedPanel>,
    failing: Vec<FailingPanel>,
}

impl Painted {
    /// The keys `specs` wants that are not known to be showing their face.
    ///
    /// Forgets a panel that left `specs` (it may come back showing anything)
    /// and one whose generation moved (it may have reset), so both are
    /// repainted in full.
    ///
    /// Leaves out every key of a failing panel whose retry deadline is after
    /// `now`, and forgets the failures of a panel that left `specs` or whose
    /// generation moved: either is a new device as far as retries go.
    ///
    /// A failing panel always has its failed key left to paint — every spec
    /// covers every grid key, and [`Self::failed`] forgot that key's face —
    /// so a lapsed retry deadline is always followed by a write that moves
    /// or clears it, never by a busy loop.
    fn plan<'a>(&mut self, specs: &'a [KeyDisplaySpec], now: Instant) -> Vec<Paint<'a>> {
        self.panels.retain(|panel| {
            specs
                .iter()
                .any(|spec| spec.route == panel.route && spec.generation == panel.generation)
        });
        self.failing.retain(|failing| {
            specs
                .iter()
                .any(|spec| spec.route == failing.route && spec.generation == failing.generation)
        });
        let panels = &self.panels;
        specs
            .iter()
            .filter(|spec| {
                !self
                    .failing
                    .iter()
                    .any(|failing| failing.route == spec.route && failing.retry_at > now)
            })
            .flat_map(|spec| unpainted(panels, spec))
            .collect()
    }

    /// Note that the device acknowledged `paint`, so its panel is not
    /// failing.
    fn record(&mut self, paint: &Paint) {
        let spec = paint.spec;
        self.failing.retain(|failing| failing.route != spec.route);
        match self
            .panels
            .iter_mut()
            .find(|panel| panel.route == spec.route)
        {
            Some(panel) if panel.generation == spec.generation => {
                panel.faces.insert(paint.key, paint.face);
            }
            // A newer generation was planned meanwhile: this paint predates a
            // reset, so it proves nothing about the panel now.
            Some(_) => {}
            None => self.panels.push(PaintedPanel {
                route: spec.route.clone(),
                generation: spec.generation,
                faces: BTreeMap::from([(paint.key, paint.face)]),
            }),
        }
    }

    /// Note at `now` that `paint`'s write failed, and push its panel's retry
    /// deadline out by its backoff. `true` when the panel was not already
    /// failing, so the failure is news worth a `warn`.
    ///
    /// Forgets the face recorded for `paint`'s key: a write whose
    /// acknowledgement was lost may still have landed, so the key may show
    /// `paint`'s face, its old one, or neither — and returning to the old
    /// binding must still repaint it.
    fn failed(&mut self, paint: &Paint, now: Instant) -> bool {
        let spec = paint.spec;
        if let Some(panel) = self
            .panels
            .iter_mut()
            .find(|panel| panel.route == spec.route)
        {
            panel.faces.remove(&paint.key);
        }
        if let Some(failing) = self
            .failing
            .iter_mut()
            .find(|failing| failing.route == spec.route)
        {
            failing.retry_at = now + next_delay(&mut failing.backoff);
            return false;
        }
        let mut backoff = ExponentialBuilder::new()
            .with_min_delay(RETRY_DELAY)
            .with_max_delay(MAX_RETRY_DELAY)
            .without_max_times()
            .build();
        let retry_at = now + next_delay(&mut backoff);
        self.failing.push(FailingPanel {
            route: spec.route.clone(),
            generation: spec.generation,
            backoff,
            retry_at,
        });
        true
    }

    /// The earliest retry deadline of a failing panel, or `None` when none
    /// is failing.
    fn next_retry(&self) -> Option<Instant> {
        self.failing.iter().map(|failing| failing.retry_at).min()
    }
}

/// Rendered tiles, by face and tile size: the same icon on two keys, or
/// again after a rebind, is encoded once.
#[derive(Default)]
struct Tiles {
    rendered: HashMap<(KeyFace, u16), Arc<KeyImageJpeg>>,
}

impl Tiles {
    fn get(&mut self, face: KeyFace, size: u16) -> Result<Arc<KeyImageJpeg>, RenderError> {
        if let Some(tile) = self.rendered.get(&(face, size)) {
            return Ok(Arc::clone(tile));
        }
        let tile = Arc::new(render_tile(face, size)?);
        self.rendered.insert((face, size), Arc::clone(&tile));
        Ok(tile)
    }

    /// The tile for `face`, or a blank one when `face` cannot be rendered —
    /// so the key never keeps showing the glyph of an action it is no longer
    /// bound to. `None` only when not even a blank tile renders.
    ///
    /// A render failure is a defect in the artwork, not the device: rendering
    /// again cannot fix it, so the blank stands in for `face` until the
    /// binding changes.
    fn get_or_blank(&mut self, face: KeyFace, size: u16) -> Option<Arc<KeyImageJpeg>> {
        match self.get(face, size) {
            Ok(tile) => Some(tile),
            Err(error) => {
                warn!(%error, ?face, "key image could not be rendered — showing it blank");
                self.get(KeyFace::Blank, size)
                    .inspect_err(|error| warn!(%error, "blank key image could not be rendered"))
                    .ok()
            }
        }
    }
}

/// Spawn the key-display manager thread.
#[must_use]
pub fn spawn(specs: &SharedKeyDisplaySpecs, access: DeviceAccess) -> WatcherHandle {
    let specs = specs.clone();
    WatcherHandle::spawn("openlogi-key-display-watcher", move |shutdown| {
        manage(specs, access, shutdown)
    })
}

async fn manage(
    mut specs: SharedKeyDisplaySpecs,
    access: DeviceAccess,
    mut shutdown: oneshot::Receiver<()>,
) -> ManagerCompletion {
    let mut painted = Painted::default();
    let mut tiles = Tiles::default();
    loop {
        let wanted = Arc::clone(&specs.borrow_and_update());
        let pending = painted.plan(&wanted, Instant::now());
        tokio::select! {
            () = paint_all(&pending, &mut painted, &mut tiles, &access) => {}
            _ = &mut shutdown => return ManagerCompletion::Graceful,
        }
        let retry = painted.next_retry();
        tokio::select! {
            changed = specs.changed() => {
                if changed.is_err() {
                    return ManagerCompletion::Unexpected;
                }
            }
            () = wait_for_deadline(retry) => {}
            _ = &mut shutdown => return ManagerCompletion::Graceful,
        }
    }
}

/// Paint every key in `pending`, recording each acknowledged one and each
/// keypad whose write failed.
///
/// The first failed write on a keypad skips that keypad's remaining keys: a
/// device that did not answer one tile will not answer the next, and each
/// would wait out its own acknowledgement timeout.
async fn paint_all(
    pending: &[Paint<'_>],
    painted: &mut Painted,
    tiles: &mut Tiles,
    access: &DeviceAccess,
) {
    let mut failed: Vec<&DeviceRoute> = Vec::new();
    for paint in pending {
        let route = &paint.spec.route;
        if failed.contains(&route) {
            continue;
        }
        let Some(tile) = tiles.get_or_blank(paint.face, paint.spec.display.key_size) else {
            continue;
        };
        let (display, key) = (paint.spec.display, paint.key);
        let result = access
            .op(route)
            .run(HidppOperation::WriteKeyImage, |channel| async move {
                write_key_image_on(&channel, display, key, &tile).await
            })
            .await;
        match result {
            Ok(()) => painted.record(paint),
            Err(error) => {
                if painted.failed(paint, Instant::now()) {
                    warn!(%error, %route, key = ?paint.key, "key image write failed — retrying");
                } else {
                    debug!(%error, %route, key = ?paint.key, "key image write failed again — retrying");
                }
                failed.push(route);
            }
        }
    }
}

#[cfg(test)]
mod tests;
