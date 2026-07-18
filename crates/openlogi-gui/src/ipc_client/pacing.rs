use std::time::{Duration, Instant};

/// Which poll period the loop should run on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cadence {
    /// `STARTUP_POLL_PERIOD` — converging on a fresh agent.
    Fast,
    /// The configured steady `poll_period`.
    Steady,
}

pub struct Pacing {
    steady_period: Duration,
    fast_cap: Duration,
    mode: Cadence,
    /// When the current fast phase began (valid while `mode == Fast`).
    fast_since: Instant,
    /// The fast phase expired without readiness. Cleared by readiness, a
    /// disconnect, or the first delivery after an outage — each starts a
    /// genuinely new episode that deserves a fresh fast phase.
    capped: bool,
    /// Whether the previous tick delivered a snapshot, so the first
    /// delivery after an outage is recognizable.
    was_delivering: bool,
}

impl Pacing {
    pub fn new(steady_period: Duration, fast_cap: Duration, now: Instant) -> Self {
        Self {
            steady_period,
            fast_cap,
            mode: Cadence::Fast,
            fast_since: now,
            capped: false,
            was_delivering: false,
        }
    }

    pub fn steady_period(&self) -> Duration {
        self.steady_period
    }

    /// A snapshot was delivered. Ready → steady; not ready → fast until
    /// the cap, then steady.
    pub fn on_delivered(&mut self, ready: bool, now: Instant) -> Option<Cadence> {
        if !self.was_delivering {
            // First delivery after an outage: a just-(re)started agent
            // deserves a fresh fast phase regardless of how the outage
            // episode ended.
            self.capped = false;
            self.fast_since = now;
        }
        self.was_delivering = true;
        if ready {
            self.capped = false;
            return self.switch(Cadence::Steady, now);
        }
        if self.capped || self.expired(now) {
            self.capped = true;
            return self.switch(Cadence::Steady, now);
        }
        self.switch(Cadence::Fast, now)
    }

    /// No agent reachable this tick (and no live connection to lose).
    pub fn on_unreachable(&mut self, now: Instant) -> Option<Cadence> {
        self.was_delivering = false;
        if self.capped || self.expired(now) {
            self.capped = true;
            return self.switch(Cadence::Steady, now);
        }
        None
    }

    /// A live connection dropped — re-converge fast, fresh phase.
    pub fn on_disconnect(&mut self, now: Instant) -> Option<Cadence> {
        self.was_delivering = false;
        self.capped = false;
        self.switch(Cadence::Fast, now)
    }

    /// The agent speaks a newer protocol: only a GUI relaunch resolves
    /// it, so fast polling buys nothing.
    pub fn on_newer_agent(&mut self, now: Instant) -> Option<Cadence> {
        self.was_delivering = false;
        self.capped = true;
        self.switch(Cadence::Steady, now)
    }

    fn expired(&self, now: Instant) -> bool {
        self.mode == Cadence::Fast && now.duration_since(self.fast_since) >= self.fast_cap
    }

    fn switch(&mut self, to: Cadence, now: Instant) -> Option<Cadence> {
        if self.mode == to {
            return None;
        }
        if to == Cadence::Fast {
            self.fast_since = now;
        }
        self.mode = to;
        Some(to)
    }
}

#[cfg(test)]
mod tests {
    use super::{Cadence, Pacing};
    use std::time::{Duration, Instant};

    const STEADY: Duration = Duration::from_secs(2);
    const CAP: Duration = Duration::from_secs(15);

    fn pacing(now: Instant) -> Pacing {
        Pacing::new(STEADY, CAP, now)
    }

    #[test]
    fn readiness_settles_to_steady_and_disconnect_rearms_fast() {
        let t0 = Instant::now();
        let mut p = pacing(t0);
        assert_eq!(p.on_delivered(false, t0), None); // already fast
        assert_eq!(p.on_delivered(true, t0), Some(Cadence::Steady));
        assert_eq!(p.on_delivered(true, t0 + STEADY), None);
        assert_eq!(p.on_disconnect(t0 + STEADY * 2), Some(Cadence::Fast));
    }

    #[test]
    fn never_ready_falls_back_to_steady_after_the_cap() {
        let t0 = Instant::now();
        let mut p = pacing(t0);
        // The first delivery opens the fast phase; the cap counts from it.
        assert_eq!(p.on_delivered(false, t0), None);
        assert_eq!(p.on_delivered(false, t0 + CAP / 2), None);
        assert_eq!(p.on_delivered(false, t0 + CAP), Some(Cadence::Steady));
        // Capped: further not-ready deliveries stay steady.
        assert_eq!(p.on_delivered(false, t0 + CAP + STEADY), None);
        // …but readiness still lands (and stays steady).
        assert_eq!(p.on_delivered(true, t0 + CAP + STEADY * 2), None);
    }

    #[test]
    fn unreachable_episode_caps_and_a_new_agent_gets_a_fresh_fast_phase() {
        let t0 = Instant::now();
        let mut p = pacing(t0);
        assert_eq!(p.on_unreachable(t0 + Duration::from_secs(1)), None);
        assert_eq!(p.on_unreachable(t0 + CAP), Some(Cadence::Steady));
        // An agent finally comes up, still scanning: fresh fast phase
        // despite the cap from the outage episode.
        assert_eq!(
            p.on_delivered(false, t0 + CAP + STEADY),
            Some(Cadence::Fast)
        );
        assert_eq!(
            p.on_delivered(true, t0 + CAP + STEADY * 2),
            Some(Cadence::Steady)
        );
    }

    #[test]
    fn newer_agent_goes_steady_immediately() {
        let t0 = Instant::now();
        let mut p = pacing(t0);
        assert_eq!(p.on_newer_agent(t0), Some(Cadence::Steady));
        assert_eq!(p.on_unreachable(t0 + STEADY), None); // stays steady
    }
}
