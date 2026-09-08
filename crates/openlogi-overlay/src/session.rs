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

    pub(crate) fn set(&self, session_id: u64) {
        self.0.store(session_id, Ordering::Release);
    }

    pub(crate) fn clear(&self) {
        self.set(0);
    }

    pub(crate) fn clear_if(&self, session_id: u64) {
        let _ = self
            .0
            .compare_exchange(session_id, 0, Ordering::AcqRel, Ordering::Acquire);
    }

    #[must_use]
    fn observe(&self) -> Option<u64> {
        match self.0.load(Ordering::Acquire) {
            0 => None,
            session_id => Some(session_id),
        }
    }
}

#[must_use]
pub(crate) const fn click_away_targets(observed: u64, open: u64) -> bool {
    observed != 0 && observed == open
}

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

#[cfg(not(target_os = "macos"))]
const fn drop_unused_click_away_monitor(_monitor: Option<platform::ClickAwayMonitor>) {}

pub(crate) fn dismiss_click_away(cx: &mut gpui::App, session_id: u64) {
    for handle in cx.windows() {
        let Some(ring) = handle.downcast::<RingView>() else {
            continue;
        };
        let _ = ring.update(cx, |view, window, cx| {
            let Some(open_session) = view.current_session() else {
                return;
            };
            if !click_away_targets(session_id, open_session) {
                return;
            }
            view.cancel();
            view.dismiss(open_session, window, cx);
        });
    }
}

pub(crate) fn claim_the_role() -> Result<Tenancy> {
    let directory = openlogi_core::paths::config_dir().context("resolving the config directory")?;
    let serving = spawned_by().unwrap_or_else(Run::mint);
    Role::new(directory, "overlay")
        .claim(&Record::new(
            Identity::new(serving, Compat::from(PROTOCOL_VERSION)),
            Tenant::current(),
        ))
        .context("Actions Ring overlay single-instance check")
}

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
    fn stale_clear_cannot_erase_replacement_session() {
        let live = ClickAwaySession::new();
        live.set(11);
        live.set(12);
        live.clear_if(11);
        assert_eq!(live.observe(), Some(12));
        live.clear_if(12);
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
