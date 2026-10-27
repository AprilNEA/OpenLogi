//! Input Monitoring permission watcher.
//!
//! macOS answers a running process's Input Monitoring check from what that
//! process saw when it first asked. A grant the user makes in System Settings
//! after the agent's launch prompt therefore reads as missing for the rest of
//! the agent's life, and `IOHIDDeviceOpen` keeps failing with it (#1304) —
//! re-reading in this process cannot observe it. A process started after the
//! grant does see it, so while this process lacks the grant each sample also
//! asks a newly started one, and the agent relaunches once that one holds it.

use std::time::{Duration, Instant};

use tokio::sync::mpsc;

use super::poll::{self, Cause, Poll};

/// How often the heartbeat alone may ask a fresh process. Each probe launches
/// a process, and an agent that is never granted Input Monitoring would
/// otherwise launch one every heartbeat for as long as it runs. A privacy
/// notification, which a grant edit posts, still asks at once.
const PROBE_HEARTBEAT: Duration = Duration::from_secs(60);

/// Where the agent stands with Input Monitoring.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InputMonitoring {
    /// This process holds the grant and can open HID devices.
    Held,
    /// The grant exists, but this process asked before it was made, and macOS
    /// applies it only to a process started after it. Only a relaunched
    /// agent can open HID devices.
    NeedsRelaunch,
    /// No grant.
    Missing,
}

impl InputMonitoring {
    /// Whether this process holds the grant — what
    /// `AgentStatus::input_monitoring_granted` reports.
    #[must_use]
    pub fn held(self) -> bool {
        matches!(self, Self::Held)
    }

    /// Classify one sample. `fresh` runs only when this process lacks the
    /// grant: it costs a process launch, and a process that holds the grant
    /// has nothing to gain from it.
    fn sample(held_here: bool, fresh: impl FnOnce() -> Option<bool>) -> Self {
        if held_here {
            return Self::Held;
        }
        match fresh() {
            Some(true) => Self::NeedsRelaunch,
            Some(false) | None => Self::Missing,
        }
    }
}

/// When the watcher last asked a fresh process, and what it answered.
#[derive(Debug, Default)]
struct Probes {
    last: Option<(Instant, Option<bool>)>,
}

impl Probes {
    /// The fresh process's answer for a sample with `cause` at `now`. It asks
    /// anew, except that a heartbeat within [`PROBE_HEARTBEAT`] of the last
    /// answer reuses that answer: repeating it keeps the reported state from
    /// flapping between probes.
    fn answer(
        &mut self,
        cause: Cause,
        now: Instant,
        ask: impl FnOnce() -> Option<bool>,
    ) -> Option<bool> {
        if let Some((at, answer)) = self.last
            && cause == Cause::Heartbeat
            && now.saturating_duration_since(at) < PROBE_HEARTBEAT
        {
            return answer;
        }
        let answer = ask();
        self.last = Some((now, answer));
        answer
    }
}

/// Watch the macOS Input Monitoring grant.
///
/// `fresh` reads the grant as a newly started agent would: `Some(granted)`,
/// or `None` when it could not tell, which counts as missing. It runs on the
/// watcher thread while this process lacks the grant — at the first sample,
/// at each privacy notification, and at most once a minute otherwise — so it
/// must return within a bounded time. Off macOS the grant is always held and
/// `fresh` never runs.
pub fn spawn<F>(heartbeat: Duration, fresh: F) -> mpsc::UnboundedReceiver<InputMonitoring>
where
    F: Fn() -> Option<bool> + Send + 'static,
{
    if !cfg!(target_os = "macos") {
        // Only macOS gates HID access behind a privacy grant.
        return poll::constant(InputMonitoring::Held);
    }
    let mut probes = Probes::default();
    Poll {
        name: "openlogi-input-monitoring-watcher",
        heartbeat,
        degrades: "the permission status won't auto-refresh",
    }
    .on_change(move |cause| {
        InputMonitoring::sample(openlogi_hid::permissions::has_access(), || {
            probes.answer(cause, Instant::now(), &fresh)
        })
    })
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::time::{Duration, Instant};

    use super::{Cause, InputMonitoring, PROBE_HEARTBEAT, Probes};

    #[test]
    fn a_sample_asks_a_fresh_process_only_without_the_grant() {
        assert_eq!(
            InputMonitoring::sample(true, || panic!("a held grant needs no fresh process")),
            InputMonitoring::Held
        );
        let cases = [
            (Some(true), InputMonitoring::NeedsRelaunch),
            (Some(false), InputMonitoring::Missing),
            // An unanswered probe must not relaunch the agent.
            (None, InputMonitoring::Missing),
        ];
        for (fresh, expected) in cases {
            assert_eq!(
                InputMonitoring::sample(false, || fresh),
                expected,
                "fresh process answered {fresh:?}"
            );
        }
    }

    #[test]
    fn only_a_held_grant_counts_as_held() {
        assert!(InputMonitoring::Held.held());
        assert!(!InputMonitoring::NeedsRelaunch.held());
        assert!(!InputMonitoring::Missing.held());
    }

    #[test]
    fn a_heartbeat_reuses_a_recent_answer_and_anything_else_asks() {
        let start = Instant::now();
        let asked = Cell::new(0);
        let mut probes = Probes::default();
        let mut answer = |cause, after: Duration, reply: Option<bool>| {
            probes.answer(cause, start + after, || {
                asked.set(asked.get() + 1);
                reply
            })
        };

        // A first heartbeat has nothing to reuse.
        assert_eq!(answer(Cause::Heartbeat, Duration::ZERO, None), None);
        assert_eq!(asked.get(), 1);
        // Within the period a heartbeat repeats the answer — even "unknown" —
        // without launching anything.
        let second = Duration::from_secs(1);
        let soon = PROBE_HEARTBEAT.saturating_sub(second);
        assert_eq!(answer(Cause::Heartbeat, soon, Some(true)), None);
        assert_eq!(asked.get(), 1);
        // A notification or a restart of the watcher always asks.
        assert_eq!(answer(Cause::Notification, soon, Some(true)), Some(true));
        assert_eq!(answer(Cause::Start, soon, Some(false)), Some(false));
        assert_eq!(asked.get(), 3);
        // The period counts from the latest answer, and expires on time.
        let latest = soon + PROBE_HEARTBEAT;
        assert_eq!(
            answer(Cause::Heartbeat, latest.saturating_sub(second), Some(true)),
            Some(false)
        );
        assert_eq!(asked.get(), 3);
        assert_eq!(answer(Cause::Heartbeat, latest, Some(true)), Some(true));
        assert_eq!(asked.get(), 4);
    }
}
