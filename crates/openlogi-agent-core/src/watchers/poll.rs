//! The shape shared by the agent's grant watchers.
//!
//! A macOS privacy grant has no public change API. It has two undocumented
//! notifications, which `axwatch` observes, and a missed one costs only a stale
//! status here — so each grant is read on a thread when a notification arrives
//! and at least every heartbeat, and reported when it changes. Writing that
//! loop once means the rules it has to get right are stated in one place, and
//! tested:
//!
//! - the first sample is always reported (the consumer has nothing until it
//!   arrives), and after that only changes are,
//! - a burst of wakes — one System Settings edit posts several — is one sample,
//! - each sample is told why it runs, so a costly read can skip heartbeats,
//! - the thread ends when the consumer stops listening,
//! - a thread that cannot start degrades the feature with a warning rather than
//!   taking the agent down, and a watch that cannot start leaves the heartbeat
//!   cadence alone, with the same warning.
//!
//! Watchers with more to say than "the value changed" — the HID inventory, the
//! gesture and pairing sessions — keep their own loops.

use std::fmt::Debug;
use std::sync::mpsc as sync_mpsc;
use std::thread;
use std::time::Duration;

use tokio::sync::mpsc;
use tracing::{debug, warn};

/// One grant watcher, described where it is started.
#[derive(Clone, Copy, Debug)]
pub struct Poll {
    /// Thread name, and the tag this watcher's log lines carry.
    pub name: &'static str,
    /// The longest the grant goes unread when no notification arrives.
    pub heartbeat: Duration,
    /// What stops working if the thread cannot start, for the warning that
    /// says so — phrased to complete "could not spawn watcher — …".
    pub degrades: &'static str,
}

/// Why a watcher takes a sample.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Cause {
    /// The watcher just started.
    Start,
    /// A privacy notification arrived: some grant, for some app, changed.
    Notification,
    /// Only time passed: the heartbeat, or the sleep that stands in for a
    /// watch that could not start.
    Heartbeat,
}

impl Poll {
    /// Report what `read` returns whenever it differs from the last value sent.
    ///
    /// `read` runs on the watcher's own thread, once per wake, with the cause
    /// of that wake, and must not block for long.
    pub fn on_change<T, F>(self, mut read: F) -> mpsc::UnboundedReceiver<T>
    where
        T: Clone + PartialEq + Debug + Send + 'static,
        F: FnMut(Cause) -> T + Send + 'static,
    {
        let (tx, rx) = mpsc::unbounded_channel();
        let spawned = thread::Builder::new()
            .name(self.name.into())
            .spawn(move || {
                let mut wakes = Wakes::start(self);
                let mut last: Option<T> = None;
                let mut cause = Cause::Start;
                loop {
                    let current = read(cause);
                    // `None` is "nothing reported yet", never a value, so the
                    // first sample always goes out even when `T` itself is an
                    // `Option` that starts at `None`.
                    if last.as_ref() != Some(&current) {
                        debug!(watcher = self.name, value = ?current, "changed");
                        if tx.send(current.clone()).is_err() {
                            debug!(watcher = self.name, "receiver dropped — exiting");
                            return;
                        }
                        last = Some(current);
                    }
                    cause = wakes.wait();
                    if tx.is_closed() {
                        debug!(watcher = self.name, "receiver dropped — exiting");
                        return;
                    }
                }
            });
        if let Err(error) = spawned {
            warn!(
                error = %error,
                watcher = self.name,
                "could not spawn watcher — {}",
                self.degrades
            );
        }
        rx
    }
}

/// What the watcher thread blocks on between samples.
enum Wakes {
    /// A privacy notification or the watch's heartbeat. The watch is owned
    /// here so it lives exactly as long as the thread and unregisters when the
    /// thread exits.
    Watch {
        _watch: axwatch::Watch,
        wakes: sync_mpsc::Receiver<axwatch::Wake>,
    },
    /// The watch could not start: the heartbeat cadence alone.
    Sleep(Duration),
}

impl Wakes {
    fn start(poll: Poll) -> Self {
        let (tx, wakes) = sync_mpsc::channel();
        match axwatch::Watch::start(Some(poll.heartbeat), move |wake| {
            // The receiver outlives every sender: the thread that owns it
            // drops the watch, and with it this handler, before exiting.
            let _ = tx.send(wake);
        }) {
            Ok(watch) => Self::Watch {
                _watch: watch,
                wakes,
            },
            Err(error) => {
                warn!(
                    error = %error,
                    watcher = poll.name,
                    heartbeat = ?poll.heartbeat,
                    "could not watch privacy notifications — sampling on the heartbeat alone"
                );
                Self::Sleep(poll.heartbeat)
            }
        }
    }

    fn wait(&mut self) -> Cause {
        match self {
            Self::Watch { wakes, .. } => {
                // The watch, and so a sender, lives as long as `self`.
                let Ok(first) = wakes.recv() else {
                    return Cause::Heartbeat;
                };
                // One edit posts several notifications; answer them once.
                let rest = std::iter::from_fn(|| wakes.try_recv().ok());
                cause_of(std::iter::once(first).chain(rest))
            }
            Self::Sleep(period) => {
                thread::sleep(*period);
                Cause::Heartbeat
            }
        }
    }
}

/// The cause of one burst of wakes: a notification if any wake in it was one.
/// The whole burst is consumed either way.
fn cause_of(burst: impl Iterator<Item = axwatch::Wake>) -> Cause {
    burst.fold(Cause::Heartbeat, |cause, wake| match wake {
        axwatch::Wake::Heartbeat => cause,
        axwatch::Wake::AccessibilityChanged | axwatch::Wake::PrivacyChanged => Cause::Notification,
    })
}

/// A watcher for a platform where the answer cannot change: `value` is
/// reported once, and nothing follows it.
pub fn constant<T>(value: T) -> mpsc::UnboundedReceiver<T> {
    let (tx, rx) = mpsc::unbounded_channel();
    // The queued value outlives the sender, so the consumer still sees it.
    let _ = tx.send(value);
    rx
}

/// A watcher for a platform that has no such source: nothing is ever reported.
pub fn never<T>() -> mpsc::UnboundedReceiver<T> {
    mpsc::unbounded_channel().1
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};
    use std::time::Instant;

    /// Drain what the watcher reports until it goes quiet, bounded so a broken
    /// watcher fails the test instead of hanging it.
    fn drain<T>(rx: &mut mpsc::UnboundedReceiver<T>, want: usize) -> Vec<T> {
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut seen = Vec::new();
        while seen.len() < want && Instant::now() < deadline {
            match rx.try_recv() {
                Ok(value) => seen.push(value),
                Err(_) => thread::sleep(Duration::from_millis(1)),
            }
        }
        seen
    }

    #[test]
    fn the_first_sample_is_reported_and_then_only_changes() {
        let samples = Mutex::new(vec![false, true, true, true, false].into_iter());
        let mut rx = Poll {
            name: "openlogi-test-watcher",
            heartbeat: Duration::from_millis(1),
            degrades: "the test learns nothing",
        }
        .on_change(move |_| {
            samples
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .next()
                // Hold the last sample once the script runs out, so the
                // watcher has nothing new to report rather than exiting.
                .unwrap_or(false)
        });

        // `false` opens the stream even though it equals `T::default()`, then
        // only the two transitions follow.
        assert_eq!(drain(&mut rx, 3), vec![false, true, false]);
    }

    #[test]
    fn the_first_sample_is_the_start_and_later_ones_are_wakes() {
        let mut rx = Poll {
            name: "openlogi-test-watcher",
            heartbeat: Duration::from_millis(1),
            degrades: "the test learns nothing",
        }
        .on_change(|cause| cause == Cause::Start);
        assert_eq!(drain(&mut rx, 2), vec![true, false]);
    }

    #[test]
    fn a_burst_is_a_notification_if_any_wake_in_it_is_one() {
        use axwatch::Wake::{AccessibilityChanged, Heartbeat, PrivacyChanged};
        let cases: [(&[axwatch::Wake], Cause); 5] = [
            (&[Heartbeat], Cause::Heartbeat),
            (&[Heartbeat, Heartbeat], Cause::Heartbeat),
            (&[PrivacyChanged], Cause::Notification),
            (&[Heartbeat, AccessibilityChanged], Cause::Notification),
            (&[PrivacyChanged, Heartbeat], Cause::Notification),
        ];
        for (burst, expected) in cases {
            assert_eq!(cause_of(burst.iter().copied()), expected, "burst {burst:?}");
        }
    }

    #[test]
    fn the_thread_stops_sampling_once_the_receiver_is_gone() {
        let reads = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&reads);
        let rx = Poll {
            name: "openlogi-test-watcher",
            heartbeat: Duration::from_millis(1),
            degrades: "the test learns nothing",
        }
        .on_change(move |_| counter.fetch_add(1, Ordering::AcqRel));
        thread::sleep(Duration::from_millis(20));
        drop(rx);

        // The thread notices within one wake; allow a few for scheduling.
        thread::sleep(Duration::from_millis(50));
        let after_drop = reads.load(Ordering::Acquire);
        thread::sleep(Duration::from_millis(50));
        assert_eq!(
            reads.load(Ordering::Acquire),
            after_drop,
            "a watcher nobody listens to keeps sampling"
        );
    }

    #[test]
    fn a_constant_watcher_reports_once() {
        let mut rx = constant(true);
        assert_eq!(drain(&mut rx, 1), vec![true]);
        rx.try_recv().unwrap_err();
    }

    #[test]
    fn a_never_watcher_reports_nothing() {
        let mut rx = never::<bool>();
        rx.try_recv().unwrap_err();
    }
}
