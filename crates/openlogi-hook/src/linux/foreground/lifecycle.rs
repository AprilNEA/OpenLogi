//! The foreground observer's two lifecycles as plain state, apart from the
//! thread and the locks that drive them: who holds the one selected source,
//! and what the observer worker has published.

use std::mem;

use crate::ForegroundApp;

#[cfg(test)]
mod tests;

/// Who holds the one selected frontmost source.
///
/// `S` is the source and `W` the worker that observes with it,
/// `Box<dyn FrontmostSource>` and `ObserverWorker` in the controller. The
/// source sits here while idle and travels with the worker while it
/// observes, so it is never in two places, and only the named transitions
/// move it.
pub(super) struct ObserverLifecycle<S, W> {
    stage: Stage<S, W>,
}

enum Stage<S, W> {
    /// No worker runs; snapshot reads ask `source` directly.
    Idle { source: S },
    /// `worker` holds the source and publishes what it reports.
    Observing { worker: W },
    /// The last observer left, and its worker is being joined outside the
    /// controller's lock. The source comes back with it; until then callers
    /// wait.
    Stopping,
}

/// What [`ObserverLifecycle::start`] did.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum Start<E> {
    /// A worker already observes or is stopping, so nothing launched.
    Busy,
    /// The idle source moved to the launched worker.
    Started,
    /// The launch failed and gave the source back, which stays idle.
    Failed(E),
}

impl<S, W> ObserverLifecycle<S, W> {
    /// Idle with `source`.
    pub(super) const fn new(source: S) -> Self {
        Self {
            stage: Stage::Idle { source },
        }
    }

    /// Whether a worker is being stopped, which callers wait out.
    pub(super) const fn is_stopping(&self) -> bool {
        matches!(self.stage, Stage::Stopping)
    }

    /// The source, while no worker holds it.
    pub(super) const fn idle_source(&mut self) -> Option<&mut S> {
        match &mut self.stage {
            Stage::Idle { source } => Some(source),
            Stage::Observing { .. } | Stage::Stopping => None,
        }
    }

    /// The worker, while it observes.
    pub(super) const fn worker(&self) -> Option<&W> {
        match &self.stage {
            Stage::Observing { worker } => Some(worker),
            Stage::Idle { .. } | Stage::Stopping => None,
        }
    }

    /// Idle → Observing: `launch` hands the idle source to a new worker. A
    /// launch that fails gives the source back with the reason, and the
    /// lifecycle stays idle with it. In any other stage this is
    /// [`Start::Busy`] and `launch` does not run.
    ///
    /// `launch` runs mid-transition under the caller's lock and must not
    /// unwind: an unwind would lose the source and leave the lifecycle
    /// [`Stopping`](Stage::Stopping) for good.
    pub(super) fn start<E>(&mut self, launch: impl FnOnce(S) -> Result<W, (S, E)>) -> Start<E> {
        self.step(|stage| match stage {
            Stage::Idle { source } => match launch(source) {
                Ok(worker) => (Stage::Observing { worker }, Start::Started),
                Err((source, error)) => (Stage::Idle { source }, Start::Failed(error)),
            },
            busy @ (Stage::Observing { .. } | Stage::Stopping) => (busy, Start::Busy),
        })
    }

    /// Observing → Stopping: take the worker out to be stopped and joined
    /// outside the lock. `None`, changing nothing, unless a worker observes.
    pub(super) fn stop(&mut self) -> Option<W> {
        self.step(|stage| match stage {
            Stage::Observing { worker } => (Stage::Stopping, Some(worker)),
            other @ (Stage::Idle { .. } | Stage::Stopping) => (other, None),
        })
    }

    /// Stopping → Idle with the source the joined worker gave back, or with
    /// the one that replaced it when the worker panicked and lost it.
    ///
    /// Ignored in any other stage. Nothing else leaves `Stopping`, so only
    /// the caller holding the worker [`Self::stop`] took ends a stop.
    pub(super) fn stopped(&mut self, source: S) {
        self.step(|stage| match stage {
            Stage::Stopping => (Stage::Idle { source }, ()),
            other @ (Stage::Idle { .. } | Stage::Observing { .. }) => (other, ()),
        });
    }

    /// Replace the stage with what `transition` makes of it. `Stopping`
    /// stands in while `transition` runs; see [`Self::start`].
    fn step<T>(&mut self, transition: impl FnOnce(Stage<S, W>) -> (Stage<S, W>, T)) -> T {
        let (stage, output) = transition(mem::replace(&mut self.stage, Stage::Stopping));
        self.stage = stage;
        output
    }
}

/// What the observer worker published, while one runs.
pub(super) struct Feed {
    session: Session,
}

enum Session {
    /// No worker observes: snapshot reads go to the idle source, and a
    /// publish is dropped.
    Inactive,
    /// A worker observes. `latest` is the last change it published, `None`
    /// until its first.
    Active { latest: Option<Published> },
}

/// One change the observer worker published.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Published {
    /// Orders the active session's changes from 1, so a subscriber drops a
    /// change older than one it already received. Versions restart with
    /// each session: one ends only after its last subscriber left, and none
    /// joins until it has, so no subscriber sees two sessions.
    pub(super) version: u64,
    /// The frontmost application, `None` when there is none or it cannot be
    /// read.
    pub(super) app: Option<ForegroundApp>,
}

/// Where a snapshot read gets its answer.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum ActiveSnapshot {
    /// No worker observes: ask the idle source.
    Idle,
    /// A worker observes: its latest change, `None` until its first.
    Active(Option<ForegroundApp>),
}

impl Feed {
    /// Inactive, with nothing published.
    pub(super) const fn new() -> Self {
        Self {
            session: Session::Inactive,
        }
    }

    /// Open a fresh session, with nothing published yet.
    pub(super) fn begin(&mut self) {
        self.session = Session::Active { latest: None };
    }

    /// Close the session and forget what it published.
    pub(super) fn finish(&mut self) {
        self.session = Session::Inactive;
    }

    /// Record `app` as the session's latest change and return it. `None`
    /// when no session is active or `app` repeats the latest change.
    pub(super) fn publish(&mut self, app: Option<ForegroundApp>) -> Option<&Published> {
        let Session::Active { latest } = &mut self.session else {
            return None;
        };
        let version = match latest {
            Some(published) if published.app == app => return None,
            Some(published) => published.version + 1,
            None => 1,
        };
        Some(latest.insert(Published { version, app }))
    }

    /// The active session's latest change.
    pub(super) const fn latest(&self) -> Option<&Published> {
        match &self.session {
            Session::Active { latest } => latest.as_ref(),
            Session::Inactive => None,
        }
    }

    /// Where a snapshot read gets its answer now.
    pub(super) fn snapshot(&self) -> ActiveSnapshot {
        match &self.session {
            Session::Inactive => ActiveSnapshot::Idle,
            Session::Active { latest } => {
                ActiveSnapshot::Active(latest.as_ref().and_then(|published| published.app.clone()))
            }
        }
    }
}
