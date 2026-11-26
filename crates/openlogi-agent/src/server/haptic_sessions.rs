//! What the ring, client plays, and the lifecycle tell the haptic worker.
//!
//! One mutable owner for the sessions the worker has not looked at yet, the
//! open client burst, and whether the agent is leaving: its methods are the
//! only writers, so a burst cannot outlive a hand-back or open while the
//! agent is leaving. The worker in [`super::RingHapticPlayer`] takes a
//! snapshot with [`Sessions::take_news`] and does the I/O.

use openlogi_hid::DeviceRoute;
use tokio::sync::oneshot;
use tokio::time::Instant;

use super::haptic_restore::{Lane, Session};
use super::{BURST_IDLE, BURST_RENEW};

/// The haptic session state the trigger, the IPC handlers, the lifecycle,
/// and the worker share.
#[derive(Debug)]
pub(super) struct Sessions {
    /// The newest ring session that opened with haptic feedback, and its
    /// device, not yet seen by the worker.
    ring_opened: Option<(u64, DeviceRoute)>,
    /// The newest ring session that closed, not yet seen by the worker.
    ring_closed: Option<u64>,
    /// The newest client burst that went idle, not yet seen by the worker.
    burst_closed: Option<u64>,
    phase: Phase,
}

#[derive(Debug)]
enum Phase {
    /// Sessions arm the engine, and consecutive client plays on one device
    /// share `burst`.
    Running { burst: Option<Burst> },
    /// The agent is leaving or handing over to a replacement: no session
    /// arms, since an enable made now could not be handed back, and client
    /// plays are refused. `hand_back` asks the worker to hand every engine
    /// back, then answers.
    Paused {
        hand_back: Option<oneshot::Sender<()>>,
    },
}

/// The haptic session consecutive client plays on one device share.
#[derive(Debug)]
struct Burst {
    session: u64,
    route: DeviceRoute,
    opened: Instant,
    last: Instant,
}

/// A client play refused because the agent is leaving.
#[derive(Debug, PartialEq, Eq)]
pub(super) struct Paused;

/// What the worker has not looked at yet, taken in one snapshot.
#[derive(Debug, Default)]
pub(super) struct News {
    pub(super) ring_opened: Option<(Session, DeviceRoute)>,
    pub(super) ring_closed: Option<Session>,
    pub(super) burst_closed: Option<Session>,
    /// No session arms, and no client play plays.
    pub(super) paused: bool,
    /// Hand every engine back now, then answer here.
    pub(super) hand_back: Option<oneshot::Sender<()>>,
}

impl Default for Sessions {
    fn default() -> Self {
        Self {
            ring_opened: None,
            ring_closed: None,
            burst_closed: None,
            phase: Phase::Running { burst: None },
        }
    }
}

impl Sessions {
    /// Ring session `session` opened on `route`.
    pub(super) fn begin_ring(&mut self, session: u64, route: DeviceRoute) {
        self.ring_opened = Some((session, route));
    }

    /// Ring session `session` closed, and with it any older ring session.
    pub(super) fn end_ring(&mut self, session: u64) {
        self.ring_closed = self.ring_closed.max(Some(session));
    }

    /// The burst session a client play on `route` at `now` belongs to.
    ///
    /// It joins the open burst on `route` unless that burst is
    /// [`BURST_RENEW`] old; otherwise it opens a new one with an ID from
    /// `new_session`, which replaces any burst open on another device.
    pub(super) fn join_burst(
        &mut self,
        route: &DeviceRoute,
        now: Instant,
        new_session: impl FnOnce() -> u64,
    ) -> Result<u64, Paused> {
        let Phase::Running { burst } = &mut self.phase else {
            return Err(Paused);
        };
        if let Some(open) = burst
            .as_mut()
            .filter(|open| open.route == *route && now < open.opened + BURST_RENEW)
        {
            open.last = now;
            return Ok(open.session);
        }
        let session = new_session();
        *burst = Some(Burst {
            session,
            route: route.clone(),
            opened: now,
            last: now,
        });
        Ok(session)
    }

    /// Close burst `session` if no play joined it for [`BURST_IDLE`] up to
    /// `now`. Returns whether it closed, so the worker hands its engine back.
    pub(super) fn close_idle_burst(&mut self, session: u64, now: Instant) -> bool {
        let Phase::Running { burst } = &mut self.phase else {
            return false;
        };
        if !burst
            .as_ref()
            .is_some_and(|open| open.session == session && now - open.last >= BURST_IDLE)
        {
            return false;
        }
        *burst = None;
        self.burst_closed = self.burst_closed.max(Some(session));
        true
    }

    /// The agent is leaving or handing over: end the burst, arm nothing, and
    /// ask the worker to hand every engine back, answering on `done`.
    pub(super) fn finish(&mut self, done: oneshot::Sender<()>) {
        self.phase = Phase::Paused {
            hand_back: Some(done),
        };
    }

    /// The agent stays after [`Self::finish`]: sessions arm again, and the
    /// next client play opens a new burst. A hand-back the worker has not
    /// taken yet is dropped with the pause: it would end sessions that open
    /// from now on.
    pub(super) fn resume(&mut self) {
        self.phase = Phase::Running { burst: None };
    }

    /// Take everything the worker has not looked at yet.
    pub(super) fn take_news(&mut self) -> News {
        let (paused, hand_back) = match &mut self.phase {
            Phase::Running { .. } => (false, None),
            Phase::Paused { hand_back } => (true, hand_back.take()),
        };
        let ring = |id| Session {
            lane: Lane::Ring,
            id,
        };
        News {
            ring_opened: self.ring_opened.take().map(|(id, route)| (ring(id), route)),
            ring_closed: self.ring_closed.take().map(ring),
            burst_closed: self.burst_closed.take().map(|id| Session {
                lane: Lane::Client,
                id,
            }),
            paused,
            hand_back,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    fn mouse() -> DeviceRoute {
        DeviceRoute::Direct {
            vendor_id: 0x046d,
            product_id: 0xb042,
        }
    }

    fn other_mouse() -> DeviceRoute {
        DeviceRoute::Direct {
            vendor_id: 0x046d,
            product_id: 0xb034,
        }
    }

    #[test]
    fn plays_on_one_device_share_a_burst_until_it_renews_or_moves() {
        let start = Instant::now();
        let mut sessions = Sessions::default();
        let mut ids = 1..;
        let mut join = |sessions: &mut Sessions, route, at| {
            sessions.join_burst(&route, at, || ids.next().expect("an ID"))
        };
        assert_eq!(join(&mut sessions, mouse(), start), Ok(1));
        assert_eq!(
            join(&mut sessions, mouse(), start + Duration::from_secs(1)),
            Ok(1)
        );
        assert_eq!(
            join(&mut sessions, mouse(), start + BURST_RENEW),
            Ok(2),
            "renewed"
        );
        assert_eq!(
            join(&mut sessions, other_mouse(), start + BURST_RENEW),
            Ok(3),
            "another device opens its own"
        );
    }

    #[test]
    fn a_burst_closes_only_once_idle_and_only_as_itself() {
        let start = Instant::now();
        let mut sessions = Sessions::default();
        assert_eq!(sessions.join_burst(&mouse(), start, || 1), Ok(1));
        let joined = start + Duration::from_secs(1);
        assert_eq!(sessions.join_burst(&mouse(), joined, || 2), Ok(1));
        assert!(
            !sessions.close_idle_burst(1, start + BURST_IDLE),
            "joined since"
        );
        assert!(
            !sessions.close_idle_burst(9, joined + BURST_IDLE),
            "not this burst"
        );
        assert!(sessions.close_idle_burst(1, joined + BURST_IDLE));
        assert_eq!(
            sessions.take_news().burst_closed,
            Some(Session {
                lane: Lane::Client,
                id: 1
            })
        );
        assert!(
            !sessions.close_idle_burst(1, joined + BURST_IDLE),
            "closed once"
        );
    }

    #[test]
    fn a_hand_back_ends_the_burst_and_refuses_client_plays_until_resumed() {
        let start = Instant::now();
        let mut sessions = Sessions::default();
        assert_eq!(sessions.join_burst(&mouse(), start, || 1), Ok(1));
        let (done, _answer) = oneshot::channel();
        sessions.finish(done);
        assert_eq!(sessions.join_burst(&mouse(), start, || 2), Err(Paused));
        assert!(
            !sessions.close_idle_burst(1, start + BURST_IDLE),
            "already ended"
        );
        let news = sessions.take_news();
        assert!(news.paused);
        assert!(
            news.hand_back.is_some(),
            "the worker hears the hand-back once"
        );
        assert!(sessions.take_news().hand_back.is_none());

        sessions.resume();
        assert!(!sessions.take_news().paused);
        assert_eq!(
            sessions.join_burst(&mouse(), start, || 3),
            Ok(3),
            "the burst from before the hand-back is gone"
        );
    }
}
