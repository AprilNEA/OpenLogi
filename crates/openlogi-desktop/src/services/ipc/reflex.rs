//! The spawn reflex: when the loop may bring the agent up itself.
//!
//! The agent is normally started by launchd; the GUI is the responder of last
//! resort when the socket stays down. The rule is all timing and no I/O,
//! driven by an explicit `now` so the tests can pin it, and reads the loop's
//! [`Link`] rather than mirroring it.

use std::time::{Duration, Instant};

use super::link::{Link, Outage};

/// Minimum gap between agent-launch attempts while the socket is unreachable.
/// Long enough that a missing or crash-looping binary can't be respawned in a
/// tight loop, short enough that a quit / crashed agent is recovered promptly.
pub(super) const SPAWN_RETRY_PERIOD: Duration = Duration::from_secs(30);

/// How long a *lost* connection must stay down before the reflex may fire.
/// Every cause of a warm loss has a better first responder — launchd's crash
/// respawn, the agent's self-exec on update, the tray-Quit deep link — and the
/// reflex waits them out (~8 reconnect attempts). A connection that never
/// existed has no first responder; the cold path fires on the first failure.
pub(super) const SPAWN_AFTER_LOSS: Duration = Duration::from_secs(2);

/// What the reflex remembers between turns: only when it last fired. Everything
/// else it decides on is the link's own state.
pub(super) struct SpawnReflex {
    last_fired: Option<Instant>,
}

impl SpawnReflex {
    pub(super) const fn new() -> Self {
        Self { last_fired: None }
    }

    /// The trigger rule: fire immediately while cold, wait out the first
    /// responders after a loss, never at a newer agent, at most once per
    /// [`SPAWN_RETRY_PERIOD`].
    pub(super) fn should_fire(&self, link: &Link, now: Instant) -> bool {
        let Link::Down(down) = link else {
            return false;
        };
        if down.agent_is_newer {
            return false;
        }
        let waited = match down.outage {
            Outage::Cold => true,
            Outage::Lost => now.saturating_duration_since(down.since) >= SPAWN_AFTER_LOSS,
        };
        waited
            && self
                .last_fired
                .is_none_or(|t| now.saturating_duration_since(t) >= SPAWN_RETRY_PERIOD)
    }

    pub(super) fn fired(&mut self, now: Instant) {
        self.last_fired = Some(now);
    }
}

#[cfg(test)]
mod tests {
    use openlogi_ipc::client::{ConnectError, ProtocolSkew};

    use super::super::link::Down;
    use super::*;

    #[test]
    fn a_never_reached_agent_is_spawned_immediately() {
        let t0 = Instant::now();
        assert!(SpawnReflex::new().should_fire(&Link::cold(t0), t0));
    }

    #[test]
    fn a_lost_connection_waits_out_the_first_responders() {
        // A supervised restart, a self-exec, or the quit deep link announce
        // themselves within the grace window; the reflex must not race them.
        let lost_at = Instant::now();
        let link = Link::Down(Down::new(Outage::Lost, lost_at));
        let reflex = SpawnReflex::new();
        assert!(!reflex.should_fire(&link, lost_at + Duration::from_secs(1)));
        assert!(reflex.should_fire(&link, lost_at + SPAWN_AFTER_LOSS));
    }

    #[test]
    fn a_live_newer_agent_is_never_spawned_at() {
        // Kickstart would no-op and a fresh copy exits as a duplicate; only
        // relaunching the GUI helps, so firing is pure churn.
        let t0 = Instant::now();
        let mut link = Link::cold(t0);
        let reflex = SpawnReflex::new();
        link.down_mut()
            .expect("cold is down")
            .connect_failed(&ConnectError::Skew(ProtocolSkew::AgentNewer {
                agent: u32::MAX,
            }));
        assert!(!reflex.should_fire(&link, t0 + Duration::from_secs(120)));

        // The newer agent going away (it was quit or replaced) re-arms the
        // reflex on the next failed attempt.
        link.down_mut()
            .expect("still down")
            .connect_failed(&std::io::Error::from(std::io::ErrorKind::ConnectionRefused).into());
        assert!(reflex.should_fire(&link, t0 + Duration::from_secs(120)));
    }

    #[test]
    fn retries_are_rate_limited() {
        let t0 = Instant::now();
        let link = Link::cold(t0);
        let mut reflex = SpawnReflex::new();
        reflex.fired(t0);
        assert!(!reflex.should_fire(&link, t0 + Duration::from_secs(29)));
        assert!(reflex.should_fire(&link, t0 + SPAWN_RETRY_PERIOD));
    }
}
