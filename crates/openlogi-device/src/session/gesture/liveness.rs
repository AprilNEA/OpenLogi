//! Event-aware liveness state for the gesture capture channel.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use tokio::sync::Notify;
use tokio::time::Instant;

/// A channel must be wholly idle for this long before capture probes it.
pub(super) const IDLE_INTERVAL: Duration = Duration::from_secs(20);

/// Consecutive all-silent probes after which capture replaces the channel.
const SILENT_STRIKES_BEFORE_RESTART: u8 = 2;

/// Coalescing signal that the HID++ read thread delivered at least one report.
///
/// Recording activity is one atomic increment plus a notification: routine
/// captured input never takes a lock, blocks the read thread, or builds an
/// unbounded queue. The generation preserves activity that races waiter setup;
/// `Notify` only avoids waiting for the next generation change.
#[derive(Default)]
pub(super) struct ChannelActivity {
    generation: AtomicU64,
    changed: Notify,
}

impl ChannelActivity {
    /// Record one or more inbound reports as a new delivery generation.
    pub(super) fn record(&self) {
        self.generation.fetch_add(1, Ordering::Release);
        self.changed.notify_one();
    }

    /// Return the latest coalesced activity generation.
    pub(super) fn generation(&self) -> u64 {
        self.generation.load(Ordering::Acquire)
    }

    /// Wait until activity differs from `observed`, including activity that
    /// lands immediately before or during waiter registration.
    pub(super) async fn changed_after(&self, observed: u64) -> u64 {
        loop {
            let notified = self.changed.notified();
            tokio::pin!(notified);

            // Register before checking the generation. An activity report
            // between these operations either changes the generation or makes
            // this already-registered future ready, so no wakeup is lost.
            let _ = notified.as_mut().enable();
            let current = self.generation();
            if current != observed {
                return current;
            }
            notified.await;
        }
    }
}

/// Whether one completed probe observed delivery on the capture channel.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum PingOutcome {
    /// A response arrived, regardless of whether it was a pong or HID++ error.
    Delivered,
    /// Neither a response nor any other report arrived before the timeout.
    AllSilent,
    /// The channel failed before delivery could be established.
    ChannelFailed,
}

/// What capture should do after accounting for a completed probe.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum LivenessDecision {
    Continue,
    Restart,
}

/// Pure deadline and strike state for one capture channel.
pub(super) struct CaptureLiveness {
    activity_generation: u64,
    idle_deadline: Instant,
    silent_strikes: u8,
}

impl CaptureLiveness {
    pub(super) fn new(now: Instant, activity_generation: u64) -> Self {
        Self {
            activity_generation,
            idle_deadline: now + IDLE_INTERVAL,
            silent_strikes: 0,
        }
    }

    pub(super) fn activity_generation(&self) -> u64 {
        self.activity_generation
    }

    pub(super) fn idle_deadline(&self) -> Instant {
        self.idle_deadline
    }

    /// Account for delivered reports and defer probing until another complete
    /// idle interval has elapsed. Any delivery also clears a silent strike.
    pub(super) fn record_activity(&mut self, now: Instant, generation: u64) {
        let _ = self.take_activity(now, generation);
    }

    /// Re-check activity when the current timer expires. This closes the race
    /// where a report lands as the deadline becomes ready.
    pub(super) fn ping_due(&mut self, now: Instant, generation: u64) -> bool {
        !self.take_activity(now, generation) && now >= self.idle_deadline
    }

    /// Account for a completed probe and schedule the next one after another
    /// full interval. Activity racing an all-silent result proves delivery and
    /// wins over the strike.
    pub(super) fn finish_ping(
        &mut self,
        now: Instant,
        generation: u64,
        outcome: PingOutcome,
    ) -> LivenessDecision {
        let activity = self.take_activity(now, generation);
        self.idle_deadline = now + IDLE_INTERVAL;
        if outcome == PingOutcome::ChannelFailed {
            return LivenessDecision::Restart;
        }
        if activity || outcome == PingOutcome::Delivered {
            self.silent_strikes = 0;
            return LivenessDecision::Continue;
        }

        self.silent_strikes = self.silent_strikes.saturating_add(1);
        if self.silent_strikes >= SILENT_STRIKES_BEFORE_RESTART {
            LivenessDecision::Restart
        } else {
            LivenessDecision::Continue
        }
    }

    fn take_activity(&mut self, now: Instant, generation: u64) -> bool {
        if generation == self.activity_generation {
            return false;
        }
        self.activity_generation = generation;
        self.idle_deadline = now + IDLE_INTERVAL;
        self.silent_strikes = 0;
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test(start_paused = true)]
    async fn active_traffic_defers_the_idle_ping() {
        let activity = ChannelActivity::default();
        let mut liveness = CaptureLiveness::new(Instant::now(), activity.generation());
        let almost_idle = IDLE_INTERVAL
            .checked_sub(Duration::from_secs(1))
            .expect("the test offset is shorter than the idle interval");

        tokio::time::advance(almost_idle).await;
        activity.record();
        liveness.record_activity(Instant::now(), activity.generation());
        tokio::time::advance(Duration::from_secs(1)).await;

        assert!(!liveness.ping_due(Instant::now(), activity.generation()));
        tokio::time::advance(almost_idle).await;
        assert!(liveness.ping_due(Instant::now(), activity.generation()));
    }

    #[tokio::test(start_paused = true)]
    async fn idle_ping_is_scheduled_after_each_full_interval() {
        let activity = ChannelActivity::default();
        let mut liveness = CaptureLiveness::new(Instant::now(), activity.generation());
        let almost_idle = IDLE_INTERVAL
            .checked_sub(Duration::from_millis(1))
            .expect("the test offset is shorter than the idle interval");

        tokio::time::advance(almost_idle).await;
        assert!(!liveness.ping_due(Instant::now(), activity.generation()));
        tokio::time::advance(Duration::from_millis(1)).await;
        assert!(liveness.ping_due(Instant::now(), activity.generation()));
        assert!(matches!(
            liveness.finish_ping(
                Instant::now(),
                activity.generation(),
                PingOutcome::AllSilent,
            ),
            LivenessDecision::Continue
        ));

        tokio::time::advance(almost_idle).await;
        assert!(!liveness.ping_due(Instant::now(), activity.generation()));
        tokio::time::advance(Duration::from_millis(1)).await;
        assert!(liveness.ping_due(Instant::now(), activity.generation()));
    }

    #[tokio::test(start_paused = true)]
    async fn delivery_resets_a_silent_strike() {
        let activity = ChannelActivity::default();
        let mut liveness = CaptureLiveness::new(Instant::now(), activity.generation());

        tokio::time::advance(IDLE_INTERVAL).await;
        assert!(matches!(
            liveness.finish_ping(
                Instant::now(),
                activity.generation(),
                PingOutcome::AllSilent,
            ),
            LivenessDecision::Continue
        ));

        tokio::time::advance(IDLE_INTERVAL).await;
        assert!(matches!(
            liveness.finish_ping(
                Instant::now(),
                activity.generation(),
                PingOutcome::Delivered,
            ),
            LivenessDecision::Continue
        ));

        tokio::time::advance(IDLE_INTERVAL).await;
        assert!(matches!(
            liveness.finish_ping(
                Instant::now(),
                activity.generation(),
                PingOutcome::AllSilent,
            ),
            LivenessDecision::Continue
        ));
        tokio::time::advance(IDLE_INTERVAL).await;
        assert!(matches!(
            liveness.finish_ping(
                Instant::now(),
                activity.generation(),
                PingOutcome::AllSilent,
            ),
            LivenessDecision::Restart
        ));
    }

    #[tokio::test(start_paused = true)]
    async fn channel_failure_restarts_without_a_second_probe() {
        let activity = ChannelActivity::default();
        let mut liveness = CaptureLiveness::new(Instant::now(), activity.generation());

        tokio::time::advance(IDLE_INTERVAL).await;
        assert!(matches!(
            liveness.finish_ping(
                Instant::now(),
                activity.generation(),
                PingOutcome::ChannelFailed,
            ),
            LivenessDecision::Restart
        ));
    }

    #[tokio::test(start_paused = true)]
    async fn activity_before_wait_registration_is_not_lost() {
        let activity = ChannelActivity::default();
        activity.record();

        let generation = tokio::time::timeout(Duration::from_secs(1), activity.changed_after(0))
            .await
            .expect("pre-existing activity must make the waiter ready");

        assert_eq!(generation, activity.generation());
    }
}
