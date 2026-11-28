//! Accessibility permission watcher.

use std::time::Duration;

use openlogi_hook::Hook;
use tokio::sync::mpsc;

use super::poll::{self, Cause, Poll};

/// The heartbeats a denied grant waits before the watcher probes again.
/// Recovery must not wait forever for a notification that was missed, and
/// every retry of a still-denied probe leaks a throwaway event tap's worth
/// (#2623) — so the retry is the shortest wait that keeps the leak at twelve
/// probes an hour at the 5 s heartbeat, against 720 for one per heartbeat.
const DENIED_RETRY_TICKS: u32 = 60;

/// Where the watcher stands with the grant. The point of the state, not just
/// two booleans: `Granted` is reachable only through a successful full probe,
/// and every read of the trust bit that says "false" demotes out of it, so no
/// caller can report a grant the tap probe never confirmed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum GrantState {
    /// The trust bit read `false`. Nothing is granted, and nothing is probed.
    Untrusted,
    /// The trust bit reads `true` but the last full probe was denied — the
    /// stale-`true` shape a row deletion leaves (#674), or a grant that
    /// failed some other way. A notification or the bounded retry probes.
    ProbeDenied,
    /// The last full probe passed.
    Granted,
}

/// The two reads a sample is made of, split out so the state machine is
/// testable without a live hook: the cheap bit and the costly truth.
trait GrantReads {
    /// The TCC trust bit. Free, but stale `true` after a row deletion (#674).
    fn trust(&mut self) -> bool;

    /// The full grant check. Proves the live tap capability by creating a
    /// throwaway event tap — which leaks while denied (#2623), so it runs
    /// only when [`GrantSampler`] has a reason.
    fn probe(&mut self) -> bool;
}

/// The production reads.
struct HookReads;

impl GrantReads for HookReads {
    fn trust(&mut self) -> bool {
        Hook::accessibility_trusted()
    }

    fn probe(&mut self) -> bool {
        Hook::has_accessibility()
    }
}

/// The watcher's whole memory, and the only place [`GrantState`] changes:
/// every wake goes through [`Self::sample`], so the cost rules — no probe
/// without a reason, no `Granted` without a passing probe — hold in one
/// function instead of in the discipline of its callers.
struct GrantSampler<R> {
    state: GrantState,
    reads: R,
    /// Heartbeats left before a [`GrantState::ProbeDenied`] earns a retry.
    denied_ticks: u32,
}

impl<R: GrantReads> GrantSampler<R> {
    fn new(reads: R) -> Self {
        Self {
            state: GrantState::Untrusted,
            reads,
            denied_ticks: 0,
        }
    }

    /// Take one sample for a wake and return what to report downstream.
    fn sample(&mut self, cause: Cause) -> bool {
        match cause {
            // The watcher starting and a privacy notification are the two
            // moments an edit may just have happened: check everything.
            Cause::Start | Cause::Notification => self.full_check(),
            Cause::Heartbeat => self.heartbeat(),
        }
    }

    /// The trust bit and, only when it is `true`, the tap probe. A `false`
    /// bit is authoritative — no tap installs without it — so it settles the
    /// sample without ever reaching the probe.
    fn full_check(&mut self) -> bool {
        if !self.reads.trust() {
            self.state = GrantState::Untrusted;
            return false;
        }
        self.probe()
    }

    /// Run the full check's second half and record its verdict.
    fn probe(&mut self) -> bool {
        if self.reads.probe() {
            self.state = GrantState::Granted;
            true
        } else {
            self.state = GrantState::ProbeDenied;
            self.denied_ticks = DENIED_RETRY_TICKS;
            false
        }
    }

    /// A heartbeat: at most the cheap bit, unless the state says the last
    /// truth is too old to repeat.
    fn heartbeat(&mut self) -> bool {
        match self.state {
            GrantState::Untrusted => {
                // The trust bit's rising edge is a grant that was just made;
                // confirm it now so the gate dismisses without waiting for a
                // notification that may not arrive.
                if self.reads.trust() {
                    self.probe()
                } else {
                    false
                }
            }
            GrantState::Granted => {
                if self.reads.trust() {
                    // Still trusted; only a notification or a falling bit
                    // can question a grant the probe already passed.
                    true
                } else {
                    self.state = GrantState::Untrusted;
                    false
                }
            }
            GrantState::ProbeDenied => {
                if !self.reads.trust() {
                    self.state = GrantState::Untrusted;
                    return false;
                }
                if self.denied_ticks == 0 {
                    return self.probe();
                }
                self.denied_ticks -= 1;
                false
            }
        }
    }
}

/// Watch macOS Accessibility permission changes.
///
/// Read for as long as a hook is installed: an active event tap that outlives
/// its grant wedges system input until reboot, so the agent has to learn about
/// a revocation on its own (see `crates/openlogi-hook/AGENTS.md`). The hook's
/// own probe cue is the safety path; this feeds the status clients see.
///
/// The full grant check (`Hook::has_accessibility`) proves the live tap
/// capability by creating a throwaway event tap, and creating a *denied* tap
/// leaks inside CoreGraphics/TCC — a probe every heartbeat grew the agent past
/// 3 GB in days (#2623). So `GrantSampler` probes only on start, on a privacy
/// notification, on the trust bit's rising edge, and — while the bit reads
/// `true` but the grant is denied — once every `DENIED_RETRY_TICKS`
/// heartbeats. The retry is what makes recovery not depend on notifications:
/// the trust bit stays `true` across a row deletion (#674), so a user who
/// deletes the row and re-grants without either edit's notification reaching
/// this watcher would otherwise be told "denied" forever.
#[must_use]
pub fn spawn(heartbeat: Duration) -> mpsc::UnboundedReceiver<bool> {
    if !cfg!(target_os = "macos") {
        // Linux and Windows gate the hook below the privacy layer, so there is
        // nothing here that can change.
        return poll::constant(true);
    }
    Poll {
        name: "openlogi-accessibility-watcher",
        heartbeat,
        degrades: "the permission gate won't auto-dismiss",
    }
    .on_change({
        let mut sampler = GrantSampler::new(HookReads);
        move |cause| sampler.sample(cause)
    })
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;

    use super::*;

    /// Scripted reads that count the probes, so a test can prove the state
    /// machine's cost and not only its answers. An unplanned probe — one the
    /// script has no value for — panics, which is the point: the leak fix is
    /// that denied heartbeats never create taps.
    struct FakeReads {
        trust: VecDeque<bool>,
        /// Once the script runs out the bit holds this, matching a grant the
        /// user leaves alone.
        trust_last: bool,
        probe: VecDeque<bool>,
        probes: usize,
    }

    impl FakeReads {
        fn new(
            trust: impl IntoIterator<Item = bool>,
            probe: impl IntoIterator<Item = bool>,
        ) -> Self {
            let trust: VecDeque<bool> = trust.into_iter().collect();
            Self {
                trust_last: *trust.back().unwrap_or(&false),
                trust,
                probe: probe.into_iter().collect(),
                probes: 0,
            }
        }
    }

    impl GrantReads for FakeReads {
        fn trust(&mut self) -> bool {
            match self.trust.pop_front() {
                Some(value) => {
                    self.trust_last = value;
                    value
                }
                None => self.trust_last,
            }
        }

        fn probe(&mut self) -> bool {
            self.probes += 1;
            self.probe
                .pop_front()
                .expect("a denied state must not probe on the heartbeat")
        }
    }

    #[test]
    fn denied_heartbeats_do_not_create_taps() {
        // The counter-proof of the leak fix: with the pre-fix reader restored
        // — a probe on every heartbeat, whatever the bit says — the empty
        // probe script below turns the first denied heartbeat into a panic.
        let mut sampler = GrantSampler::new(FakeReads::new([false; 16], []));
        assert!(!sampler.sample(Cause::Start));
        for _ in 0..15 {
            assert!(!sampler.sample(Cause::Heartbeat));
        }
        assert_eq!(sampler.reads.probes, 0);
    }

    #[test]
    fn a_rising_trust_edge_probes_once() {
        let mut sampler = GrantSampler::new(FakeReads::new([false, true, true], [true]));
        assert!(!sampler.sample(Cause::Start));
        // The bit flips: confirm the grant once.
        assert!(sampler.sample(Cause::Heartbeat));
        // Held heartbeats repeat `Granted` without probing again.
        assert!(sampler.sample(Cause::Heartbeat));
        assert_eq!(sampler.reads.probes, 1);
    }

    #[test]
    fn a_notification_re_probes_a_granted_watcher() {
        let mut sampler = GrantSampler::new(FakeReads::new([true, true], [true, true]));
        assert!(sampler.sample(Cause::Start));
        assert!(sampler.sample(Cause::Notification));
        assert_eq!(sampler.reads.probes, 2);
    }

    #[test]
    fn delete_then_regrant_without_notification_recovers() {
        // Delete the row: the edit's notification arrives, the bit is stale
        // `true` (#674), the probe says denied. Re-grant: no notification
        // reaches the watcher at all. The bounded retry is the only path
        // back to `Granted`.
        let mut sampler = GrantSampler::new(FakeReads::new([true], [true, false, false, true]));
        assert!(sampler.sample(Cause::Start));
        assert!(!sampler.sample(Cause::Notification));
        assert_eq!(sampler.state, GrantState::ProbeDenied);
        // The denied heartbeats in between cost nothing.
        for _ in 0..DENIED_RETRY_TICKS {
            assert!(!sampler.sample(Cause::Heartbeat));
        }
        assert_eq!(sampler.reads.probes, 2);
        // The first retry: still deleted.
        assert!(!sampler.sample(Cause::Heartbeat));
        assert_eq!(sampler.reads.probes, 3);
        assert_eq!(sampler.state, GrantState::ProbeDenied);
        for _ in 0..DENIED_RETRY_TICKS {
            assert!(!sampler.sample(Cause::Heartbeat));
        }
        // The second retry finds the re-grant.
        assert!(sampler.sample(Cause::Heartbeat));
        assert_eq!(sampler.state, GrantState::Granted);
    }

    #[test]
    fn a_false_trust_read_never_reports_granted() {
        // Even a probe that would say "granted" must not be consulted, let
        // alone reported, against a `false` bit: refused edge by construction.
        let mut sampler = GrantSampler::new(FakeReads::new([false], [true]));
        assert!(!sampler.sample(Cause::Notification));
        assert_eq!(sampler.state, GrantState::Untrusted);
        assert_eq!(sampler.reads.probes, 0);
    }

    #[test]
    fn a_failed_probe_never_promotes() {
        let mut sampler = GrantSampler::new(FakeReads::new([true], [false]));
        assert!(!sampler.sample(Cause::Start));
        assert_eq!(sampler.state, GrantState::ProbeDenied);
        // Before the retry is due, heartbeats hold the denial without probing.
        for _ in 0..DENIED_RETRY_TICKS {
            assert!(!sampler.sample(Cause::Heartbeat));
        }
        assert_eq!(sampler.reads.probes, 1);
    }

    #[test]
    fn a_falling_bit_demotes_without_a_probe() {
        let mut sampler = GrantSampler::new(FakeReads::new([true, false, true], [true, true]));
        assert!(sampler.sample(Cause::Start));
        // A plain toggle-off drops the bit: report denied from the cheap read.
        assert!(!sampler.sample(Cause::Heartbeat));
        assert_eq!(sampler.state, GrantState::Untrusted);
        // And the rising edge back costs its one probe.
        assert!(sampler.sample(Cause::Heartbeat));
        assert_eq!(sampler.reads.probes, 2);
    }
}
