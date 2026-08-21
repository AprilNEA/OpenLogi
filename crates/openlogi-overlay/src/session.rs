//! Two things that decide when this process should stop showing a ring, or
//! stop running at all: the native click-away monitor, and the `succession`
//! role that binds exactly one overlay to one agent run.

use std::sync::{
    Arc, OnceLock,
    atomic::{AtomicU64, Ordering},
};

use anyhow::{Context as _, Result};
use openlogi_ipc::{Identity, PROTOCOL_VERSION, RUN_ENV};
use succession::{Allegiance, Compat, Record, Role, Run, Tenancy, Tenant};
use tokio::sync::mpsc;
use tracing::warn;

use crate::platform;
use crate::ring::RingView;

pub(crate) struct ClickAwaySession(AtomicU64);

impl ClickAwaySession {
    pub(crate) const fn new() -> Self {
        Self(AtomicU64::new(0))
    }

    /// Publish which ring is showing, or `0` while none is.
    pub(crate) fn set(&self, session_id: u64) {
        self.0.store(session_id, Ordering::Release);
    }

    /// Forget the showing session so later clicks cannot name it.
    pub(crate) fn clear(&self) {
        self.set(0);
    }

    /// Session id at click time, or `None` when no ring is showing.
    #[must_use]
    fn observe(&self) -> Option<u64> {
        match self.0.load(Ordering::Acquire) {
            0 => None,
            session_id => Some(session_id),
        }
    }
}

/// True when the click still names the ring that is open.
#[must_use]
pub(crate) const fn click_away_targets(observed: u64, open: u64) -> bool {
    observed != 0 && observed == open
}

/// Dismiss a showing ring when the user clicks anywhere off it, the way a
/// transient popup closes on click-away — without swallowing that click.
///
/// The ring window only covers its own 360×360 bounds, so an outside click
/// never reaches the window's handlers. A global monitor closes the gap:
/// macOS only delivers it events routed to *other* applications, so clicks on
/// the ring itself can't race the slot/cancel handlers, and monitors can't
/// consume events, so the click lands where the user aimed it. The handler
/// snapshots the showing session onto a channel; teardown runs on the GPUI
/// side, and only that session is cancelled so a queued click cannot close a
/// ring that opened afterward.
pub(crate) fn spawn_click_away_dismissal(cx: &mut gpui::App, live: Arc<ClickAwaySession>) {
    let (clicks_tx, mut clicks) = mpsc::unbounded_channel();
    let monitor = platform::watch_clicks_outside(move || {
        if let Some(session_id) = live.observe() {
            let _ = clicks_tx.send(session_id);
        }
    });
    if monitor.is_none() && cfg!(target_os = "macos") {
        warn!(
            "could not install the click-away monitor; the ring will not dismiss on outside clicks"
        );
    }
    cx.spawn(async move |cx| {
        #[cfg(target_os = "macos")]
        let _monitor = monitor;
        #[cfg(not(target_os = "macos"))]
        drop_unused_click_away_monitor(monitor);
        while let Some(session_id) = clicks.recv().await {
            cx.update(|cx| dismiss_click_away(cx, session_id));
        }
    })
    .detach();
}

/// Drop the stub monitor; non-macOS has no native owner to keep alive.
#[cfg(not(target_os = "macos"))]
const fn drop_unused_click_away_monitor(_monitor: Option<platform::ClickAwayMonitor>) {}

/// Cancel the open ring only if it is still the session the click named.
pub(crate) fn dismiss_click_away(cx: &mut gpui::App, session_id: u64) {
    for handle in cx.windows() {
        let Some(ring) = handle.downcast::<RingView>() else {
            continue;
        };
        let _ = ring.update(cx, |view, window, _| {
            if !click_away_targets(session_id, view.session_id()) {
                return;
            }
            view.cancel();
            window.remove_window();
        });
    }
}

pub(crate) fn claim_the_role() -> Result<Tenancy> {
    let directory = openlogi_core::paths::config_dir().context("resolving the config directory")?;
    let tenancy = Role::new(directory, "overlay")
        .claim()
        .context("Actions Ring overlay single-instance check")?;
    let serving = spawned_by().unwrap_or_else(Run::mint);
    if let Err(error) = tenancy.publish(&Record::new(
        Identity::new(serving, Compat::from(PROTOCOL_VERSION)),
        Tenant::current(),
    )) {
        warn!(%error, "could not publish the overlay claim record");
    }
    Ok(tenancy)
}

/// The agent run this overlay serves.
///
/// Seeded from the run token the supervisor passes in the environment, so even
/// the first handshake catches an overlay left behind by a previous agent; a
/// hand-started overlay adopts whichever run answers first.
pub(crate) fn allegiance() -> &'static Allegiance {
    static SERVING: OnceLock<Allegiance> = OnceLock::new();
    SERVING.get_or_init(|| {
        let ours = Compat::from(PROTOCOL_VERSION);
        match spawned_by() {
            Some(run) => Allegiance::to(ours, run),
            None => Allegiance::new(ours),
        }
    })
}

/// The run token of the agent that started this process, when there is one.
pub(crate) fn spawned_by() -> Option<Run> {
    std::env::var(RUN_ENV).ok()?.parse().ok().map(Run::from_raw)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_click_is_observed_when_no_ring_is_showing() {
        let live = ClickAwaySession::new();
        assert_eq!(live.observe(), None);
        live.set(11);
        live.clear();
        assert_eq!(live.observe(), None);
    }

    #[test]
    fn a_click_queued_before_a_new_ring_does_not_target_it() {
        let live = ClickAwaySession::new();
        live.set(11);
        let queued = live.observe().expect("a showing ring is observable");
        live.set(12);
        assert!(
            !click_away_targets(queued, live.observe().expect("replacement is showing")),
            "a click snapshotted against the previous session must not close the new ring"
        );
    }

    #[test]
    fn a_click_against_the_showing_ring_targets_it() {
        let live = ClickAwaySession::new();
        live.set(7);
        let queued = live.observe().expect("a showing ring is observable");
        assert!(click_away_targets(queued, 7));
    }
}
