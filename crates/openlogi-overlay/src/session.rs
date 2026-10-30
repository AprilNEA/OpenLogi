//! Two things that decide when this process should stop showing a ring, or
//! stop running at all: the native click-away monitor, and the `succession`
//! role that binds exactly one overlay to one agent run.

use std::sync::{
    Arc, OnceLock,
    atomic::{AtomicU64, AtomicUsize, Ordering},
};

use anyhow::{Context as _, Result};
use gpui::{AppContext as _, AsyncApp};
use openlogi_ipc::{Identity, PROTOCOL_VERSION, RUN_ENV};
use succession::{Allegiance, Compat, Record, Role, Run, Tenancy, Tenant};
use tokio::sync::mpsc;
use tracing::warn;

use crate::platform::{
    self,
    motion::{MotionPreference, MotionSetting},
};
use crate::ring::RingView;

pub(crate) struct ClickAwaySession {
    session_id: AtomicU64,
    windows: AtomicUsize,
}

/// Publication guard tied to one window of a ring.
///
/// Wayland opens one window per display, so one ring holds several guards. The
/// session stays published until the last of them drops, and that drop
/// compares the id it published so it cannot clear a ring that replaced it.
#[must_use = "the guard must live as long as its ring view"]
pub(crate) struct ShowingRing {
    live: Arc<ClickAwaySession>,
    session_id: u64,
}

impl ClickAwaySession {
    pub(crate) const fn new() -> Self {
        Self {
            session_id: AtomicU64::new(0),
            windows: AtomicUsize::new(0),
        }
    }

    /// Publish `session_id` until the last guard opened for this ring is dropped.
    pub(crate) fn showing(self: &Arc<Self>, session_id: u64) -> ShowingRing {
        // Publish the id before the count so a reader cannot pair the previous ring with this window.
        if self.session_id.load(Ordering::Acquire) != session_id {
            self.session_id.store(session_id, Ordering::Release);
        }
        self.windows.fetch_add(1, Ordering::AcqRel);
        ShowingRing {
            live: Arc::clone(self),
            session_id,
        }
    }

    /// Session id at click time, or `None` when no ring is showing.
    #[must_use]
    fn observe(&self) -> Option<u64> {
        if self.windows.load(Ordering::Acquire) == 0 {
            return None;
        }
        match self.session_id.load(Ordering::Acquire) {
            0 => None,
            session_id => Some(session_id),
        }
    }
}

impl Drop for ShowingRing {
    fn drop(&mut self) {
        if self.live.windows.fetch_sub(1, Ordering::AcqRel) == 1 {
            let _ = self.live.session_id.compare_exchange(
                self.session_id,
                0,
                Ordering::AcqRel,
                Ordering::Acquire,
            );
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
/// The ring window only covers its own panel, so an outside click
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

/// Close every window of `session_id` from a click handled by `current`.
pub(crate) fn dismiss_from_window(cx: &mut gpui::App, current: &mut gpui::Window, session_id: u64) {
    // `current` is checked out of the app while its click handler runs.
    close_ring_windows(cx, session_id);
    current.remove_window();
}

/// Close every window showing ring `session_id`, reporting whether any was.
///
/// A ring is one window per display on Wayland (see [`crate::ring`]), so
/// closing "the" ring means closing all of them: the windows the cursor was
/// not on are transparent, but they still cover their display and would go on
/// swallowing input after the ring the user interacted with had gone. The
/// session id is what keeps a late teardown — a timeout, a queued click — from
/// closing a ring that opened after it.
pub(crate) fn close_ring_windows(cx: &mut gpui::App, session_id: u64) -> bool {
    let mut closed = false;
    for handle in cx.windows() {
        let Some(ring) = handle.downcast::<RingView>() else {
            continue;
        };
        let removed = ring.update(cx, |view, window, _| {
            if view.session_id() != session_id {
                return false;
            }
            window.remove_window();
            true
        });
        closed |= removed.unwrap_or(false);
    }
    closed
}

/// Close every ring window, whichever session it shows.
///
/// For when the agent names the ring that should be showing, or none: any
/// other ring is stale. Only ring windows close, so a notice that is showing
/// stays up while rings come and go. A ring already playing its exit removes
/// itself when that ends; cutting it short would skip the animation.
pub(crate) fn close_all_ring_windows(cx: &mut gpui::App) {
    for handle in cx.windows() {
        if let Some(ring) = handle.downcast::<RingView>() {
            let _ = ring.update(cx, |view, window, _| {
                if !view.is_closing() {
                    window.remove_window();
                }
            });
        }
    }
}

/// Cancel the open ring only if it is still the session the click named.
pub(crate) fn dismiss_click_away(cx: &mut gpui::App, session_id: u64) {
    let target = cx.windows().into_iter().find_map(|handle| {
        let ring = handle.downcast::<RingView>()?;
        ring.update(cx, |view, _, _| {
            click_away_targets(session_id, view.session_id()).then(|| view.cancel())
        })
        .ok()
        .flatten()
    });
    if target.is_some() {
        close_ring_windows(cx, session_id);
    }
}

pub(crate) fn claim_the_role() -> Result<Tenancy> {
    let directory = openlogi_core::paths::config_dir().context("resolving the config directory")?;
    let serving = spawned_by().unwrap_or_else(Run::mint);
    // Identity comes with the claim: an overlay the agent cannot recognize
    // holds the role while being unevictable by the ordinary path, so failing
    // here and letting the supervisor start a replacement beats running as
    // one (#842).
    Role::new(directory, "overlay")
        .claim(&Record::new(
            Identity::new(serving, Compat::from(PROTOCOL_VERSION)),
            Tenant::current(),
        ))
        .context("Actions Ring overlay single-instance check")
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

/// Race a background motion read against invocation observations until the
/// read finishes, and apply what it read.
///
/// While the read runs (up to 200 ms on Linux) the overlay keeps receiving on
/// `invocations`. A dismissal (`None`) returns immediately without waiting
/// for the read. A replacement `Some(B)` supersedes the invocation that
/// started the settle but keeps the same in-flight read. When the read
/// completes, the latest observation wins (including anything already
/// queued), GPUI's `reduce_motion` is set from the read through
/// `preference`, then the result is returned. If `invocations` closes while
/// waiting, returns `None`.
pub(crate) async fn settle_latest_invocation<T>(
    cx: &mut AsyncApp,
    first: T,
    invocations: &mut mpsc::UnboundedReceiver<Option<T>>,
    preference: &mut MotionPreference,
    read: impl std::future::Future<Output = Option<MotionSetting>> + Send + 'static,
) -> Option<T> {
    let mut latest = Some(first);
    let mut read_task = std::pin::pin!(cx.background_spawn(read));
    loop {
        tokio::select! {
            biased;
            message = invocations.recv() => match message {
                None | Some(None) => return None,
                Some(Some(value)) => latest = Some(value),
            },
            fresh = read_task.as_mut() => {
                let setting = preference.resolve(fresh);
                cx.update(|cx| cx.set_reduce_motion(setting.reduces()));
                while let Ok(message) = invocations.try_recv() {
                    latest = message;
                }
                return latest;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_click_is_observed_when_no_ring_is_showing() {
        let live = Arc::new(ClickAwaySession::new());
        assert_eq!(live.observe(), None);
        let showing = live.showing(11);
        assert_eq!(live.observe(), Some(11));
        drop(showing);
        assert_eq!(live.observe(), None);
    }

    #[test]
    fn a_click_queued_before_a_new_ring_does_not_target_it() {
        let live = Arc::new(ClickAwaySession::new());
        let previous = live.showing(11);
        let queued = live.observe().expect("a showing ring is observable");
        let replacement = live.showing(12);
        drop(previous);
        assert!(
            !click_away_targets(queued, live.observe().expect("replacement is showing")),
            "a click snapshotted against the previous session must not close the new ring"
        );
        drop(replacement);
        assert_eq!(live.observe(), None);
    }

    #[test]
    fn a_click_against_the_showing_ring_targets_it() {
        let live = Arc::new(ClickAwaySession::new());
        let _showing = live.showing(7);
        let queued = live.observe().expect("a showing ring is observable");
        assert!(click_away_targets(queued, 7));
    }

    #[test]
    fn a_ring_stays_observable_until_its_last_window_drops() {
        let live = Arc::new(ClickAwaySession::new());
        let first = live.showing(4);
        let second = live.showing(4);
        drop(first);
        assert_eq!(live.observe(), Some(4));
        drop(second);
        assert_eq!(live.observe(), None);
    }

    #[test]
    fn an_older_window_dropping_leaves_the_replacement_observable() {
        let live = Arc::new(ClickAwaySession::new());
        let older = live.showing(4);
        let replacement = live.showing(5);
        drop(older);
        assert_eq!(live.observe(), Some(5));
        drop(replacement);
        assert_eq!(live.observe(), None);
    }

    #[test]
    fn a_replacement_that_drops_first_does_not_stay_observable() {
        let live = Arc::new(ClickAwaySession::new());
        let older = live.showing(4);
        let replacement = live.showing(5);
        drop(replacement);
        drop(older);
        assert_eq!(
            live.observe(),
            None,
            "every window has dropped, so no ring is showing"
        );
    }
}

#[cfg(test)]
mod settle_tests {
    use std::cell::Cell;
    use std::rc::Rc;

    use gpui::TestAppContext;

    use super::settle_latest_invocation;
    use crate::platform::motion::{MotionPreference, MotionSetting};

    /// Open one ring with nothing else queued and report the GPUI flag it got.
    async fn open_with_read(
        cx: &mut gpui::AsyncApp,
        preference: &mut MotionPreference,
        read: Option<MotionSetting>,
    ) -> bool {
        let (_sender, mut receiver) = tokio::sync::mpsc::unbounded_channel::<Option<u64>>();
        let opened =
            settle_latest_invocation(cx, 1, &mut receiver, preference, async move { read }).await;
        assert_eq!(opened, Some(1));
        cx.update(|cx| cx.reduce_motion())
    }

    #[gpui::test]
    async fn motion_reduced_between_opens_applies_to_the_next_ring(cx: &mut TestAppContext) {
        cx.spawn(|mut async_cx| async move {
            let mut preference = MotionPreference::default();
            assert!(
                !open_with_read(&mut async_cx, &mut preference, Some(MotionSetting::Full)).await
            );
            assert!(
                open_with_read(&mut async_cx, &mut preference, Some(MotionSetting::Reduced)).await
            );
        })
        .await;
    }

    #[gpui::test]
    async fn an_unknown_setting_on_first_open_keeps_motion_full(cx: &mut TestAppContext) {
        cx.spawn(|mut async_cx| async move {
            let mut preference = MotionPreference::default();
            assert!(!open_with_read(&mut async_cx, &mut preference, None).await);
            assert_eq!(preference.resolve(None), MotionSetting::Full);
            assert!(
                open_with_read(&mut async_cx, &mut preference, Some(MotionSetting::Reduced)).await
            );
        })
        .await;
    }

    #[gpui::test]
    fn a_dismissal_while_the_read_is_pending_returns_before_it_completes(cx: &mut TestAppContext) {
        let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel();
        let (read_tx, read_rx) = tokio::sync::oneshot::channel::<Option<MotionSetting>>();
        let settled = Rc::new(Cell::new(None::<Option<u64>>));
        let settled_handle = settled.clone();
        let task = cx.spawn(|mut async_cx| async move {
            let mut preference = MotionPreference::default();
            let value = settle_latest_invocation(
                &mut async_cx,
                1_u64,
                &mut receiver,
                &mut preference,
                async move { read_rx.await.unwrap_or(None) },
            )
            .await;
            settled_handle.set(Some(value));
        });
        cx.run_until_parked();
        assert!(settled.get().is_none());
        let _ = sender.send(None);
        cx.run_until_parked();
        assert_eq!(settled.get(), Some(None));
        let _ = read_tx.send(Some(MotionSetting::Reduced));
        cx.run_until_parked();
        assert_eq!(settled.get(), Some(None));
        drop(task);
    }

    #[gpui::test]
    fn a_replacement_while_the_read_is_pending_opens_the_latest_after_it(cx: &mut TestAppContext) {
        let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel();
        let (read_tx, read_rx) = tokio::sync::oneshot::channel::<Option<MotionSetting>>();
        let settled = Rc::new(Cell::new(None::<Option<u64>>));
        let settled_handle = settled.clone();
        let task = cx.spawn(|mut async_cx| async move {
            let mut preference = MotionPreference::default();
            let value = settle_latest_invocation(
                &mut async_cx,
                1_u64,
                &mut receiver,
                &mut preference,
                async move { read_rx.await.unwrap_or(None) },
            )
            .await;
            settled_handle.set(Some(value));
            async_cx.update(|cx| assert!(cx.reduce_motion()));
        });
        cx.run_until_parked();
        assert!(settled.get().is_none());
        let _ = sender.send(Some(2_u64));
        cx.run_until_parked();
        assert!(settled.get().is_none());
        let _ = read_tx.send(Some(MotionSetting::Reduced));
        cx.run_until_parked();
        assert_eq!(settled.get(), Some(Some(2)));
        drop(task);
    }

    #[gpui::test]
    fn a_closed_invocation_channel_while_the_read_is_pending_returns_none(cx: &mut TestAppContext) {
        let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel();
        let (_read_tx, read_rx) = tokio::sync::oneshot::channel::<Option<MotionSetting>>();
        let settled = Rc::new(Cell::new(None::<Option<u64>>));
        let settled_handle = settled.clone();
        let task = cx.spawn(|mut async_cx| async move {
            let mut preference = MotionPreference::default();
            let value = settle_latest_invocation(
                &mut async_cx,
                1_u64,
                &mut receiver,
                &mut preference,
                async move { read_rx.await.unwrap_or(None) },
            )
            .await;
            settled_handle.set(Some(value));
        });
        cx.run_until_parked();
        drop(sender);
        cx.run_until_parked();
        assert_eq!(settled.get(), Some(None));
        drop(task);
    }

    #[gpui::test]
    async fn a_replacement_then_a_dismissal_during_the_read_returns_none(cx: &mut TestAppContext) {
        cx.spawn(|mut async_cx| async move {
            let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel();
            let mut preference = MotionPreference::default();
            let settled = settle_latest_invocation(
                &mut async_cx,
                1_u64,
                &mut receiver,
                &mut preference,
                async move {
                    let _ = sender.send(Some(2_u64));
                    let _ = sender.send(None);
                    Some(MotionSetting::Full)
                },
            )
            .await;
            assert_eq!(settled, None);
        })
        .await;
    }

    #[gpui::test]
    async fn a_dismissal_then_a_replacement_during_the_read_returns_none(cx: &mut TestAppContext) {
        cx.spawn(|mut async_cx| async move {
            let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel();
            let mut preference = MotionPreference::default();
            let settled = settle_latest_invocation(
                &mut async_cx,
                1_u64,
                &mut receiver,
                &mut preference,
                async move {
                    let _ = sender.send(None);
                    let _ = sender.send(Some(3_u64));
                    Some(MotionSetting::Full)
                },
            )
            .await;
            assert_eq!(settled, None);
        })
        .await;
    }

    #[gpui::test]
    async fn an_empty_queue_returns_the_first_and_applies_the_read(cx: &mut TestAppContext) {
        cx.spawn(|mut async_cx| async move {
            let (_sender, mut receiver) = tokio::sync::mpsc::unbounded_channel();
            let mut preference = MotionPreference::default();
            let settled = settle_latest_invocation(
                &mut async_cx,
                7_u64,
                &mut receiver,
                &mut preference,
                async { Some(MotionSetting::Reduced) },
            )
            .await;
            assert_eq!(settled, Some(7));
            async_cx.update(|cx| assert!(cx.reduce_motion()));
        })
        .await;
    }
}
